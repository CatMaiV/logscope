<script setup lang="ts">
// M0 空壳 + 核心服务连通性自检。M3 在此挂载完整布局。
import { onMounted, ref } from "vue";
import { listFiles } from "./services/ipc";

const coreStatus = ref<"checking" | "ok" | "error">("checking");
const coreError = ref("");

onMounted(async () => {
  try {
    // 冒烟：invoke Rust 命令链路（list_files 是最轻的命令）
    const files = await listFiles();
    coreStatus.value = "ok";
    console.log(`[logscope] 核心服务连通，当前已导入 ${files.length} 个文件`);
  } catch (e) {
    coreStatus.value = "error";
    coreError.value = String(e);
    console.error("[logscope] 核心服务调用失败", e);
  }
});
</script>

<template>
  <main class="shell">
    <h1>LogScope</h1>
    <p>本地日志可视化工具 — 工程基础已就绪（M0/M1）。</p>
    <p v-if="coreStatus === 'checking'" class="dim">正在连接核心服务…</p>
    <p v-else-if="coreStatus === 'ok'" class="ok">✔ 核心服务连通（Rust 命令链路正常）</p>
    <p v-else class="err">✘ 核心服务调用失败：{{ coreError }}</p>
    <p class="dim">0.1.0 (M0/M1) — 时间轴与列表将在 M3 就绪</p>
  </main>
</template>

<style>
:root {
  color-scheme: dark;
  font-family: "Segoe UI", "Microsoft YaHei", system-ui, sans-serif;
}
body {
  margin: 0;
  background: #1a1b26;
  color: #d5d6db;
}
.shell {
  padding: 32px 40px;
}
.dim {
  color: #6b7280;
  font-size: 13px;
}
.ok {
  color: #6ec98f;
}
.err {
  color: #e0706c;
}
</style>
