<script setup lang="ts">
/**
 * 日志密度时间轴 —— 时间范围选择器。
 *
 * 交互（单一逻辑）：
 * - 在图上按住左键横向拖拽 → 选中的时间段自动加入顶部的时间过滤条件
 *   （可反复拖拽，每次把范围收窄/改选；顶部标签随时可删除）
 * - 删除顶部时间条件（或双击 / 点「重置」）→ 时间轴恢复全量显示
 * 选中的范围同时作用于列表与图表；图形按当前范围重新聚合（更细的桶）。
 *
 * 坐标约定：x 轴是 time 轴，像素与时间线性对应，刻度标签落在它所标示的时间点上。
 * 桶 i 覆盖 [startMs + i·bucketMs, startMs + (i+1)·bucketMs)，柱子画在桶的中点。
 * 框选取光标处的精确时间，两端各自向外取整到桶边界（闭区间）：
 * 起点 ≤ 按下处的时间 < 起点 + 桶宽，终点同理；落在范围之外的端点夹到首/末桶。
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
const noTimeCount = ref(0);
let chart: echarts.ECharts | null = null;
let resizeObserver: ResizeObserver | null = null;
/** 最近一次绘制的桶数据：像素↔时间换算与提示框都按它计算 */
let view: TimelineDto | null = null;

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

const MINUTE = 60_000;
const DAY = 24 * 60 * MINUTE;
const p2 = (n: number) => String(n).padStart(2, "0");

/** 刻度标签文本（工作区时区的墙钟时间）：零点标日期，整分标时分，其余带秒 */
function axisLabelText(ms: number): string {
  const local = ms + store.tzOffsetMin * MINUTE;
  const d = new Date(local);
  const within = (unit: number) => ((local % unit) + unit) % unit;
  const hm = `${p2(d.getUTCHours())}:${p2(d.getUTCMinutes())}`;
  if (within(DAY) === 0) return `${p2(d.getUTCMonth() + 1)}-${p2(d.getUTCDate())}`;
  if (within(MINUTE) === 0) return hm;
  return `${hm}:${p2(d.getUTCSeconds())}`;
}

/** 提示框里的桶时间范围；起止同一天时止点只写时分秒。不足 2 秒的桶带毫秒，否则起止会显示成同一秒 */
function bucketRangeLabel(t0: number, t1: number, bucketMs: number): string {
  const stamp = (ms: number) => {
    const s = fmtMs(ms, store.tzOffsetMin);
    return bucketMs < 2000 ? s : s.slice(0, 19);
  };
  const a = stamp(t0);
  const b = stamp(t1);
  return a.slice(0, 10) === b.slice(0, 10) ? `${a} ~ ${b.slice(11)}` : `${a} ~ ${b}`;
}

const clampIdx = (i: number, n: number) => Math.max(0, Math.min(n - 1, i));

/** 像素区间 → 时间区间：两端各自向外取整到桶边界（闭区间，ms）；整段落在时间范围之外返回 null */
function pixelRectToTime(left: number, right: number): [number, number] | null {
  if (!chart || !view) return null;
  const { startMs, bucketMs, buckets } = view;
  const timeAt = (px: number) => chart!.convertFromPixel({ xAxisIndex: 0 }, px) as number;
  const from = Math.min(timeAt(left), timeAt(right));
  const to = Math.max(timeAt(left), timeAt(right));
  const last = startMs + buckets.length * bucketMs;
  if (!(to >= startMs && from <= last)) return null; // NaN 也落在这里
  const i0 = clampIdx(Math.floor((from - startMs) / bucketMs), buckets.length);
  const i1 = clampIdx(Math.ceil((to - startMs) / bucketMs) - 1, buckets.length);
  return [startMs + i0 * bucketMs, startMs + (i1 + 1) * bucketMs - 1];
}

async function refresh() {
  if (!files.value.length) {
    noTimeCount.value = 0;
    view = null;
    chart?.clear();
    return;
  }
  loading.value = true;
  try {
    const dto = await getTimeline({
      fileIds: store.selectedFileIds.length ? store.selectedFileIds : undefined,
      timeRange: timeRange.value ?? undefined,
    });
    noTimeCount.value = dto.noTimeEvents ?? 0;
    render(dto);
  } finally {
    loading.value = false;
  }
}

function render(dto: TimelineDto) {
  if (!chart) return;
  const n = dto.buckets.length;
  if (!n) {
    view = null;
    chart.clear();
    return;
  }
  view = dto;
  const { startMs, bucketMs } = dto;
  // 每个桶都留一个数据点（高度 0 的为 null）：柱宽与提示框阴影才按桶宽计算
  const points = (height: (b: [number, number, number]) => number) =>
    dto.buckets.map(
      (b, i) => [startMs + (i + 0.5) * bucketMs, height(b) || null] as [number, number | null],
    );

  chart.setOption(
    {
      animation: false,
      // 右侧留白要放得下半个 "HH:mm:ss" 标签：末刻度可能正好落在范围终点上
      grid: { left: 46, right: 28, top: 10, bottom: 20 },
      tooltip: {
        trigger: "axis",
        axisPointer: { type: "shadow" },
        formatter: (params: unknown) => {
          const k = (params as { dataIndex: number }[])[0]?.dataIndex;
          const bucket = view?.buckets[k];
          if (!view || !bucket) return "";
          const t0 = view.startMs + k * view.bucketMs;
          const t1 = t0 + view.bucketMs - 1;
          return `<b>${bucketRangeLabel(t0, t1, view.bucketMs)}</b><br/>共 ${bucket[0]} 条 · WARN ${bucket[1]} · ERROR ${bucket[2]}<br/><span style="opacity:.65">按住左键拖拽可选择时间段</span>`;
        },
      },
      xAxis: {
        type: "time",
        min: startMs,
        max: startMs + n * bucketMs,
        axisLabel: { hideOverlap: true, fontSize: 10, color: "#8a8f98", formatter: axisLabelText },
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
      // 不用 stack：堆叠在下层为空时从 0 起算，而 0 在 log 轴上映射成 NaN，整根柱子都不画
      // ——纯 WARN / 纯 ERROR 的桶因此从图上消失。改为三层都从轴底画起、后画的盖住先画的（barGap -100%），视觉相同。
      // 所以「正常」画总高，「WARN」画到 WARN+ERROR，「ERROR」画到 ERROR；提示框不读这里的值。
      series: [
        { name: "正常", type: "bar", barGap: "-100%", data: points((b) => b[0]), itemStyle: { color: "#4f9cf9" }, barMaxWidth: 8 },
        { name: "WARN", type: "bar", barGap: "-100%", data: points((b) => b[1] + b[2]), itemStyle: { color: "#e0a84e" }, barMaxWidth: 8 },
        { name: "ERROR", type: "bar", barGap: "-100%", data: points((b) => b[2]), itemStyle: { color: "#e05c5c" }, barMaxWidth: 8 },
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
      <NTag v-if="noTimeCount > 0" size="tiny" type="warning">
        {{ noTimeCount.toLocaleString() }} 条事件无时间戳（列表中可见）
      </NTag>
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
  border-bottom: 1px solid var(--n-border-color, var(--border));
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
  color: var(--text);
  background: var(--overlay-bg);
  border: 1px solid #4f9cf9;
  border-radius: 3px;
  padding: 1px 5px;
}
.hint {
  display: inline-flex;
  width: 15px;
  height: 15px;
  border-radius: 50%;
  border: 1px solid var(--text-dim);
  color: var(--text-dim);
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
  color: var(--text-dim);
}
.tiny {
  font-size: 11px;
}
</style>
