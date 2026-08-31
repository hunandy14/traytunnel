/**
 * 發佈流程唯一一份「HTTP 取得＋404／暫時性失敗分野＋退避重試」政策。
 *
 * 這條分野是整個發佈管線的安全核心，不能弄反：
 *   HTTP 404      → 確定「線上真的沒有這個東西」（首發的底稿、還沒建立的
 *                   release）。立刻回傳 { notFound: true }，由呼叫端決定
 *                   那代表「空底稿」還是「硬失敗」，不進重試。
 *   其他任何失敗  → 網路抖動、5xx、rate limit、或 2xx 但內容驗證不過（被
 *                   中間層攔截的錯誤頁、半截內容）——一律視為暫時性，退避
 *                   重試；重試用盡才丟錯。
 *   把「暫時性失敗」誤判成「確定沒有」，會讓底稿合併把另一個平台的
 *   latest.json 條目／SHA256SUMS.txt checksum 整個抹掉、或讓「已存在的
 *   release」被當成不存在而覆寫掉人工編輯過的 release notes——兩者都是
 *   全綠的無聲事故。
 *
 * 退避與記錄只寫在這裡一次（過去 fetch-baseline.mjs 的迴圈裡抄三份、
 * release.yml 的 bash 又手刻第四份）：第 i 次失敗後 sleep i*5 秒，跟原本
 * 的 bash／node 版本一致。
 *
 * 純函式層，不碰檔案系統，也不 process.exit——I/O 與退場碼交給呼叫端
 * （scripts/fetch-baseline.mjs、scripts/probe-release.mjs）。
 */

/** 內部用：標記「這次嘗試失敗了，但屬於可重試的那一類」 */
class TransientFailure extends Error {}

/** 重試用盡。帶 label／attempts／lastFailureDetail 供呼叫端組訊息 */
export class FetchRetryExhaustedError extends Error {
  constructor(message, { label, attempts, lastFailureDetail }) {
    super(message);
    this.name = "FetchRetryExhaustedError";
    this.label = label;
    this.attempts = attempts;
    this.lastFailureDetail = lastFailureDetail;
  }
}

function describe(err) {
  return err instanceof Error ? err.message : String(err);
}

function realSleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * @param {string} url
 * @param {object} [options]
 * @param {Record<string, string>} [options.headers]
 * @param {number} [options.attempts=5] 最多嘗試幾次
 * @param {(text: string) => void} [options.validate]
 *   2xx 之後用來確認內容真的是合法回應（不是錯誤頁／半截內容）；丟例外＝驗證
 *   失敗＝暫時性失敗，會進重試。
 * @param {string} [options.label] log／錯誤訊息裡的人話說明，預設用 url
 * @param {number} [options.timeoutMs=60000] 單次請求逾時（對齊原本 curl --max-time 60）
 * @param {string} [options.exhaustedHint] 重試用盡時附在錯誤訊息後面的呼叫端說明
 * @param {(msg: string) => void} [options.log]
 * @param {typeof fetch} [options.fetchImpl] 測試用注入點
 * @param {(ms: number) => Promise<void>} [options.sleep] 測試用注入點
 * @returns {Promise<{ notFound: boolean, status: number, text: string }>}
 *   notFound:true  → HTTP 404（text 為空字串）
 *   notFound:false → 2xx 且（若有給 validate）驗證通過
 * @throws {FetchRetryExhaustedError} 重試用盡
 */
export async function fetchWithRetry(url, options = {}) {
  const {
    headers,
    attempts = 5,
    validate,
    label = url,
    timeoutMs = 60_000,
    exhaustedHint = "",
    log = (msg) => console.log(msg),
    fetchImpl = fetch,
    sleep = realSleep,
  } = options;

  let lastFailureDetail = "（尚未嘗試）";

  for (let i = 1; i <= attempts; i += 1) {
    try {
      let response;
      try {
        response = await fetchImpl(url, {
          redirect: "follow",
          headers,
          signal: AbortSignal.timeout(timeoutMs),
        });
      } catch (err) {
        throw new TransientFailure(`連線失敗：${describe(err)}`);
      }

      // 404 是唯一「確定的否定答案」，立刻回傳、不重試。
      if (response.status === 404) return { notFound: true, status: 404, text: "" };

      if (response.status < 200 || response.status >= 300) {
        throw new TransientFailure(`HTTP ${response.status}`);
      }

      let text;
      try {
        text = await response.text();
      } catch (err) {
        throw new TransientFailure(`HTTP ${response.status} 但讀取內容失敗：${describe(err)}`);
      }

      if (validate) {
        try {
          validate(text);
        } catch (err) {
          throw new TransientFailure(`HTTP ${response.status} 但內容驗證失敗：${describe(err)}`);
        }
      }

      return { notFound: false, status: response.status, text };
    } catch (err) {
      if (!(err instanceof TransientFailure)) throw err;
      // 退避與記錄的唯一一份實作。
      lastFailureDetail = err.message;
      log(`第 ${i} 次：${lastFailureDetail}，視為暫時性失敗`);
      if (i < attempts) await sleep(i * 5_000);
    }
  }

  const base = `連續 ${attempts} 次都拿不到 ${label}（最後一次：${lastFailureDetail}），且不是 404。`;
  throw new FetchRetryExhaustedError(exhaustedHint ? `${base}${exhaustedHint}` : base, {
    label,
    attempts,
    lastFailureDetail,
  });
}
