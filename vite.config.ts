import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";

// @ts-expect-error process 是 nodejs 全域變數
const host = process.env.TAURI_DEV_HOST;

/**
 * 版本號單一來源是 package.json（scripts/bump.mjs 負責跟 Cargo.toml 對齊）。
 * 這裡讀出來餵給兩個消費端，兩邊都不再各自硬編碼：
 *
 *   - src/version.ts 的 DEV_VERSION（dev-mock 模式的假版號）用
 *     __APP_VERSION__ 這個 define 常數。
 *   - index.html 的 #app-version 靜態佔位文字（JS 跑起來前的第一幀，或
 *     dev-mock 尚未安裝完成前短暫可見）用下面的 transformIndexHtml 取代。
 */
const pkgVersion = (
  JSON.parse(readFileSync(fileURLToPath(new URL("./package.json", import.meta.url)), "utf8")) as {
    version: string;
  }
).version;

function htmlVersionPlugin(version: string): Plugin {
  return {
    name: "html-version-placeholder",
    transformIndexHtml(html) {
      return html.replace("__APP_VERSION_PLACEHOLDER__", version);
    },
  };
}

export default defineConfig(async () => ({
  // 不要蓋掉 rust 端的錯誤訊息
  clearScreen: false,
  // 打包後的 webview 是靠 tauri 的自訂協定（不是真的 http 伺服器根目錄）讀資源。
  // Vite 預設輸出的是絕對路徑（/assets/xxx.js），這在 dev（真的 http://localhost）
  // 底下永遠正常，但在正式建置後由自訂協定載入時，絕對路徑要不要解得回同一份
  // 資源，實測會隨 WebKit／WebView2 版本而不一致——這正是 Tauri 官方與社群多篇
  // 「white screen in production」文章共同指向的頭號成因。改成相對路徑
  // （./assets/xxx.js）就不依賴協定有沒有一個「根」，兩邊都吃得動，是 Tauri＋Vite
  // 專案的標準建議做法，不是 mac 專屬修正。
  base: "./",
  plugins: [htmlVersionPlugin(pkgVersion)],
  define: {
    __APP_VERSION__: JSON.stringify(pkgVersion),
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
