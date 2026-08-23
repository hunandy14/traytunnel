/**
 * 跨檔共用的小工具：純字串處理，不碰 DOM、不碰 IPC。
 *
 * 這裡放的都是「同一條規則被兩個以上的呼叫端需要」的東西——真後端、假後端與
 * 表單三邊各寫一份的話，改規則時漏改一處不會編譯失敗，只會在執行期出現
 * 前後端說法不一致的驗證訊息。
 */

/** 取路徑的最後一段（Windows 反斜線與 POSIX 斜線都吃），切不出東西就退回原字串 */
export function basename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/**
 * 連線名稱的三條規則，ssh 源與 wg 連線共用同一個命名空間所以共用同一套檢查，
 * 與 Rust 端的 valid_source_name 對齊：不可空白、不可含空白字元、不可含中括號
 * （日誌行前綴是 `[連線名]`，名字裡再冒出一個 `]` 會讓前端切不出正確的名字）。
 *
 * 回傳 null 代表通過；回傳的訊息不帶 `name: ` 前綴，要逐欄顯示的呼叫端自己加。
 */
export function validateConnName(name: string): string | null {
  if (!name) return "name is required";
  if (/\s/.test(name)) return "must not contain spaces";
  if (/[[\]]/.test(name)) return "must not contain brackets";
  return null;
}
