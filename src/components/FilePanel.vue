<script setup lang="ts">
/**
 * 左侧文件面板：导入（对话框 + 拖拽提示）、文件卡片、勾选过滤、关闭。
 */
import { storeToRefs } from "pinia";
import { open } from "@tauri-apps/plugin-dialog";
import { NButton, NCheckbox, NProgress, NScrollbar, NSpin, NTag, NTooltip } from "naive-ui";
import { useWorkspace } from "@/stores/workspace";
import type { ImportProgress } from "@/services/ipc";
import { fmtBytes, fmtMs } from "@/services/format";

const store = useWorkspace();
const { files, importing, importProgress, importErrors, selectedFileIds } = storeToRefs(store);

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
      <NButton size="tiny" type="primary" :loading="importing" @click="pickFiles">
        导入
      </NButton>
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
          <NTooltip trigger="hover">
            <template #trigger>
              <button class="fc-close" title="关闭文件" @click.stop="store.closeFile(f.fileId)">
                ×
              </button>
            </template>
            关闭并释放索引
          </NTooltip>
        </div>
        <div class="fc-stats">
          <span>{{ fmtBytes(f.fileSize) }}</span>
          <span>{{ f.events.toLocaleString() }} 事件</span>
          <span class="dim">{{ f.encoding }}</span>
        </div>
        <div class="fc-stats">
          <NTag v-if="f.errorCount" size="tiny" type="error" :bordered="false">
            ERROR {{ f.errorCount }}
          </NTag>
          <NTag v-if="f.warnCount" size="tiny" type="warning" :bordered="false">
            WARN {{ f.warnCount }}
          </NTag>
          <NTag v-if="f.unknownCount" size="tiny" :bordered="false">
            未识别 {{ f.unknownCount }}
          </NTag>
          <span class="dim fc-profile">{{ f.profile ?? "" }}</span>
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
  </aside>
</template>

<style scoped>
.file-panel {
  width: 250px;
  min-width: 250px;
  display: flex;
  flex-direction: column;
  border-right: 1px solid var(--n-border-color, #2a2b3a);
  background: rgba(0, 0, 0, 0.12);
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
  color: #6b7280;
  font-size: 13px;
}
.empty-icon {
  font-size: 28px;
  margin-bottom: 8px;
}
.dim {
  color: #6b7280;
}
.file-card {
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
  justify-content: space-between;
  align-items: center;
}
.fc-name {
  font-weight: 600;
  max-width: 150px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.fc-close {
  border: none;
  background: none;
  color: #6b7280;
  cursor: pointer;
  font-size: 14px;
  line-height: 1;
  padding: 2px 6px;
  border-radius: 4px;
}
.fc-close:hover {
  color: #e05c5c;
  background: #e05c5c1a;
}
.fc-stats {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-top: 4px;
  flex-wrap: wrap;
}
.fc-profile {
  font-size: 11px;
}
.fc-time {
  margin-top: 3px;
  font-size: 11px;
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
