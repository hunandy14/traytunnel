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
 * 2. 陳舊條目斷言（需要呼叫端傳 options.tag）：被保留的條目，其 url 必須指向
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
 * 純函式，不碰檔案系統／網路——I/O 交給呼叫端（見 scripts/compose-latest-json.mjs）。
 */

import { SEMVER_RE, compareSemver } from "./semver.mjs";

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
 * @param {{ tag?: string, allowStalePlatforms?: boolean, onWarning?: (msg: string) => void }} [options]
 *   tag                  這次的 release tag（例如 v0.6.6）。有傳才會做陳舊條目斷言
 *   allowStalePlatforms  true＝陳舊條目只警告不擋（顯式扛責的逃生門）
 *   onWarning            警告輸出管道，預設 console.warn
 * @returns {{ version: string, pub_date: string, platforms: Record<string, { signature: string, url: string }> }}
 */
export function mergeLatestJson(baseline, current, options = {}) {
  const { tag, allowStalePlatforms = false, onWarning = (msg) => console.warn(msg) } = options;

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

  const baselineIsObject = !!baseline && typeof baseline === "object";
  const basePlatforms =
    baselineIsObject && baseline.platforms && typeof baseline.platforms === "object"
      ? baseline.platforms
      : {};

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
    if (tag && !entry.url.includes(`/${tag}/`)) {
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

  checkVersionMonotonicity(baselineIsObject ? baseline.version : undefined, current.version, onWarning);

  return {
    version: current.version,
    pub_date: current.pub_date,
    platforms: {
      ...basePlatforms,
      ...currentPlatforms,
    },
  };
}
