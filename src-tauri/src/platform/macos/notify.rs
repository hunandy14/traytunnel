//! 系統通知。
//!
//! macOS 走 `notify_rust`——不是直接呼叫官方 `tauri-plugin-notification` 的
//! Rust API（`NotificationExt`），是刻意的：那個 API 是 `tauri::Manager` 的擴充
//! 方法，要拿 `AppHandle`／`App` 才叫得到 `app.notification().builder()...`；
//! 但 platform 介面的 `show_notification`／`prepare_notifications` 是自由函式
//! （`aumid: &str, title: &str, body: &str`），跟 Windows 那邊直接組
//! `tauri_winrt_notification::Toast` 是同一種簽章，也是 `lib.rs::balloon` 已經在用
//! 的形狀——不能為了 macOS 一個平台去動共用介面或 Windows 那半邊（那條路徑
//! 「一字不動」）。
//!
//! 往下挖一層就會發現這其實不是繞過官方外掛，而是走它自己的骨幹：
//! `tauri-plugin-notification` 2.x 的桌面後端（`desktop.rs`）在 macOS 上就是組一顆
//! `notify_rust::Notification`，`show()` 前呼叫 `notify_rust::set_application` 掛名；
//! 它的 `request_permission()`／`permission_state()` 在桌面平台則是硬編碼永遠回
//! `Granted`（沒有實際跟系統要授權這回事）。這裡直接依賴同一顆 `notify_rust`
//! （已經是既有相依樹裡的版本，見 Cargo.toml 註解），效果與外掛完全一致，
//! 只是不必經過那層需要 AppHandle 的 Manager 綁定。

use std::path::Path;

/// 彈一顆系統通知。失敗只記一行警告——通知從來不是關鍵路徑，
/// 與 Windows `show_notification` 的失敗處理原則一致。
pub fn show_notification(aumid: &str, title: &str, body: &str) {
    if let Err(e) = notify_rust::set_application(aumid) {
        log::warn!("could not set the notification application id: {e}");
    }
    if let Err(e) = notify_rust::Notification::new().summary(title).body(body).show() {
        log::warn!("failed to show notification: {e}");
    }
}

/// 通知掛名的自註冊：把行程掛到 `aumid`（即 `tauri.conf.json` 的 identifier）名下，
/// 之後每一顆通知才會顯示成 Traytunnel 而不是 Terminal／cargo。
///
/// 與 Windows 的 AUMID 三件事（`SetCurrentProcessExplicitAppUserModelID`、開始選單
/// 捷徑、HKCU 顯示名稱與圖示）不對稱：macOS 沒有等價的「開始選單捷徑」與登錄機碼，
/// 應用身分與圖示都直接來自 `.app` bundle 的 `CFBundleIdentifier`／`CFBundleIconFile`，
/// `set_application` 是唯一需要在建立任何 UI 之前做的事。`product`／`exe` 目前用不到
/// （Windows 那邊要組捷徑與登錄機碼才需要），簽章維持跟 Windows 一致方便呼叫端共用。
pub fn prepare_notifications(aumid: &str, _product: &str, _exe: &Path) -> Vec<String> {
    match notify_rust::set_application(aumid) {
        Ok(()) => Vec::new(),
        Err(e) => vec![format!("could not set the notification application id: {e}")],
    }
}
