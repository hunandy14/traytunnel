/**
 * scripts/lib/latest-json.mjs 的自測案例。
 *
 * 用 Node 內建 test runner，無外部相依：
 *   node --test scripts/lib/latest-json.test.mjs
 * 或跑整個 scripts/ 底下所有 *.test.mjs：
 *   npm run test:release
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { mergeLatestJson } from "./latest-json.mjs";

const WINDOWS_ENTRY = {
  signature: "windows-old-signature",
  url: "https://github.com/hunandy14/traytunnel/releases/download/v0.6.4/traytunnel-0.6.4-setup.exe",
};
const MAC_ENTRY_NEW = {
  signature: "darwin-new-signature",
  url: "https://github.com/hunandy14/traytunnel/releases/download/v0.6.5/traytunnel-0.6.5-aarch64.app.tar.gz",
};
const WINDOWS_ENTRY_NEW = {
  signature: "windows-new-signature",
  url: "https://github.com/hunandy14/traytunnel/releases/download/v0.6.5/traytunnel-0.6.5-setup.exe",
};

test("案例 1：底稿有 windows 條目，本次只建 mac → 兩條都在（windows 保留舊值，mac 是新值）", () => {
  const baseline = {
    version: "0.6.4",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: { "windows-x86_64": WINDOWS_ENTRY },
  };
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: { "darwin-aarch64": MAC_ENTRY_NEW },
  };

  const merged = mergeLatestJson(baseline, current);

  assert.equal(merged.version, "0.6.5");
  assert.equal(merged.pub_date, "2026-08-29T00:00:00.000Z");
  assert.deepEqual(Object.keys(merged.platforms).sort(), ["darwin-aarch64", "windows-x86_64"]);
  assert.deepEqual(merged.platforms["windows-x86_64"], WINDOWS_ENTRY, "windows 條目應原樣保留（底稿舊值）");
  assert.deepEqual(merged.platforms["darwin-aarch64"], MAC_ENTRY_NEW, "mac 條目應是這次建置的新值");
});

test("案例 2：首發，無底稿（undefined）→ 只有本次平台", () => {
  const current = {
    version: "0.1.0",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: { "windows-x86_64": WINDOWS_ENTRY_NEW },
  };

  const merged = mergeLatestJson(undefined, current);

  assert.deepEqual(Object.keys(merged.platforms), ["windows-x86_64"]);
  assert.deepEqual(merged.platforms["windows-x86_64"], WINDOWS_ENTRY_NEW);
});

test("案例 2b：首發，底稿是空物件 {} → 只有本次平台", () => {
  const current = {
    version: "0.1.0",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: { "darwin-aarch64": MAC_ENTRY_NEW },
  };

  const merged = mergeLatestJson({}, current);

  assert.deepEqual(Object.keys(merged.platforms), ["darwin-aarch64"]);
});

test("案例 3：兩平台齊發 → 兩條都是這次的新值（即使底稿兩個都有舊值，也應被覆寫）", () => {
  const baseline = {
    version: "0.6.4",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: {
      "windows-x86_64": WINDOWS_ENTRY,
      "darwin-aarch64": { signature: "darwin-old-signature", url: "https://example.invalid/old.tar.gz" },
    },
  };
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: {
      "windows-x86_64": WINDOWS_ENTRY_NEW,
      "darwin-aarch64": MAC_ENTRY_NEW,
    },
  };

  const merged = mergeLatestJson(baseline, current);

  assert.deepEqual(Object.keys(merged.platforms).sort(), ["darwin-aarch64", "windows-x86_64"]);
  assert.deepEqual(merged.platforms["windows-x86_64"], WINDOWS_ENTRY_NEW);
  assert.deepEqual(merged.platforms["darwin-aarch64"], MAC_ENTRY_NEW);
});

test("案例 4：底稿含這次沒建置的第三個平台 key（例如未來的 linux-x86_64）→ 原樣保留", () => {
  const baseline = {
    version: "0.6.4",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: {
      "windows-x86_64": WINDOWS_ENTRY,
      "linux-x86_64": { signature: "linux-sig", url: "https://example.invalid/linux.AppImage" },
    },
  };
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: { "windows-x86_64": WINDOWS_ENTRY_NEW },
  };

  const merged = mergeLatestJson(baseline, current);

  assert.deepEqual(Object.keys(merged.platforms).sort(), ["linux-x86_64", "windows-x86_64"]);
  assert.deepEqual(merged.platforms["linux-x86_64"], baseline.platforms["linux-x86_64"]);
  assert.deepEqual(merged.platforms["windows-x86_64"], WINDOWS_ENTRY_NEW);
});

test("邊界：current 缺 platforms（或空物件）要丟錯，不能靜默生出空的 latest.json", () => {
  assert.throws(() => mergeLatestJson(null, { version: "0.1.0", pub_date: "2026-01-01T00:00:00.000Z" }));
  assert.throws(() =>
    mergeLatestJson(null, { version: "0.1.0", pub_date: "2026-01-01T00:00:00.000Z", platforms: {} }),
  );
});

test("邊界：current 缺 version 或 pub_date 要丟錯", () => {
  assert.throws(() =>
    mergeLatestJson(null, { pub_date: "2026-01-01T00:00:00.000Z", platforms: { x: WINDOWS_ENTRY } }),
  );
  assert.throws(() => mergeLatestJson(null, { version: "0.1.0", platforms: { x: WINDOWS_ENTRY } }));
});

test("邊界：baseline 是壞掉的形狀（platforms 不是物件、或整個是字串）不應炸掉，視同無底稿", () => {
  const current = {
    version: "0.1.0",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: { "windows-x86_64": WINDOWS_ENTRY_NEW },
  };

  assert.deepEqual(mergeLatestJson({ platforms: "not-an-object" }, current).platforms, {
    "windows-x86_64": WINDOWS_ENTRY_NEW,
  });
  assert.deepEqual(mergeLatestJson("garbage", current).platforms, {
    "windows-x86_64": WINDOWS_ENTRY_NEW,
  });
});
