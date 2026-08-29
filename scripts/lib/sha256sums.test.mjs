/**
 * scripts/lib/sha256sums.mjs 的自測案例。
 *
 * 用 Node 內建 test runner，無外部相依：
 *   node --test scripts/lib/sha256sums.test.mjs
 * 或跑整個 scripts/ 底下所有 *.test.mjs：
 *   npm run test:release
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { formatSha256Sums, mergeSha256Sums, parseSha256Sums } from "./sha256sums.mjs";

const WINDOWS_EXE = "a".repeat(64);
const WINDOWS_P_EXE = "b".repeat(64);
const WINDOWS_SETUP_EXE = "c".repeat(64);
const WINDOWS_SETUP_EXE_NEW = "d".repeat(64);
const MAC_DMG = "e".repeat(64);
const MAC_TAR_GZ = "f".repeat(64);

test("parseSha256Sums：一般兩個空白格式（sha256sum 文字模式輸出）", () => {
  const text = `${WINDOWS_EXE}  traytunnel-0.6.5.exe\n${WINDOWS_SETUP_EXE}  traytunnel-0.6.5-setup.exe\n`;
  assert.deepEqual(parseSha256Sums(text), {
    "traytunnel-0.6.5.exe": WINDOWS_EXE,
    "traytunnel-0.6.5-setup.exe": WINDOWS_SETUP_EXE,
  });
});

test("parseSha256Sums：星號（二進位模式）格式也要能解析", () => {
  const text = `${MAC_DMG} *traytunnel-0.6.5-aarch64.dmg\n`;
  assert.deepEqual(parseSha256Sums(text), { "traytunnel-0.6.5-aarch64.dmg": MAC_DMG });
});

test("parseSha256Sums：空字串／null／undefined → 空物件", () => {
  assert.deepEqual(parseSha256Sums(""), {});
  assert.deepEqual(parseSha256Sums(null), {});
  assert.deepEqual(parseSha256Sums(undefined), {});
});

test("parseSha256Sums：忽略空白行，hash 統一轉小寫", () => {
  const text = `\n${WINDOWS_EXE.toUpperCase()}  traytunnel-0.6.5.exe\n\n`;
  assert.deepEqual(parseSha256Sums(text), { "traytunnel-0.6.5.exe": WINDOWS_EXE });
});

test("parseSha256Sums：看不懂的行要丟錯，不能靜默丟掉", () => {
  assert.throws(() => parseSha256Sums("這不是一行合法的 sha256sum 輸出"), /看不懂/);
  assert.throws(() => parseSha256Sums("deadbeef  太短的hash.exe"));
});

test("formatSha256Sums：依檔名排序、兩個空白、每行換行結尾", () => {
  const out = formatSha256Sums({
    "traytunnel-0.6.5-setup.exe": WINDOWS_SETUP_EXE,
    "traytunnel-0.6.5.exe": WINDOWS_EXE,
  });
  assert.equal(
    out,
    `${WINDOWS_SETUP_EXE}  traytunnel-0.6.5-setup.exe\n${WINDOWS_EXE}  traytunnel-0.6.5.exe\n`,
  );
});

test("案例 1（致命 bug 的回歸測試）：底稿有 windows 三行，本次只建 mac 兩行 → 五行都在", () => {
  const baseline = formatSha256Sums({
    "traytunnel-0.6.5.exe": WINDOWS_EXE,
    "traytunnel-0.6.5p.exe": WINDOWS_P_EXE,
    "traytunnel-0.6.5-setup.exe": WINDOWS_SETUP_EXE,
  });
  const current = {
    "traytunnel-0.6.5-aarch64.dmg": MAC_DMG,
    "traytunnel-0.6.5-aarch64.app.tar.gz": MAC_TAR_GZ,
  };

  const merged = mergeSha256Sums(baseline, current);

  assert.deepEqual(Object.keys(merged).sort(), [
    "traytunnel-0.6.5-aarch64.app.tar.gz",
    "traytunnel-0.6.5-aarch64.dmg",
    "traytunnel-0.6.5-setup.exe",
    "traytunnel-0.6.5.exe",
    "traytunnel-0.6.5p.exe",
  ]);
  assert.equal(merged["traytunnel-0.6.5.exe"], WINDOWS_EXE, "windows 條目應原樣保留（底稿舊值）");
  assert.equal(merged["traytunnel-0.6.5p.exe"], WINDOWS_P_EXE);
  assert.equal(merged["traytunnel-0.6.5-setup.exe"], WINDOWS_SETUP_EXE);
  assert.equal(merged["traytunnel-0.6.5-aarch64.dmg"], MAC_DMG, "mac 條目應是這次建置的新值");
  assert.equal(merged["traytunnel-0.6.5-aarch64.app.tar.gz"], MAC_TAR_GZ);
});

test("案例 2：首發，無底稿（undefined）→ 只有本次檔案", () => {
  const current = { "traytunnel-0.1.0-setup.exe": WINDOWS_SETUP_EXE };
  assert.deepEqual(mergeSha256Sums(undefined, current), current);
});

test("案例 2b：首發，底稿是空字串 → 只有本次檔案", () => {
  const current = { "traytunnel-0.1.0-aarch64.dmg": MAC_DMG };
  assert.deepEqual(mergeSha256Sums("", current), current);
});

test("案例 3：兩平台齊發（全平台重發）→ 同名行以這次為準（即使底稿有舊值也覆寫）", () => {
  const baseline = formatSha256Sums({
    "traytunnel-0.6.5-setup.exe": WINDOWS_SETUP_EXE,
    "traytunnel-0.6.5-aarch64.dmg": "0".repeat(64),
  });
  const current = {
    "traytunnel-0.6.5-setup.exe": WINDOWS_SETUP_EXE_NEW,
    "traytunnel-0.6.5-aarch64.dmg": MAC_DMG,
  };

  const merged = mergeSha256Sums(baseline, current);

  assert.equal(merged["traytunnel-0.6.5-setup.exe"], WINDOWS_SETUP_EXE_NEW);
  assert.equal(merged["traytunnel-0.6.5-aarch64.dmg"], MAC_DMG);
});

test("案例 4：底稿含這次沒建置的第三個檔案（例如未來的 linux 產物）→ 原樣保留", () => {
  const baseline = formatSha256Sums({
    "traytunnel-0.6.5-setup.exe": WINDOWS_SETUP_EXE,
    "traytunnel-0.6.5.AppImage": "9".repeat(64),
  });
  const current = { "traytunnel-0.6.5-setup.exe": WINDOWS_SETUP_EXE_NEW };

  const merged = mergeSha256Sums(baseline, current);

  assert.deepEqual(Object.keys(merged).sort(), [
    "traytunnel-0.6.5-setup.exe",
    "traytunnel-0.6.5.AppImage",
  ]);
  assert.equal(merged["traytunnel-0.6.5.AppImage"], "9".repeat(64));
  assert.equal(merged["traytunnel-0.6.5-setup.exe"], WINDOWS_SETUP_EXE_NEW);
});

test("邊界：currentMap 空物件或缺省要丟錯，不能靜默生出只有底稿的 SHA256SUMS.txt", () => {
  assert.throws(() => mergeSha256Sums(null, {}));
  assert.throws(() => mergeSha256Sums(null, undefined));
});

test("邊界：currentMap 裡的 hash 不是合法 64 碼十六進位要丟錯", () => {
  assert.throws(() => mergeSha256Sums(null, { "x.exe": "not-a-hash" }));
  assert.throws(() => mergeSha256Sums(null, { "x.exe": "deadbeef" }));
  assert.throws(() => mergeSha256Sums(null, { "x.exe": 12345 }));
});

test("底稿壞掉的行不會靜默消失：mergeSha256Sums 直接把 parseSha256Sums 的例外往外丟", () => {
  assert.throws(() => mergeSha256Sums("這一行看不懂", { "x.exe": WINDOWS_EXE }));
});
