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
 *     --validator json|sha256sums --on-404 empty-json|empty \
 *     [--attempts 5] [--label <log 用說明文字>]
 *
 * --validator  200 回應後，用什麼邏輯確認內容真的是合法底稿（不是被中間層
 *              換掉的錯誤頁、或半截內容）：
 *                json         JSON.parse 能解開即可（給 latest.json 用）
 *                sha256sums   scripts/lib/sha256sums.mjs 的 parseSha256Sums
 *                             能解開即可（給 SHA256SUMS.txt 用）
 *              驗證失敗跟其他狀態碼一樣，視為暫時性失敗、進入重試。
 *
 * --on-404     確定 404（該資產目前線上真的不存在——首發，或這個 tag 還沒
 *              發過任何一腿）時要寫進 --out 的內容：
 *                empty-json   寫 `{}`（latest.json 用：mergeLatestJson 讀到
 *                             空物件視為沒有底稿）
 *                empty        寫空字串（SHA256SUMS.txt 用：mergeSha256Sums
 *                             讀到空字串視為沒有底稿）
 *
 * 404 與「其他任何失敗」的分野是這支腳本存在的核心理由，不能弄反：
 *   HTTP 404      → 確定沒有底稿，寫 --on-404 指定的內容，不擋流程。
 *   其他任何失敗  → 網路抖動、5xx、rate limit、或 200 但驗證不過（被攔截的
 *                   錯誤頁、半截內容）——一律視為暫時性，重試；重試用盡仍
 *                   失敗就以非零狀態退出，並印 ::error::。
 *   把「暫時性失敗」誤判成「確定沒有」，會讓底稿合併邏輯把另一個平台的
 *   latest.json 條目／SHA256SUMS.txt checksum 整個抹掉，而且沒有任何錯誤
 *   訊號——這正是這條腳本要擋下來的那類發佈事故。
 *
 * 重試：預設 5 次，第 i 次失敗後 sleep i*5 秒再重試（跟原本 bash 版本一致）。
 *
 * 無外部套件相依：用 Node 內建 global fetch，直接 node scripts/fetch-baseline.mjs ...。
 */

import { writeFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { parseSha256Sums } from "./lib/sha256sums.mjs";

const VALIDATORS = {
  json: (text) => {
    JSON.parse(text);
  },
  sha256sums: (text) => {
    parseSha256Sums(text);
  },
};

const ON_404_CONTENT = {
  "empty-json": "{}\n",
  empty: "",
};

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function main() {
  const { values } = parseArgs({
    options: {
      url: { type: "string" },
      out: { type: "string" },
      validator: { type: "string" },
      "on-404": { type: "string" },
      attempts: { type: "string", default: "5" },
      label: { type: "string" },
    },
  });

  if (!values.url || !values.out || !values.validator || !values["on-404"]) {
    console.error(
      "用法：node scripts/fetch-baseline.mjs --url <URL> --out <輸出檔> " +
        "--validator json|sha256sums --on-404 empty-json|empty [--attempts 5] [--label <說明文字>]",
    );
    process.exit(1);
  }

  const validate = VALIDATORS[values.validator];
  if (!validate) {
    console.error(`::error::不認得的 --validator ${values.validator}（要 json 或 sha256sums）`);
    process.exit(1);
  }
  const emptyContent = ON_404_CONTENT[values["on-404"]];
  if (emptyContent === undefined) {
    console.error(`::error::不認得的 --on-404 ${values["on-404"]}（要 empty-json 或 empty）`);
    process.exit(1);
  }

  const label = values.label || values.url;
  const attempts = Number(values.attempts);
  let lastFailureDetail = "（尚未嘗試）";

  for (let i = 1; i <= attempts; i += 1) {
    let response;
    try {
      // 60 秒逾時、跟隨轉址，跟原本 bash 版本的 `curl -sSL --max-time 60` 對齊。
      response = await fetch(values.url, { redirect: "follow", signal: AbortSignal.timeout(60_000) });
    } catch (err) {
      lastFailureDetail = `連線失敗：${err instanceof Error ? err.message : String(err)}`;
      console.log(`第 ${i} 次：${lastFailureDetail}，視為暫時性失敗`);
      if (i < attempts) await sleep(i * 5_000);
      continue;
    }

    if (response.status === 404) {
      console.log(`HTTP 404：${label} 目前不存在——視為空底稿`);
      writeFileSync(values.out, emptyContent, "utf8");
      return;
    }

    if (response.status === 200) {
      const text = await response.text();
      try {
        validate(text);
      } catch (err) {
        lastFailureDetail = `HTTP 200 但內容驗證失敗：${err instanceof Error ? err.message : String(err)}`;
        console.log(`第 ${i} 次：${lastFailureDetail}，視為暫時性失敗`);
        if (i < attempts) await sleep(i * 5_000);
        continue;
      }
      writeFileSync(values.out, text, "utf8");
      console.log(`抓到現行 ${label} 當底稿：`);
      console.log(text);
      return;
    }

    lastFailureDetail = `HTTP ${response.status}`;
    console.log(`第 ${i} 次：${lastFailureDetail}，視為暫時性失敗`);
    if (i < attempts) await sleep(i * 5_000);
  }

  console.error(
    `::error::連續 ${attempts} 次都拿不到現行 ${label}（最後一次：${lastFailureDetail}），且不是 404。` +
      `無法確認線上底稿內容，繼續下去可能把其他平台的條目/checksum 從底稿抹掉，因此中止。請稍後重跑。`,
  );
  process.exit(1);
}

main().catch((err) => {
  console.error(`::error::${err instanceof Error ? err.message : String(err)}`);
  process.exit(1);
});
