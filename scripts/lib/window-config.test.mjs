/**
 * 守住 `src-tauri/tauri.macos.conf.json` 對 `app.windows[0]` 的漂移。
 *
 * tauri 的平台覆寫檔走 JSON Merge Patch（RFC 7396）：物件逐鍵合併，但陣列
 * 欄位整組取代，不是逐項合併。`windows` 是陣列，所以 mac 檔一旦覆寫它，
 * `windows[0]` 就得把主檔（`tauri.conf.json`）那份**整組重抄一次**——只有
 * mac 原生標題列風格真的需要覆寫的那幾個鍵（decorations／titleBarStyle／
 * hiddenTitle，外加 mac 檔額外補的 url）例外，其餘每一個鍵都必須跟主檔完全
 * 相等。這件事沒有任何編譯期或執行期訊號會檢查：改了主檔的 width、忘了同步
 * mac 檔，mac build 出來的視窗尺寸就悄悄跟 Windows 分岔，只有實機比對才會
 * 發現。
 *
 * 用 Node 內建 test runner，無外部相依：
 *   node --test scripts/lib/window-config.test.mjs
 * 或跑整個 scripts/ 底下所有 *.test.mjs：
 *   npm run test:release
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const MAIN_CONF_PATH = fileURLToPath(new URL("../../src-tauri/tauri.conf.json", import.meta.url));
const MACOS_CONF_PATH = fileURLToPath(
  new URL("../../src-tauri/tauri.macos.conf.json", import.meta.url),
);

// mac 原生標題列風格刻意覆寫的欄位：decorations 開回原生視窗框、
// titleBarStyle／hiddenTitle 疊出「隱藏標題但留紅綠燈」的效果，url 是主檔
// 本來就沒有寫（隱含預設 index.html）、mac 檔額外補上的一個欄位。這四個鍵
// 允許兩邊不同，其餘一律要求相等。
const INTENTIONAL_OVERRIDE_KEYS = new Set(["decorations", "titleBarStyle", "hiddenTitle", "url"]);

function loadJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

test("tauri.macos.conf.json 的 windows[0] 除了刻意覆寫的欄位，其餘每一鍵都要跟主檔一致", () => {
  const mainConf = loadJson(MAIN_CONF_PATH);
  const macConf = loadJson(MACOS_CONF_PATH);

  const mainWindow = mainConf?.app?.windows?.[0];
  const macWindow = macConf?.app?.windows?.[0];
  assert.ok(mainWindow, "主檔 tauri.conf.json 缺少 app.windows[0]，設定檔本身壞了");
  assert.ok(
    macWindow,
    "tauri.macos.conf.json 缺少 app.windows[0]——JSON Merge Patch 對 windows 陣列是整組取代，" +
      "漏寫等於 mac build 整組視窗設定消失",
  );

  const drifted = [];
  for (const [key, value] of Object.entries(mainWindow)) {
    if (INTENTIONAL_OVERRIDE_KEYS.has(key)) continue;
    if (!(key in macWindow)) {
      drifted.push(`  - ${key}: 主檔有 ${JSON.stringify(value)}，mac 檔缺這個鍵`);
      continue;
    }
    try {
      assert.deepStrictEqual(macWindow[key], value);
    } catch {
      drifted.push(`  - ${key}: 主檔=${JSON.stringify(value)} mac 檔=${JSON.stringify(macWindow[key])}`);
    }
  }

  assert.deepEqual(
    drifted,
    [],
    `tauri.macos.conf.json 的 windows[0] 跟主檔分岔了，補上這些鍵：\n${drifted.join("\n")}`,
  );
});

test("刻意覆寫的欄位在主檔與 mac 檔裡至少有一邊真的定義了（不是打錯字打成兩邊都沒有）", () => {
  const mainConf = loadJson(MAIN_CONF_PATH);
  const macConf = loadJson(MACOS_CONF_PATH);
  const mainWindow = mainConf.app.windows[0];
  const macWindow = macConf.app.windows[0];

  for (const key of INTENTIONAL_OVERRIDE_KEYS) {
    assert.ok(
      key in mainWindow || key in macWindow,
      `${key} 兩邊都沒有定義，INTENTIONAL_OVERRIDE_KEYS 裡的名字是不是打錯了？`,
    );
  }
});
