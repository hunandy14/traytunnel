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
