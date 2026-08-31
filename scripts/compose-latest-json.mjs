/**
 * CLI：組出這次發佈要用的 latest.json，套用 scripts/lib/latest-json.mjs 的
 * 「底稿合併」邏輯（見該檔開頭註解）。給 release.yml 的 compose job 用，
 * 在下載完雙平台 workflow artifact、抓完現行 releases/latest 底稿之後呼叫。
 *
 * Usage:
 *   node scripts/compose-latest-json.mjs \
 *     --version 0.6.5 --tag v0.6.5 --pub-date 2026-08-29T00:00:00.000Z \
 *     --dir out --platforms "windows-x86_64 darwin-aarch64" \
 *     --repo owner/repo \
 *     --baseline baseline-latest.json --out out/latest.json
 *
 * --dir／--platforms 取代舊版逐一手寫的 --platform <key>=<簽章檔路徑>=<網址>：
 *   這裡直接讀 scripts/package.mjs 產出的 out/manifest.<platform_key>.json
 *   （scripts/lib/manifest.mjs 的 readManifest）取得 asset／sig 檔名，網址用
 *   --repo 與 --tag 現組——release.yml 過去得先呼叫兩次
 *   print-manifest-field.mjs（asset、sig 各一次）把這些欄位組成字串再傳進來，
 *   四處手寫同一組檔名規則的風險（以及每個欄位一個 node 行程的開銷）就這樣
 *   消失，manifest 只有這裡一個讀取點。
 *
 * --platforms 空白分隔，是這次 build matrix「應該建置成功」的平台 key 清單，
 *   與 release.yml plan job 算出來的 matrix 同源（不是「out/ 底下剛好有哪些
 *   manifest」）。這裡對每個 key 做 fail-closed 檢查：manifest 或它指到的
 *   簽章檔缺一個就中止，不會靜默沿用底稿舊值（理由跟下面 --tag 段落一樣：
 *   沉默的部分成功，比整條紅掉更危險）。
 *
 * --tag 必填：除了用來組下載網址，也是「陳舊條目斷言」要用的值——底稿裡這次
 *   沒建置、要沿用的條目，其 url 必須屬於這次的 release tag，否則中止（語意
 *   與理由見 scripts/lib/latest-json.mjs 開頭）。刻意設成必填而不是可選：
 *   漏傳就等於整條防線靜默失效，那正是這次要修掉的那類 bug。
 *
 * --allow-stale-platforms 是上面那條斷言的逃生門（對應 release.yml 的同名輸入）：
 *   帶了就只警告不中止。用在「往既有的 release 補另一條腿」這種安全情境。
 *
 * --baseline 可省略，或指向不存在／空白的檔案——都視為「沒有底稿」（首發，
 * 或線上確定沒有 latest.json）。注意呼叫端有義務區分「確定沒有」與「抓取失敗」：
 * 把網路抖動當成空底稿，會把另一個平台的條目整個抹掉（見 scripts/fetch-baseline.mjs）。
 *
 * 無外部套件相依，直接 node scripts/compose-latest-json.mjs ...。
 */

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { readBaselineText } from "./lib/baseline.mjs";
import { runCli } from "./lib/cli.mjs";
import { mergeLatestJson } from "./lib/latest-json.mjs";
import { readManifest } from "./lib/manifest.mjs";

/**
 * 對每個「這次應建置」的平台 key，讀 manifest 取得 asset／sig 檔名，驗證簽章檔
 * 真的存在，並用 --repo／--tag 現組下載網址。任何一步缺失都直接丟例外中止
 * （fail-closed）——見上面檔頭註解「--platforms」段落。
 */
function resolvePlatformsFromManifests(dir, platformKeys, repo, tag) {
  const platforms = {};
  for (const key of platformKeys) {
    // 缺 manifest 或欄位不全時，readManifest 自己就會丟出清楚的錯誤訊息
    const manifest = readManifest(dir, key);
    const sigPath = join(dir, manifest.sig);
    if (!existsSync(sigPath)) {
      throw new Error(
        `out/manifest.${key}.json 指到的簽章檔 ${sigPath} 不存在。latest.json 的 ${key} 條目無法產生，因此中止。`,
      );
    }
    const signature = readFileSync(sigPath, "utf8").trim();
    if (!signature) {
      throw new Error(`簽章檔 ${sigPath}（平台 ${key}）是空的`);
    }
    platforms[key] = {
      signature,
      url: `https://github.com/${repo}/releases/download/${tag}/${manifest.asset}`,
    };
  }
  return platforms;
}

/** 「沒有底稿」在 mergeLatestJson 這一側用空物件表示（見 scripts/lib/baseline.mjs） */
function readBaseline(path) {
  const raw = readBaselineText(path);
  if (raw === null) return {};
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
      dir: { type: "string" },
      platforms: { type: "string" },
      repo: { type: "string" },
      baseline: { type: "string" },
      out: { type: "string" },
      "allow-stale-platforms": { type: "boolean", default: false },
    },
  });

  if (
    !values.version ||
    !values.tag ||
    !values["pub-date"] ||
    !values.dir ||
    !values.platforms?.trim() ||
    !values.repo ||
    !values.out
  ) {
    console.error(
      "用法：node scripts/compose-latest-json.mjs --version <v> --tag <vX.Y.Z> --pub-date <iso> " +
        '--dir <out 目錄> --platforms "<空白分隔的 platform-key 清單>" --repo <owner/repo> ' +
        "[--baseline <底稿檔>] [--allow-stale-platforms] --out <輸出檔>",
    );
    process.exit(1);
  }

  const platformKeys = values.platforms.split(/\s+/).filter(Boolean);
  if (platformKeys.length === 0) {
    throw new Error("--platforms 至少要有一個平台 key");
  }

  const platforms = resolvePlatformsFromManifests(values.dir, platformKeys, values.repo, values.tag);

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

runCli(main);
