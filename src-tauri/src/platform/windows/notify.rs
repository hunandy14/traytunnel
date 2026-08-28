//! 系統通知。掛名（AUMID）的自註冊在 [`super::aumid`]，這裡只有「彈一顆」
//! 與「開機前把掛名準備好」兩個對外入口。
//!
//! 彈通知那段原本在 `lib.rs::balloon`：直接組 Toast 而不是走
//! tauri-plugin-notification 的 builder，因為那個 builder 在 Windows 分支沒接圖示
//! （icon()/attachment() 都到不了 notify-rust 底層），要讓 toast 內文左側出現大 logo
//! （appLogoOverride）只有 `Toast::icon()` 這條路。

use std::path::Path;

use tauri_winrt_notification::{IconCrop, Toast};

/// 彈一顆系統通知。失敗只記一行警告——通知從來不是關鍵路徑。
pub fn show_notification(aumid: &str, title: &str, body: &str) {
    let mut toast = Toast::new(aumid).title(title).text1(body);
    if let Some(icon) = super::aumid::icon_file_path(aumid) {
        toast = toast.icon(&icon, IconCrop::Square, title);
    }
    if let Err(e) = toast.show() {
        log::warn!("failed to show toast notification: {e}");
    }
}

/// 通知掛名的自註冊，回傳要補進活動日誌的行。必須在建立任何 UI 之前跑完。
pub fn prepare_notifications(aumid: &str, product: &str, exe: &Path) -> Vec<String> {
    super::aumid::prepare(aumid, product, exe)
}
