/**
 * 同步版本號到 package.json：
 *
 *   src-tauri/Cargo.toml [package] 區塊的 version（單一權威——Tauri v2 官方 fallback
 *                                    機制：tauri.conf.json 省略 "version" 時，
 *                                    getVersion / NSIS / tauri-action 都會自動讀這裡）
 *   package.json         的 "version"（純門面，不影響建置行為）
 *
 * 用法：npm run bump <x.y.z>
 *
 * 修改前會先確認兩處現值一致，不一致就列出來中止，要求先手動對齊。
 * 只換版本號那個 token，其餘格式（縮排、換行）原樣保留。
 *
 * 不會自動 commit、不會自動 tag——成功後印一行建議指令供複製貼上。
 *
 * 無相依，直接 node scripts/bump.mjs <x.y.z>。
 */

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { packageSectionRange, readCargoVersion } from "./lib/cargo-version.mjs";
import { SEMVER_RE } from "./lib/semver.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const newVersion = process.argv[2];

if (!newVersion || !SEMVER_RE.test(newVersion)) {
  console.error(`用法：npm run bump <x.y.z>（嚴格 semver，例如 1.2.3；不可帶 v 前綴或空白）`);
  console.error(`收到的參數：${JSON.stringify(newVersion ?? "")}`);
  process.exit(1);
}

const files = {
  cargoToml: join(root, "src-tauri", "Cargo.toml"),
  packageJson: join(root, "package.json"),
};

const VERSION_FIELD_RE = /"version"\s*:\s*"([^"]+)"/;

/** package.json 只在頂層有一個 "version" 欄位，直接抓第一個 match */
function readJsonVersion(path, text) {
  const m = text.match(VERSION_FIELD_RE);
  if (!m) {
    throw new Error(`${path} 找不到 "version" 欄位`);
  }
  return m[1];
}

const cargoTomlText = readFileSync(files.cargoToml, "utf8");
const packageJsonText = readFileSync(files.packageJson, "utf8");

const current = {
  cargoToml: readCargoVersion(files.cargoToml, cargoTomlText),
  packageJson: readJsonVersion(files.packageJson, packageJsonText),
};

const distinct = new Set(Object.values(current));
if (distinct.size > 1) {
  console.error("兩處版本號現值不一致，先手動對齊再重跑：");
  console.error(`  src-tauri/Cargo.toml : ${current.cargoToml}`);
  console.error(`  package.json         : ${current.packageJson}`);
  process.exit(1);
}

const oldVersion = current.cargoToml;

if (oldVersion === newVersion) {
  console.error(`目前版本已經是 ${newVersion}，沒有需要改的`);
  process.exit(1);
}

const { sectionStart, sectionEnd } = packageSectionRange(files.cargoToml, cargoTomlText);
const newSection = cargoTomlText
  .slice(sectionStart, sectionEnd)
  .replace(/^version\s*=\s*"([^"]+)"/m, `version = "${newVersion}"`);
const newCargoTomlText =
  cargoTomlText.slice(0, sectionStart) + newSection + cargoTomlText.slice(sectionEnd);

const newPackageJsonText = packageJsonText.replace(VERSION_FIELD_RE, `"version": "${newVersion}"`);

writeFileSync(files.cargoToml, newCargoTomlText);
writeFileSync(files.packageJson, newPackageJsonText);

console.log(`${oldVersion} → ${newVersion}，2 檔已同步`);
console.log("");
console.log("建議指令（不會自動執行，複製貼上）：");
console.log(
  `  git add src-tauri/Cargo.toml package.json && git commit -m "版本升級至 ${newVersion}" && git tag -a v${newVersion} -m "v${newVersion}"`,
);
