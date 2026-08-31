/**
 * scripts/lib/fetch-retry.mjs 的自測案例。
 *
 * 用 Node 內建 test runner，無外部相依：
 *   node --test scripts/lib/fetch-retry.test.mjs
 * 或跑整個 scripts/ 底下所有 *.test.mjs：
 *   npm run test:release
 *
 * fetch 與 sleep 都用注入的假實作，測試不碰網路、也不會真的等 5 秒。
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { FetchRetryExhaustedError, fetchWithRetry } from "./fetch-retry.mjs";

/** 依序回傳預先排好的回應（或丟出預先排好的例外），並記錄被呼叫幾次 */
function fakeFetch(script) {
  const calls = [];
  const impl = async (url, init) => {
    calls.push({ url, init });
    const step = script[Math.min(calls.length - 1, script.length - 1)];
    if (step.throws) throw step.throws;
    return {
      status: step.status,
      text: async () => step.body ?? "",
    };
  };
  impl.calls = calls;
  return impl;
}

/** 記錄退避秒數，但不真的等 */
function fakeSleep() {
  const waited = [];
  const impl = async (ms) => {
    waited.push(ms);
  };
  impl.waited = waited;
  return impl;
}

const quiet = () => {};

test("404 立刻分類回傳，不重試", async () => {
  const impl = fakeFetch([{ status: 404 }]);
  const sleep = fakeSleep();

  const result = await fetchWithRetry("https://example.invalid/x", {
    fetchImpl: impl,
    sleep,
    log: quiet,
  });

  assert.equal(result.notFound, true);
  assert.equal(result.status, 404);
  assert.equal(impl.calls.length, 1, "404 不應該再重試");
  assert.deepEqual(sleep.waited, []);
});

test("200 且驗證通過立刻回傳內容", async () => {
  const impl = fakeFetch([{ status: 200, body: '{"ok":true}' }]);
  const sleep = fakeSleep();
  let validated = null;

  const result = await fetchWithRetry("https://example.invalid/x", {
    validate: (text) => {
      validated = text;
      JSON.parse(text);
    },
    fetchImpl: impl,
    sleep,
    log: quiet,
  });

  assert.equal(result.notFound, false);
  assert.equal(result.status, 200);
  assert.equal(result.text, '{"ok":true}');
  assert.equal(validated, '{"ok":true}');
  assert.equal(impl.calls.length, 1);
  assert.deepEqual(sleep.waited, []);
});

test("暫時性失敗（連線失敗 → 5xx → 200 但驗證不過 → 200 合法）會重試，退避 i*5 秒", async () => {
  const impl = fakeFetch([
    { throws: new Error("socket hang up") },
    { status: 503 },
    { status: 200, body: "not json" },
    { status: 200, body: '{"ok":true}' },
  ]);
  const sleep = fakeSleep();
  const logged = [];

  const result = await fetchWithRetry("https://example.invalid/x", {
    validate: (text) => JSON.parse(text),
    fetchImpl: impl,
    sleep,
    log: (msg) => logged.push(msg),
  });

  assert.equal(result.text, '{"ok":true}');
  assert.equal(impl.calls.length, 4);
  assert.deepEqual(sleep.waited, [5_000, 10_000, 15_000], "退避是第 i 次失敗後 i*5 秒");
  assert.equal(logged.length, 3);
  assert.match(logged[0], /連線失敗/);
  assert.match(logged[1], /HTTP 503/);
  assert.match(logged[2], /內容驗證失敗/);
});

test("重試用盡丟 FetchRetryExhaustedError，訊息帶最後一次失敗細節", async () => {
  const impl = fakeFetch([{ status: 500 }]);
  const sleep = fakeSleep();

  await assert.rejects(
    () =>
      fetchWithRetry("https://example.invalid/x", {
        attempts: 3,
        label: "測試底稿",
        exhaustedHint: "請稍後重跑。",
        fetchImpl: impl,
        sleep,
        log: quiet,
      }),
    (err) => {
      assert.ok(err instanceof FetchRetryExhaustedError);
      assert.equal(err.attempts, 3);
      assert.equal(err.lastFailureDetail, "HTTP 500");
      assert.match(err.message, /測試底稿/);
      assert.match(err.message, /HTTP 500/);
      assert.match(err.message, /請稍後重跑。$/);
      return true;
    },
  );

  assert.equal(impl.calls.length, 3, "attempts 次數就是實際嘗試次數");
  assert.deepEqual(sleep.waited, [5_000, 10_000], "最後一次失敗後不再退避");
});

test("attempts=1 邊界：一次就失敗，不退避直接丟錯", async () => {
  const impl = async () => {
    throw new Error("boom");
  };
  const sleep = fakeSleep();
  await assert.rejects(
    () => fetchWithRetry("https://example.invalid/x", { attempts: 1, fetchImpl: impl, sleep, log: quiet }),
    FetchRetryExhaustedError,
  );
  assert.deepEqual(sleep.waited, []);
});

test("headers 會原樣傳給 fetch", async () => {
  const impl = fakeFetch([{ status: 200, body: "ok" }]);
  await fetchWithRetry("https://example.invalid/x", {
    headers: { Authorization: "Bearer TOKEN" },
    fetchImpl: impl,
    sleep: fakeSleep(),
    log: quiet,
  });
  assert.deepEqual(impl.calls[0].init.headers, { Authorization: "Bearer TOKEN" });
});
