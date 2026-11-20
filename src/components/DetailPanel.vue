<script setup lang="ts">
/**
 * 底部全宽详情面板（DevTools 风格，替代侧边抽屉）：
 * - 跨满主区域宽度，长堆栈行/上下文不再被窄抽屉挤压
 * - 拖拽顶部分隔条调整高度；双击分隔条或点「最大化」切换全高
 * - 「原文 / 上下文」两个标签页，各自使用完整宽度
 * - 上下文支持物理顺序 / 同线程两种模式
 */
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import { NButton, NSpin, NTag, NTooltip, useMessage } from "naive-ui";
import { useWorkspace } from "@/stores/workspace";
import {
  getLogContext,
  getLogDetail,
  type ContextItem,
  type ContextMode,
  type LogDetail,
} from "@/services/ipc";
import { copyText, fmtBytes, fmtMs, fmtMsShort, LEVEL_COLORS } from "@/services/format";

const store = useWorkspace();
const { detailRef } = storeToRefs(store);
const message = useMessage();

const detail = ref<LogDetail | null>(null);
const loading = ref(false);
const context = ref<ContextItem[] | null>(null);
const contextMode = ref<ContextMode>("physical");
const contextFellBack = ref(false);
const contextLoading = ref(false);

const tab = ref<"raw" | "context">("raw");

/** 面板高度（px），默认约 45% 窗口高，可拖拽 */
const panelHeight = ref(Math.round(window.innerHeight * 0.45));
let savedHeight = panelHeight.value;
const dragging = ref(false);

const open = computed(() => detailRef.value !== null);
const maximized = computed(() => panelHeight.value >= window.innerHeight * 0.82);

const MIN_H = 160;
function clampHeight(h: number): number {
  return Math.max(MIN_H, Math.min(window.innerHeight - 190, Math.round(h)));
}

async function load() {
  if (!detailRef.value) return;
  loading.value = true;
  contextFellBack.value = false;
  // 默认展示上下文（排查时更常用），可切到「原文」页。
  // 不立即清空旧上下文，避免在上下文行间连续跳转时列表闪烁。
  tab.value = "context";
  try {
    detail.value = await getLogDetail(detailRef.value.fileId, detailRef.value.seq);
  } catch (e) {
    message.error(`读取详情失败: ${e}`);
    detail.value = null;
    return;
  } finally {
    loading.value = false;
  }
  void loadContext(false); // 后台预热物理上下文（一次批量读取，开销很小）
}

async function loadContext(sameThread: boolean) {
  if (!detailRef.value) return;
  contextLoading.value = true;
  try {
    const res = await getLogContext(
      detailRef.value.fileId,
      detailRef.value.seq,
      20,
      20,
      sameThread,
    );
    context.value = res.items;
    contextMode.value = res.mode;
    contextFellBack.value = sameThread && res.mode === "physical";
  } catch (e) {
    message.error(`读取上下文失败: ${e}`);
  } finally {
    contextLoading.value = false;
  }
}

async function copy() {
  if (!detail.value) return;
  const ok = await copyText(detail.value.text);
  ok ? message.success("已复制原文") : message.error("复制失败");
}

function toggleMaximize() {
  if (maximized.value) {
    panelHeight.value = clampHeight(savedHeight);
  } else {
    savedHeight = panelHeight.value;
    panelHeight.value = clampHeight(window.innerHeight * 0.85);
  }
}

function startDrag(e: MouseEvent) {
  e.preventDefault();
  dragging.value = true;
  const startY = e.clientY;
  const startH = panelHeight.value;
  const onMove = (ev: MouseEvent) => {
    panelHeight.value = clampHeight(startH + (startY - ev.clientY));
  };
  const onUp = () => {
    dragging.value = false;
    window.removeEventListener("mousemove", onMove);
    window.removeEventListener("mouseup", onUp);
  };
  window.addEventListener("mousemove", onMove);
  window.addEventListener("mouseup", onUp);
}

onBeforeUnmount(() => {
  dragging.value = false;
});

watch(detailRef, () => void load());
</script>

<template>
  <div v-if="open" class="detail-panel" :style="{ height: `${panelHeight}px` }">
    <div class="splitter" :class="{ dragging }" @mousedown="startDrag" @dblclick="toggleMaximize" />

    <div class="panel-head">
      <span class="panel-title">事件详情</span>
      <template v-if="detail">
        <NTag
          size="small"
          :color="{ color: LEVEL_COLORS[detail.level] ?? '#8a8f98', textColor: '#fff' }"
        >
          {{ detail.level }}
        </NTag>
        <span class="mono dim">{{ fmtMs(detail.timestampMs) }}</span>
        <span class="dim">#{{ detail.sequenceNo }} · 文件 {{ detail.fileId }}</span>
        <span class="dim">{{ fmtBytes(detail.endOffset - detail.startOffset) }}</span>
        <NTag v-if="detail.truncated" size="tiny" type="warning">已截断</NTag>
        <NTag v-if="detail.badTimestamp" size="tiny" type="warning">时间戳非法</NTag>
      </template>
      <span class="spacer" />
      <NTooltip trigger="hover">
        <template #trigger>
          <NButton size="tiny" @click="copy">复制原文</NButton>
        </template>
        复制当前事件的完整原文（含多行堆栈）
      </NTooltip>
      <NButton size="tiny" @click="toggleMaximize">
        {{ maximized ? "还原" : "最大化" }}
      </NButton>
      <NButton size="tiny" quaternary @click="store.closeDetail()">关闭 ✕</NButton>
    </div>

    <div class="panel-tabs">
      <NButton
        size="tiny"
        :type="tab === 'raw' ? 'primary' : 'default'"
        @click="tab = 'raw'"
      >
        原文
      </NButton>
      <NButton
        size="tiny"
        :type="tab === 'context' ? 'primary' : 'default'"
        @click="tab = 'context'"
      >
        上下文{{ context ? ` (${context.length})` : "" }}
      </NButton>
      <template v-if="tab === 'context'">
        <span class="divider" />
        <NButton
          size="tiny"
          :type="contextMode === 'physical' ? 'info' : 'default'"
          :loading="contextLoading && contextMode === 'physical'"
          @click="loadContext(false)"
        >
          物理顺序
        </NButton>
        <NTooltip trigger="hover">
          <template #trigger>
            <NButton
              size="tiny"
              :type="contextMode === 'thread' ? 'info' : 'default'"
              :loading="contextLoading && contextMode === 'thread'"
              @click="loadContext(true)"
            >
              同线程
            </NButton>
          </template>
          只看与当前事件同一线程的前后事件（跟踪单个请求执行路径）
        </NTooltip>
        <NTag v-if="contextFellBack" size="tiny" type="warning">该事件无线程信息，已回退物理顺序</NTag>
        <span class="dim tiny">点击任意行跳转</span>
      </template>
      <span class="spacer" />
      <span v-if="detail" class="dim tiny mono">
        @{{ detail.startOffset }}-{{ detail.endOffset }}
      </span>
    </div>

    <div class="panel-body">
      <div v-if="loading" class="center"><NSpin /></div>

      <template v-else-if="detail">
        <!-- 原文页：完整宽度，长行自动换行 -->
        <pre v-if="tab === 'raw'" class="raw">{{ detail.text }}</pre>

        <!-- 上下文页：全宽列表 -->
        <div v-else-if="context" class="ctx-list">
          <div
            v-for="(c, i) in context"
            :key="i"
            class="ctx-item"
            :class="{ current: c.sequenceNo === detail.sequenceNo }"
            @click="store.openDetail(detail.fileId, c.sequenceNo)"
          >
            <span class="ctx-mark">{{ c.sequenceNo === detail.sequenceNo ? "▶" : "" }}</span>
            <span class="ctx-time mono">{{ fmtMsShort(c.timestampMs) }}</span>
            <span class="ctx-level" :style="{ color: LEVEL_COLORS[c.level] ?? '#8a8f98' }">
              {{ c.level }}
            </span>
            <span class="ctx-head" :title="c.text">{{ c.text.split("\n")[0] }}</span>
          </div>
        </div>
        <div v-else-if="contextLoading" class="center"><NSpin size="small" /></div>
        <div v-else class="center dim">无上下文</div>
      </template>

      <div v-else class="center dim">未选择事件</div>
    </div>
  </div>
</template>

<style scoped>
.detail-panel {
  display: flex;
  flex-direction: column;
  border-top: 1px solid var(--border-strong);
  background: var(--bg-elevated);
  min-height: 0;
}
.splitter {
  height: 5px;
  cursor: row-resize;
  background: linear-gradient(var(--splitter), transparent);
  flex-shrink: 0;
}
.splitter:hover,
.splitter.dragging {
  background: #4f9cf966;
}
.panel-head {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 2px 14px 4px;
  flex-shrink: 0;
  flex-wrap: wrap;
}
.panel-title {
  font-weight: 600;
  font-size: 13px;
}
.panel-tabs {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 14px 6px;
  flex-shrink: 0;
  flex-wrap: wrap;
}
.divider {
  width: 1px;
  height: 16px;
  background: var(--border-strong);
}
.panel-body {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  padding: 0 14px 12px;
}
.raw {
  flex: 1;
  margin: 0;
  background: var(--bg-surface);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 12px 16px;
  font-family: Consolas, "Cascadia Mono", monospace;
  font-size: 12.5px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
  overflow: auto;
}
.ctx-list {
  flex: 1;
  min-height: 0;
  border: 1px solid var(--border);
  border-radius: 6px;
  overflow: auto;
  font-family: Consolas, "Cascadia Mono", monospace;
}
.ctx-item {
  display: flex;
  gap: 10px;
  align-items: center;
  padding: 4px 12px;
  font-size: 12px;
  line-height: 1.5;
  cursor: pointer;
  border-bottom: 1px solid var(--hairline);
  white-space: nowrap;
}
.ctx-item:last-child {
  border-bottom: none;
}
.ctx-item:hover {
  background: var(--hover);
}
.ctx-item.current {
  background: #4f9cf91f;
}
.ctx-mark {
  width: 14px;
  flex-shrink: 0;
  color: #4f9cf9;
  font-size: 10px;
}
.ctx-time {
  color: var(--text-mute);
  flex-shrink: 0;
  font-size: 11px;
}
.ctx-level {
  width: 52px;
  flex-shrink: 0;
  font-weight: 600;
  font-size: 11px;
}
.ctx-head {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
}
.spacer {
  flex: 1;
}
.mono {
  font-family: Consolas, monospace;
}
.tiny {
  font-size: 11px;
}
.dim {
  color: var(--text-dim);
}
.center {
  display: flex;
  justify-content: center;
  padding: 30px;
}
</style>
