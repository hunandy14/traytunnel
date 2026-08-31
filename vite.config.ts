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
  // 底下永遠正常；改成相對路徑（./assets/xxx.js）是 Tauri＋Vite 專案常見的建議
  // 做法，出發點是「不要依賴協定有沒有一個『根』」。
  //
  // 誠實記錄這個改動的份量：**這是一次推測性的緩解，不是有證據的修復**。原始
  // 白屏在本機從來沒有重現過，Windows 的 WebView2 也沒有實測過這一版產物；
  // 「解不回同一份資源」是社群文章的說法，不是我們自己量到的。
  //
  // 之所以留著而不是退回：prod 唯一的載入點就在協定的根（index.html 由自訂
  // 協定從根送出），在那個位置 `/assets/x.js` 與 `./assets/x.js` 解出來的是
  // 同一個 URL，兩種寫法等價——留著的成本是零，能不能真的擋掉那個症狀則未知。
  // 真要確認只有一條路：拿 Windows 的正式產物開窗看一眼。
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
