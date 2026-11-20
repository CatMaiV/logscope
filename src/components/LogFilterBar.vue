<script setup lang="ts">
/**
 * 顶部过滤栏：关键词（正则/大小写）、级别多选、查询历史、时间范围、进度与取消。
 * 快捷键：Ctrl+F 聚焦输入框，Ctrl+Enter 执行搜索。
 */
import { onMounted, onUnmounted, ref } from "vue";
import { storeToRefs } from "pinia";
import {
  NButton,
  NDatePicker,
  NDropdown,
  NInput,
  NPopover,
  NPopselect,
  NSpace,
  NSwitch,
  NTag,
  NTooltip,
} from "naive-ui";
import { useWorkspace, queryLabel } from "@/stores/workspace";
import { fmtMs } from "@/services/format";
import type { SearchParams } from "@/services/ipc";

const store = useWorkspace();
const { searchState, searchTotal, searchProgress, timeRange, queryHistory, lastQuery } =
  storeToRefs(store);

const searchText = ref(lastQuery.value?.text ?? "");
const useRegex = ref(lastQuery.value?.regex ?? false);
const caseInsensitive = ref(lastQuery.value?.caseInsensitive ?? true);
const selectedLevels = ref<string[]>(lastQuery.value?.levels ?? []);
const inputRef = ref<InstanceType<typeof NInput> | null>(null);

const ALL_LEVELS = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL", "UNKNOWN"];

function buildParams(): SearchParams {
  return {
    text: searchText.value.trim() || undefined,
    regex: useRegex.value || undefined,
    caseInsensitive: caseInsensitive.value || undefined,
    levels: selectedLevels.value.length ? selectedLevels.value : undefined,
    fileIds: store.selectedFileIds.length ? store.selectedFileIds : undefined,
    timeRange: timeRange.value ?? undefined,
  };
}

function run() {
  void store.runSearch(buildParams());
}

function cancel() {
  void store.cancelRunningSearch();
}

function rerunHistory(q: SearchParams) {
  searchText.value = q.text ?? "";
  useRegex.value = q.regex ?? false;
  caseInsensitive.value = q.caseInsensitive ?? true;
  selectedLevels.value = q.levels ?? [];
  void store.runSearch(q);
}

function clearTimeRange() {
  store.setTimeRange(null);
}

/** 日期时间范围选择（与时间轴点击互补：这里可精确定位任意区间） */
const timePickerValue = ref<[number, number] | null>(timeRange.value ?? null);
const timePopoverOpen = ref(false);

function applyTimePicker() {
  if (timePickerValue.value) {
    store.setTimeRange([timePickerValue.value[0], timePickerValue.value[1]]);
  } else {
    store.setTimeRange(null);
  }
  timePopoverOpen.value = false;
}

function pickWholeDay() {
  // 取当前文件集的最早/最晚时间作为整段范围（等价于"全部时间"）
  timePickerValue.value = null;
}

function onKey(e: KeyboardEvent) {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
    e.preventDefault();
    inputRef.value?.focus();
  } else if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
    e.preventDefault();
    run();
  } else if (e.key === "Escape" && store.detailRef) {
    store.closeDetail();
  }
}

onMounted(() => window.addEventListener("keydown", onKey));
onUnmounted(() => window.removeEventListener("keydown", onKey));
</script>

<template>
  <div class="filter-bar">
    <NInput
      ref="inputRef"
      v-model:value="searchText"
      class="search-input"
      size="small"
      placeholder="搜索关键词（Ctrl+F 聚焦，Ctrl+Enter 执行）"
      clearable
      @keyup.enter="run"
    >
      <template #suffix>
        <span class="dim tiny">{{ searchTotal.toLocaleString() }} 条</span>
      </template>
    </NInput>

    <NTooltip trigger="hover">
      <template #trigger>
        <NSwitch v-model:value="useRegex" size="small" />
      </template>
      正则模式（Rust regex，不支持前后向断言）
    </NTooltip>
    <NTooltip trigger="hover">
      <template #trigger>
        <NSwitch v-model:value="caseInsensitive" size="small" />
      </template>
      忽略大小写
    </NTooltip>

    <NPopselect v-model:value="selectedLevels" multiple :options="ALL_LEVELS.map((l) => ({ label: l, value: l }))" trigger="click">
      <NButton size="small">
        级别{{ selectedLevels.length ? ` (${selectedLevels.length})` : "" }}
      </NButton>
    </NPopselect>

    <NButton size="small" type="primary" :loading="searchState === 'running'" @click="run">
      搜索
    </NButton>
    <NButton v-if="searchState === 'running'" size="small" @click="cancel"> 取消 </NButton>

    <NDropdown
      v-if="queryHistory.length"
      :options="queryHistory.map((q, i) => ({ key: i, label: queryLabel(q) }))"
      @select="(i: number) => rerunHistory(queryHistory[i])"
    >
      <NButton size="small" quaternary> 历史 </NButton>
    </NDropdown>

    <NPopover
      v-model:show="timePopoverOpen"
      trigger="click"
      placement="bottom-start"
      :width="420"
    >
      <template #trigger>
        <NButton size="small" :type="timeRange ? 'info' : 'default'">
          时间{{ timeRange ? "" : "（全部）" }}
        </NButton>
      </template>
      <div class="time-pop">
        <NDatePicker
          v-model:value="timePickerValue"
          type="datetimerange"
          size="small"
          clearable
          style="width: 100%"
        />
        <div class="time-hint dim tiny">
          可选：在时间轴上点击某个峰值桶快速定位；框选可缩放
        </div>
        <div class="time-actions">
          <NButton size="tiny" @click="pickWholeDay">全部时间</NButton>
          <NButton size="tiny" type="primary" @click="applyTimePicker">应用</NButton>
        </div>
      </div>
    </NPopover>
    <NTag v-if="timeRange" closable size="small" @close="clearTimeRange">
      {{ fmtMs(timeRange[0]) }} ~ {{ fmtMs(timeRange[1]) }}
    </NTag>

    <div v-if="searchState === 'running' && searchProgress" class="progress-line dim tiny">
      匹配 {{ searchProgress.matched.toLocaleString() }} /
      {{ searchProgress.candidatesTotal.toLocaleString() }}
    </div>
    <div v-else-if="searchState === 'cancelled'" class="dim tiny">已取消</div>
    <div v-else-if="searchState === 'failed'" class="err tiny" :title="searchProgress?.message ?? ''">
      失败：{{ (searchProgress?.message ?? "").slice(0, 60) }}
    </div>
    <div class="spacer" />
    <NSpace :size="4" class="dim tiny legend">
      <span>点击时间轴柱子或拖拽框选可缩小时间范围</span>
    </NSpace>
  </div>
</template>

<style scoped>
.filter-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px;
  border-bottom: 1px solid var(--n-border-color, var(--border));
  flex-wrap: wrap;
}
.search-input {
  width: 340px;
  max-width: 40vw;
}
.dim {
  color: var(--text-dim);
}
.tiny {
  font-size: 11px;
}
.err {
  color: #e05c5c;
}
.spacer {
  flex: 1;
}
.legend {
  white-space: nowrap;
}
.time-pop {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.time-hint {
  line-height: 1.4;
}
.time-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
</style>
