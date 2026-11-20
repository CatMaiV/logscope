import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [vue()],

  // Vite + Tauri: 固定端口，避免每次随机端口要与 tauri.conf.json 对齐
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      // 让 Vite 忽略 src-tauri 的构建产物，避免 Rust 编译触发前端重启
      ignored: ["**/src-tauri/**"],
    },
  },
});
