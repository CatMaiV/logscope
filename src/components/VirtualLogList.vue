<script setup lang="ts">
/**
 * 虚拟滚动日志列表：只渲染可见窗口（固定行高），数据经 search_page 游标分页按需加载。
 * 滚动过程中不传输全量数据（计划 §4.6）。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import { NEmpty, NSpin } from "naive-ui";
import { useWorkspace } from "@/stores/workspace";
import { searchPage, type EventDto } from "@/services/ipc";
import { fmtMsShort, LEVEL_COLORS } from "@/services/format";

const store = useWorkspace();
const { searchId, searchState, searchTotal } = storeToRefs(store);

const ROW_H = 28;
const PAGE = 200;
const BUFFER_ROWS = 20;

const containerRef = ref<HTMLDivElement | null>(null);
const scrollTop = ref(0);
const viewportH = ref(600);

/** 页缓存：页号 → 该页事件（searchId 变化时整体作废） */
const pages = ref(new Map<number, EventDto[]>());
const inflight = new Set<number>();

const totalRows = computed(() => (searchState.value === "done" ? searchTotal.value : 0));
const spacerH = computed(() => totalRows.value * ROW_H);
const firstVisible = computed(() =>
  Math.max(0, Math.floor(scrollTop.value / ROW_H) - BUFFER_ROWS),
);
const lastVisible = computed(() =>
  Math.min(
    totalRows.value - 1,
    Math.ceil((scrollTop.value + viewportH.value) / ROW_H) + BUFFER_ROWS,
  ),
);

const visibleRows = computed<{ index: number; ev: EventDto | null }[]>(() => {
  const out: { index: number; ev: EventDto | null }[] = [];
  for (let i = firstVisible.value; i <= lastVisible.value; i++) {
    const page = pages.value.get(Math.floor(i / PAGE));
    const ev = page ? page[i % PAGE] : null;
    out.push({ index: i, ev });
  }
  return out;
});

/** 滚动到指定序号（详情导航用） */
function scrollToIndex(seq: number) {
  const el = containerRef.value;
  if (el) el.scrollTop = seq * ROW_H;
}

async function ensurePages() {
  if (searchState.value !== "done" || !searchId.value) return;
  const p0 = Math.floor(firstVisible.value / PAGE);
  const p1 = Math.floor(lastVisible.value / PAGE);
  for (let p = p0; p <= p1; p++) {
    if (pages.value.has(p) || inflight.has(p)) continue;
    if (p * PAGE >= totalRows.value) continue;
    inflight.add(p);
    searchPage(searchId.value, p * PAGE, PAGE)
      .then((res) => {
        pages.value.set(p, res.events);
        // 触发响应式更新
        pages.value = new Map(pages.value);
      })
      .catch(() => undefined)
      .finally(() => inflight.delete(p));
  }
}

watch([firstVisible, lastVisible, searchId, searchState], () => void ensurePages());

// searchId 变化：清空页缓存并回顶
watch(searchId, () => {
  pages.value.clear();
  pages.value = new Map();
  if (containerRef.value) containerRef.value.scrollTop = 0;
  scrollTop.value = 0;
});

function onScroll() {
  const el = containerRef.value;
  if (!el) return;
  scrollTop.value = el.scrollTop;
}

function rowClass(ev: EventDto | null): string {
  if (!ev) return "";
  if (ev.level === "ERROR" || ev.level === "FATAL") return "row-error";
  if (ev.level === "WARN") return "row-warn";
  return "";
}

let ro: ResizeObserver | null = null;
onMounted(() => {
  if (containerRef.value) {
    viewportH.value = containerRef.value.clientHeight;
    ro = new ResizeObserver(() => {
      if (containerRef.value) viewportH.value = containerRef.value.clientHeight;
    });
    ro.observe(containerRef.value);
  }
});
onBeforeUnmount(() => ro?.disconnect());

defineExpose({ scrollToIndex });
</script>

<template>
  <div ref="containerRef" class="log-list" @scroll.passive="onScroll">
    <div :style="{ height: `${spacerH}px`, position: 'relative' }">
      <div
        v-for="{ index, ev } in visibleRows"
        :key="index"
        class="log-row"
        :class="rowClass(ev)"
        :style="{ position: 'absolute', top: `${index * ROW_H}px`, height: `${ROW_H}px` }"
        @click="ev && store.openDetail(ev.fileId, ev.sequenceNo)"
      >
        <template v-if="ev">
          <span class="col-time">{{ fmtMsShort(ev.timestampMs) }}</span>
          <span class="col-level" :style="{ color: LEVEL_COLORS[ev.level] ?? '#8a8f98' }">
            {{ ev.level }}
          </span>
          <span class="col-file dim">#{{ ev.fileId }}</span>
          <span class="col-head" :title="ev.head">{{ ev.head }}</span>
          <span v-if="ev.truncated" class="badge">截断</span>
          <span v-if="ev.unknown" class="badge">未识别</span>
        </template>
        <span v-else class="dim">…</span>
      </div>
    </div>

    <div v-if="searchState === 'running'" class="overlay">
      <NSpin size="small" />
      <span class="dim">检索中…</span>
    </div>
    <NEmpty
      v-else-if="searchState === 'done' && totalRows === 0"
      class="overlay"
      description="无匹配事件（调整过滤条件）"
    />
    <NEmpty
      v-else-if="searchState === 'idle'"
      class="overlay"
      description="未导入文件（拖入日志或点左上角「导入」）"
    />
  </div>
</template>

<style scoped>
.log-list {
  flex: 1;
  min-height: 0; /* 允许被下方详情面板压缩 */
  overflow-y: auto;
  position: relative;
  font-family: Consolas, "Cascadia Mono", "Courier New", monospace;
  font-size: 12px;
}
.log-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 0 10px;
  white-space: nowrap;
  cursor: pointer;
  border-bottom: 1px solid var(--hairline);
}
.log-row:hover {
  background: var(--hover);
}
.row-error {
  background: #e05c5c0f;
}
.row-warn {
  background: #e0a84e0a;
}
.col-time {
  color: var(--text-mute);
  flex-shrink: 0;
}
.col-level {
  width: 56px;
  flex-shrink: 0;
  font-weight: 600;
}
.col-file {
  flex-shrink: 0;
  font-size: 11px;
}
.col-head {
  overflow: hidden;
  text-overflow: ellipsis;
}
.badge {
  flex-shrink: 0;
  font-size: 10px;
  border: 1px solid var(--text-dim);
  border-radius: 3px;
  padding: 0 3px;
  color: var(--text-dim);
}
.dim {
  color: var(--text-dim);
}
.overlay {
  position: sticky;
  bottom: 12px;
  margin: 0 auto;
  width: fit-content;
  display: flex;
  gap: 8px;
  align-items: center;
  padding: 8px 14px;
  background: var(--overlay-bg);
  border-radius: 6px;
  border: 1px solid var(--border-strong);
}
</style>
