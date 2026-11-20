<script setup lang="ts">
/**
 * 日志密度时间轴 —— 时间范围选择器。
 *
 * 交互（单一逻辑）：
 * - 在图上按住左键横向拖拽 → 选中的时间段自动加入顶部的时间过滤条件
 *   （可反复拖拽，每次把范围收窄/改选；顶部标签随时可删除）
 * - 删除顶部时间条件（或双击 / 点「重置」）→ 时间轴恢复全量显示
 * 选中的范围同时作用于列表与图表；图形按当前范围重新聚合（更细的桶）。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import * as echarts from "echarts";
import { NButton, NTag, NTooltip } from "naive-ui";
import { useWorkspace } from "@/stores/workspace";
import { getTimeline, type TimelineDto } from "@/services/ipc";
import { fmtMs } from "@/services/format";

const store = useWorkspace();
const { files, timeRange, lastQuery } = storeToRefs(store);

const wrapEl = ref<HTMLDivElement | null>(null);
const loading = ref(false);
let chart: echarts.ECharts | null = null;
let resizeObserver: ResizeObserver | null = null;

/** 拖拽选区（像素，相对容器） */
const dragging = ref(false);
const dragStartX = ref(0);
const dragCurX = ref(0);
const DRAG_THRESHOLD = 5; // 小于该宽度视为点击，不触发过滤

const dragRect = computed(() => {
  if (!dragging.value) return null;
  const a = Math.min(dragStartX.value, dragCurX.value);
  const b = Math.max(dragStartX.value, dragCurX.value);
  return { left: a, width: b - a };
});

/** 拖拽预览的时间范围文案 */
const dragPreview = computed(() => {
  if (!dragRect.value) return "";
  const r = pixelRectToTime(dragRect.value.left, dragRect.value.left + dragRect.value.width);
  return r ? `${fmtMs(r[0])} ~ ${fmtMs(r[1])}` : "";
});

function msToAxis(ms: number): string {
  const d = new Date(ms + store.tzOffsetMin * 60_000);
  return d.toISOString().slice(0, 19).replace("T", " ");
}

function axisToMs(label: string): number {
  return Date.parse(label.replace(" ", "T") + "Z") - store.tzOffsetMin * 60_000;
}

/** 当前图表 x 轴刻度（时间标签）与桶宽 */
function axisInfo(): { labels: string[]; span: number } | null {
  const opt = chart?.getOption();
  const x = (opt?.xAxis as { data?: string[] }[] | undefined)?.[0]?.data;
  if (!x || x.length < 2) return null;
  return { labels: x, span: axisToMs(x[1]) - axisToMs(x[0]) };
}

/** 像素区间 → 时间区间（夹紧到图表范围；无效返回 null） */
function pixelRectToTime(left: number, right: number): [number, number] | null {
  const info = axisInfo();
  if (!chart || !info) return null;
  const idxOf = (px: number): number => {
    const v = chart!.convertFromPixel({ xAxisIndex: 0 }, px) as unknown;
    const n = typeof v === "number" ? v : Array.isArray(v) ? Number(v[0]) : NaN;
    return Number.isFinite(n) ? n : 0;
  };
  let i0 = Math.floor(Math.min(idxOf(left), idxOf(right)));
  let i1 = Math.ceil(Math.max(idxOf(left), idxOf(right)));
  i0 = Math.max(0, Math.min(info.labels.length - 1, i0));
  i1 = Math.max(0, Math.min(info.labels.length - 1, i1));
  const t0 = axisToMs(info.labels[i0]);
  const t1 = axisToMs(info.labels[i1]) + info.span - 1;
  return [t0, t1];
}

async function refresh() {
  if (!files.value.length) return;
  loading.value = true;
  try {
    const dto = await getTimeline({
      fileIds: store.selectedFileIds.length ? store.selectedFileIds : undefined,
      timeRange: timeRange.value ?? undefined,
    });
    render(dto);
  } finally {
    loading.value = false;
  }
}

function render(dto: TimelineDto) {
  if (!chart) return;
  const x: string[] = [];
  const normal: number[] = [];
  const warn: number[] = [];
  const error: number[] = [];
  for (let i = 0; i < dto.buckets.length; i++) {
    const [t, w, e] = dto.buckets[i];
    x.push(msToAxis(dto.startMs + i * dto.bucketMs));
    normal.push(Math.max(0, t - w - e));
    warn.push(w);
    error.push(e);
  }
  const trim = (a: number[]) => a.map((v) => (v === 0 ? null : v));

  chart.setOption(
    {
      animation: false,
      grid: { left: 46, right: 12, top: 10, bottom: 20 },
      tooltip: {
        trigger: "axis",
        axisPointer: { type: "shadow" },
        formatter: (params: unknown) => {
          const arr = params as { name: string; value: number | null; seriesName: string }[];
          if (!arr.length) return "";
          const total = arr.reduce((s, p) => s + (p.value ?? 0), 0);
          const err = arr.find((p) => p.seriesName === "ERROR")?.value ?? 0;
          const wrn = arr.find((p) => p.seriesName === "WARN")?.value ?? 0;
          return `<b>${arr[0].name}</b><br/>共 ${total} 条 · WARN ${wrn} · ERROR ${err}<br/><span style="opacity:.65">按住左键拖拽可选择时间段</span>`;
        },
      },
      xAxis: {
        type: "category",
        data: x,
        axisLabel: { fontSize: 10, color: "#8a8f98" },
        axisLine: { lineStyle: { color: "#3a3b4a" } },
        axisTick: { show: false },
      },
      yAxis: {
        type: "log",
        logBase: 10,
        min: 1,
        axisLabel: { fontSize: 10, color: "#8a8f98" },
        splitLine: { lineStyle: { color: "#ffffff10" } },
      },
      series: [
        { name: "正常", type: "bar", stack: "all", data: trim(normal), itemStyle: { color: "#4f9cf9" }, barMaxWidth: 8 },
        { name: "WARN", type: "bar", stack: "all", data: trim(warn), itemStyle: { color: "#e0a84e" }, barMaxWidth: 8 },
        { name: "ERROR", type: "bar", stack: "all", data: trim(error), itemStyle: { color: "#e05c5c" }, barMaxWidth: 8 },
      ],
    },
    { notMerge: true },
  );
}

// ---------------- 拖拽选区 ----------------

function relX(e: PointerEvent): number {
  const rect = wrapEl.value!.getBoundingClientRect();
  return e.clientX - rect.left;
}

function onPointerDown(e: PointerEvent) {
  if (e.button !== 0 || !chart) return;
  dragging.value = true;
  dragStartX.value = relX(e);
  dragCurX.value = dragStartX.value;
  wrapEl.value?.setPointerCapture(e.pointerId);
}

function onPointerMove(e: PointerEvent) {
  if (!dragging.value) return;
  dragCurX.value = relX(e);
}

function onPointerUp(e: PointerEvent) {
  if (!dragging.value) return;
  // 先取坐标：dragRect 依赖 dragging，复位后就变 null 了
  const startX = dragStartX.value;
  const endX = relX(e);
  dragCurX.value = endX;
  dragging.value = false;
  try {
    wrapEl.value?.releasePointerCapture(e.pointerId);
  } catch {
    /* 已释放 */
  }
  const left = Math.min(startX, endX);
  const width = Math.abs(endX - startX);
  if (width < DRAG_THRESHOLD) return; // 点击不触发过滤
  const range = pixelRectToTime(left, left + width);
  if (range) store.setTimeRange(range);
}

function resetRange() {
  store.setTimeRange(null);
}

watch([files, () => store.selectedFileIds, timeRange], () => void refresh(), { deep: true });

onMounted(async () => {
  if (!wrapEl.value) return;
  chart = echarts.init(wrapEl.value);
  chart.getZr().on("dblclick", resetRange);
  wrapEl.value.addEventListener("pointerdown", onPointerDown);
  wrapEl.value.addEventListener("pointermove", onPointerMove);
  wrapEl.value.addEventListener("pointerup", onPointerUp);
  resizeObserver = new ResizeObserver(() => chart?.resize());
  resizeObserver.observe(wrapEl.value);
  await refresh();
});

onBeforeUnmount(() => {
  resizeObserver?.disconnect();
  chart?.dispose();
  chart = null;
});
</script>

<template>
  <div class="timeline-wrap">
    <div class="timeline-head">
      <span class="dim tiny">日志密度</span>
      <NTag v-if="timeRange" size="tiny" type="info" closable @close="resetRange">
        时间范围：{{ fmtMs(timeRange[0]) }} ~ {{ fmtMs(timeRange[1]) }}
      </NTag>
      <NButton v-if="timeRange" size="tiny" quaternary @click="resetRange">重置</NButton>
      <NTooltip trigger="hover">
        <template #trigger>
          <span class="hint">?</span>
        </template>
        按住左键在图上拖拽：选中时间段并加入顶部过滤条件（可反复拖拽逐层收窄）<br />
        删除顶部时间条件 / 双击 / 重置：恢复全量
      </NTooltip>
      <span v-if="!timeRange" class="dim tiny hint-text">按住左键拖拽选择时间段</span>
      <span class="spacer" />
      <NTag v-if="lastQuery?.text" size="tiny"> 分桶不受文本条件影响 </NTag>
      <span v-if="loading" class="dim tiny">加载中…</span>
    </div>
    <div class="timeline-body">
      <div ref="wrapEl" class="timeline-chart" />
      <div
        v-if="dragRect"
        class="drag-mask"
        :style="{ left: `${dragRect.left}px`, width: `${dragRect.width}px` }"
      >
        <div class="drag-label">{{ dragPreview }}</div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.timeline-wrap {
  display: flex;
  flex-direction: column;
  height: 190px;
  min-height: 190px;
  border-bottom: 1px solid var(--n-border-color, #2a2b3a);
}
.timeline-head {
  display: flex;
  gap: 8px;
  align-items: center;
  padding: 4px 10px 0;
}
.timeline-body {
  flex: 1;
  position: relative;
  min-height: 0;
}
.timeline-chart {
  position: absolute;
  inset: 0;
  cursor: crosshair;
}
.drag-mask {
  position: absolute;
  top: 10px;
  bottom: 20px;
  background: #4f9cf933;
  border-left: 1px solid #4f9cf9;
  border-right: 1px solid #4f9cf9;
  pointer-events: none;
}
.drag-label {
  position: absolute;
  top: -2px;
  left: 50%;
  transform: translateX(-50%);
  white-space: nowrap;
  font-size: 10px;
  color: #d5d6db;
  background: #1a1b26e6;
  border: 1px solid #4f9cf9;
  border-radius: 3px;
  padding: 1px 5px;
}
.hint {
  display: inline-flex;
  width: 15px;
  height: 15px;
  border-radius: 50%;
  border: 1px solid #6b7280;
  color: #6b7280;
  font-size: 10px;
  align-items: center;
  justify-content: center;
  cursor: help;
}
.hint-text {
  margin-left: 2px;
}
.spacer {
  flex: 1;
}
.dim {
  color: #6b7280;
}
.tiny {
  font-size: 11px;
}
</style>
