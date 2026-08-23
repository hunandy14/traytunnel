/**
 * 設定頁 ABOUT 區塊的版本號。
 *
 * getVersion() 讀的版號 fallback 到 src-tauri/Cargo.toml 的 package.version
 * （tauri.conf.json 沒有 version 欄位，這是 Tauri v2 官方行為），走 core:app 模組的
 * plugin:app|version 指令，capabilities/default.json 的 core:default
 * 已經涵蓋。dev-mock 模式（`npm run dev` 且偵測不到 Tauri runtime）沒有
 * 真正的後端可以回應這個指令，isTauri() 會是 false，直接給假版本號；
 * 萬一實機呼叫仍然失敗，也退回同一個假版本號，不讓 ABOUT 區塊留空。
 *
 * 假版本號不硬編碼：__APP_VERSION__ 是 vite.config.ts 的 define，
 * 建置期從 package.json 的 version 欄位注入，與 scripts/bump.mjs 同步的
 * 那個版號同一個來源。
 */

import { getVersion } from "@tauri-apps/api/app";
import { isTauri } from "@tauri-apps/api/core";

const DEV_VERSION = `v${__APP_VERSION__} (dev)`;

export async function loadAppVersion(): Promise<string> {
  try {
    if (!isTauri()) return DEV_VERSION;
    const v = await getVersion();
    return v ? `v${v}` : DEV_VERSION;
  } catch {
    return DEV_VERSION;
  }
}
