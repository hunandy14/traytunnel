//! 設定檔落腳處的 macOS 實作。

use std::path::PathBuf;

/// W3：macOS 的家目錄是 `$HOME`，語意與 Windows 的 `%USERPROFILE%` 對得上，
/// 但「設定檔到底該放家目錄的點檔還是 `~/Library/Application Support`」
/// 還沒裁決，所以先不給答案。
pub fn home_dir() -> Option<PathBuf> {
    todo!("W3: macOS 的設定檔家目錄尚未裁決")
}

/// macOS **不做可攜模式**（W3 決議已定），所以這裡一律 false。
///
/// 這是整包 macOS stub 裡唯一一個「回正常值」的例外：它不是佔位，是決議本身。
/// 理由是可攜模式的整套前提在 macOS 上不成立——程式是包在 `.app` bundle 裡發佈的，
/// 執行檔躺在 `Traytunnel.app/Contents/MacOS/` 底下，「設定檔放執行檔旁邊」等於
/// 寫進 bundle 內部，那會破壞簽章、也會在下一次更新時整包被換掉。
///
/// TODO(W3)：如果之後真的要做，記號不會是檔名尾碼 p，而是另一套設計，
/// 屆時連 `config::resolve_location` 的第二個觸發條件（exe 旁邊已有
/// `traytunnel.toml`）一起重新裁決。
pub fn stem_marks_portable(_exe_stem: &str) -> bool {
    false
}
