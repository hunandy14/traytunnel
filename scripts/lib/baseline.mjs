/**
 * 「底稿檔」的讀取層——scripts/compose-latest-json.mjs 與
 * scripts/compose-sha256sums.mjs 共用。
 *
 * 兩支 compose CLI 對「沒有底稿」的預設值不同（前者要 {}，後者要 null），
 * 但「怎麼算沒有底稿」的判定必須一致：沒給路徑、檔案不存在、內容純空白，
 * 三種都是「沒有底稿」。過去這段判定各抄一份，像「BOM 也視為空」這類修法
 * 只會落到其中一邊（REU-5）。
 *
 * 呼叫端有義務區分「確定沒有底稿」與「這次沒抓到」：把網路抖動當成空底稿，
 * 會把另一個平台的條目／checksum 整個抹掉（見 scripts/fetch-baseline.mjs）。
 * 這一層只負責「檔案裡有沒有東西」，不負責那個判斷。
 */

import { existsSync, readFileSync } from "node:fs";

/**
 * @param {string|undefined} path 底稿檔路徑；省略即視為沒有底稿
 * @returns {string|null} 檔案原文；沒給路徑／檔案不存在／內容純空白時回 null
 */
export function readBaselineText(path) {
  if (!path || !existsSync(path)) return null;
  const raw = readFileSync(path, "utf8");
  return raw.trim() === "" ? null : raw;
}
