//! 應用內更新的 stub。
//!
//! W3：Windows 那條路整個綁死在 NSIS——判定「是不是安裝版」讀的是 HKCU 的
//! 解除安裝機碼，暫存的是一顆 `setup.exe`，交棒是把它 spawn 起來然後自己 `exit(0)`。
//! macOS 對應的是 `.app` bundle 的整包替換（tauri-plugin-updater 在 macOS 上收的是
//! `.tar.gz`／`.zip`，而 Cargo.toml 目前把 `zip` 特性關掉了，W3 要一起處理），
//! 「另一個實例在不在跑」也沒有具名互斥鎖可探。
//!
//! 因此整包重寫，不是補幾個 `cfg` 就能收工的東西。

use crate::state::UpdateInfo;
use crate::Shared;

/// 已經下載完、等下一次啟動才安裝的那一版。
///
/// macOS 上還沒有任何東西會產生它——欄位只留共用核心真的會讀的那一個
/// （`AppState::staged_version`），W3 實作時照 macOS 自己的暫存格式重寫。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub version: String,
}

/// 啟動最早期的交棒判定。回傳要補進活動日誌的行。
pub fn apply_pending_at_startup(_tray: bool) -> Vec<String> {
    todo!("W3: macOS 的更新交棒尚未實作")
}

/// 把暫存區裡那份就緒的更新認回狀態。
pub fn restore_staged(_st: &Shared) {
    todo!("W3: macOS 的更新暫存區尚未實作")
}

/// 背景檢查的排程。
pub fn spawn_checker(_st: &Shared) {
    todo!("W3: macOS 的更新檢查尚未實作")
}

/// 使用者剛把「Automatic updates」打開時立刻查一次。
pub fn check_now(_st: &Shared) {
    todo!("W3: macOS 的更新檢查尚未實作")
}

/// 使用者主動按下的檢查，結果要回傳給按鈕呈現瞬態。
pub async fn check_manually(_st: &Shared) -> Result<Option<UpdateInfo>, String> {
    Err("checking for updates is not implemented on macOS yet".into())
}

/// 關掉自動更新時把暫存區清乾淨。
pub fn discard_staged(_st: &Shared) {
    todo!("W3: macOS 的更新暫存區尚未實作")
}

/// 套用已經下載好的那一份（系統匣的「Restart to update」）。
pub fn apply_now(_st: &Shared) -> Result<(), String> {
    Err("applying a staged update is not implemented on macOS yet".into())
}

/// 設定頁那顆主鈕：沒下載就下載，下載好了就交棒。
pub async fn install(_st: &Shared) -> Result<(), String> {
    Err("installing an update is not implemented on macOS yet".into())
}

/// 某一版的 release 頁。
pub fn open_release_page(_st: &Shared, _version: Option<&str>) {
    todo!("W3: macOS 的開瀏覽器尚未實作")
}

/// Releases 列表頁。
pub fn open_releases_page(_st: &Shared) {
    todo!("W3: macOS 的開瀏覽器尚未實作")
}
