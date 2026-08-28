//! 系統通知的 stub。
//!
//! W3：macOS 走 `UNUserNotificationCenter`（tauri-plugin-notification 在 macOS
//! 分支就是它），掛名靠 app bundle 的 `CFBundleIdentifier`，沒有 AUMID 這一層，
//! 因此 `prepare_notifications` 在 macOS 上八成是個空清單而不是三件事。

use std::path::Path;

pub fn show_notification(_aumid: &str, _title: &str, _body: &str) {
    todo!("W3: macOS 的系統通知尚未實作")
}

pub fn prepare_notifications(_aumid: &str, _product: &str, _exe: &Path) -> Vec<String> {
    todo!("W3: macOS 的通知掛名尚未實作")
}
