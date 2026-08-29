/**
 * 讀取 scripts/package.mjs 產出的 out/manifest.<platform_key>.json——release.yml
 * 的 compose job（組 latest.json）與 build job（驗證簽章一致性）共用同一套讀取
 * 邏輯，兩邊都不再手寫 traytunnel-<version>-... 這類檔名字面值。
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
 * @returns {{ platform_key: string, version: string, asset: string, sig: string, bundle_source: string }}
 */
export function readManifest(dir, platformKey) {
  const path = join(dir, manifestFileName(platformKey));
  if (!existsSync(path)) {
    throw new Error(
      `找不到 ${path}——package.mjs 應該已經在該平台的建置步驟（npm run build:dist）裡產生它，` +
        `檢查該平台的建置有沒有成功、或簽署產物是否真的被複製進 out/。`,
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
  return manifest;
}
