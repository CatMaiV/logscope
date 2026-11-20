/**
 * 工作区状态：文件导入、检索任务、时间范围、详情选择。
 * 数据全部来自 Rust 命令（services/ipc），本 store 不持有原始日志。
 */
import { defineStore } from "pinia";
import {
  cancelImport,
  cancelSearch,
  closeFiles,
  closeSearch,
  getSearchStatus,
  listFiles,
  openFiles,
  searchLogs,
  type FileSummary,
  type ImportProgress,
  type SearchParams,
  type SearchProgress,
} from "@/services/ipc";

export type SearchState = "idle" | "running" | "done" | "cancelled" | "failed";

/** 查询历史的可读标签 */
export function queryLabel(p: SearchParams): string {
  const parts: string[] = [];
  if (p.text) parts.push(p.regex ? `/${p.text}/` : `"${p.text}"`);
  if (p.levels?.length) parts.push(p.levels.join(",").toLowerCase());
  if (p.timeRange) parts.push("时间段");
  if (p.fileIds?.length) parts.push(`${p.fileIds.length} 个文件`);
  return parts.length ? parts.join(" · ") : "全部事件";
}

export const useWorkspace = defineStore("workspace", {
  state: () => ({
    files: [] as FileSummary[],
    /** 空数组 = 全部文件 */
    selectedFileIds: [] as number[],
    importing: false,
    importProgress: null as ImportProgress | null,
    importErrors: [] as string[],

    searchId: null as string | null,
    searchState: "idle" as SearchState,
    searchTotal: 0,
    searchProgress: null as SearchProgress | null,
    lastQuery: null as SearchParams | null,
    /** 会话内查询历史（计划 §4.4：最新在前，去重，上限 20） */
    queryHistory: [] as SearchParams[],

    /** 列表时间过滤（来自时间轴框选/点击）；null = 全时间 */
    timeRange: null as [number, number] | null,
    /** 时间轴基准：null = 全量时间轴；设置时显示"过滤结果"标记 */
    timelineScope: "full" as "full" | "filtered",

    detailRef: null as { fileId: number; seq: number } | null,
    /** 详情抽屉内上下文 */
    contextOpen: false,

    tzOffsetMin: 480,
  }),

  getters: {
    effectiveQuery(state): SearchParams {
      const q: SearchParams = {
        fileIds: state.selectedFileIds.length ? state.selectedFileIds : undefined,
        timeRange: state.timeRange ?? undefined,
        levels: state.lastQuery?.levels,
        text: state.lastQuery?.text,
        caseInsensitive: state.lastQuery?.caseInsensitive,
        regex: state.lastQuery?.regex,
      };
      return q;
    },
    busy(state): boolean {
      return state.importing || state.searchState === "running";
    },
  },

  actions: {
    async refreshFiles() {
      this.files = await listFiles();
      // 清掉已消失文件的选择
      const alive = new Set(this.files.map((f) => f.fileId));
      this.selectedFileIds = this.selectedFileIds.filter((id) => alive.has(id));
    },

    async importPaths(paths: string[], settings?: { customPattern?: string }) {
      if (!paths.length || this.importing) return;
      this.importing = true;
      this.importErrors = [];
      let finished = false;
      try {
        const done = new Promise<void>((resolve) => {
          const timer = setInterval(() => {
            if (finished) {
              clearInterval(timer);
              resolve();
            }
          }, 100);
        });
        void openFiles(
          paths,
          (p) => {
            this.importProgress = p;
            if (p.phase === "failed" && p.message) {
              this.importErrors.push(`${p.file}: ${p.message}`);
            }
            if (p.phase === "all-done" || p.phase === "cancelled") {
              finished = true;
            }
          },
          { customPattern: settings?.customPattern },
        ).catch((e) => {
          this.importErrors.push(String(e));
          finished = true;
        });
        await done;
        await this.refreshFiles();
        if (this.files.length > 0) {
          await this.runSearch(); // 导入完成自动加载列表
        }
      } finally {
        this.importing = false;
        this.importProgress = null;
      }
    },

    async cancelImportTask() {
      if (this.importProgress) await cancelImport(this.importProgress.taskId);
    },

    /** 发起（或用给定条件重跑）检索；自动接管进度 Channel */
    async runSearch(params?: SearchParams) {
      const q = params ?? this.effectiveQuery;
      this.lastQuery = q;

      // 释放上一个检索结果
      if (
        this.searchId &&
        (this.searchState === "running" || this.searchState === "done")
      ) {
        if (this.searchState === "running") await cancelSearch(this.searchId);
        await closeSearch(this.searchId).catch(() => undefined);
      }
      this.searchId = null;

      // 工作区为空：回到空闲态，不发检索请求；同时清除时间条件
      if (this.files.length === 0) {
        this.searchState = "idle";
        this.searchTotal = 0;
        this.searchProgress = null;
        this.timeRange = null;
        this.timelineScope = "full";
        return;
      }

      this.searchState = "running";
      this.searchTotal = 0;
      this.searchProgress = null;
      try {
        const id = await searchLogs(q, (p) => {
          this.searchProgress = p;
          if (p.phase === "done") this.searchTotal = p.matched;
          if (p.phase !== "running") this.searchState = p.phase as SearchState;
        });
        this.searchId = id;
        // 兜底轮询（Channel 丢失场景）
        if (this.searchState === "running") {
          for (let i = 0; i < 150; i++) {
            await sleep(200);
            if (this.searchState !== "running") break;
            try {
              const st = await getSearchStatus(id);
              if (st.status !== "running" && st.status !== "pending") {
                this.searchState = st.status as SearchState;
                if (st.status === "done") this.searchTotal = st.total;
                break;
              }
            } catch {
              break; // 任务已被替换
            }
          }
        }
        this.pushHistory(q);
      } catch (e) {
        this.searchState = "failed";
        this.searchProgress = {
          searchId: "",
          phase: "failed",
          matched: 0,
          candidatesDone: 0,
          candidatesTotal: 0,
          scannedBytes: 0,
          message: String(e),
        };
      }
    },

    async cancelRunningSearch() {
      if (this.searchId && this.searchState === "running") {
        await cancelSearch(this.searchId);
      }
    },

    async closeFile(fileId: number) {
      // 详情面板正指向该文件时先关闭，避免后续读取悬空引用
      if (this.detailRef?.fileId === fileId) this.closeDetail();
      try {
        await closeFiles([fileId]);
      } catch (e) {
        this.importErrors.push(`移除文件失败: ${e}`);
        return;
      }
      await this.refreshFiles();
      await this.runSearch();
    },

    /** 关闭全部文件（释放全部索引与句柄） */
    async closeAllFiles() {
      const ids = this.files.map((f) => f.fileId);
      if (!ids.length) return;
      this.closeDetail();
      try {
        await closeFiles(ids);
      } catch (e) {
        this.importErrors.push(`移除全部文件失败: ${e}`);
        return;
      }
      await this.refreshFiles();
      await this.runSearch();
    },

    /** 关闭单个文件并按自定义规则重新导入（M4） */
    async reimportWithPattern(file: FileSummary, customPattern: string) {
      await closeFiles([file.fileId]);
      await this.refreshFiles();
      await this.importPaths([file.path], { customPattern });
    },

    toggleFileSelected(fileId: number) {
      const i = this.selectedFileIds.indexOf(fileId);
      if (i >= 0) this.selectedFileIds.splice(i, 1);
      else this.selectedFileIds.push(fileId);
      void this.runSearch();
    },

    setTimeRange(range: [number, number] | null) {
      this.timeRange = range;
      this.timelineScope = range ? "filtered" : "full";
      void this.runSearch();
    },

    openDetail(fileId: number, seq: number) {
      this.detailRef = { fileId, seq: seq };
      this.contextOpen = false;
    },

    closeDetail() {
      this.detailRef = null;
      this.contextOpen = false;
    },

    pushHistory(q: SearchParams) {
      const label = queryLabel(q);
      const same = (a: SearchParams) => queryLabel(a) === label;
      if (!this.queryHistory.some(same)) {
        this.queryHistory.unshift(q);
        if (this.queryHistory.length > 20) this.queryHistory.pop();
      }
    },
  },
});

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
