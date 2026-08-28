//! 設定檔落腳處的 macOS 實作。

use std::path::{Path, PathBuf};

/// macOS 的設定檔家目錄：`$HOME`，語意與 Windows 的 `%USERPROFILE%` 對得上。
///
/// 裁決：家目錄模式的檔案就放 `$HOME/.traytunnel.toml`（點檔），不是
/// `~/Library/Application Support`——後者是給沙盒化、走 App Store 上架的應用
/// 用的容器路徑，traytunnel 不走沙盒，點檔與 Windows 的 `%USERPROFILE%\.traytunnel.toml`
/// 語意一致，使用者也比較容易在 Finder（顯示隱藏檔案）或終端機裡找到它。
/// 空字串視同沒有，與 Windows 那邊的規則一致。
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// macOS **不做可攜模式**（W3 決議已定），所以這裡一律 false。
///
/// 這是整包 macOS stub 裡「回正常值」的例外之一：它不是佔位，是決議本身。
/// 理由是可攜模式的整套前提在 macOS 上不成立——程式是包在 `.app` bundle 裡發佈的，
/// 執行檔躺在 `Traytunnel.app/Contents/MacOS/` 底下，「設定檔放執行檔旁邊」等於
/// 寫進 bundle 內部，那會破壞簽章、也會在下一次更新時整包被換掉。
pub fn stem_marks_portable(_exe_stem: &str) -> bool {
    false
}

/// 可攜模式的第二個觸發條件（exe 旁已有 `traytunnel.toml`）在 macOS 上同樣恆 false，
/// 理由與 [`stem_marks_portable`] 完全一致——不是「還沒做」，是決議本身：
/// 就算有人把一份 `traytunnel.toml` 塞進 `Traytunnel.app/Contents/MacOS/` 旁邊，
/// 程式也不會把它當成生效設定去讀，一律走家目錄的點檔。
pub fn exe_toml_marks_portable(_path: &Path) -> bool {
    false
}
