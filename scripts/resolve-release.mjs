/**
 * 解析這次 release 要用的版本號與 tag，寫進 GITHUB_OUTPUT。
 *
 * 唯一呼叫端是 release.yml 的 plan job（見該 job 的 Resolve release version
 * and tag 步驟）：版本／tag 只在 plan 解析一次，build（Windows／macOS 兩腿）
 * 與 compose 都直接吃 plan 的 job outputs（needs.plan.outputs.version /
 * release_tag），不再各自重跑一次解析——過去 build job 的 Windows 腿（pwsh
 * 手刻翻版）、macOS 腿、compose job 三處各自呼叫一次同一套邏輯的作法已經
 * 拿掉，避免三份同步改壞其中一處卻沒改到另外兩處的風險。
 *
 * 環境變數（GITHUB_EVENT_NAME / GITHUB_REF_NAME / GITHUB_OUTPUT 由 Actions 自動提供）：
 *   GITHUB_EVENT_NAME  tag push 事件是 "push"，其餘（workflow_dispatch）視為手動觸發
 *   GITHUB_REF_NAME    push 事件時的 tag 名稱（例如 v0.6.5）
 *   DRY_RUN            "true" 時，手動觸發情境下略過「遠端 tag 是否存在」的檢查
 *                       （純演練用；正式發佈路徑，也就是 tag push 或
 *                       dry_run!=true 的 workflow_dispatch，這關卡照舊）
 *
 * 輸出（寫進 GITHUB_OUTPUT）：version、release_tag
 *
 * 無相依，直接 node scripts/resolve-release.mjs。
 */

import { appendFileSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { readCargoVersion } from "./lib/cargo-version.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const cargoTomlPath = join(root, "src-tauri", "Cargo.toml");
const version = readCargoVersion(cargoTomlPath, readFileSync(cargoTomlPath, "utf8"));
console.log(`Cargo.toml 版本：${version}`);

const eventName = process.env.GITHUB_EVENT_NAME || "";
const dryRun = process.env.DRY_RUN === "true";

let releaseTag;
if (eventName === "push") {
  const tagName = process.env.GITHUB_REF_NAME || "";
  const tagVersion = tagName.replace(/^v/, "");
  if (tagVersion !== version) {
    console.error(`Tag ${tagName} 與 src-tauri/Cargo.toml 的版本 ${version} 不一致，請確認後重新打 tag`);
    process.exit(1);
  }
  releaseTag = tagName;
} else {
  releaseTag = `v${version}`;
  if (dryRun) {
    console.log(
      "dry_run=true：workflow_dispatch 手動觸發，略過遠端 tag 存在性檢查（僅供演練，不建立/發佈任何東西）",
    );
  } else {
    const result = spawnSync("git", ["ls-remote", "--tags", "origin", `refs/tags/${releaseTag}`], {
      encoding: "utf8",
    });
    if (result.status !== 0) {
      console.error(`git ls-remote 失敗：${result.stderr || result.error?.message || ""}`);
      process.exit(1);
    }
    if (!result.stdout.trim()) {
      console.error(
        `workflow_dispatch 手動觸發，但 tag ${releaseTag} 不存在——請先 npm run bump、commit、` +
          `git tag ${releaseTag} 並 git push --tags，再重新執行本 workflow`,
      );
      process.exit(1);
    }
    console.log(`workflow_dispatch：沿用既有 tag ${releaseTag}`);
  }
}

console.log(`Release tag：${releaseTag}`);

const githubOutput = process.env.GITHUB_OUTPUT;
if (!githubOutput) {
  throw new Error("GITHUB_OUTPUT 環境變數未設定（此腳本應在 GitHub Actions step 內執行）");
}
appendFileSync(githubOutput, `version=${version}\n`);
appendFileSync(githubOutput, `release_tag=${releaseTag}\n`);
