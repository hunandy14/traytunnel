/**
 * 把建置產物複製成「發佈用檔名」放進根目錄的 out/。
 *
 * 版本號的單一來源是 src-tauri/Cargo.toml 的 [package] version——這也是
 * tauri.conf.json 省略 "version" 時 Tauri v2 的官方 fallback 來源，跟
 * exe 內嵌的版本、NSIS 安裝檔名用的是同一個值，這裡不另外維護一份。
 *
 * 依 process.platform 分流（跟 release.yml 的 build matrix 一一對應；
 * Windows 這條路徑維持原樣一字不動，新增的是 macOS 分支）：
 *
 * Windows（process.platform === "win32"）產出三個檔案（來源不存在的那項會
 * 跳過並印一行提示，所以只編 exe 不打包 NSIS 的 build:exe 也能照跑）：
 *
 *   traytunnel-<v>.exe        一般單檔，設定檔走 %USERPROFILE%\.traytunnel.toml
 *   traytunnel-<v>p.exe       可攜版，與上面同一顆二進位，差別只在檔名結尾的 p
 *                             會讓程式改用 exe 旁邊的 traytunnel.toml
 *   traytunnel-<v>-setup.exe  NSIS 安裝檔
 *
 * macOS（process.platform === "darwin"）只出 aarch64（Apple Silicon），
 * 產出：
 *
 *   traytunnel-<v>-aarch64.dmg              DMG 安裝映像
 *   traytunnel-<v>-aarch64.app.tar.gz       updater 用的壓縮包（createUpdaterArtifacts）
 *   traytunnel-<v>-aarch64.app.tar.gz.sig   上面那顆的 minisign 簽章內容
 *
 * 無相依，直接 node scripts/package.mjs。
 */

import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { readCargoVersion } from "./lib/cargo-version.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outDir = join(root, "out");

/** 版本號的單一來源：src-tauri/Cargo.toml 的 [package] version（解析邏輯見 scripts/lib/cargo-version.mjs） */
function readVersion() {
  const cargoToml = join(root, "src-tauri", "Cargo.toml");
  return readCargoVersion(cargoToml, readFileSync(cargoToml, "utf8"));
}

/** 位元組數字加上千分位，只是印出來好讀 */
function bytes(n) {
  return n.toLocaleString("en-US");
}

const version = readVersion();
const release = join(root, "src-tauri", "target", "release");

const jobs =
  process.platform === "darwin"
    ? [
        {
          from: join(release, "bundle", "dmg", `traytunnel_${version}_aarch64.dmg`),
          to: `traytunnel-${version}-aarch64.dmg`,
          note: "DMG 安裝映像",
        },
        {
          from: join(release, "bundle", "macos", "traytunnel.app.tar.gz"),
          to: `traytunnel-${version}-aarch64.app.tar.gz`,
          note: "updater 用的壓縮包",
        },
        {
          from: join(release, "bundle", "macos", "traytunnel.app.tar.gz.sig"),
          to: `traytunnel-${version}-aarch64.app.tar.gz.sig`,
          note: "updater 壓縮包的 minisign 簽章",
        },
      ]
    : [
        {
          from: join(release, "traytunnel.exe"),
          to: `traytunnel-${version}.exe`,
          note: "一般單檔",
        },
        {
          from: join(release, "traytunnel.exe"),
          to: `traytunnel-${version}p.exe`,
          note: "可攜版（檔名結尾 p，設定檔放 exe 旁邊）",
        },
        {
          from: join(release, "bundle", "nsis", `traytunnel_${version}_x64-setup.exe`),
          to: `traytunnel-${version}-setup.exe`,
          note: "NSIS 安裝檔",
        },
      ];

// 每次都重來，才不會留下上一版的檔案讓人拿錯
rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

let made = 0;
let skipped = 0;
for (const job of jobs) {
  if (!existsSync(job.from)) {
    console.log(`  跳過 ${job.to}（來源還沒建出來：${job.from}）`);
    skipped += 1;
    continue;
  }
  const dest = join(outDir, job.to);
  copyFileSync(job.from, dest);
  console.log(`  out/${job.to}  ${bytes(statSync(dest).size)} bytes  ${job.note}`);
  made += 1;
}

console.log(
  `發佈檔已放進 out/：${made} 個${skipped > 0 ? `，跳過 ${skipped} 個` : ""}（版本 ${version}）`,
);
