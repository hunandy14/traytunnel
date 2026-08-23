/**
 * 嚴格 semver 子集：僅接受 x.y.z（皆為非負整數三段，不支援 pre-release / build metadata，
 * 不可帶 v 前綴或空白）。本專案版本號（package.json、Cargo.toml）全部走這個子集，
 * bump.mjs 與 release.mjs 共用同一份規則與比較邏輯，避免各自實作。
 *
 * 若未來真的需要 pre-release（例如 1.2.3-beta.1），這裡要先擴充比較規則再放寬 SEMVER_RE，
 * 目前故意不支援：帶 pre-release 的輸入一律被 SEMVER_RE 擋掉。
 */

export const SEMVER_RE = /^\d+\.\d+\.\d+$/;

/**
 * 比較兩個嚴格 semver 版本字串，依序比較 major/minor/patch（數字比較，不是字串比較，
 * 所以 0.10.0 > 0.9.0）。回傳負數表示 a < b，0 表示相等，正數表示 a > b。
 * 兩個參數都必須先通過 SEMVER_RE，否則丟例外（呼叫端應該先驗證過再呼叫這個函式）。
 */
export function compareSemver(a, b) {
  if (!SEMVER_RE.test(a)) {
    throw new Error(`不是嚴格 semver（x.y.z，不支援 pre-release）：${a}`);
  }
  if (!SEMVER_RE.test(b)) {
    throw new Error(`不是嚴格 semver（x.y.z，不支援 pre-release）：${b}`);
  }
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < 3; i++) {
    if (pa[i] !== pb[i]) return pa[i] - pb[i];
  }
  return 0;
}
