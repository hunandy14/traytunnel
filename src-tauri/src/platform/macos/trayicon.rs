//! 系統匣圖示的 macOS 側：**圖與 template 旗標一起決定**。
//!
//! 這個模組存在的理由就是那個旗標。舊做法把兩件事拆在 `lib.rs::build_tray` 的
//! 兩段 `cfg` 裡各自決定——一段挑圖（template PNG 失敗就退回彩色 ICO），另一段
//! 無條件 `icon_as_template(true)`——於是 template PNG 解不開時會拿彩色 ICO 去
//! 套 template：AppKit 只看 alpha 通道重畫剪影，顏色整個被丟掉，系統匣上出現的
//! 是一團走樣的黑影。回傳 `(Image, bool)` 之後這個分岔不可能再發生，旗標的值就是
//! 「這張圖到底是不是 template」的事實。
//!
//! Windows 側的同名函式在 `platform::windows::trayicon`，旗標恆 `false`。

use tauri::image::Image;
use tauri::AppHandle;

/// macOS 系統匣要的 template image：純黑＋透明的剪影，系統依明暗模式（與選取
/// 狀態）自動套色，不像 Windows 版直接吃自己的顏色。這份 PNG 不是從 `icon.ico`
/// 挑層來的——彩色圖硬套 template 只會依 alpha 通道畫出走樣的剪影——而是
/// `assets/gen-tray-template.py` 另外算的一份：22×22pt、Retina 2x＝44×44px，
/// 盾形當實心剪影、中央的「通道」環挖成真的透明洞、洞裡留一顆實心節點。
const TRAY_TEMPLATE_PNG: &[u8] = include_bytes!("../../../icons/tray-template.png");

/// 解出 template 圖。失敗回 `None`，由 [`tray_icon`] 決定怎麼退。
pub(crate) fn tray_icon_template() -> Option<Image<'static>> {
    match Image::from_bytes(TRAY_TEMPLATE_PNG) {
        Ok(img) => Some(img),
        Err(e) => {
            log::warn!("could not decode the macOS tray template icon: {e}");
            None
        }
    }
}

/// 系統匣圖示與它的 template 旗標。
///
/// 順序是 template PNG →（解不開就）內嵌 ICO 挑層 →（再挑不到就）codegen 內建
/// 圖示。只有第一條回得了 `true`：後兩條都是彩色圖，套上 `icon_as_template`
/// 會走樣，所以旗標跟著圖一起回，呼叫端不必也不能自己猜。
///
/// 全部落空時回 `None`，呼叫端（`lib.rs::build_tray`）寧可先把系統匣建起來也
/// 不要讓整支程式 panic，圖示之後照樣可以補。
pub fn tray_icon(app: &AppHandle) -> Option<(Image<'static>, bool)> {
    if let Some(img) = tray_icon_template() {
        return Some((img, true));
    }
    crate::appicon::tray_icon()
        .or_else(|| app.default_window_icon().cloned().map(Image::to_owned))
        .map(|img| (img, false))
}

/// 通知裡「怎麼把視窗叫回來」那半句。
///
/// **與 `lib.rs::build_tray` 的點擊政策綁在一起**：D4 決議 macOS 上左右鍵一律
/// 開選單（`show_menu_on_left_click(true)`），沒有雙擊語意，所以這句只能指向
/// 選單裡的「Open window」項（`traymenu::ID_OPEN` 的標籤）。哪天點擊政策改了，
/// 這個常數要跟著改，否則通知會教使用者做一個做不到的手勢。
///
/// 只給句子的前半（動作），尾巴的「to reopen.」／「to open.」由呼叫端接上——
/// 兩種語境共用同一份手勢描述，不必維護兩個平行常數。
pub const TRAY_OPEN_GESTURE_HINT: &str = "Choose \"Open window\" from the tray icon's menu";

#[cfg(test)]
mod tests {
    use super::*;

    /// template PNG 要解得開，而且是張正方形、真的帶透明像素的圖——
    /// 全不透明的話就不是剪影，套上 `icon_as_template` 只會變成一塊實心色塊。
    /// （原本掛在 `crate::appicon` 的同名測試，跟著 cfg 一起搬進平台層。）
    #[test]
    fn tray_template_decodes_with_transparency() {
        let img = tray_icon_template().expect("template PNG 要解得開");
        assert_eq!(img.width(), img.height(), "template 圖應為正方");
        assert!(img.width() >= 32, "解析度太低，Retina 下會模糊：{}", img.width());
        let rgba = img.rgba();
        assert_eq!(rgba.len(), (img.width() * img.height() * 4) as usize);
        let (pixels, _) = rgba.as_chunks::<4>();
        assert!(pixels.iter().any(|px| px[3] < 250), "整張圖都不透明，不像是剪影");
        assert!(pixels.iter().any(|px| px[3] > 5), "整張圖都透明，畫不出東西");
    }
}
