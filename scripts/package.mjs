/**
 * 把建置產物複製成「發佈用檔名」放進根目錄的 out/。
 *
 * 版本號的單一來源是 src-tauri/Cargo.toml 的 [package] version——這也是
 * tauri.conf.json 省略 "version" 時 Tauri v2 的官方 fallback 來源，跟
 * exe 內嵌的版本、NSIS 安裝檔名用的是同一個值，這裡不另外維護一份。
 *
 * platform_key／要建置的平台家族（Windows／macOS）由 --target（或
 * TAURI_BUILD_TARGET 環境變數）指定的 Rust target triple 推導，不再單純
 * 用 process.platform 猜——猜測法的問題：release.yml 過去 `tauri build`
 * 沒帶 --target，一個 runner（例如 Intel 版 macOS runner）只要架構跟
 * platform_key 現在寫死的假設（Apple Silicon）不同，就會把 x86_64 的二進位
 * 標成 darwin-aarch64 發出去，使用者抓到跑不動的檔案，而且沒有任何訊號。
 * release.yml 的 build 步驟一律會帶 --target（見該檔 matrix.rust_target）；
 * 這裡沒收到 --target 時（本機手動 `npm run build:dist` 之類）才退回用
 * process.platform／process.arch 猜，並印警告——純粹是本機開發便利，不代表
 * CI 允許猜。
 *
 * Windows 分支多出一筆 .sig：舊管線是 Windows 單平台，compose 那步就跑在同一個
 * job 裡，直接從 src-tauri/target/release/bundle/nsis/ 讀簽章，不需要經過 out/。
 * 雙平台化之後 compose 被拆成獨立 job，兩腿只靠 out/ 打包成 artifact 交接——
 * 簽章沒被複製進 out/ 就等於在 compose 端不存在，Windows 條目會靜默沿用底稿舊值。
 *
 * Windows（platform_key windows-x86_64）產出四個檔案：
 *
 *   traytunnel-<v>.exe            一般單檔，設定檔走 %USERPROFILE%\.traytunnel.toml
 *   traytunnel-<v>p.exe           可攜版，與上面同一顆二進位，差別只在檔名結尾的 p
 *                                 會讓程式改用 exe 旁邊的 traytunnel.toml
 *   traytunnel-<v>-setup.exe      NSIS 安裝檔
 *   traytunnel-<v>-setup.exe.sig  上面那顆的 minisign 簽章內容——latest.json 的
 *                                 windows-x86_64 signature 就是讀這顆；漏了它，
 *                                 compose 會抓不到簽章（見 release.yml 的
 *                                 fail-closed 檢查）。
 *
 * macOS（platform_key darwin-aarch64）產出三個檔案：
 *
 *   traytunnel-<v>-aarch64.dmg              DMG 安裝映像
 *   traytunnel-<v>-aarch64.app.tar.gz       updater 用的壓縮包（createUpdaterArtifacts）
 *   traytunnel-<v>-aarch64.app.tar.gz.sig   上面那顆的 minisign 簽章內容
 *
 * 「預期必有」的產物與 --target／--allow-partial 的關係：release.yml 的 CI
 * 建置一律帶 --target（見上段），這裡只要收到 --target 就視為「這次是正式
 * 發佈建置」，上面列的每一個檔案都必須真的產出，缺一個就 exit 1——舊行為
 * 只印一行「跳過」然後放行，manifest 依然可能寫出去，缺一顆 dmg／exe 的
 * release 就這樣悄悄發出去，沒有任何訊號。本機沒帶 --target 手動跑（例如
 * `npm run build:win:exe` 這種刻意只建部分產物的既有流程）維持舊的寬鬆
 * 「跳過並印一行」行為，不受這道檢查影響。
 *
 * 另外產出 out/manifest.<platform_key>.json（見下面 signedArtifact 區塊）：
 * 記錄「這次要拿去發佈、latest.json 會提到的那顆 updater 產物」的檔名／簽章檔
 * 名／原始（tauri 實際簽章的）路徑。這三個名字過去在 release.yml 裡跟這裡各自
 * 手寫一份（package.mjs 的 to、compose job 的 case 陳述式、build job 兩個
 * 「Verify updater signature」步驟），四處對同一組檔名字面值，改一處很容易漏
 * 改另外三處。package.mjs 是唯一真的知道「這次到底複製出了什麼檔案」的地方，
 * 讓它順手把答案寫成 manifest，release.yml 只讀 manifest、不再手寫檔名。
 *
 * 檔名刻意帶 platform_key 後綴（manifest.windows-x86_64.json／
 * manifest.darwin-aarch64.json），不是單純 manifest.json：compose job 用
 * actions/download-artifact 的 merge-multiple 把兩腿的 out/ 攤平進同一個目
 * 錄，兩腿若都寫 manifest.json 會直接互相覆蓋，其中一個平台的 manifest 就
 * 這樣消失。
 *
 * Usage:
 *   node scripts/package.mjs [--target <rust-triple>]
 * （或設 TAURI_BUILD_TARGET 環境變數代替 --target；兩者都沒有就退回猜測。）
 *
 * 無外部套件相依，直接 node scripts/package.mjs。
 */

import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join, relative, resolve, sep } from "node:path";
import { parseArgs } from "node:util";
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

// Rust target triple → release.yml plan job 用的 platform_key。新增平台
// （例如 x86_64-apple-darwin／aarch64-pc-windows-msvc）先在這裡補一筆，
// 再去 release.yml 的 plan job 補對應的 matrix entry。
const TARGET_PLATFORM_KEYS = {
  "aarch64-apple-darwin": "darwin-aarch64",
  "x86_64-apple-darwin": "darwin-x86_64",
  "x86_64-pc-windows-msvc": "windows-x86_64",
  "aarch64-pc-windows-msvc": "windows-aarch64",
};

// mac 架構驗證用：target triple 的 CPU 段 → lipo/file 認得的架構名稱。
const TARGET_LIPO_ARCH = {
  "aarch64-apple-darwin": "arm64",
  "x86_64-apple-darwin": "x86_64",
};

function parseCliTarget() {
  const { values } = parseArgs({
    options: { target: { type: "string" } },
    strict: false,
    allowPositionals: true,
  });
  return values.target || process.env.TAURI_BUILD_TARGET || null;
}

const version = readVersion();
const target = parseCliTarget();

// Cargo／Tauri 的建置輸出目錄規則：完全不帶 --target 時，輸出在
// target/release/（host 三元組隱含在路徑之外）；只要明確帶了 --target
// （不管是不是剛好等於 host 的三元組），輸出一律搬進 target/<triple>/release/
// 這一層額外的子目錄——這是 rustc／cargo 本身的行為，tauri 的 bundler 照樣
// 沿用（見 tauri build 實際印出的 Bundling .../target/<triple>/release/bundle/...
// 路徑）。M9 把 --target 從「不傳」改成「一律明確傳」之後，這裡如果還沿用
// 舊的 target/release/ 路徑，會導致所有來源檔案都找不到——而且 M10 的
// fail-closed 檢查會忠實地把這個路徑錯誤回報成「產物缺失」，看起來像建置
// 失敗，其實只是這裡沒跟著 --target 調整路徑。
const release = target
  ? join(root, "src-tauri", "target", target, "release")
  : join(root, "src-tauri", "target", "release");

let platformKey;
let osFamily; // "darwin" | "windows"
if (target) {
  platformKey = TARGET_PLATFORM_KEYS[target];
  if (!platformKey) {
    console.error(
      `::error::不認得的建置目標 --target ${target}（支援：${Object.keys(TARGET_PLATFORM_KEYS).join("、")}）。` +
        `新增平台前先在 scripts/package.mjs 的 TARGET_PLATFORM_KEYS 補上對應的 platform_key，` +
        `不要放行一個猜出來、可能標錯架構的 key。`,
    );
    process.exit(1);
  }
  osFamily = target.includes("windows") ? "windows" : "darwin";
} else {
  const archMap = { arm64: "aarch64", x64: "x86_64" };
  osFamily = process.platform === "darwin" ? "darwin" : "windows";
  const guessedArch = archMap[process.arch] || "x86_64";
  platformKey = `${osFamily}-${guessedArch}`;
  console.log(
    `  警告：沒有帶 --target（或 TAURI_BUILD_TARGET 環境變數），退回用 process.platform／` +
      `process.arch 猜出 platform_key=${platformKey}。這只適合本機開發；release.yml 的 CI ` +
      `建置一律會明確傳 --target，不應該依賴這裡的猜測。`,
  );
}

// 是否為「正式發佈建置」：帶了 --target 就代表這是 release.yml 的 CI 建置，
// 「預期必有」的產物缺一個就要讓整個建置失敗，不能放行不完整的發佈（見檔頭
// 註解）。本機沒帶 --target 手動跑，維持舊的「跳過並印一行」寬鬆行為，不擋
// build:win:exe 這類刻意只建部分產物的既有流程。
const strict = Boolean(target);

/** 對應 job 的 .sig 檔：來源與目的地都是原始檔名／發佈檔名各自加上 .sig 後綴——
 *  這條規則兩個平台都成立（見下面 jobs 定義），不再另外手寫一份 signatureFor
 *  字串去跟 signed job 的 to 逐字比對；.sig 是不是「這顆 signed 產物的簽章」
 *  由呼叫端傳入哪個 job 決定，不是靠字串巧合對出來的。
 */
function sigJobFor(signedJob, noteSuffix) {
  return {
    from: `${signedJob.from}.sig`,
    to: `${signedJob.to}.sig`,
    note: `${signedJob.note}${noteSuffix}`,
  };
}

const baseJobs =
  osFamily === "darwin"
    ? [
        {
          // bundle 檔名跟的是 tauri.macos.conf.json 的 productName："Traytunnel"
          // （首字大寫，對齊 mac 選單列慣例——見該檔與 platform/macos/menu.rs
          // 的說明）。Windows 那邊 productName 維持 "traytunnel" 沒有動，這條
          // 大小寫差異只在這裡的 from 路徑出現；out/ 的目標檔名兩邊都刻意保持
          // 小寫 traytunnel-<version>-...，不隨 productName 變。
          from: join(release, "bundle", "dmg", `Traytunnel_${version}_aarch64.dmg`),
          to: `traytunnel-${version}-aarch64.dmg`,
          note: "DMG 安裝映像",
        },
        {
          from: join(release, "bundle", "macos", "Traytunnel.app.tar.gz"),
          to: `traytunnel-${version}-aarch64.app.tar.gz`,
          note: "updater 用的壓縮包",
          // 這顆是 latest.json 會提到、release.yml 要驗證簽章一致性的「已簽署
          // updater 產物」——manifest 的 asset／bundle_source 就是從這筆算出來的
          // （見下面 signedArtifact）。
          signed: true,
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
          signed: true,
        },
      ];

const signedBaseJob = baseJobs.find((job) => job.signed);
const jobs = signedBaseJob ? [...baseJobs, sigJobFor(signedBaseJob, "的 minisign 簽章")] : baseJobs;

// 每次都重來，才不會留下上一版的檔案讓人拿錯
rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

let made = 0;
let skipped = 0;
const producedTo = new Set();
for (const job of jobs) {
  if (!existsSync(job.from)) {
    if (strict) {
      console.error(
        `::error::預期必有的發佈產物缺失：${job.to}（${job.note}）——來源不存在：${job.from}。` +
          `這是帶 --target 的正式建置（release.yml），任何一項缺失都必須讓建置失敗，不能放行` +
          `不完整的發佈。`,
      );
      process.exit(1);
    }
    console.log(`  跳過 ${job.to}（來源還沒建出來：${job.from}）`);
    skipped += 1;
    continue;
  }
  const dest = join(outDir, job.to);
  copyFileSync(job.from, dest);
  console.log(`  out/${job.to}  ${bytes(statSync(dest).size)} bytes  ${job.note}`);
  made += 1;
  producedTo.add(job.to);
}

console.log(
  `發佈檔已放進 out/：${made} 個${skipped > 0 ? `，跳過 ${skipped} 個` : ""}（版本 ${version}）`,
);

/**
 * mac 專用：驗證實際建出來的 .app 二進位架構跟 platform_key 宣稱的一致，
 * 不符就硬失敗（M9 的核心——光靠 --target 推導 platform_key 還不夠，
 * --target 跟 runner 實際架構本身也可能不一致，例如 CI 設定改壞、或在
 * 不支援的 runner 上要求交叉編譯，這裡是最後一道能在發佈前擋下來的關卡）。
 */
function verifyMacBinaryArch() {
  const expectedArch = TARGET_LIPO_ARCH[target] ?? (platformKey === "darwin-aarch64" ? "arm64" : "x86_64");
  // Tauri 沒設 mainBinaryName，bundle 內執行檔預設用 Cargo [package] name
  // （見 src-tauri/Cargo.toml），跟 out/traytunnel.exe 用的是同一個名字。
  const appBinary = join(release, "bundle", "macos", "Traytunnel.app", "Contents", "MacOS", "traytunnel");
  if (!existsSync(appBinary)) {
    console.log(`  跳過架構驗證（找不到 ${appBinary}）`);
    return;
  }

  let archOutput = null;
  const lipoResult = spawnSync("lipo", ["-archs", appBinary], { encoding: "utf8" });
  if (!lipoResult.error && lipoResult.status === 0) {
    archOutput = lipoResult.stdout;
  } else {
    const fileResult = spawnSync("file", ["-b", appBinary], { encoding: "utf8" });
    if (!fileResult.error && fileResult.status === 0) {
      archOutput = fileResult.stdout;
    }
  }

  if (archOutput === null) {
    console.error(
      `::error::既沒有 lipo 也沒有 file 可用來驗證 ${appBinary} 的架構，中止——缺這道檢查，` +
        `架構標錯的風險就是這次要修的問題本身，不能悄悄放行。`,
    );
    process.exit(1);
  }
  if (!archOutput.includes(expectedArch)) {
    console.error(
      `::error::架構不符：${appBinary} 實際是「${archOutput.trim()}」，跟 platform_key ${platformKey}` +
        `（預期 ${expectedArch}）對不上。Runner 架構跟 --target 不一致，中止發布——標錯架構的產物` +
        `一旦發出去，使用者會抓到跑不動的二進位。`,
    );
    process.exit(1);
  }
  console.log(`  架構驗證通過：${appBinary} 是 ${expectedArch}（platform_key ${platformKey}）`);
}

const signedJob = jobs.find((job) => job.signed);
const sigJob = signedJob && jobs.find((job) => job.from === `${signedJob.from}.sig`);
const signedProduced = Boolean(signedJob && sigJob && producedTo.has(signedJob.to) && producedTo.has(sigJob.to));

if (osFamily === "darwin" && signedProduced) {
  verifyMacBinaryArch();
}

// manifest.<platform_key>.json：只在「已簽署 updater 產物」與它的 .sig 都真的
// 複製進 out/ 的時候才寫——例如 build:win:exe（tauri build --no-bundle）不會
// 產生 NSIS 安裝檔，這種情況下沒有簽章可驗證，manifest 索性不存在，跟舊行為
// （skip 且不假裝有簽章）一致；release.yml compose job 的 fail-closed 檢查會
// 在「這次矩陣排了這個平台、manifest 卻不存在」時中止，不會靜默沿用底稿舊值。
if (signedProduced) {
  const manifest = {
    platform_key: platformKey,
    version,
    asset: signedJob.to,
    sig: sigJob.to,
    // 相對於 repo 根目錄、一律用正斜線——bash（含 windows-latest 的 git bash）
    // 與 pwsh 都能直接吃這個路徑，不必再分平台處理反斜線。
    bundle_source: relative(root, signedJob.from).split(sep).join("/"),
  };
  const manifestPath = join(outDir, `manifest.${platformKey}.json`);
  writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
  console.log(`  out/manifest.${platformKey}.json  已簽署產物：${manifest.asset}`);
} else {
  console.log(`  跳過 manifest.${platformKey}.json（已簽署 updater 產物或其 .sig 還沒建出來）`);
}
