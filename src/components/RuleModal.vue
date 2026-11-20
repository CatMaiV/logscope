<script setup lang="ts">
/**
 * 自定义解析规则（M4）：编辑正则 → 实时预览识别结果 → 应用并重新导入。
 * 正则须包含命名组 ts 与 level；可选 tz / thread / logger / msg。
 */
import { computed, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NCode,
  NModal,
  NScrollbar,
  NSpin,
  NTag,
  useMessage,
} from "naive-ui";
import type { FileSummary, PreviewResult } from "@/services/ipc";
import { previewParse } from "@/services/ipc";
import { useWorkspace } from "@/stores/workspace";
import { fmtMs } from "@/services/format";

const props = defineProps<{ file: FileSummary | null }>();
const show = defineModel<boolean>("show", { default: false });

const store = useWorkspace();
const message = useMessage();

const pattern = ref("");
const loading = ref(false);
const applying = ref(false);
const preview = ref<PreviewResult | null>(null);

const TEMPLATE = String.raw`(?-u)^(?P<ts>\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:[.,]\d{1,9})?)(?P<tz>Z|[+-]\d{2}:?\d{2})?\s+\[(?P<thread>[^\]]*)\]\s+(?P<level>TRACE|DEBUG|INFO|WARN|ERROR|FATAL)\s+(?P<logger>\S+)\s+-\s?(?P<msg>.*)$`;

const fileName = computed(() => props.file?.fileName ?? "");

watch(show, (v) => {
  if (v) {
    pattern.value = "";
    preview.value = null;
  }
});

async function doPreview() {
  if (!props.file) return;
  loading.value = true;
  try {
    preview.value = await previewParse(
      props.file.path,
      pattern.value.trim() || undefined,
    );
  } catch (e) {
    message.error(`预览失败: ${e}`);
    preview.value = null;
  } finally {
    loading.value = false;
  }
}

async function apply() {
  if (!props.file) return;
  if (!pattern.value.trim()) {
    message.warning("请输入自定义规则（或直接关闭，保持自动识别）");
    return;
  }
  applying.value = true;
  try {
    await store.reimportWithPattern(props.file, pattern.value.trim());
    message.success("已按新规则重新导入");
    show.value = false;
  } catch (e) {
    message.error(`应用失败: ${e}`);
  } finally {
    applying.value = false;
  }
}

function useTemplate() {
  pattern.value = TEMPLATE;
}
</script>

<template>
  <NModal v-model:show="show" preset="card" style="width: min(1000px, 92vw)" title="自定义日志格式">
    <div class="rule-body">
      <div class="rule-file dim">
        文件：<b>{{ fileName }}</b>
        <span v-if="file?.profile"> · 当前规则：{{ file.profile }}</span>
      </div>

      <NAlert type="info" :bordered="false" class="rule-tip">
        自定义正则需包含命名组 <NCode code="(?P<ts>…)" inline /> 与
        <NCode code="(?P<level>…)" inline />；可选
        <NCode code="(?P<tz>…)" inline /> / <NCode code="(?P<thread>…)" inline /> /
        <NCode code="(?P<logger>…)" inline /> / <NCode code="(?P<msg>…)" inline />。
        建议以 <NCode code="(?-u)" inline /> 开头（字节语义，兼容 GBK）。
        时间戳支持 <NCode code="yyyy-MM-dd[ T]HH:mm:ss[.,SSS]" inline />。
      </NAlert>

      <textarea v-model="pattern" class="pattern-input" rows="5" spellcheck="false" />

      <div class="rule-actions">
        <NButton size="small" @click="useTemplate">填入模板</NButton>
        <NButton size="small" type="primary" :loading="loading" @click="doPreview">
          预览
        </NButton>
        <span class="dim tiny">留空预览则使用自动识别</span>
        <span class="spacer" />
        <NButton size="small" :loading="applying" :disabled="!pattern.trim()" @click="apply">
          应用并重新导入
        </NButton>
      </div>

      <div v-if="loading" class="center"><NSpin size="small" /></div>

      <template v-else-if="preview">
        <div class="preview-meta">
          <NTag v-if="preview.detected" size="small" type="success">
            自动识别：{{ preview.detected }}
          </NTag>
          <NTag v-else size="small" type="info">使用自定义规则</NTag>
          <NTag size="small">
            命中 {{ preview.matched }} / {{ preview.sampled }} 行
          </NTag>
          <span v-if="preview.matched === 0" class="err tiny">
            没有一行被识别——请检查正则，或粘贴一行真实日志对照修改
          </span>
        </div>

        <NScrollbar class="preview-scroll">
          <div class="preview-head">
            <span class="c-ok" />
            <span class="c-time">时间</span>
            <span class="c-level">级别</span>
            <span class="c-thread">线程</span>
            <span class="c-logger">Logger</span>
            <span class="c-line">原文</span>
          </div>
          <div
            v-for="(r, i) in preview.rows"
            :key="i"
            class="preview-row"
            :class="{ unmatched: !r.matched }"
          >
            <span class="c-ok">{{ r.matched ? "✓" : "✗" }}</span>
            <span class="c-time mono">{{ r.matched ? fmtMs(r.timestampMs) : "" }}</span>
            <span class="c-level">{{ r.level ?? "" }}</span>
            <span class="c-thread mono">{{ r.thread ?? "" }}</span>
            <span class="c-logger mono">{{ r.logger ?? "" }}</span>
            <span class="c-line mono" :title="r.line">{{ r.line }}</span>
          </div>
        </NScrollbar>
      </template>
    </div>
  </NModal>
</template>

<style scoped>
.rule-body {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.rule-file {
  font-size: 12px;
}
.rule-tip {
  font-size: 12px;
}
.pattern-input {
  width: 100%;
  box-sizing: border-box;
  background: var(--bg-surface);
  color: var(--text);
  border: 1px solid var(--border-strong);
  border-radius: 6px;
  padding: 10px 12px;
  font-family: Consolas, "Cascadia Mono", monospace;
  font-size: 12px;
  line-height: 1.5;
  resize: vertical;
}
.rule-actions {
  display: flex;
  gap: 8px;
  align-items: center;
  flex-wrap: wrap;
}
.preview-meta {
  display: flex;
  gap: 8px;
  align-items: center;
  flex-wrap: wrap;
}
.preview-scroll {
  max-height: 40vh;
  border: 1px solid var(--border);
  border-radius: 6px;
}
.preview-head,
.preview-row {
  display: flex;
  gap: 8px;
  padding: 3px 10px;
  font-size: 12px;
  line-height: 1.5;
  white-space: nowrap;
  min-width: max-content;
}
.preview-head {
  color: var(--text-mute);
  border-bottom: 1px solid var(--border);
  font-weight: 600;
  position: sticky;
  top: 0;
  background: var(--bg-elevated);
  z-index: 1;
}
.preview-row {
  border-bottom: 1px solid var(--hairline);
  font-family: Consolas, "Cascadia Mono", monospace;
}
.preview-row.unmatched {
  color: var(--text-mute);
  background: #e0a84e0a;
}
.c-ok {
  width: 16px;
  color: #6ec98f;
  flex-shrink: 0;
}
.preview-row.unmatched .c-ok {
  color: #e05c5c;
}
.c-time {
  width: 170px;
  flex-shrink: 0;
}
.c-level {
  width: 54px;
  flex-shrink: 0;
  font-weight: 600;
}
.c-thread {
  width: 150px;
  flex-shrink: 0;
  overflow: hidden;
  text-overflow: ellipsis;
}
.c-logger {
  width: 220px;
  flex-shrink: 0;
  overflow: hidden;
  text-overflow: ellipsis;
}
.c-line {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
}
.mono {
  font-family: Consolas, "Cascadia Mono", monospace;
}
.dim {
  color: var(--text-dim);
}
.tiny {
  font-size: 11px;
}
.err {
  color: #e0a84e;
}
.spacer {
  flex: 1;
}
.center {
  display: flex;
  justify-content: center;
  padding: 20px;
}
</style>
