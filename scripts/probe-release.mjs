/**
 * CLI：查這個 tag 底下「現在」有沒有 GitHub Release，把結果寫進 GITHUB_OUTPUT
 * （exists=true|false、is_draft=true|false）。給 release.yml 的 compose job 用，
 * 決定要走「建立新 release（帶 body）」還是「更新既有 release（不碰 body，保留
 * 人工編輯過的說明）」。
 *
 * Usage:
 *   GH_TOKEN=<token> node scripts/probe-release.mjs --repo owner/repo --tag v0.7.0
 *
 * 為什麼是「列舉 releases 比對 tag_name」而不是 `releases/tags/<tag>`：
 *   releases/tags/<tag> 這個端點對 **draft** release 一律回 404（draft 還沒有
 *   真正的 tag）。把「已存在的 draft」誤判成「不存在」，會讓 workflow 走 new
 *   分支帶 body + generate_release_notes，softprops 卻會找到那個 draft 並 PATCH
 *   它——人工編輯過的說明被覆寫；更糟的是資產進了 draft，下載網址對外 404，
 *   下一次補另一條腿時 fetch-baseline 抓到空底稿，另一個平台的 latest.json 條目／
 *   SHA256SUMS.txt checksum 就永久消失（正是這條管線一直在修的那類事故）。
 *   列舉端點（GET /repos/{owner}/{repo}/releases）帶著有 push 權限的 token 時
 *   會把 draft 一起列出來，所以改用它。
 *
 * 翻頁上限固定 2 頁（每頁 100 筆）：跟 softprops/action-gh-release v3.0.2 的
 * RECENT_RELEASE_SCAN_PAGES=2 對齊——真正決定「這次會不會撞到既有 release」的
 * 是 softprops 自己看得到的範圍，這裡掃得比它多或少都只會讓兩邊的判斷分岔。
 *
 * 分流一律依 HTTP 狀態碼（見 scripts/lib/fetch-retry.mjs），不解析任何 CLI 的
 * 文字輸出——舊版 bash 靠 `grep -q "HTTP 404"` 比對 gh 的 stderr，gh 哪天改一
 * 行訊息格式就會把「不存在」誤判成暫時性失敗，或反過來覆寫掉手改的 notes。
 *
 * 無外部套件相依：用 Node 內建 global fetch，直接 node scripts/probe-release.mjs ...。
 */

import { appendFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { runCli } from "./lib/cli.mjs";
import { fetchWithRetry } from "./lib/fetch-retry.mjs";

/** 與 softprops/action-gh-release v3.0.2 的 RECENT_RELEASE_SCAN_PAGES 對齊（見檔頭） */
const SCAN_PAGES = 2;
const PER_PAGE = 100;

function parseReleaseList(text) {
  const data = JSON.parse(text);
  if (!Array.isArray(data)) {
    throw new Error("回應不是 JSON 陣列（預期 releases 列表）");
  }
  return data;
}

async function main() {
  const { values } = parseArgs({
    options: {
      repo: { type: "string" },
      tag: { type: "string" },
      attempts: { type: "string", default: "5" },
    },
  });

  if (!values.repo || !values.tag) {
    throw new Error("用法：node scripts/probe-release.mjs --repo <owner/repo> --tag <vX.Y.Z>");
  }
  const token = process.env.GH_TOKEN;
  if (!token) {
    throw new Error("GH_TOKEN 環境變數未設定——列舉 releases 需要它才看得到 draft release");
  }
  const githubOutput = process.env.GITHUB_OUTPUT;
  if (!githubOutput) {
    throw new Error("GITHUB_OUTPUT 環境變數未設定（此腳本應在 GitHub Actions step 內執行）");
  }

  const headers = {
    Accept: "application/vnd.github+json",
    Authorization: `Bearer ${token}`,
    "X-GitHub-Api-Version": "2022-11-28",
    "User-Agent": "traytunnel-release-workflow",
  };

  let found = null;
  for (let page = 1; page <= SCAN_PAGES && found === null; page += 1) {
    const url = `https://api.github.com/repos/${values.repo}/releases?per_page=${PER_PAGE}&page=${page}`;
    const result = await fetchWithRetry(url, {
      headers,
      attempts: Number(values.attempts),
      validate: parseReleaseList,
      label: `${values.repo} 的 releases 列表（第 ${page} 頁）`,
      exhaustedHint:
        "無法判斷這次該不該覆寫 release body，因此中止——誤判成「不存在」會覆寫掉人工編輯過的 release notes。請稍後重跑。",
    });

    if (result.notFound) {
      // 這個端點回 404 不是「release 不存在」，而是「repo 不存在／token 看不到它」。
      throw new Error(
        `列舉 ${values.repo} 的 releases 時得到 HTTP 404——這代表 repo 不存在或 GH_TOKEN 沒有權限，` +
          `不是「release 尚未建立」。中止，避免把權限問題誤判成「可以建立新 release」。`,
      );
    }

    const releases = parseReleaseList(result.text);
    found = releases.find((release) => release.tag_name === values.tag) ?? null;
    if (releases.length < PER_PAGE) break; // 已經是最後一頁
  }

  const exists = found !== null;
  const isDraft = exists && found.draft === true;

  if (!exists) {
    console.log(`release ${values.tag} 尚不存在：這次會用固定的 body 建立新 release`);
  } else if (isDraft) {
    console.log(`release ${values.tag} 已存在，而且是 draft（id=${found.id}）`);
  } else {
    console.log(
      `release ${values.tag} 已存在（id=${found.id}）：這次 Create GitHub Release 不會傳 body／` +
        `generate_release_notes，保留人工編輯過的說明`,
    );
  }

  appendFileSync(githubOutput, `exists=${exists}\n`);
  appendFileSync(githubOutput, `is_draft=${isDraft}\n`);
}

runCli(main);
