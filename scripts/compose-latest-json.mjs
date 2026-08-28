/**
 * CLI：組出這次發佈要用的 latest.json，套用 scripts/lib/latest-json.mjs 的
 * 「底稿合併」邏輯（見該檔開頭註解）。給 release.yml 的 compose job 用，
 * 在下載完雙平台 workflow artifact、抓完現行 releases/latest 底稿之後呼叫。
 *
 * Usage:
 *   node scripts/compose-latest-json.mjs \
 *     --version 0.6.5 \
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
 * --baseline 可省略，或指向不存在／空白的檔案——都視為「沒有底稿」（首發，
 * 或抓不到現行 latest.json）。
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
      "pub-date": { type: "string" },
      platform: { type: "string", multiple: true },
      baseline: { type: "string" },
      out: { type: "string" },
    },
  });

  if (!values.version || !values["pub-date"] || !values.platform?.length || !values.out) {
    console.error(
      "用法：node scripts/compose-latest-json.mjs --version <v> --pub-date <iso> " +
        "--platform <key>=<簽章檔路徑>=<網址> [--platform ...] [--baseline <底稿檔>] --out <輸出檔>",
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

  const merged = mergeLatestJson(baseline, current);
  writeFileSync(values.out, `${JSON.stringify(merged, null, 2)}\n`, "utf8");
  console.log(`已寫出 ${values.out}：`);
  console.log(JSON.stringify(merged, null, 2));
}

main();
