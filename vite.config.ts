import { defineConfig } from "vite";

// @ts-expect-error process 是 nodejs 全域變數
const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async () => ({
  // 不要蓋掉 rust 端的錯誤訊息
  clearScreen: false,
  build: {
    // 主視窗與設定視窗各一個頁面
    rollupOptions: {
      input: {
        main: "index.html",
        settings: "settings.html",
      },
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
}));
