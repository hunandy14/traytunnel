/**
 * CLI：組出這次發佈要用的 latest.json，套用 scripts/lib/latest-json.mjs 的
 * 「底稿合併」邏輯（見該檔開頭註解）。給 release.yml 的 compose job 用，
 * 在下載完雙平台 workflow artifact、抓完現行 releases/latest 底稿之後呼叫。
 *
 * Usage:
 *   node scripts/compose-latest-json.mjs \
 *     --version 0.6.5 \
 *     --tag v0.6.5 \
 *     --pub-date 2026-08-29T00:00:00.000Z \
 *     --platform windows-x86_64=out/traytunnel-0.6.5-setup.exe.sig=https://github.com/x/y/releases/download/v0.6.5/traytunnel-0.6.5-setup.exe \
 *     --platform darwin-aarch64=out/traytunnel-0.6.5-aarch64.app.tar.gz.sig=https://github.com/x/y/releases/download/v0.6.5/traytunnel-0.6.5-aarch64.app.tar.gz \
 *     --baseline baseline-latest.json \
 *     --out out/latest.json
 *
 * --platform 可重複，每個平台一個，格式是 <platform-key>=<簽章檔路徑>=<下載網址>
 *   （網址本身可能含 =，所以用「切兩刀」：第一個 = 切 key，第二個 = 切
 *   簽章檔路徑，剩下的全部當網址）。
 *
 * --tag 必填：用來做「陳舊條目斷言」——底稿裡這次沒建置、要沿用的條目，其 url
 *   必須屬於這次的 release tag，否則中止（語意與理由見 scripts/lib/latest-json.mjs
 *   開頭）。刻意設成必填而不是可選：漏傳就等於整條防線靜默失效，那正是這次要
 *   修掉的那類 bug。
 *
 * --allow-stale-platforms 是上面那條斷言的逃生門（對應 release.yml 的同名輸入）：
 *   帶了就只警告不中止。用在「往既有的 release 補另一條腿」這種安全情境。
 *
 * --baseline 可省略，或指向不存在／空白的檔案——都視為「沒有底稿」（首發，
 * 或線上確定沒有 latest.json）。注意呼叫端有義務區分「確定沒有」與「抓取失敗」：
 * 把網路抖動當成空底稿，會把另一個平台的條目整個抹掉（見 release.yml 的重試）。
 *
 * 無相依，直接 node scripts/compose-latest-json.mjs ...。
 */

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { mergeLatestJson } from "./lib/latest-json.mjs";

function parsePlatformArg(raw) {
  const firstEq = raw.indexOf("=");
  if (firstEq < 0) {
    throw new Error(`--platform 格式錯誤（要 <key>=<簽章檔路徑>=<網址>）：${raw}`);
  }
  const key = raw.slice(0, firstEq);
  const rest = raw.slice(firstEq + 1);
  const secondEq = rest.indexOf("=");
  if (secondEq < 0) {
    throw new Error(`--platform 格式錯誤（要 <key>=<簽章檔路徑>=<網址>）：${raw}`);
  }
  const sigPath = rest.slice(0, secondEq);
  const url = rest.slice(secondEq + 1);
  if (!key || !sigPath || !url) {
    throw new Error(`--platform 格式錯誤（key／簽章檔路徑／網址不可為空）：${raw}`);
  }
  return { key, sigPath, url };
}

function readBaseline(path) {
  if (!path || !existsSync(path)) return {};
  const raw = readFileSync(path, "utf8").trim();
  if (!raw) return {};
  try {
    return JSON.parse(raw);
  } catch (err) {
    throw new Error(`底稿 ${path} 不是合法 JSON：${err.message}`);
  }
}

function main() {
  const { values } = parseArgs({
    options: {
      version: { type: "string" },
      tag: { type: "string" },
      "pub-date": { type: "string" },
      platform: { type: "string", multiple: true },
      baseline: { type: "string" },
      out: { type: "string" },
      "allow-stale-platforms": { type: "boolean", default: false },
    },
  });

  if (!values.version || !values.tag || !values["pub-date"] || !values.platform?.length || !values.out) {
    console.error(
      "用法：node scripts/compose-latest-json.mjs --version <v> --tag <vX.Y.Z> --pub-date <iso> " +
        "--platform <key>=<簽章檔路徑>=<網址> [--platform ...] [--baseline <底稿檔>] " +
        "[--allow-stale-platforms] --out <輸出檔>",
    );
    process.exit(1);
  }

  const platforms = {};
  for (const raw of values.platform) {
    const { key, sigPath, url } = parsePlatformArg(raw);
    if (!existsSync(sigPath)) {
      throw new Error(`找不到簽章檔 ${sigPath}（平台 ${key}）`);
    }
    const signature = readFileSync(sigPath, "utf8").trim();
    if (!signature) {
      throw new Error(`簽章檔 ${sigPath}（平台 ${key}）是空的`);
    }
    platforms[key] = { signature, url };
  }

  const baseline = readBaseline(values.baseline);
  const current = {
    version: values.version,
    pub_date: values["pub-date"],
    platforms,
  };

  const merged = mergeLatestJson(baseline, current, {
    tag: values.tag,
    allowStalePlatforms: values["allow-stale-platforms"],
    // 走 GitHub Actions 的 workflow command，讓警告直接浮到 run 摘要上，
    // 不會埋在幾百行 log 中間。本地直接跑也只是多一個前綴，無害。
    onWarning: (msg) => console.warn(`::warning::${msg}`),
  });
  writeFileSync(values.out, `${JSON.stringify(merged, null, 2)}\n`, "utf8");
  console.log(`已寫出 ${values.out}：`);
  console.log(JSON.stringify(merged, null, 2));
}

// 這支腳本的每一種失敗都是「刻意擋下來的發佈事故」，訊息本身才是重點——
// 直接讓例外冒出去只會在 CI log 裡留下一坨 stack trace，真正要看的那句話還
// 得自己撈。統一收斂成 ::error:: 註記（會浮到 run 摘要），並以 exit 1 結束。
try {
  main();
} catch (err) {
  console.error(`::error::${err instanceof Error ? err.message : String(err)}`);
  process.exit(1);
}
