//! 系統查詢與系統動作的 stub。對照組是 `platform/windows/winsys.rs`。

use std::io;
use std::path::Path;

// ---------------------------------------------------------------- 本地埠偵測

/// W3：Windows 走 `GetExtendedTcpTable`，macOS 這邊要另找一條
/// （libproc 的 `proc_pidfdinfo`，或退而求其次去 parse `lsof -nP -iTCP -sTCP:LISTEN`）。
///
/// 這一支**絕對不可以**先回 `false` 頂著：連線判定與 spawn 前的埠檢查都吃它的答案，
/// 一律回 false 會讓每一條隧道都停在 connecting、而且埠被佔用時照樣硬 spawn。
pub fn is_listening(_port: u16) -> bool {
    todo!("W3: macOS 的本地 listener 偵測尚未實作")
}

// ---------------------------------------------------------------- 時間

/// W3：Windows 是 `GetLocalTime`。macOS 沒有等價的 Win32 呼叫，
/// 要嘛拉一顆時間 crate，要嘛自己走 `libc::localtime_r`。
pub fn local_time_hms() -> String {
    todo!("W3: macOS 的本地時間戳尚未實作")
}

// ---------------------------------------------------------------- 圖示與 DPI

/// 系統匣圖示（NSStatusItem）的挑層目標尺寸。
///
/// macOS 沒有 Windows 那種隨 DPI 變動的 `SM_CXSMICON`：tray-icon 這顆底層 crate
/// 把選單列圖示的顯示高度寫死在 18pt（見其 `platform_impl/macos/mod.rs` 的
/// `icon_height`），跟這裡塞進去的點陣圖實際像素尺寸無關——那個高度是事後用
/// `NSImage::setSize` 定死的，點陣圖的像素數只決定 Retina 下夠不夠清晰。因此只要
/// 給一張解析度夠高的正方形圖即可：Apple 選單列圖示的建議尺寸是 22×22pt，
/// 2x（Retina）算下來是 44×44px，這裡就回這個目標尺寸。
///
/// macOS 系統匣現在的主要圖示是 [`crate::appicon::tray_icon_template`]（另外
/// 一份純黑＋透明的 template PNG，見 `assets/gen-tray-template.py`），不會走這裡的
/// ICO 挑層；這一支純粹是 `appicon::tray_icon()`（template 圖載不到時的退路）要用
/// 的「想要哪個 ICO 層」目標尺寸。
pub fn small_icon_size() -> (u32, u32) {
    (44, 44)
}

/// 「視窗大圖示」在 macOS 沒有 Windows 工作列按鈕那種對應物——Dock 圖示是
/// app bundle 自帶的 `icon.icns`，跟這支函式的回傳值無關。這裡沿用與 Windows 版
/// 同樣的比例關係（大圖示是小圖示的兩倍），單純讓 `appicon::window_icon()`
/// （`win.set_icon` 的來源；即使這個呼叫在 macOS 上是否真的顯示不影響這裡的職責）
/// 挑到一層夠大、不失真的圖。64 剛好是內嵌 ICO 現成的一層，挑得到就不必再讓系統
/// 縮放。
pub fn large_icon_size() -> (u32, u32) {
    (64, 64)
}

/// 從一組尺寸裡挑最接近 `want` 的一層，回傳索引。
///
/// 純數字邏輯，不靠任何系統 API，故意與 Windows 版
/// （`platform::windows::winsys::pick_icon_layer`）用同一套演算法：完全相符優先；
/// 沒有就取「大於它的最小一層」（縮小而不是放大，縮小遠比放大乾淨）；再沒有就
/// 退而取最大的一層。macOS 沒有 per-monitor DPI 挑層的問題，這裡的 `want` 只來自
/// `small_icon_size`／`large_icon_size` 這兩個固定值，不會隨螢幕或執行時狀態變動，
/// 因此不必像 Windows 版那樣另外查詢系統度量。
pub fn pick_icon_layer(sizes: &[u32], want: u32) -> Option<usize> {
    if sizes.is_empty() {
        return None;
    }
    if let Some(exact) = sizes.iter().position(|s| *s == want) {
        return Some(exact);
    }
    let bigger = sizes
        .iter()
        .enumerate()
        .filter(|(_, s)| **s > want)
        .min_by_key(|(_, s)| **s)
        .map(|(i, _)| i);
    bigger.or_else(|| sizes.iter().enumerate().max_by_key(|(_, s)| **s).map(|(i, _)| i))
}

// ---------------------------------------------------------------- 開機自啟

/// W3：macOS 走 `~/Library/LaunchAgents/<label>.plist`
/// （或 `SMAppService`，看最低支援版本怎麼訂）。
pub fn autostart_enabled(_name: &str) -> bool {
    todo!("W3: macOS 的開機自啟尚未實作")
}

/// W3：對應 Windows 讀 HKCU Run 值，macOS 這邊是讀 plist 裡的
/// `ProgramArguments`，回一行給自癒邏輯比對。
pub fn read_autostart_command(_name: &str) -> Option<String> {
    todo!("W3: macOS 的開機自啟尚未實作")
}

/// W3：寫出 LaunchAgent plist 並 `launchctl load`（或 `SMAppService::register`）。
pub fn enable_autostart(_name: &str, _exe: &Path) -> io::Result<()> {
    Err(io::Error::other("autostart is not implemented on macOS yet"))
}

/// W3：`launchctl unload` 並刪掉 plist。
pub fn disable_autostart(_name: &str) -> io::Result<()> {
    Err(io::Error::other("autostart is not implemented on macOS yet"))
}

// ---------------------------------------------------------------- 開外部程式

/// W3：`open -R <path>`（或 `NSWorkspace::activateFileViewerSelecting`）。
///
/// 「開瀏覽器」不在這裡：那件事只有 [`super::update`] 用得到，一併留給它。
pub fn reveal_in_file_manager(_path: &Path) -> io::Result<()> {
    Err(io::Error::other("revealing a file in Finder is not implemented on macOS yet"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 圖示工廠產出的層序，測試照著它走（與 `appicon.rs` 內嵌的那顆 ICO 同一份）
    const LAYERS: [u32; 9] = [16, 20, 24, 28, 32, 48, 64, 128, 256];

    /// 完全相符的層優先
    #[test]
    fn exact_layer_wins() {
        assert_eq!(pick_icon_layer(&LAYERS, 16), Some(0));
        assert_eq!(pick_icon_layer(&LAYERS, 64), Some(6));
    }

    /// 沒有專用層時寧可讓系統縮小，也不要放大
    #[test]
    fn falls_back_to_the_next_size_up() {
        assert_eq!(pick_icon_layer(&LAYERS, 44), Some(5)); // 44 -> 48
        assert_eq!(pick_icon_layer(&LAYERS, 20), Some(1));
    }

    /// 要的比所有層都大時只能拿最大的那層；空清單回 None
    #[test]
    fn falls_back_to_the_largest_layer() {
        assert_eq!(pick_icon_layer(&LAYERS, 1024), Some(8));
        assert_eq!(pick_icon_layer(&[], 16), None);
    }

    /// 這台機器兩種圖示尺寸的合理性：與 Windows 版
    /// `metrics_are_sane_on_this_machine` 同樣的斷言，只是這裡的值是固定常數
    #[test]
    fn metrics_are_sane_on_this_machine() {
        let (sw, sh) = small_icon_size();
        let (lw, lh) = large_icon_size();
        assert_eq!(sw, sh, "小圖示應為正方");
        assert_eq!(lw, lh, "大圖示應為正方");
        assert!(sw >= 16 && lw >= 32, "small={sw} large={lw}");
        assert!(lw >= sw && lh >= sh, "大圖示不該小於小圖示");
    }
}
