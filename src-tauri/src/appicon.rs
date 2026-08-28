//! 應用程式圖示：多層 ICO 的挑層與解碼。
//!
//! 系統匣與主視窗要的像素尺寸不同（SM_CXSMICON 與 SM_CXICON），高 DPI 下又會
//! 各自變大，因此兩邊都自己挑層而不是交給 Tauri codegen 的預設圖示。

use std::io::Cursor;

use tauri::image::Image;

use crate::platform;

/// 多層 ICO 直接內嵌，才不必依賴磁碟上有沒有圖示檔。
/// 系統匣與視窗圖示都從這一顆挑層，assets/gen-tray-icons.py 產生。
const APP_ICO: &[u8] = include_bytes!("../icons/icon.ico");

/// 從內嵌的多層 ICO 裡挑最接近 `want` 的一層，解成 RGBA。
///
/// Tauri codegen 的 `default_window_icon()` 只取 ICO 的第一層固定尺寸（我們的第一層
/// 是 16px），高 DPI 時交給 GDI 拉伸就會模糊（tauri#14596、#9335），所以系統匣與主
/// 視窗都自己挑層再明確設定。
fn ico_layer(want: u32, purpose: &str) -> Option<Image<'static>> {
    let dir = ico::IconDir::read(Cursor::new(APP_ICO)).ok()?;
    let sizes: Vec<u32> = dir.entries().iter().map(|e| e.width()).collect();
    let idx = platform::pick_icon_layer(&sizes, want)?;
    let img = dir.entries()[idx].decode().ok()?;
    log::info!("{purpose} icon: system wants {want}px, using the {}px layer", img.width());
    Some(Image::new_owned(img.rgba_data().to_vec(), img.width(), img.height()))
}

/// 系統匣圖示：照 SM_CXSMICON（100% 是 16、175% 是 28）挑層
pub fn tray_icon() -> Option<Image<'static>> {
    ico_layer(platform::small_icon_size().0, "tray")
}

/// 主視窗圖示：照 SM_CXICON（100% 是 32、175% 是 56）挑層。
/// 這是工作列的視窗按鈕與 Alt+Tab 取的尺寸。
pub fn window_icon() -> Option<Image<'static>> {
    ico_layer(platform::large_icon_size().0, "window")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 內嵌的 ICO 必須含系統匣（SM_CXSMICON）與視窗（SM_CXICON）常用的整數縮放
    /// 尺寸，少了哪一層就會退回讓 GDI 拉伸而模糊。125%／150%／175% 的 40／48／56
    /// 沒有專用層，靠 pick_icon_layer 往上取一層再由系統縮小。
    #[test]
    fn embedded_ico_has_the_icon_layers() {
        let dir = ico::IconDir::read(Cursor::new(APP_ICO)).expect("內嵌的 ICO 要解得開");
        let sizes: Vec<u32> = dir.entries().iter().map(|e| e.width()).collect();
        for want in [16u32, 20, 24, 28, 32, 48, 64, 128, 256] {
            assert!(sizes.contains(&want), "缺 {want}px 層，現有 {sizes:?}");
        }
    }

    /// 每一層都要解得開，而且解出來的點陣圖尺寸與目錄項相符
    #[test]
    fn every_layer_decodes_to_its_declared_size() {
        let dir = ico::IconDir::read(Cursor::new(APP_ICO)).unwrap();
        for entry in dir.entries() {
            let img = entry.decode().expect("每一層都要解得開");
            assert_eq!(img.width(), entry.width());
            assert_eq!(img.height(), entry.height());
            assert_eq!(img.rgba_data().len(), (img.width() * img.height() * 4) as usize);
        }
    }

    /// 這台機器實際要的兩個尺寸都要挑得到層
    #[test]
    fn picks_layers_for_this_machine() {
        let (small, _) = platform::small_icon_size();
        let (large, _) = platform::large_icon_size();
        assert!(small >= 16, "SM_CXSMICON = {small}");
        assert!(large >= small, "SM_CXICON {large} 不該小於 SM_CXSMICON {small}");
        assert!(tray_icon().is_some(), "系統匣挑不到層");
        assert!(window_icon().is_some(), "視窗挑不到層");
    }
}
