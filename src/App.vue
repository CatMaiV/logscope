<script setup lang="ts">
/**
 * 应用布局（开发计划 §4.6）：
 * 顶部过滤栏 | 左文件面板 + 时间轴 + 虚拟列表 | 详情抽屉
 * 支持：拖拽导入、暗/亮主题切换。
 */
import { onMounted, onUnmounted, ref } from "vue";
import {
  NButton,
  NConfigProvider,
  NMessageProvider,
  darkTheme,
  dateZhCN,
  zhCN,
  type GlobalTheme,
} from "naive-ui";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useWorkspace } from "@/stores/workspace";
import FilePanel from "@/components/FilePanel.vue";
import LogFilterBar from "@/components/LogFilterBar.vue";
import LogTimeline from "@/components/LogTimeline.vue";
import VirtualLogList from "@/components/VirtualLogList.vue";
import DetailPanel from "@/components/DetailPanel.vue";

const store = useWorkspace();
const light = ref(false);

let unlisten: (() => void) | null = null;

onMounted(async () => {
  await store.refreshFiles();
  if (store.files.length > 0) {
    await store.runSearch();
  }
  // 拖拽导入（Tauri webview 事件）
  try {
    unlisten = await getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "drop") {
        void store.importPaths(event.payload.paths);
      }
    });
  } catch (e) {
    console.warn("拖拽事件不可用", e);
  }
});

onUnmounted(() => {
  unlisten?.();
  unlisten = null;
});
</script>

<template>
  <NConfigProvider
    :theme="(light ? undefined : darkTheme) as GlobalTheme | undefined"
    :locale="zhCN"
    :date-locale="dateZhCN"
    style="height: 100vh"
  >
    <NMessageProvider>
      <div class="app" :class="{ light }">
        <header class="app-header">
          <div class="brand">
            <span class="logo">▚</span> LogScope
            <span class="dim tiny">本地日志分析</span>
          </div>
          <NButton size="tiny" quaternary @click="light = !light">
            {{ light ? "🌙 暗色" : "☀️ 亮色" }}
          </NButton>
        </header>

        <div class="app-body">
          <FilePanel />
          <main class="main">
            <LogFilterBar />
            <LogTimeline />
            <VirtualLogList />
            <DetailPanel />
          </main>
        </div>

      </div>
    </NMessageProvider>
  </NConfigProvider>
</template>

<style>
:root {
  color-scheme: dark;
  font-family: "Segoe UI", "Microsoft YaHei", system-ui, sans-serif;
}
html,
body,
#app {
  margin: 0;
  height: 100%;
}
.app {
  /* 主题变量：暗色（默认） */
  --bg: #1a1b26;
  --bg-elevated: #1b1c28;
  --bg-surface: #131420;
  --bg-panel: rgba(0, 0, 0, 0.18);
  --border: #2a2b3a;
  --border-strong: #3a3b4a;
  --overlay-bg: #1a1b26e6;
  --splitter: #3a3b4a66;
  --text: #d5d6db;
  --text-dim: #6b7280;
  --text-mute: #8a8f98;
  --hover: #ffffff0d;
  --hairline: #ffffff08;

  height: 100vh;
  display: flex;
  flex-direction: column;
  background: var(--bg);
  color: var(--text);
}
.app.light {
  --bg: #f5f6f8;
  --bg-elevated: #ffffff;
  --bg-surface: #ffffff;
  --bg-panel: #ffffff;
  --border: #e0e2e8;
  --border-strong: #c9cdd4;
  --overlay-bg: #ffffffe6;
  --splitter: #c9cdd466;
  --text: #26272e;
  --text-dim: #7b8087;
  --text-mute: #8a8f98;
  --hover: rgba(0, 0, 0, 0.05);
  --hairline: rgba(0, 0, 0, 0.06);

  background: var(--bg);
  color: var(--text);
}
.app-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 6px 14px;
  border-bottom: 1px solid var(--border);
  flex-shrink: 0;
}

.brand {
  font-weight: 700;
  font-size: 14px;
  display: flex;
  align-items: center;
  gap: 6px;
}
.logo {
  color: #4f9cf9;
}
.app-body {
  flex: 1;
  display: flex;
  min-height: 0;
}
.main {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.dim {
  color: var(--text-dim);
}
.tiny {
  font-size: 11px;
}
</style>
