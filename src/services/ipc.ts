/**
 * Tauri invoke / Channel 封装。
 * 类型与 src-tauri DTO 保持同步（camelCase 由 serde rename_all 保证）。
 */
import { Channel, invoke } from "@tauri-apps/api/core";

/** 稳定事件标识（与 logscope-core `EventRef` 对应） */
export interface EventRef {
  fileId: number;
  sequenceNo: number;
}

export interface FileSummary {
  fileId: number;
  fileName: string;
  path: string;
  fileSize: number;
  encoding: string;
  profile: string | null;
  state: string;
  events: number;
  warnCount: number;
  errorCount: number;
  unknownCount: number;
  timeMinMs: number | null;
  timeMaxMs: number | null;
  truncatedEvents: number;
  durationMs: number;
}

export interface ImportProgress {
  taskId: string;
  file: string;
  phase:
    | "detect"
    | "indexing"
    | "done"
    | "cancelled"
    | "failed"
    | "file-done"
    | "all-done";
  bytesDone: number;
  bytesTotal: number;
  events: number;
  message: string | null;
}

export interface SearchProgress {
  searchId: string;
  phase: "running" | "done" | "cancelled" | "failed";
  matched: number;
  candidatesDone: number;
  candidatesTotal: number;
  scannedBytes: number;
  message: string | null;
}

export interface SearchParams {
  fileIds?: number[];
  timeRange?: [number, number];
  levels?: string[];
  text?: string;
  caseInsensitive?: boolean;
  regex?: boolean;
}

export interface EventDto {
  fileId: number;
  sequenceNo: number;
  timestampMs: number | null;
  level: string;
  truncated: boolean;
  unknown: boolean;
  byteLen: number;
  head: string;
}

export interface SearchPage {
  total: number;
  offset: number;
  returned: number;
  has_more: boolean;
  events: EventDto[];
  tzOffsetMin?: number;
}

/** 列出当前已导入文件 */
export async function listFiles(): Promise<FileSummary[]> {
  return invoke<FileSummary[]>("list_files");
}

/** 发起导入：立即返回任务 ID，进度经 Channel 推送 */
export function openFiles(
  paths: string[],
  onProgress: (p: ImportProgress) => void,
  settings: { profile?: string; encoding?: string; tzOffsetMin?: number } = {},
): Promise<string> {
  const ch = new Channel<ImportProgress>();
  ch.onmessage = onProgress;
  return invoke<string>("open_files", {
    paths,
    settings: {
      profile: settings.profile ?? null,
      encoding: settings.encoding ?? null,
      tzOffsetMin: settings.tzOffsetMin ?? null,
    },
    onProgress: ch,
  });
}

export async function cancelImport(taskId: string): Promise<boolean> {
  return invoke<boolean>("cancel_import", { taskId });
}

/** 发起检索（后台任务）：立即返回检索 ID，进度经 Channel 推送；完成后 searchPage 分页读取 */
export function searchLogs(
  params: SearchParams,
  onProgress: (p: SearchProgress) => void,
): Promise<string> {
  const ch = new Channel<SearchProgress>();
  ch.onmessage = onProgress;
  return invoke<string>("search_logs", { params, onProgress: ch });
}

export async function getSearchStatus(searchId: string): Promise<{
  searchId: string;
  status: string;
  total: number;
}> {
  return invoke("get_search_status", { searchId });
}

/** 游标分页读取检索结果 */
export async function searchPage(
  searchId: string,
  offset = 0,
  limit = 200,
): Promise<SearchPage> {
  return invoke<SearchPage>("search_page", { searchId, offset, limit });
}

export async function cancelSearch(searchId: string): Promise<boolean> {
  return invoke<boolean>("cancel_search", { searchId });
}

export async function closeSearch(searchId: string): Promise<boolean> {
  return invoke<boolean>("close_search", { searchId });
}

export interface TimelineDto {
  startMs: number;
  bucketMs: number;
  count: number;
  /** 每桶 [total, warn, error] */
  buckets: [number, number, number][];
  totalEvents: number;
}

export async function getTimeline(req: {
  fileIds?: number[];
  timeRange?: [number, number];
  bucketMs?: number;
}): Promise<TimelineDto> {
  return invoke<TimelineDto>("get_timeline", {
    req: {
      fileIds: req.fileIds ?? null,
      timeRange: req.timeRange ?? null,
      bucketMs: req.bucketMs ?? null,
    },
  });
}

export interface LogDetail {
  fileId: number;
  sequenceNo: number;
  timestampMs: number | null;
  level: string;
  truncated: boolean;
  unknown: boolean;
  badTimestamp: boolean;
  startOffset: number;
  endOffset: number;
  text: string;
}

export async function getLogDetail(fileId: number, sequenceNo: number): Promise<LogDetail> {
  return invoke<LogDetail>("get_log_detail", { fileId, sequenceNo });
}

export interface ContextItem {
  sequenceNo: number;
  timestampMs: number | null;
  level: string;
  text: string;
}

/** 上下文模式：thread = 同线程；physical = 同文件落盘顺序（无线程信息时自动回退） */
export type ContextMode = "thread" | "physical";

export interface ContextResult {
  mode: ContextMode;
  items: ContextItem[];
}

export async function getLogContext(
  fileId: number,
  sequenceNo: number,
  before = 20,
  after = 20,
  sameThread = false,
): Promise<ContextResult> {
  return invoke<ContextResult>("get_log_context", {
    fileId,
    sequenceNo,
    before,
    after,
    sameThread,
  });
}

export async function closeFiles(fileIds: number[]): Promise<number> {
  return invoke<number>("close_files", { fileIds });
}
