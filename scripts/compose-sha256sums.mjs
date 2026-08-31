/**
 * CLI：組出這次發佈要用的 SHA256SUMS.txt，套用 scripts/lib/sha256sums.mjs 的
 * 「底稿合併」邏輯（見該檔開頭註解）。給 release.yml 的 compose job 用，在
 * 下載完雙平台 workflow artifact、抓完現行（該 release tag 的）SHA256SUMS.txt
 * 底稿之後呼叫。
 *
 * Usage:
 *   node scripts/compose-sha256sums.mjs --dir out --baseline baseline-SHA256SUMS.txt --out out/SHA256SUMS.txt
 *
 * --dir       這次建置產出、要發佈的二進位檔所在目錄（非遞迴掃描，只挑
 *             *.exe / *.dmg / *.tar.gz——跟舊版純 bash 版本的 glob pattern
 *             一致）。雜湊只對這個目錄底下實際存在的檔案計算，不會憑空
 *             杜撰出底稿裡才有的檔案。
 * --baseline  可省略，或指向不存在／空白的檔案——都視為「沒有底稿」（首發，
 *             或該 release tag 確定沒有 SHA256SUMS.txt 這個資產）。呼叫端
 *             有義務區分「確定沒有」與「抓取失敗」：把網路抖動當成空底稿，
 *             會把另一個平台的 checksum 整個抹掉（見 release.yml 的重試）。
 * --out       輸出檔路徑
 *
 * 無相依，直接 node scripts/compose-sha256sums.mjs ...。
 */

import { createHash } from "node:crypto";
import { createReadStream, existsSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { readBaselineText } from "./lib/baseline.mjs";
import { runCli } from "./lib/cli.mjs";
import { formatSha256Sums, mergeSha256Sums } from "./lib/sha256sums.mjs";

/** 跟舊版 `for pattern in '*.exe' '*.dmg' '*.tar.gz'` 等價：只看副檔名，不遞迴 */
function isReleaseBinary(filename) {
  return filename.endsWith(".exe") || filename.endsWith(".dmg") || filename.endsWith(".tar.gz");
}

function sha256File(path) {
  return new Promise((resolvePromise, reject) => {
    const hash = createHash("sha256");
    const stream = createReadStream(path);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.on("error", reject);
    stream.on("end", () => resolvePromise(hash.digest("hex")));
  });
}

async function main() {
  const { values } = parseArgs({
    options: {
      dir: { type: "string" },
      baseline: { type: "string" },
      out: { type: "string" },
    },
  });

  if (!values.dir || !values.out) {
    console.error("用法：node scripts/compose-sha256sums.mjs --dir <二進位檔目錄> [--baseline <底稿檔>] --out <輸出檔>");
    process.exit(1);
  }

  const dir = values.dir;
  if (!existsSync(dir) || !statSync(dir).isDirectory()) {
    throw new Error(`--dir 指到的目錄不存在：${dir}`);
  }

  const filenames = readdirSync(dir)
    .filter((name) => isReleaseBinary(name) && statSync(join(dir, name)).isFile())
    .sort();

  if (filenames.length === 0) {
    throw new Error(`${dir} 底下沒有任何要發佈的二進位檔（*.exe / *.dmg / *.tar.gz）`);
  }

  const currentMap = {};
  for (const filename of filenames) {
    currentMap[filename] = await sha256File(join(dir, filename));
  }

  // mergeSha256Sums 的「沒有底稿」用 null 表示（見 scripts/lib/baseline.mjs）
  const baselineText = readBaselineText(values.baseline);
  const merged = mergeSha256Sums(baselineText, currentMap);

  const formatted = formatSha256Sums(merged);
  writeFileSync(values.out, formatted, "utf8");
  console.log(`已寫出 ${values.out}：`);
  process.stdout.write(formatted);
}

runCli(main);
