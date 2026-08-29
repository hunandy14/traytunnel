/**
 * CLI：印出 out/manifest.<platform_key>.json 裡的單一欄位到 stdout，給
 * release.yml 的 bash 步驟用 $(...) 取值——這樣 compose／verify 步驟只要
 * 讀 manifest，不必在 YAML 裡手寫 traytunnel-<version>-... 這類檔名字面值。
 *
 * Usage:
 *   node scripts/print-manifest-field.mjs --dir out --platform-key windows-x86_64 --field asset
 *
 * --field 是 scripts/lib/manifest.mjs 的 readManifest() 回傳物件裡的任一鍵
 * （platform_key / version / asset / sig / bundle_source）。
 *
 * 無相依，直接 node scripts/print-manifest-field.mjs ...。
 */

import { parseArgs } from "node:util";
import { readManifest } from "./lib/manifest.mjs";

function main() {
  const { values } = parseArgs({
    options: {
      dir: { type: "string" },
      "platform-key": { type: "string" },
      field: { type: "string" },
    },
  });

  if (!values.dir || !values["platform-key"] || !values.field) {
    console.error(
      "用法：node scripts/print-manifest-field.mjs --dir <out 目錄> --platform-key <key> --field <欄位>",
    );
    process.exit(1);
  }

  const manifest = readManifest(values.dir, values["platform-key"]);
  if (!(values.field in manifest)) {
    throw new Error(`manifest 沒有欄位 ${values.field}（有：${Object.keys(manifest).join(", ")}）`);
  }
  process.stdout.write(String(manifest[values.field]));
}

try {
  main();
} catch (err) {
  console.error(`::error::${err instanceof Error ? err.message : String(err)}`);
  process.exit(1);
}
