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

  // windows 條目留著 v0.6.4 的舊 url，這次的 tag 是 v0.6.5，本來就是陳舊條目
  // 斷言要擋的情境——這裡顯式 allowStalePlatforms 只是為了單獨驗證合併邏輯本身。
  const merged = mergeLatestJson(baseline, current, { tag: "v0.6.5", allowStalePlatforms: true });

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

  const merged = mergeLatestJson(undefined, current, { tag: "v0.1.0" });

  assert.deepEqual(Object.keys(merged.platforms), ["windows-x86_64"]);
  assert.deepEqual(merged.platforms["windows-x86_64"], WINDOWS_ENTRY_NEW);
});

test("案例 2b：首發，底稿是空物件 {} → 只有本次平台", () => {
  const current = {
    version: "0.1.0",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: { "darwin-aarch64": MAC_ENTRY_NEW },
  };

  const merged = mergeLatestJson({}, current, { tag: "v0.1.0" });

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

  const merged = mergeLatestJson(baseline, current, { tag: "v0.6.5" });

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

  // linux-x86_64 的 url 不屬於這次的 tag（本來就不是這次 release 建置出來的），
  // 顯式 allowStalePlatforms 只是為了單獨驗證「原樣保留」這件事，不是在測陳舊
  // 條目斷言本身（那部分見下面「陳舊條目」區塊）。
  const merged = mergeLatestJson(baseline, current, { tag: "v0.6.5", allowStalePlatforms: true });

  assert.deepEqual(Object.keys(merged.platforms).sort(), ["linux-x86_64", "windows-x86_64"]);
  assert.deepEqual(merged.platforms["linux-x86_64"], baseline.platforms["linux-x86_64"]);
  assert.deepEqual(merged.platforms["windows-x86_64"], WINDOWS_ENTRY_NEW);
});

test("邊界：current 缺 platforms（或空物件）要丟錯，不能靜默生出空的 latest.json", () => {
  assert.throws(() =>
    mergeLatestJson(null, { version: "0.1.0", pub_date: "2026-01-01T00:00:00.000Z" }, { tag: "v0.1.0" }),
  );
  assert.throws(() =>
    mergeLatestJson(
      null,
      { version: "0.1.0", pub_date: "2026-01-01T00:00:00.000Z", platforms: {} },
      { tag: "v0.1.0" },
    ),
  );
});

test("邊界：current 缺 version 或 pub_date 要丟錯", () => {
  assert.throws(() =>
    mergeLatestJson(
      null,
      { pub_date: "2026-01-01T00:00:00.000Z", platforms: { x: WINDOWS_ENTRY } },
      { tag: "v0.1.0" },
    ),
  );
  assert.throws(() =>
    mergeLatestJson(null, { version: "0.1.0", platforms: { x: WINDOWS_ENTRY } }, { tag: "v0.1.0" }),
  );
});

test("邊界：options.tag 缺省要丟錯（陳舊條目斷言不能被靜默停用）", () => {
  const current = {
    version: "0.1.0",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: { "windows-x86_64": WINDOWS_ENTRY_NEW },
  };
  assert.throws(() => mergeLatestJson(null, current), /tag/);
  assert.throws(() => mergeLatestJson(null, current, {}), /tag/);
});

// SCR-2 規格翻轉：這個案例原本鎖定「壞形狀底稿＝視同無底稿」（fail-open），
// 現在改成硬失敗（fail-closed）。理由：底稿是合法 JSON 但形狀壞掉時靜默當成
// 「沒有底稿」，會讓這次沒建置的那個平台的條目從 latest.json 無聲消失，
// 而且整條發佈流程全綠——正是這支模組要擋下來的那類事故。
test("邊界：baseline 是壞掉的形狀（platforms 不是物件、或整個是字串）要丟錯，不可視同無底稿", () => {
  const current = {
    version: "0.1.0",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: { "windows-x86_64": WINDOWS_ENTRY_NEW },
  };

  assert.throws(
    () => mergeLatestJson({ platforms: "not-an-object" }, current, { tag: "v0.1.0" }),
    /platforms/,
  );
  assert.throws(() => mergeLatestJson({ platforms: [] }, current, { tag: "v0.1.0" }), /platforms/);
  assert.throws(() => mergeLatestJson("garbage", current, { tag: "v0.1.0" }), /不是 JSON 物件/);
  assert.throws(() => mergeLatestJson([], current, { tag: "v0.1.0" }), /陣列/);
});

test("邊界：baseline 是 null／undefined／{} 仍視同無底稿（首發的 404 路徑必須保持綠）", () => {
  const current = {
    version: "0.1.0",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: { "windows-x86_64": WINDOWS_ENTRY_NEW },
  };

  for (const baseline of [null, undefined, {}]) {
    assert.deepEqual(mergeLatestJson(baseline, current, { tag: "v0.1.0" }).platforms, {
      "windows-x86_64": WINDOWS_ENTRY_NEW,
    });
  }
});

// --- 保留條目的形狀驗證（壞條目原樣傳播會讓 updater 整份 manifest 反序列化
// --- 失敗，兩個平台的更新一起無聲死掉，所以要在 CI 擋下來）

test("保留條目缺 url → 丟錯（不可原樣傳播）", () => {
  const baseline = {
    version: "0.6.4",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: { "windows-x86_64": { signature: "windows-old-signature" } },
  };
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: { "darwin-aarch64": MAC_ENTRY_NEW },
  };

  assert.throws(() => mergeLatestJson(baseline, current, { tag: "v0.6.5" }), /windows-x86_64/);
});

test("保留條目缺 signature、或欄位不是字串／是空字串 → 丟錯", () => {
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: { "darwin-aarch64": MAC_ENTRY_NEW },
  };
  const withWindows = (entry) => ({
    version: "0.6.4",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: { "windows-x86_64": entry },
  });

  assert.throws(() => mergeLatestJson(withWindows({ url: WINDOWS_ENTRY.url }), current, { tag: "v0.6.5" }));
  assert.throws(() =>
    mergeLatestJson(withWindows({ signature: 12345, url: WINDOWS_ENTRY.url }), current, { tag: "v0.6.5" }),
  );
  assert.throws(() =>
    mergeLatestJson(withWindows({ signature: "sig", url: "" }), current, { tag: "v0.6.5" }),
  );
  assert.throws(() =>
    mergeLatestJson(withWindows({ signature: "   ", url: WINDOWS_ENTRY.url }), current, { tag: "v0.6.5" }),
  );
  assert.throws(() => mergeLatestJson(withWindows(null), current, { tag: "v0.6.5" }));
});

test("這次有建置的平台，底稿裡對應的壞條目不該擋路（反正會被覆寫）", () => {
  const baseline = {
    version: "0.6.4",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: { "windows-x86_64": { signature: null } },
  };
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: { "windows-x86_64": WINDOWS_ENTRY_NEW },
  };

  assert.deepEqual(mergeLatestJson(baseline, current, { tag: "v0.6.5" }).platforms, {
    "windows-x86_64": WINDOWS_ENTRY_NEW,
  });
});

// --- 陳舊條目斷言（options.tag）

test("陳舊條目：保留的 windows 條目 url 不含這次的 tag → 預設硬失敗", () => {
  const baseline = {
    version: "0.6.4",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: { "windows-x86_64": WINDOWS_ENTRY }, // url 是 v0.6.4
  };
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: { "darwin-aarch64": MAC_ENTRY_NEW },
  };

  assert.throws(
    () => mergeLatestJson(baseline, current, { tag: "v0.6.5" }),
    /allow_stale_platforms/,
    "危險情境（全新版本只發一個平台）必須擋下來，並指出逃生門",
  );
});

test("陳舊條目：allow_stale_platforms=true → 放行，但要吐警告且結果正確", () => {
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

  const warnings = [];
  const merged = mergeLatestJson(baseline, current, {
    tag: "v0.6.5",
    allowStalePlatforms: true,
    onWarning: (msg) => warnings.push(msg),
  });

  assert.equal(warnings.length, 1, "放行也要留下警告，不能靜悄悄");
  assert.match(warnings[0], /windows-x86_64/);
  assert.deepEqual(merged.platforms["windows-x86_64"], WINDOWS_ENTRY);
  assert.deepEqual(merged.platforms["darwin-aarch64"], MAC_ENTRY_NEW);
});

test("陳舊條目：安全情境（往既有 release 補另一條腿，url 已含同一個 tag）→ 自動通過", () => {
  const baseline = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: { "windows-x86_64": WINDOWS_ENTRY_NEW }, // url 已經是 v0.6.5
  };
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T01:00:00.000Z",
    platforms: { "darwin-aarch64": MAC_ENTRY_NEW },
  };

  const warnings = [];
  const merged = mergeLatestJson(baseline, current, {
    tag: "v0.6.5",
    onWarning: (msg) => warnings.push(msg),
  });

  assert.deepEqual(warnings, [], "安全情境不該有任何警告");
  assert.deepEqual(Object.keys(merged.platforms).sort(), ["darwin-aarch64", "windows-x86_64"]);
});

test("陳舊條目：tag 比對是整段路徑，v0.6.5 不該誤判成 v0.6.50 的前綴", () => {
  const baseline = {
    version: "0.6.50",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: {
      "windows-x86_64": {
        signature: "windows-old-signature",
        url: "https://github.com/hunandy14/traytunnel/releases/download/v0.6.50/traytunnel-0.6.50-setup.exe",
      },
    },
  };
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: { "darwin-aarch64": MAC_ENTRY_NEW },
  };

  assert.throws(() => mergeLatestJson(baseline, current, { tag: "v0.6.5" }));
});

test("陳舊條目：這次有建置的平台不受斷言影響（它的 url 由呼叫端用本次 tag 組出來）", () => {
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: {
      "windows-x86_64": WINDOWS_ENTRY_NEW,
      "darwin-aarch64": MAC_ENTRY_NEW,
    },
  };

  const merged = mergeLatestJson({ platforms: { "windows-x86_64": WINDOWS_ENTRY } }, current, {
    tag: "v0.6.5",
  });
  assert.deepEqual(merged.platforms["windows-x86_64"], WINDOWS_ENTRY_NEW);
});

// --- version 單調性（取捨：警告不硬擋，理由見 latest-json.mjs）

test("version 單調性：這次比底稿舊 → 警告（不丟錯）", () => {
  const baseline = {
    version: "0.7.0",
    pub_date: "2026-01-01T00:00:00.000Z",
    platforms: {},
  };
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: { "darwin-aarch64": MAC_ENTRY_NEW },
  };

  const warnings = [];
  const merged = mergeLatestJson(baseline, current, {
    tag: "v0.6.5",
    onWarning: (msg) => warnings.push(msg),
  });

  assert.equal(merged.version, "0.6.5");
  assert.equal(warnings.length, 1);
  assert.match(warnings[0], /0\.7\.0/);
});

test("version 單調性：前進或同版（重發同一個 release）不該有警告", () => {
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: { "darwin-aarch64": MAC_ENTRY_NEW },
  };

  for (const baselineVersion of ["0.6.4", "0.6.5"]) {
    const warnings = [];
    mergeLatestJson(
      { version: baselineVersion, pub_date: "2026-01-01T00:00:00.000Z", platforms: {} },
      current,
      { tag: "v0.6.5", onWarning: (msg) => warnings.push(msg) },
    );
    assert.deepEqual(warnings, [], `底稿 ${baselineVersion} 不該觸發警告`);
  }
});

test("version 單調性：底稿版本不是嚴格 semver 就跳過比較，不該炸掉", () => {
  const current = {
    version: "0.6.5",
    pub_date: "2026-08-29T00:00:00.000Z",
    platforms: { "darwin-aarch64": MAC_ENTRY_NEW },
  };

  const warnings = [];
  for (const baselineVersion of ["v0.7.0", "1.2.3-beta.1", "", 42, undefined]) {
    mergeLatestJson({ version: baselineVersion, platforms: {} }, current, {
      tag: "v0.6.5",
      onWarning: (msg) => warnings.push(msg),
    });
  }
  assert.deepEqual(warnings, []);
});
