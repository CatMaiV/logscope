<script setup lang="ts">
/**
 * 左侧文件面板：导入（对话框 + 拖拽提示）、文件卡片、勾选过滤、关闭。
 */
import { ref } from "vue";
import { storeToRefs } from "pinia";
import { open } from "@tauri-apps/plugin-dialog";
import { NButton, NCheckbox, NProgress, NScrollbar, NSpin, NTag } from "naive-ui";
import { useWorkspace } from "@/stores/workspace";
import type { ImportProgress } from "@/services/ipc";
import { fmtBytes, fmtMs } from "@/services/format";
import RuleModal from "@/components/RuleModal.vue";
import type { FileSummary } from "@/services/ipc";

const store = useWorkspace();
const { files, importing, importProgress, importErrors, selectedFileIds } = storeToRefs(store);

/** 解析规则弹窗 */
const ruleFile = ref<FileSummary | null>(null);
const ruleShow = ref(false);
function openRule(f: FileSummary) {
  ruleFile.value = f;
  ruleShow.value = true;
}

async function pickFiles() {
  const picked = await open({
    multiple: true,
    title: "选择日志文件",
    filters: [{ name: "日志", extensions: ["log", "txt"] }],
  });
  if (!picked) return;
  await store.importPaths(picked);
}

function pct(p: ImportProgress | null): number {
  if (!p || !p.bytesTotal) return 0;
  return Math.min(100, (p.bytesDone / p.bytesTotal) * 100);
}
</script>

<template>
  <aside class="file-panel">
    <div class="panel-head">
      <span class="title">文件（{{ files.length }}）</span>
      <div class="head-actions">
        <button
          v-if="files.length"
          type="button"
          class="close-all"
          title="移除全部文件并释放索引"
          @click="store.closeAllFiles()"
        >
          全部关闭
        </button>
        <NButton size="tiny" type="primary" :loading="importing" @click="pickFiles">
          导入
        </NButton>
      </div>
    </div>

    <div v-if="importing && importProgress" class="import-progress">
      <div class="ip-file" :title="importProgress.file">
        {{ importProgress.file.split(/[\\/]/).pop() }}
      </div>
      <NProgress
        type="line"
        :percentage="pct(importProgress)"
        :height="6"
        :show-indicator="false"
      />
      <div class="ip-meta">
        {{ fmtBytes(importProgress.bytesDone) }} / {{ fmtBytes(importProgress.bytesTotal) }}
        <NTag v-if="importProgress.events" size="tiny" :bordered="false">
          {{ importProgress.events }} 事件
        </NTag>
      </div>
    </div>

    <NScrollbar class="file-list">
      <div v-if="files.length === 0 && !importing" class="empty">
        <div class="empty-icon">📭</div>
        <p>拖入 .log / .txt 文件</p>
        <p class="dim">或点击「导入」选择</p>
      </div>

      <div
        v-for="f in files"
        :key="f.fileId"
        class="file-card"
        :class="{ active: selectedFileIds.includes(f.fileId) }"
      >
        <div class="fc-row1">
          <NCheckbox
            size="small"
            :checked="selectedFileIds.includes(f.fileId)"
            @update:checked="store.toggleFileSelected(f.fileId)"
          >
            <span class="fc-name" :title="f.path">{{ f.fileName }}</span>
          </NCheckbox>
          <button
            type="button"
            class="fc-close"
            title="移除该文件（释放索引）"
            @click.stop="store.closeFile(f.fileId)"
          >
            ✕
          </button>
        </div>
        <div class="fc-meta">
          {{ fmtBytes(f.fileSize) }} · {{ f.events.toLocaleString() }} 事件 · {{ f.encoding }}
        </div>
        <div v-if="f.errorCount || f.warnCount || f.unknownCount" class="fc-tags">
          <NTag v-if="f.errorCount" size="tiny" type="error" :bordered="false">
            ERROR {{ f.errorCount }}
          </NTag>
          <NTag v-if="f.warnCount" size="tiny" type="warning" :bordered="false">
            WARN {{ f.warnCount }}
          </NTag>
          <NTag v-if="f.unknownCount" size="tiny" :bordered="false">
            未识别 {{ f.unknownCount }}
          </NTag>
        </div>
        <div class="fc-footer">
          <span class="fc-profile" :title="f.profile ?? ''">{{ f.profile ?? "未知格式" }}</span>
          <button
            type="button"
            class="fc-rule"
            :class="{ warn: f.unknownCount > 0 || (f.profile ?? '').includes('未识别') }"
            title="自定义日志格式：编辑正则、预览识别结果后重新导入"
            @click.stop="openRule(f)"
          >
            自定义格式
          </button>
        </div>
        <div v-if="f.timeMinMs !== null" class="fc-time dim">
          {{ fmtMs(f.timeMinMs) }} ~ {{ fmtMs(f.timeMaxMs) }}
        </div>
      </div>

      <NSpin v-if="importing" size="small" class="import-spin" />
    </NScrollbar>

    <div v-if="importErrors.length" class="import-errors">
      <div v-for="(e, i) in importErrors.slice(0, 3)" :key="i" class="err-line" :title="e">
        ⚠ {{ e }}
      </div>
      <div v-if="importErrors.length > 3" class="dim">… 共 {{ importErrors.length }} 条错误</div>
    </div>
    <RuleModal v-model:show="ruleShow" :file="ruleFile" />
  </aside>
</template>

<style scoped>
.file-panel {
  width: 268px;
  min-width: 268px;
  display: flex;
  flex-direction: column;
  border-right: 1px solid var(--n-border-color, var(--border));
  background: var(--bg-panel);
}
.panel-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 10px 12px 6px;
}
.title {
  font-weight: 600;
  font-size: 13px;
}
.import-progress {
  padding: 4px 12px 8px;
}
.ip-file {
  font-size: 12px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-bottom: 4px;
}
.ip-meta {
  display: flex;
  gap: 8px;
  align-items: center;
  font-size: 11px;
  margin-top: 3px;
}
.file-list {
  flex: 1;
}
.empty {
  text-align: center;
  padding: 40px 12px;
  color: var(--text-dim);
  font-size: 13px;
}
.empty-icon {
  font-size: 28px;
  margin-bottom: 8px;
}
.dim {
  color: var(--text-dim);
}
.file-card {
  overflow: hidden;
  margin: 4px 8px;
  padding: 8px;
  border-radius: 6px;
  border: 1px solid transparent;
  font-size: 12px;
}
.file-card.active {
  border-color: #4f9cf966;
  background: #4f9cf91a;
}
.fc-row1 {
  display: flex;
  gap: 6px;
  align-items: center;
  min-width: 0;
}
/* 复选框（含文件名）占据剩余空间并允许收缩，保证右侧按钮始终可见 */
.fc-row1 > :first-child {
  flex: 1 1 auto;
  min-width: 0;
  overflow: hidden;
}
:deep(.n-checkbox) {
  min-width: 0;
  overflow: hidden;
}
:deep(.n-checkbox__label) {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  min-width: 0;
}
.fc-name {
  font-weight: 600;
  display: block;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.head-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}
.close-all {
  border: 1px solid var(--border-strong);
  background: none;
  color: var(--text-mute);
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 4px;
  cursor: pointer;
}
.close-all:hover {
  color: #e05c5c;
  border-color: #e05c5c88;
}
.fc-rule {
  flex-shrink: 0;
  border: 1px solid var(--border-strong);
  background: none;
  color: var(--text-mute);
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 4px;
  cursor: pointer;
  white-space: nowrap;
}
.fc-rule:hover {
  color: #4f9cf9;
  border-color: #4f9cf9;
}
.fc-rule.warn {
  color: #e0a84e;
  border-color: #e0a84e88;
}
.fc-close {
  border: 1px solid var(--border-strong);
  background: var(--hairline);
  color: var(--text-mute);
  cursor: pointer;
  font-size: 12px;
  line-height: 1;
  padding: 3px 7px;
  border-radius: 4px;
  flex-shrink: 0;
}
.fc-close:hover {
  color: #e05c5c;
  border-color: #e05c5c88;
  background: #e05c5c1a;
}
.fc-close {
  font-size: 11px;
  white-space: nowrap;
}
.fc-close:hover {
  color: #e05c5c;
  background: #e05c5c1a;
  border-radius: 4px;
}
.fc-meta {
  margin-top: 5px;
  font-size: 11px;
  color: var(--text-dim);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.fc-tags {
  display: flex;
  gap: 6px;
  align-items: center;
  margin-top: 5px;
  flex-wrap: wrap;
}
.fc-footer {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 6px;
  min-width: 0;
}
.fc-profile {
  flex: 1 1 auto;
  min-width: 0;
  font-size: 11px;
  color: var(--text-mute);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.fc-time {
  margin-top: 5px;
  font-size: 11px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.import-spin {
  display: block;
  margin: 8px auto;
}
.import-errors {
  padding: 6px 12px;
  border-top: 1px solid #e0a84e33;
}
.err-line {
  color: #e0a84e;
  font-size: 11px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
