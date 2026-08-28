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
 * （首發或抓不到就當空底稿），只用這次建置出來的平台覆寫對應的 platforms
 * key，其餘 key 原樣保留。version / pub_date 一律採用這次發佈的值。
 *
 * 純函式，不碰檔案系統／網路——I/O 交給呼叫端（見 scripts/compose-latest-json.mjs）。
 */

/**
 * @param {unknown} baseline 現行 latest.json 的內容（首發或抓不到就傳 null / undefined / {}）
 * @param {{ version: string, pub_date: string, platforms: Record<string, { signature: string, url: string }> }} current
 *   這次發佈實際建置出來的內容，platforms 只包含「這次有建置」的平台 key
 * @returns {{ version: string, pub_date: string, platforms: Record<string, { signature: string, url: string }> }}
 */
export function mergeLatestJson(baseline, current) {
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
    if (!entry || typeof entry !== "object" || !entry.signature || !entry.url) {
      throw new Error(`current.platforms["${key}"] 必須是含 signature 與 url 的物件`);
    }
  }

  const basePlatforms =
    baseline &&
    typeof baseline === "object" &&
    baseline.platforms &&
    typeof baseline.platforms === "object"
      ? baseline.platforms
      : {};

  return {
    version: current.version,
    pub_date: current.pub_date,
    platforms: {
      ...basePlatforms,
      ...currentPlatforms,
    },
  };
}
