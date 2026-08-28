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

/// W3：macOS 的系統匣圖示是 template image，尺寸規則與 Windows 的
/// `SM_CXSMICON` 完全不同（點數固定、由 backing scale 決定像素）。
pub fn small_icon_size() -> (u32, u32) {
    todo!("W3: macOS 的系統匣圖示尺寸尚未決定")
}

/// W3：macOS 沒有「視窗大圖示」這個概念（Dock 圖示走 app bundle 的 icns）。
pub fn large_icon_size() -> (u32, u32) {
    todo!("W3: macOS 的視窗圖示尺寸尚未決定")
}

/// W3：挑層規則本身是純邏輯，但要挑的是哪一種資源（ICO 層還是 icns）
/// 得等 macOS 的圖示方案定案。
pub fn pick_icon_layer(_sizes: &[u32], _want: u32) -> Option<usize> {
    todo!("W3: macOS 的圖示挑層尚未實作")
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
