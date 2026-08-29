/**
 * 把 macOS 的 `.app` bundle 複製進 repo 根目錄的 bin/，方便本機直接雙擊試跑，
 * 不必每次都到 src-tauri/target/release/bundle/macos/ 底下找。
 *
 * bin/ 整個在 .gitignore 裡（不進版本控制，純本機產物），所以每次都可以放心
 * 整個砍掉重建，不怕誤刪任何要保留的東西。
 *
 * 用法：npm run build:mac（先跑 `tauri build --bundles app --config
 * '{"bundle":{"createUpdaterArtifacts":false}}'`，這支腳本只管複製；已經
 * build 過的話也可以單獨重跑 node scripts/copy-app-bundle.mjs）。
 *
 * 那個 --config 覆寫是刻意的：base tauri.conf.json 開了
 * bundle.createUpdaterArtifacts，若不覆寫，`--bundles app` 也會嘗試產出
 * updater 用的 .app.tar.gz 並用 TAURI_SIGNING_PRIVATE_KEY 簽章——本機平常
 * 沒有這把私鑰，簽不出來就會讓整個 `tauri build` 以非零狀態結束（.app 其實
 * 已經建好，只是簽不出 .sig），這支腳本就不會被 `&&` 接著跑到。這裡只是要
 * 本機快速試跑 .app，updater 產物與簽章跟這個用途無關，直接關掉最乾脆；
 * 真正需要簽出 updater 產物的路徑是 `npm run build:dist`（CI 的
 * release.yml 用的就是這顆，那邊有 TAURI_SIGNING_PRIVATE_KEY 可用）。
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
