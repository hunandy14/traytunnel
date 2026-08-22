/**
 * 設定頁 ABOUT 區塊的版本號。
 *
 * getVersion() 讀的版號 fallback 到 src-tauri/Cargo.toml 的 package.version
 * （tauri.conf.json 沒有 version 欄位，這是 Tauri v2 官方行為），走 core:app 模組的
 * plugin:app|version 指令，capabilities/default.json 的 core:default
 * 已經涵蓋。dev-mock 模式（`npm run dev` 且偵測不到 Tauri runtime）沒有
 * 真正的後端可以回應這個指令，isTauri() 會是 false，直接給假版本號；
 * 萬一實機呼叫仍然失敗，也用同一個假版本號兜底，不讓 ABOUT 區塊開天窗。
 */

import { getVersion } from "@tauri-apps/api/app";
import { isTauri } from "@tauri-apps/api/core";

const DEV_VERSION = "0.2.0 (dev)";

export async function loadAppVersion(): Promise<string> {
  try {
    if (!isTauri()) return DEV_VERSION;
    const v = await getVersion();
    return v ? `v${v}` : DEV_VERSION;
  } catch {
    return DEV_VERSION;
  }
}
