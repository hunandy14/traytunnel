/**
 * latest.json 的「底稿合併」邏輯——支援「單平台可控發佈」。
 *
 * 背景：updater 的 endpoint 是 releases/latest/download/latest.json（固定網址，
 * 永遠指向最新一個 release 上同名的資產）。release.yml 支援用 workflow_dispatch
 * 只重發某一個平台（例如只補發 macOS，或只修 Windows）；如果每次都從零組
 * latest.json，單平台發佈會讓「這次沒建置」的另一個平台的 platforms key
 * 整個從 latest.json 消失，那個平台的使用者就再也收不到更新提示。
 *
 * 作法：發佈前先抓現行 releases/latest/download/latest.json 當「底稿」
 * （首發或確定 404 就當空底稿；抓取失敗不算——見 release.yml 的重試邏輯），
 * 只用這次建置出來的平台覆寫對應的 platforms key，其餘 key 原樣保留。
 * version / pub_date 一律採用這次發佈的值。
 *
 * ---------------------------------------------------------------------------
 * 「保留條目」的兩道防線（都在這裡，因為兩者都只有合併當下才有足夠資訊判斷）
 *
 * 1. 形狀驗證：被保留的底稿條目必須是「signature 與 url 皆為非空字串」的物件。
 *    壞掉的條目原樣傳播出去，後果不是只有那一個平台壞掉——Tauri updater 反
 *    序列化整份 RemoteRelease 會直接失敗，等於**兩個平台的更新一起無聲死掉**。
 *    寧可在 CI 擋下來。
 *
 * 2. 陳舊條目斷言（options.tag 必填）：被保留的條目，其 url 必須指向
 *    這次的 release tag，否則預設硬失敗。語意分野：
 *      - 安全情境：往「既有的 release」補另一條腿（例如 v0.6.6 已經發過
 *        Windows，現在補 macOS）。此時底稿裡 Windows 的 url 本來就含 v0.6.6，
 *        斷言自動通過，不會擋到人。
 *      - 危險情境：對「全新版本」只發一個平台。另一平台的條目還指著舊 tag，
 *        但 latest.json 的 version 已經是新版——那個平台的使用者會看到「有新
 *        版本」，抓到的卻是舊檔案（甚至可能是這個新 release 上根本不存在的
 *        資產）。這種情況要人顯式扛責：allow_stale_platforms=true。
 *
 * 這裡也是 Tauri manifest 格式的先天限制暴露處：latest.json 只有一個全域
 * version 欄位，沒有 per-platform 版本。所以「單發某一平台的新版本」在語意上
 * 必然讓另一平台看到 version 前進、卻拿到上一版的 url——不是這支腳本能修的，
 * 只能擋下來要人確認。真正的解法是兩個平台一起發（platform=all）。
 *
 * 3. version 單調性：這次的 version 比底稿還舊時發出警告（不硬擋）。取捨理由
 *    寫在 checkVersionMonotonicity 上面。
 *
 * 4. 底稿本身的形狀（assertBaselineShape，fail-closed）：底稿只有「null／
 *    undefined＝確定沒有」與「合法的 updater manifest」兩種合法狀態。是合法
 *    JSON 但形狀壞掉（頂層 null／陣列／字串，或 platforms 是陣列）時硬失敗，
 *    不再靜默視為「沒有底稿」——後者會讓另一個平台的條目無聲消失。
 *
 * 純函式，不碰檔案系統／網路——I/O 交給呼叫端（見 scripts/compose-latest-json.mjs）。
 */

import { SEMVER_RE, compareSemver } from "./semver.mjs";

/** 非 null、非陣列的物件（JSON 物件）——陣列也是 typeof "object"，要另外排除 */
function isPlainObject(value) {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

/**
 * 底稿的形狀驗證（SCR-2，fail-closed）。
 *
 * 舊行為是 fail-open：底稿只要不是「物件且 platforms 是物件」就靜默當成
 * 「沒有底稿」。那正好與這支模組的目標相反——底稿是合法 JSON 但形狀壞掉
 * （頂層是 null／陣列／字串，或 platforms 是陣列）時，另一個平台的條目會
 * 無聲消失：platform=macos 單腿補發時 releases/latest 回 200 但內容是
 * null／[]，產出的 latest.json 只剩 darwin-aarch64，windows-x86_64 從
 * updater endpoint 消失，而且整條流程全綠。
 *
 * 所以：null／undefined（＝確定沒有底稿，首發的 404 路徑）放行；其餘一律
 * 必須是「非陣列物件」，platforms 若存在也必須是「非陣列物件」，否則丟錯。
 *
 * allowAbsent=false 給「手上已經有一份實際內容」的呼叫端用（例如
 * fetch-baseline 對 HTTP 200 回應做的驗證、或 compose 讀到一個非空的底稿
 * 檔）：那種情境下連 JSON null 都是壞形狀，不是「沒有底稿」——「沒有底稿」
 * 只能由 404／檔案不存在／檔案空白來表示。
 *
 * @param {unknown} baseline
 * @param {{ allowAbsent?: boolean }} [options]
 * @returns {Record<string, unknown>} 底稿的 platforms（沒有底稿時是空物件）
 */
export function assertBaselineShape(baseline, options = {}) {
  const { allowAbsent = true } = options;
  if (baseline === null || baseline === undefined) {
    if (allowAbsent) return {};
    throw new Error(
      "現行 latest.json 的內容是 JSON null／undefined，不是有效的 updater manifest。" +
        "「沒有底稿」只能由 HTTP 404（或底稿檔不存在／空白）表示，不能由內容本身表示，因此中止。",
    );
  }
  if (!isPlainObject(baseline)) {
    throw new Error(
      `現行 latest.json 的底稿不是 JSON 物件（實際是 ${Array.isArray(baseline) ? "陣列" : typeof baseline}）。` +
        `這代表抓到的內容不是有效的 updater manifest；靜默當成「沒有底稿」會把另一個平台的條目` +
        `從 latest.json 抹掉，因此中止。`,
    );
  }
  if (baseline.platforms === undefined) return {};
  if (!isPlainObject(baseline.platforms)) {
    throw new Error(
      `現行 latest.json 底稿的 platforms 不是 JSON 物件（實際是 ` +
        `${baseline.platforms === null ? "null" : Array.isArray(baseline.platforms) ? "陣列" : typeof baseline.platforms}）。` +
        `靜默當成「沒有底稿」會把另一個平台的條目從 latest.json 抹掉，因此中止。`,
    );
  }
  return baseline.platforms;
}

/** 條目必須是 { signature: 非空字串, url: 非空字串 }；其餘欄位不管 */
function isWellFormedEntry(entry) {
  return (
    !!entry &&
    typeof entry === "object" &&
    typeof entry.signature === "string" &&
    entry.signature.trim() !== "" &&
    typeof entry.url === "string" &&
    entry.url.trim() !== ""
  );
}

/**
 * 這次的 version 比底稿舊時警告。
 *
 * 取捨：警告而非硬擋。真正會傷到使用者的情境——「保留的條目指向別的 tag」——
 * 已經被上面的陳舊條目斷言硬擋住了（版本倒退時，底稿留下來的條目必然帶著比
 * 這次更新的 tag，斷言會直接失敗）。剩下的純倒退情境（例如重跑一次舊 tag 的
 * workflow）產出的 latest.json 內部仍然自洽，危害有限，卻是很典型的「人為
 * 誤觸」訊號。硬擋會讓半夜的緊急重發被卡死，收益不對稱，所以只吼一聲。
 */
function checkVersionMonotonicity(baselineVersion, currentVersion, warn) {
  if (typeof baselineVersion !== "string" || !SEMVER_RE.test(baselineVersion)) return;
  if (!SEMVER_RE.test(currentVersion)) return;
  if (compareSemver(currentVersion, baselineVersion) < 0) {
    warn(
      `這次發佈的版本 ${currentVersion} 比現行 latest.json 的 ${baselineVersion} 還舊——` +
        `確認這是不是拿錯 tag／重跑了舊的 workflow。`,
    );
  }
}

/**
 * @param {unknown} baseline 現行 latest.json 的內容（首發或確定沒有就傳 null / undefined / {}）
 * @param {{ version: string, pub_date: string, platforms: Record<string, { signature: string, url: string }> }} current
 *   這次發佈實際建置出來的內容，platforms 只包含「這次有建置」的平台 key
 * @param {{ tag: string, allowStalePlatforms?: boolean, onWarning?: (msg: string) => void }} options
 *   tag                  這次的 release tag（例如 v0.6.6）。必填——陳舊條目斷言
 *                         （見上方防線 2）永遠會執行，不能靠漏傳 tag 靜默停用它
 *   allowStalePlatforms  true＝陳舊條目只警告不擋（顯式扛責的逃生門）
 *   onWarning            警告輸出管道，預設 console.warn
 * @returns {{ version: string, pub_date: string, platforms: Record<string, { signature: string, url: string }> }}
 */
export function mergeLatestJson(baseline, current, options = {}) {
  const { tag, allowStalePlatforms = false, onWarning = (msg) => console.warn(msg) } = options;

  if (!tag || typeof tag !== "string") {
    throw new Error("options.tag 必填（字串），陳舊條目斷言需要它才能判斷保留的條目是否屬於這次的 release");
  }
  if (!current || typeof current !== "object") {
    throw new Error("current 必填，且需含 version / pub_date / platforms");
  }
  if (!current.version || typeof current.version !== "string") {
    throw new Error("current.version 必填（字串）");
  }
  if (!current.pub_date || typeof current.pub_date !== "string") {
    throw new Error("current.pub_date 必填（字串）");
  }

  const currentPlatforms =
    current.platforms && typeof current.platforms === "object" ? current.platforms : {};
  if (Object.keys(currentPlatforms).length === 0) {
    throw new Error("current.platforms 至少要有一個平台 key，這次發佈總得建置點什麼");
  }
  for (const [key, entry] of Object.entries(currentPlatforms)) {
    if (!isWellFormedEntry(entry)) {
      throw new Error(`current.platforms["${key}"] 必須是含 signature 與 url（皆非空字串）的物件`);
    }
  }

  // fail-closed：形狀壞掉的底稿直接丟錯，不當成「沒有底稿」（見 assertBaselineShape）
  const basePlatforms = assertBaselineShape(baseline);

  // 只驗「這次沒建置、要從底稿沿用」的條目：這次有建置的 key 反正會被覆寫掉，
  // 底稿裡就算是壞的也無所謂。
  const retainedKeys = Object.keys(basePlatforms).filter((key) => !(key in currentPlatforms));
  for (const key of retainedKeys) {
    const entry = basePlatforms[key];
    if (!isWellFormedEntry(entry)) {
      throw new Error(
        `現行 latest.json 的 platforms["${key}"] 缺 signature 或 url（或不是字串），` +
          `這次沒有建置該平台、無法重新產生它。原樣沿用會讓 Tauri updater 整份 manifest ` +
          `反序列化失敗，連有效平台的更新一起壞掉，因此中止。請改成連該平台一起建置。`,
      );
    }
    if (!entry.url.includes(`/${tag}/`)) {
      const detail =
        `現行 latest.json 的 platforms["${key}"] 是這次沒建置、從底稿沿用的條目，` +
        `但它的 url 不屬於這次的 release ${tag}：${entry.url}。合併後 version 會變成 ` +
        `${current.version}，該平台的使用者會被通知「有新版本」卻抓到上一版的檔案。`;
      if (!allowStalePlatforms) {
        throw new Error(
          `${detail} 正確作法是連該平台一起建置（platform=all）；若你很清楚這樣做沒問題` +
            `（例如是在往既有的 ${tag} release 補另一條腿），請顯式帶 allow_stale_platforms=true 重跑。`,
        );
      }
      onWarning(`${detail} 已因 allow_stale_platforms=true 放行。`);
    }
  }

  checkVersionMonotonicity(isPlainObject(baseline) ? baseline.version : undefined, current.version, onWarning);

  return {
    version: current.version,
    pub_date: current.pub_date,
    platforms: {
      ...basePlatforms,
      ...currentPlatforms,
    },
  };
}
