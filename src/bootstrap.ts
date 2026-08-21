import { isTauri } from "@tauri-apps/api/core";

/**
 * 頁面啟動點。
 *
 * 只有在「開發模式」而且「偵測不到 Tauri runtime」時，才動態載入瀏覽器用的
 * 假後端。isTauri() 是 Tauri v2 官方提供的偵測方式（等同於檢查
 * window.__TAURI_INTERNALS__）。
 *
 * import.meta.env.DEV 在正式建置時是常數 false，Rollup 會把整個分支連同
 * 那個動態 import 一起消掉，所以 dev-mock 不會出現在打包產物裡。
 */
export async function bootstrap(start: () => Promise<void> | void) {
  if (import.meta.env.DEV && !isTauri()) {
    const { installDevMock } = await import("./dev-mock");
    installDevMock();
  }
  await start();
}
