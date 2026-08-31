/**
 * 讀取 scripts/package.mjs 產出的 out/manifest.<platform_key>.json，讓消費端不必
 * 手寫 traytunnel-<version>-... 這類檔名字面值。
 *
 * 誠實的現況：目前只有 compose job 這一側（scripts/compose-latest-json.mjs）真的
 * 用這支模組。release.yml 的 build job「Verify updater signature matches release
 * asset」步驟是自己用 jq 讀同一份 manifest 的 .bundle_source／.asset，並沒有共用
 * 這裡的讀取與驗證邏輯——所以這支模組加的欄位檢查（型別、platform_key／version
 * 一致性）對 build job 那一側不生效。把那一步也 node 化、讓兩側共用同一個讀取點
 * 是 backlog（REU-2），不是這支模組現在的事實。
 *
 * 檔名帶 platform_key 後綴的理由見 scripts/package.mjs 開頭註解：
 * download-artifact 的 merge-multiple 會把兩腿的 out/ 攤平進同一個目錄，共用
 * 同一個檔名（例如單純 manifest.json）會讓其中一腿的內容被另一腿覆蓋掉。
 */

import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

/** @param {string} platformKey @returns {string} */
export function manifestFileName(platformKey) {
  return `manifest.${platformKey}.json`;
}

/**
 * @param {string} dir out/ 目錄路徑
 * @param {string} platformKey 例如 "windows-x86_64" / "darwin-aarch64"
 * @param {string} [expectedVersion] 這次發佈的版本；有給就一併比對 manifest.version
 * @returns {{ platform_key: string, version: string, asset: string, sig: string, bundle_source: string }}
 */
export function readManifest(dir, platformKey, expectedVersion) {
  const path = join(dir, manifestFileName(platformKey));
  if (!existsSync(path)) {
    throw new Error(
      `找不到 ${path}——package.mjs 應該已經在該平台的建置步驟（CI 是 tauri build --target ` +
        `<matrix.rust_target> + package.mjs --target <matrix.rust_target>，本機等價流程是 ` +
        `npm run build:dist，不帶 --target）裡產生它，檢查該平台的建置有沒有成功、或簽署產物` +
        `是否真的被複製進 out/。`,
    );
  }
  const manifest = JSON.parse(readFileSync(path, "utf8"));
  for (const field of ["platform_key", "version", "asset", "sig", "bundle_source"]) {
    if (!manifest[field] || typeof manifest[field] !== "string") {
      throw new Error(`${path} 缺少或欄位型別不對：${field}`);
    }
  }
  if (manifest.platform_key !== platformKey) {
    throw new Error(
      `${path} 的 platform_key（${manifest.platform_key}）跟檔名裡的 ${platformKey} 對不起來`,
    );
  }
  // WRP-5：跟上面的 platform_key 檢查同構的防禦。manifest 的 asset／sig 會被
  // 拿去組 latest.json 的下載網址，而網址的版本段來自這次發佈的 tag——out/
  // 底下若殘留上一版的 manifest（今天 package.mjs 每次都 rmSync，所以不會
  // 發生；但這條假設不該是唯一的防線），latest.json 的 version 就會跟下載
  // URL 的版本對不上，updater 下載 404 或直接裝回舊版。
  if (expectedVersion !== undefined && manifest.version !== expectedVersion) {
    throw new Error(
      `${path} 的 version（${manifest.version}）跟這次發佈的 ${expectedVersion} 對不起來——` +
        `out/ 底下可能殘留上一版的產物。latest.json 會用這次的版本組下載網址，` +
        `跟舊 manifest 指到的檔名對不上，因此中止。`,
    );
  }
  return manifest;
}
