/**
 * 把 macOS 的 `.app` bundle 複製進 repo 根目錄的 bin/，方便本機直接雙擊試跑，
 * 不必每次都到 src-tauri/target/release/bundle/macos/ 底下找。
 *
 * bin/ 整個在 .gitignore 裡（不進版本控制，純本機產物），所以每次都可以放心
 * 整個砍掉重建，不怕誤刪任何要保留的東西。
 *
 * 用法：npm run build:mac（先跑 `tauri build --bundles app`，這支腳本只管複製；
 * 已經 build 過的話也可以單獨重跑 node scripts/copy-app-bundle.mjs）。
 *
 * 無相依，直接 node scripts/copy-app-bundle.mjs。
 */

import { cpSync, existsSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const src = join(root, "src-tauri", "target", "release", "bundle", "macos", "Traytunnel.app");
const dest = join(root, "bin", "Traytunnel.app");

if (!existsSync(src)) {
  console.error(`找不到建置產物：${src}`);
  console.error("先跑 `tauri build --bundles app`（或直接 `npm run build:mac`）再重試");
  process.exit(1);
}

// 每次都重來，才不會讓上一版殘留的檔案跟這一版混在一起
rmSync(dest, { recursive: true, force: true });
cpSync(src, dest, { recursive: true });

console.log(`已複製 .app 到 ${dest}`);
