//! 設定檔落腳處要問系統的那兩件事。兩支都是從 `config.rs` 原樣搬出來的，
//! 路徑優先序的邏輯（`resolve_location`）仍在 config，維持純函式、維持可測。

use std::path::PathBuf;

/// 使用者家目錄；空字串視同沒有
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// 執行檔主檔名裡的可攜記號：沿用 Rufus 的 `rufus-4.5p.exe` 慣例，記號是**結尾**的 p。
///
/// 只認結尾而不是任意位置，否則 Windows 複製檔案自動取的
/// 「traytunnel - Copy.exe」（Copy 裡有 p）會莫名其妙變成可攜模式。
/// `traytunnel` 本身不是 p 結尾，所以結尾的 p 一定是使用者刻意加的，
/// 例如 `traytunnel-p.exe`、`traytunnel-0.2.0p.exe`。大小寫不敏感。
pub fn stem_marks_portable(exe_stem: &str) -> bool {
    matches!(exe_stem.chars().next_back(), Some(c) if c.eq_ignore_ascii_case(&'p'))
}

/// 可攜模式的第二個觸發條件：執行檔旁邊已經有 `traytunnel.toml`。
///
/// Windows 上單純看檔案存不存在；`config::resolve_location` 傳進來的 `path` 就是
/// `exe_dir.join(TOML_NAME)`，這裡不重新拼路徑。行為原樣照舊（W3 只是把判斷收進
/// platform，沒有改邏輯）——macOS 那邊這一支恆回 false（見同名函式的 macOS 版）。
pub fn exe_toml_marks_portable(path: &std::path::Path) -> bool {
    path.is_file()
}
