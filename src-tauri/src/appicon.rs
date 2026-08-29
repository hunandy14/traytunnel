//! 應用程式圖示：多層 ICO 的挑層與解碼（Windows），以及 template PNG 的解碼
//! （macOS）。
//!
//! 系統匣與主視窗要的像素尺寸不同（SM_CXSMICON 與 SM_CXICON），高 DPI 下又會
//! 各自變大，因此兩邊都自己挑層而不是交給 Tauri codegen 的預設圖示。macOS 的
//! 系統匣圖示走完全不同的機制（template image，見 [`tray_icon_template`]），
//! 不吃這裡的 ICO 挑層，但仍共用同一個模組，方便對照兩邊的職責。

use std::io::Cursor;

use tauri::image::Image;

use crate::platform;

/// 多層 ICO 直接內嵌，才不必依賴磁碟上有沒有圖示檔。
/// 系統匣與視窗圖示都從這一顆挑層，assets/gen-tray-icons.py 產生。
const APP_ICO: &[u8] = include_bytes!("../icons/icon.ico");

/// 從一組尺寸裡挑最接近 `want` 的一層，回傳索引。
///
/// 優先完全相符（完全不縮放）；沒有就取「大於它的最小一層」，讓系統縮小而不是
/// 放大（縮小遠比放大乾淨）；再沒有就退而取最大的一層。
///
/// 純數字邏輯，不靠任何系統 API，因此兩個平台原本各自維護一份逐字相同的實作
/// （`platform::windows::winsys`／`platform::macos::sys`），現在只在這裡留一份
/// ——這裡是唯一的呼叫端（[`ico_layer`]），不必比照 `platform/mod.rs` 那份「兩邊
/// 各自實作」的介面清單。
fn pick_icon_layer(sizes: &[u32], want: u32) -> Option<usize> {
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

/// 從內嵌的多層 ICO 裡挑最接近 `want` 的一層，解成 RGBA。
///
/// Tauri codegen 的 `default_window_icon()` 只取 ICO 的第一層固定尺寸（我們的第一層
/// 是 16px），高 DPI 時交給 GDI 拉伸就會模糊（tauri#14596、#9335），所以系統匣與主
/// 視窗都自己挑層再明確設定。
fn ico_layer(want: u32, purpose: &str) -> Option<Image<'static>> {
    let dir = ico::IconDir::read(Cursor::new(APP_ICO)).ok()?;
    let sizes: Vec<u32> = dir.entries().iter().map(|e| e.width()).collect();
    let idx = pick_icon_layer(&sizes, want)?;
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

/// macOS 系統匣要的 template image：純黑＋透明的剪影，系統依明暗模式（與選取狀態）
/// 自動套色，不像 Windows 版直接吃自己的顏色。這份 PNG 不是從 `icon.ico` 挑層來
/// 的——彩色圖硬套 template 只會依 alpha 通道畫出走樣的剪影——而是
/// `assets/gen-tray-template.py` 另外算的一份：22×22pt、Retina 2x＝44×44px，
/// 盾形當實心剪影、中央的「通道」環挖成真的透明洞、洞裡留一顆實心節點。
///
/// 只在 macOS 編譯：Windows 的系統匣圖示路徑（[`tray_icon`]）完全不動，
/// 兩邊二選一發生在 `lib.rs::build_tray` 的 `cfg` 分支。
#[cfg(target_os = "macos")]
const TRAY_TEMPLATE_PNG: &[u8] = include_bytes!("../icons/tray-template.png");

/// 解出 macOS 的 template 圖。失敗時回 `None`，呼叫端會退回 [`tray_icon`]
/// （再退回 codegen 內建圖示），不會讓系統匣整個起不來。
#[cfg(target_os = "macos")]
pub fn tray_icon_template() -> Option<Image<'static>> {
    match Image::from_bytes(TRAY_TEMPLATE_PNG) {
        Ok(img) => Some(img),
        Err(e) => {
            log::warn!("could not decode the macOS tray template icon: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 圖示工廠產出的層序，測試照著它走（與內嵌的那顆 ICO 同一份）。
    /// 兩平台原本各自維護一份同名常數，這裡合併成一份。
    const LAYERS: [u32; 9] = [16, 20, 24, 28, 32, 48, 64, 128, 256];

    /// 完全相符的層優先。這裡合併了 Windows 版（「GDI 完全不用縮放」）與 macOS
    /// 版兩組原本各自的斷言，兩邊挑的尺寸不完全一樣，斷言本身一字未改。
    #[test]
    fn exact_layer_wins() {
        assert_eq!(pick_icon_layer(&LAYERS, 16), Some(0));
        assert_eq!(pick_icon_layer(&LAYERS, 64), Some(6));
        // 175% DPI 的 28px 有專用層
        assert_eq!(pick_icon_layer(&LAYERS, 28), Some(3));
        assert_eq!(pick_icon_layer(&LAYERS, 32), Some(4));
    }

    /// 視窗大圖示（SM_CXICON）在各 DPI 下都該挑到「不小於它」的層：
    /// 放大會模糊，縮小不會（原 Windows 版 `large_icon_sizes_never_upscale`）
    #[test]
    fn large_icon_sizes_never_upscale() {
        // 100%／125%／150%／175%／200%／250%／300% 的 SM_CXICON
        let ladder = [(32, 32), (40, 48), (48, 48), (56, 64), (64, 64), (80, 128), (96, 128)];
        for (want, expect) in ladder {
            let idx = pick_icon_layer(&LAYERS, want).expect("一定挑得到一層");
            assert_eq!(LAYERS[idx], expect, "{want}px 挑錯層");
        }
    }

    /// 沒有專用層時寧可讓系統縮小，也不要放大。兩平台原本各自的斷言都保留：
    /// macOS 版對著 `LAYERS` 挑，Windows 版另外用一組較小的清單挑。
    #[test]
    fn falls_back_to_the_next_size_up() {
        assert_eq!(pick_icon_layer(&LAYERS, 44), Some(5)); // 44 -> 48
        assert_eq!(pick_icon_layer(&LAYERS, 20), Some(1));

        let sizes = [16, 24, 32];
        assert_eq!(pick_icon_layer(&sizes, 20), Some(1)); // 24 縮到 20
        assert_eq!(pick_icon_layer(&sizes, 28), Some(2)); // 32 縮到 28
    }

    /// 要的比所有層都大時只能拿最大的那層；空清單回 None。
    /// 兩平台原本各自的斷言都保留（空清單那一條兩邊相同，只留一次）。
    #[test]
    fn falls_back_to_the_largest_layer() {
        assert_eq!(pick_icon_layer(&LAYERS, 1024), Some(8));
        assert_eq!(pick_icon_layer(&[16, 32, 24], 64), Some(1));
        assert_eq!(pick_icon_layer(&[], 16), None);
    }

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

    /// 這台機器實際要的兩個尺寸都要挑得到層。
    ///
    /// Windows 問的是系統實際要的圖示尺寸（SM_CXSMICON／SM_CXICON）；macOS 問的是
    /// `small_icon_size`／`large_icon_size` 回的固定目標尺寸（22pt／32pt 的 2x）。
    /// 兩邊都只是純數字挑層＋解 ICO，不靠任何系統 API，因此這條測試原本只掛
    /// `#[cfg(windows)]`（macOS 那三支還是 `todo!()`），現在 macOS 也有真的實作了，
    /// 解除隔離讓兩個平台共用同一組斷言（斷言本身一字未改）。
    #[test]
    fn picks_layers_for_this_machine() {
        let (small, _) = platform::small_icon_size();
        let (large, _) = platform::large_icon_size();
        assert!(small >= 16, "SM_CXSMICON = {small}");
        assert!(large >= small, "SM_CXICON {large} 不該小於 SM_CXSMICON {small}");
        assert!(tray_icon().is_some(), "系統匣挑不到層");
        assert!(window_icon().is_some(), "視窗挑不到層");
    }

    /// macOS 的 template PNG 要解得開，而且是張正方形、真的帶透明像素的圖——
    /// 全不透明的話就不是剪影，套上 `icon_as_template` 只會變成一塊實心色塊
    #[cfg(target_os = "macos")]
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
