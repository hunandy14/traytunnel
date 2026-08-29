/**
 * SHA256SUMS.txt 的「底稿合併」邏輯——道理和 scripts/lib/latest-json.mjs 的
 * latest.json 底稿合併完全一樣，差別只在合併鍵是「檔名」而不是「platform key」。
 *
 * 背景：release.yml 的 compose job 過去只用「這次建置、下載進 out/ 的檔案」
 * 重新生成 SHA256SUMS.txt。softprops/action-gh-release 上傳資產時預設
 * overwrite——單平台補腿（例如 v0.6.6 已經發過 Windows，這次只補 macOS）時，
 * 這樣生出來的 SHA256SUMS.txt 只有 macOS 兩行，一旦上傳蓋掉舊檔，Windows
 * 那三個 .exe 的 checksum 就從 release 資產裡永久消失——即使 .exe 本身還在
 * release 上，也沒有任何 checksum 可以核對它們。這是致命 bug：使用者拿到的
 * release 頁面看起來完全正常，卻悄悄少了一半資產的完整性驗證依據。
 *
 * 作法與 latest.json 一致：發佈前先抓現行 SHA256SUMS.txt 當底稿，只用這次
 * 建置出來的檔案覆寫同名的行，其餘行原樣保留。
 *
 * 跟 latest.json 的底稿抓取不同的一點：這裡的底稿網址是「這次要發佈的 tag」
 * 本身（releases/download/<tag>/SHA256SUMS.txt），不是 releases/latest 那個
 * 浮動指標。SHA256SUMS.txt 沒有 latest.json 那種「manifest 只有一個全域
 * version 欄位」的先天限制，不需要靠 releases/latest 對齊 updater endpoint；
 * 用浮動指標反而會在「要補腿的不是目前最新一個 release」時抓錯底稿（例如
 * 補一個較舊 tag 的腿，此時 releases/latest 指向的是更新的另一個 tag）。
 * tag 對應的 release 還不存在時（首發，或這次是全新版本尚未發過任何一腿）
 * GET 回 404，視為空底稿——這三種情境（首發 / 補腿 / 全平台重發）的邊界
 * 都在呼叫端（scripts/compose-sha256sums.mjs 與 release.yml）處理，這裡只管
 * 純合併。
 *
 * 純函式，不碰檔案系統／網路——I/O 交給呼叫端。
 */

/** GNU coreutils sha256sum 的輸出格式：<64 hex hash><一個空白><空白或星號 表示文字/二進位模式><檔名> */
const LINE_RE = /^([0-9a-f]{64})[ \t]+[* ]?(.+)$/i;

/**
 * 解析 SHA256SUMS.txt 內容成 { 檔名: 小寫 hash } 的物件。
 * 空白行忽略；無法辨識的行一律丟錯——底稿如果已經壞掉，寧可讓 CI 紅，也不要
 * 靜默丟掉裡面看不懂的那幾行（那幾行往往正是要保留的另一平台 checksum）。
 *
 * @param {string | null | undefined} text
 * @returns {Record<string, string>}
 */
export function parseSha256Sums(text) {
  const map = {};
  if (!text) return map;
  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line) continue;
    const m = line.match(LINE_RE);
    if (!m) {
      throw new Error(`SHA256SUMS 底稿有一行看不懂格式，無法解析：${JSON.stringify(rawLine)}`);
    }
    const [, hash, filename] = m;
    map[filename] = hash.toLowerCase();
  }
  return map;
}

/**
 * 把 { 檔名: hash } 的物件格式化成 SHA256SUMS.txt 內容（依檔名排序，行序穩定，
 * 方便 diff／人工核對），格式對齊 GNU sha256sum 的文字模式輸出（兩個空白）。
 *
 * @param {Record<string, string>} map
 * @returns {string}
 */
export function formatSha256Sums(map) {
  return Object.keys(map)
    .sort()
    .map((filename) => `${map[filename]}  ${filename}\n`)
    .join("");
}

/**
 * 合併現行 SHA256SUMS.txt 底稿與這次建置出來的檔案雜湊。
 *
 * @param {string | null | undefined} baselineText 現行 SHA256SUMS.txt 的內容
 *   （首發、或該 release tag 底下確定沒有這個資產，就傳 null / undefined / 空字串）
 * @param {Record<string, string>} currentMap 這次建置實際產出的「檔名 → SHA256 hash」，
 *   只包含這次真的建置出來、要發佈的檔案（不是整個底稿的疊加）
 * @returns {Record<string, string>} 合併後的「檔名 → hash」，這次建置的檔名覆寫
 *   底稿同名行，底稿其餘檔名原樣保留
 */
export function mergeSha256Sums(baselineText, currentMap) {
  if (!currentMap || typeof currentMap !== "object" || Object.keys(currentMap).length === 0) {
    throw new Error("currentMap 必須至少有一筆，這次發佈總得建置出點什麼二進位檔");
  }
  for (const [filename, hash] of Object.entries(currentMap)) {
    if (typeof hash !== "string" || !/^[0-9a-f]{64}$/i.test(hash)) {
      throw new Error(`currentMap["${filename}"] 不是合法的 SHA256 雜湊（要 64 碼十六進位）：${hash}`);
    }
  }

  const baselineMap = parseSha256Sums(baselineText);

  return {
    ...baselineMap,
    ...currentMap,
  };
}
