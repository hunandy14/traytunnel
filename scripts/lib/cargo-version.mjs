/**
 * 讀取 src-tauri/Cargo.toml 的 [package] version（單一權威版本來源——見 bump.mjs 開頭註解）。
 *
 * bump.mjs（改版號）與 release.mjs（發版前置檢查）共用同一份解析邏輯，避免各自實作、
 * 日後改格式要改兩處又容易漏掉一處。
 *
 * 「manifest 在 repo 的哪個位置」與「讀檔＋解析」也收在這裡（cargoTomlPath／
 * readRepoCargoVersion）：package.mjs／release.mjs／resolve-release.mjs 過去各自
 * 抄一份 join(root,"src-tauri","Cargo.toml") + readFileSync + readCargoVersion，
 * manifest 位置或編碼處理一改就要動好幾處（REU-4）。這三支現在都走
 * readRepoCargoVersion。bump.mjs 是例外：它要拿 Cargo.toml 原文把新版號寫回去，
 * 需要的是那份 text 本身，所以仍然自己讀檔（也仍然自己組路徑——cargoTomlPath
 * 匯出給那類「只要路徑、不要解析結果」的呼叫端用）。
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";

/** 只在 [package] 區塊裡找（到下一個 [section] 或檔尾為止），避免誤傷 dependencies 裡同名的 version */
export function packageSectionRange(path, text) {
  const startMatch = text.match(/^\[package\]\s*$/m);
  if (!startMatch) {
    throw new Error(`${path} 找不到 [package] 區塊`);
  }
  const sectionStart = startMatch.index + startMatch[0].length;
  const rest = text.slice(sectionStart);
  const nextSection = rest.match(/^\[.*\]\s*$/m);
  const sectionEnd = nextSection ? sectionStart + nextSection.index : text.length;
  return { sectionStart, sectionEnd };
}

export function readCargoVersion(path, text) {
  const { sectionStart, sectionEnd } = packageSectionRange(path, text);
  const section = text.slice(sectionStart, sectionEnd);
  const m = section.match(/^version\s*=\s*"([^"]+)"/m);
  if (!m) {
    throw new Error(`${path} 的 [package] 區塊找不到 version`);
  }
  return m[1];
}

/**
 * repo 根目錄底下 Cargo.toml 的位置（單一定義點）
 * @param {string} root repo 根目錄
 * @returns {string}
 */
export function cargoTomlPath(root) {
  return join(root, "src-tauri", "Cargo.toml");
}

/**
 * 讀 repo 的 src-tauri/Cargo.toml 並取出 [package] version
 * @param {string} root repo 根目錄
 * @returns {string}
 */
export function readRepoCargoVersion(root) {
  const path = cargoTomlPath(root);
  return readCargoVersion(path, readFileSync(path, "utf8"));
}
