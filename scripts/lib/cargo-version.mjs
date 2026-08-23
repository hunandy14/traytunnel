/**
 * 讀取 src-tauri/Cargo.toml 的 [package] version（單一權威版本來源——見 bump.mjs 開頭註解）。
 *
 * bump.mjs（改版號）與 release.mjs（發版前置檢查）共用同一份解析邏輯，避免各自實作、
 * 日後改格式要改兩處又容易漏掉一處。
 */

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
