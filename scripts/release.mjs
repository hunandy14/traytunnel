/**
 * 一鍵發版：bump → 鎖檔同步 → 分支 → commit → push → 開 PR → 掛 auto-merge。
 *
 * 用法：npm run release <x.y.z> [-- --dry-run]
 *   （npm 轉發純位置參數不需要 `--`，但 `--dry-run` 這種旗標視 npm 版本可能要加，
 *    保險起見兩種寫法都能用；也可以直接 node scripts/release.mjs <x.y.z> --dry-run）
 *
 * 背景：main 已開 branch protection，要求 ci 檢查綠才可合併。這支腳本開完 PR 後
 * 立刻 `gh pr merge --auto --merge`：CI 綠了 GitHub 就自動合併，合併觸發
 * autotag.yml 貼 tag、release.yml 建置發佈，全程不必再手動介入。
 *
 * 步驟（依序，任一步失敗立即停止，不自動回滾）：
 *   git checkout -b release/<x.y.z>
 *   node scripts/bump.mjs <x.y.z>                              重用既有 bump 邏輯
 *   cargo update -p traytunnel --manifest-path src-tauri/Cargo.toml   同步 Cargo.lock
 *   npm install --package-lock-only                             同步 package-lock.json
 *   git add ... && git commit -m "版本升級至 <x.y.z>"
 *   git push -u origin release/<x.y.z>
 *   gh pr create --title "版本升級至 <x.y.z>" --body ...
 *   gh pr merge --auto --merge
 *   git checkout main                                            收尾留在 main
 *
 * --dry-run：只印出每一步將執行的指令，不做任何會改動 git/gh/檔案狀態的動作。
 * 前置檢查裡「唯讀」的幾項（git status、目前分支、gh auth status）不論
 * dry-run 與否都真的執行——反正不會動到任何東西，也才能在 dry-run 下驗證
 * 失敗訊息。`git pull --ff-only` 因為會真的移動本地 main，dry-run 時只印出
 * 指令、不執行。
 *
 * 無相依，直接 node scripts/release.mjs <x.y.z> [--dry-run]。
 */

import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const SEMVER_RE = /^\d+\.\d+\.\d+$/;

const rawArgs = process.argv.slice(2);
const dryRun = rawArgs.includes("--dry-run");
const version = rawArgs.find((a) => a !== "--dry-run");

if (!version || !SEMVER_RE.test(version)) {
  console.error(`用法：npm run release <x.y.z> [--dry-run]（嚴格 semver，例如 1.2.3；不可帶 v 前綴或空白）`);
  console.error(`收到的參數：${JSON.stringify(rawArgs)}`);
  process.exit(1);
}

const branch = `release/${version}`;

/** 只是給印出來的指令行加點可讀性，不影響實際執行（實際執行一律走 args 陣列，不經 shell 解析） */
function formatCommand(command, args) {
  const parts = args.map((a) => (/\s/.test(a) ? `"${a}"` : a));
  return [command, ...parts].join(" ");
}

/**
 * 唯讀查詢：不論 dry-run 與否都真的執行（不會動到任何東西）。
 * npm 在 Windows 上是 npm.cmd，需要 shell:true 才能被 spawnSync 找到；
 * git/gh/cargo/node 都是 .exe，直接執行即可，不必經過 shell（避免參數裡的
 * 中文與標點被 shell 重新解析）。
 */
function runReadOnly(command, args) {
  const needsShell = process.platform === "win32" && command === "npm";
  return spawnSync(command, args, { cwd: root, encoding: "utf8", shell: needsShell });
}

/** 會改動 git/gh/檔案狀態的步驟：dry-run 時只印出指令，不執行；否則真的跑並檢查結束碼 */
function runMutating(command, args) {
  const label = formatCommand(command, args);
  if (dryRun) {
    console.log(`[dry-run] $ ${label}`);
    return null;
  }
  console.log(`$ ${label}`);
  const needsShell = process.platform === "win32" && command === "npm";
  const result = spawnSync(command, args, { cwd: root, encoding: "utf8", shell: needsShell });
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
  if (result.error) {
    throw new Error(`執行失敗：${label}\n${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error(`指令結束碼非 0（${result.status}）：${label}`);
  }
  return result;
}

function fail(message, cleanup) {
  console.error("");
  console.error(`發版中止：${message}`);
  if (cleanup) {
    console.error("");
    console.error("目前狀態與收拾方式：");
    console.error(cleanup);
  }
  process.exit(1);
}

console.log(`== 發版：${version}${dryRun ? "（--dry-run，僅模擬，不會有任何實際變更）" : ""} ==`);
console.log("");

// ---- 前置檢查 ----
console.log("-- 前置檢查 --");

const statusResult = runReadOnly("git", ["status", "--porcelain"]);
if (statusResult.status !== 0) {
  fail(`\`git status --porcelain\` 執行失敗：${statusResult.stderr || statusResult.error?.message || ""}`);
}
if (statusResult.stdout.trim() !== "") {
  fail(
    "工作區不乾淨，有未提交的變更：\n" + statusResult.stdout,
    "先 commit 或 stash 掉這些變更，再重新執行 npm run release。",
  );
}
console.log("  工作區乾淨");

const branchResult = runReadOnly("git", ["rev-parse", "--abbrev-ref", "HEAD"]);
if (branchResult.status !== 0) {
  fail(`\`git rev-parse --abbrev-ref HEAD\` 執行失敗：${branchResult.stderr || branchResult.error?.message || ""}`);
}
const currentBranch = branchResult.stdout.trim();
if (currentBranch !== "main") {
  fail(
    `目前分支是 \`${currentBranch}\`，必須在 \`main\` 上才能發版。`,
    "執行 `git checkout main` 切回主分支後再重試。",
  );
}
console.log("  目前在 main");

try {
  runMutating("git", ["pull", "--ff-only", "origin", "main"]);
} catch (err) {
  fail(
    `\`git pull --ff-only origin main\` 失敗：${err.message}`,
    "本地 main 可能落後或有分岔，手動處理（例如先確認沒有本地獨有 commit 再 reset，或解決衝突）後再重試。",
  );
}
if (dryRun) {
  console.log("  （dry-run：略過實際 pull，假設會成功）");
} else {
  console.log("  main 已同步遠端最新");
}

const authResult = runReadOnly("gh", ["auth", "status"]);
if (authResult.status !== 0) {
  fail(
    `\`gh auth status\` 沒過：\n${authResult.stdout}${authResult.stderr}`,
    "先 `gh auth login` 完成登入後再重試。",
  );
}
console.log("  gh 已登入");

console.log("");
console.log("-- 執行步驟 --");

let step = "建立分支";
let prCreateResult = null;
try {
  runMutating("git", ["checkout", "-b", branch]);

  step = "同步版本號（bump）";
  runMutating("node", ["scripts/bump.mjs", version]);

  step = "同步 Cargo.lock";
  runMutating("cargo", ["update", "-p", "traytunnel", "--manifest-path", "src-tauri/Cargo.toml"]);

  step = "同步 package-lock.json";
  runMutating("npm", ["install", "--package-lock-only"]);

  step = "git add";
  runMutating("git", [
    "add",
    "src-tauri/Cargo.toml",
    "src-tauri/Cargo.lock",
    "package.json",
    "package-lock.json",
  ]);

  step = "git commit";
  runMutating("git", ["commit", "-m", `版本升級至 ${version}`]);

  step = "git push";
  runMutating("git", ["push", "-u", "origin", branch]);

  step = "gh pr create";
  const prBody = `版本升級至 ${version}。合併進 main 後 autotag.yml 會自動建立並推送 tag v${version}，接著觸發 release.yml 建置發佈——不需要再手動貼 tag。`;
  prCreateResult = runMutating("gh", [
    "pr",
    "create",
    "--title",
    `版本升級至 ${version}`,
    "--body",
    prBody,
  ]);

  step = "gh pr merge --auto";
  runMutating("gh", ["pr", "merge", "--auto", "--merge"]);

  step = "切回 main";
  runMutating("git", ["checkout", "main"]);
} catch (err) {
  fail(
    `「${step}」這一步失敗：${err.message}`,
    [
      `目前應該還停在分支 \`${branch}\`（除非失敗發生在「建立分支」之前，那樣仍在 main 上）。`,
      "確認錯誤原因並修好後，可以手動接著跑剩下的步驟；",
      "若要放棄這次嘗試：",
      `  git checkout main && git branch -D ${branch}`,
      `  （若已經 push 過，還要：git push origin --delete ${branch}）`,
      "若 PR 已經建立但還沒掛 auto-merge 或想取消：gh pr close <PR 編號>",
    ].join("\n"),
  );
}

console.log("");
if (dryRun) {
  console.log(`== dry-run 結束：以上是 \`npm run release ${version}\` 實際會做的每一步，沒有任何真實變更 ==`);
} else {
  console.log("== 發版流程已完成 ==");
  console.log("");
  const prUrlMatch = prCreateResult?.stdout.match(/https:\/\/\S+/);
  if (prUrlMatch) {
    console.log(`PR：${prUrlMatch[0]}`);
  }
  console.log(
    "auto-merge 已掛上，CI 綠後自動合併 → 貼 tag → 出貨；" +
      "反悔請在 CI 完成前 `gh pr merge --disable-auto` 或關閉 PR。",
  );
}
