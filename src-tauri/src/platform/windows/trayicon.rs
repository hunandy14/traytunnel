//! 系統匣圖示的 Windows 側。
//!
//! Windows 沒有「template image」這回事——系統匣圖示直接吃自己的顏色，不會被
//! 系統依明暗模式重畫，所以旗標恆 `false`。回傳形狀與 macOS 側同名同簽章
//! （見 `platform::macos::trayicon` 模組開頭：旗標與圖一起回，是為了讓
//! 「拿到的圖到底是不是 template」不可能再跟 `icon_as_template` 的呼叫分岔）。

use tauri::image::Image;
use tauri::AppHandle;

/// 系統匣圖示與它的 template 旗標（Windows 恆 `false`）。
///
/// 圖從內嵌的多層 ICO 挑最接近 `SM_CXSMICON` 的一層（`crate::appicon`，
/// 一字不動的既有行為）；挑不到就退回 codegen 內建的圖示；再落空就回 `None`，
/// 由呼叫端決定「沒有圖也要先把系統匣建起來」。
pub fn tray_icon(app: &AppHandle) -> Option<(Image<'static>, bool)> {
    crate::appicon::tray_icon()
        .or_else(|| app.default_window_icon().cloned().map(Image::to_owned))
        .map(|img| (img, false))
}

/// 通知裡「怎麼把視窗叫回來」那半句。
///
/// **與 `lib.rs::build_tray` 的點擊政策綁在一起**：Windows 維持既有語意
/// （`show_menu_on_left_click(false)` ＋ 左鍵雙擊開主視窗），所以這句講的是
/// 雙擊。哪天點擊政策改了，這個常數要跟著改，否則通知會教使用者做一個做不到的
/// 手勢。
///
/// 只給句子的前半（動作），尾巴的「to reopen.」／「to open.」由呼叫端接上——
/// 兩種語境共用同一份手勢描述，不必維護兩個平行常數。
pub const TRAY_OPEN_GESTURE_HINT: &str = "Double-click the tray icon";
