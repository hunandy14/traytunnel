/**
 * CLI：抓遠端的「底稿」檔案（latest.json 或 SHA256SUMS.txt），帶重試與
 * 404／暫時性失敗的語意判斷。給 release.yml 的 compose job 用，取代原本
 * 兩段幾乎逐字重複的 bash curl 迴圈（一段抓 releases/latest 的
 * latest.json，一段抓某個 tag 的 SHA256SUMS.txt；重試次數、退避秒數、
 * 200/404/其他狀態碼的分流邏輯完全相同，只有 URL 跟「200 之後怎麼驗證
 * 內容合法」不一樣）——兩步各呼叫這支腳本一次，帶不同的 --url／--validator。
 *
 * Usage:
 *   node scripts/fetch-baseline.mjs --url <URL> --out <輸出檔> \
 *     --validator json|sha256sums [--attempts 5] [--label <log 用說明文字>]
 *
 * --validator  200 回應後，用什麼邏輯確認內容真的是合法底稿（不是被中間層
 *              換掉的錯誤頁、或半截內容）：
 *                json         JSON.parse 能解開即可（給 latest.json 用）
 *                sha256sums   scripts/lib/sha256sums.mjs 的 parseSha256Sums
 *                             能解開即可（給 SHA256SUMS.txt 用）
 *              驗證失敗跟其他狀態碼一樣，視為暫時性失敗、進入重試。
 *
 * 確定 404（該資產目前線上真的不存在——首發，或這個 tag 還沒發過任何一腿）
 * 時一律寫空字串。兩邊的合併邏輯對「空底稿」的判定本來就一致：
 * compose-latest-json.mjs 的底稿讀取把純空白視為「沒有底稿」（等同 {}），
 * mergeSha256Sums 讀到空字串也視為沒有底稿——過去的 --on-404 empty-json|empty
 * 兩個值下游沒有任何行為差異，純粹是死重量（SIM-2）。
 *
 * 404 與「其他任何失敗」的分野是這支腳本存在的核心理由，不能弄反：
 *   HTTP 404      → 確定沒有底稿，寫空字串，不擋流程。
 *   其他任何失敗  → 網路抖動、5xx、rate limit、或 200 但驗證不過（被攔截的
 *                   錯誤頁、半截內容）——一律視為暫時性，重試；重試用盡仍
 *                   失敗就以非零狀態退出，並印 ::error::。
 *   把「暫時性失敗」誤判成「確定沒有」，會讓底稿合併邏輯把另一個平台的
 *   latest.json 條目／SHA256SUMS.txt checksum 整個抹掉，而且沒有任何錯誤
 *   訊號——這正是這條腳本要擋下來的那類發佈事故。
 *
 * 重試：預設 5 次，第 i 次失敗後 sleep i*5 秒再重試（跟原本 bash 版本一致）。
 * 這套「404 立刻分類／其餘退避重試」的政策實作在 scripts/lib/fetch-retry.mjs，
 * 這支腳本只負責「404 要寫什麼、200 要寫哪裡」這層檔案語意；scripts/probe-release.mjs
 * 共用同一份政策（過去 release.yml 的 bash 又手刻了第四份）。
 *
 * 無外部套件相依：用 Node 內建 global fetch，直接 node scripts/fetch-baseline.mjs ...。
 */

import { writeFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { runCli } from "./lib/cli.mjs";
import { fetchWithRetry } from "./lib/fetch-retry.mjs";
import { parseSha256Sums } from "./lib/sha256sums.mjs";

const VALIDATORS = {
  json: (text) => {
    JSON.parse(text);
  },
  sha256sums: (text) => {
    parseSha256Sums(text);
  },
};

async function main() {
  const { values } = parseArgs({
    options: {
      url: { type: "string" },
      out: { type: "string" },
      validator: { type: "string" },
      attempts: { type: "string", default: "5" },
      label: { type: "string" },
    },
  });

  if (!values.url || !values.out || !values.validator) {
    console.error(
      "用法：node scripts/fetch-baseline.mjs --url <URL> --out <輸出檔> " +
        "--validator json|sha256sums [--attempts 5] [--label <說明文字>]",
    );
    process.exit(1);
  }

  const validate = VALIDATORS[values.validator];
  if (!validate) {
    console.error(`::error::不認得的 --validator ${values.validator}（要 json 或 sha256sums）`);
    process.exit(1);
  }

  const label = values.label || values.url;

  // 60 秒逾時、跟隨轉址，跟原本 bash 版本的 `curl -sSL --max-time 60` 對齊；
  // 重試次數／退避秒數／404 與暫時性失敗的分野都由 fetchWithRetry 統一處理。
  const result = await fetchWithRetry(values.url, {
    attempts: Number(values.attempts),
    validate,
    label: `現行 ${label}`,
    exhaustedHint:
      "無法確認線上底稿內容，繼續下去可能把其他平台的條目/checksum 從底稿抹掉，因此中止。請稍後重跑。",
  });

  if (result.notFound) {
    console.log(`HTTP 404：${label} 目前不存在——視為空底稿`);
    writeFileSync(values.out, "", "utf8");
    return;
  }

  writeFileSync(values.out, result.text, "utf8");
  console.log(`抓到現行 ${label} 當底稿：`);
  console.log(result.text);
}

runCli(main);
