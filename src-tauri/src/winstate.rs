//! 主視窗位置／大小記憶：tauri-plugin-window-state 的旗標選擇，
//! 以及外掛沒做但自己得補的一層防護。
//!
//! 外掛本身只在 `RunEvent::Exit` 落地寫檔（見它的 `on_event`），CloseRequested
//! 只更新記憶體快取，因此與我們 prevent_close＋hide_to_tray 的視窗事件處理並不衝突：
//! 一般關到系統匣不會誤觸落地，真正退出（`app.exit(0)`）時 Exit 事件照樣會發。
//!
//! 就地更新那條路是例外——`tauri-plugin-updater` 在 Windows 上裝完新版是直接
//! `std::process::exit(0)`，繞過 `RunEvent::Exit`，外掛的存檔 hook 因此不會跑。
//! 這個落差在 `update::install` 裡用 `updater_builder().on_before_exit(..)` 補：
//! 交棒給安裝程式之前自己呼叫一次 `save_window_state`。

use tauri::{PhysicalSize, Runtime, WebviewWindow};
use tauri_plugin_window_state::StateFlags;

/// 還原／記錄時要用的旗標：只有位置、尺寸、最大化，刻意不含 `VISIBLE`。
///
/// 外掛看到 `VISIBLE` 才會在還原完之後自己 `show()`／`set_focus()`；主視窗的顯示
/// 時機（一般啟動走 `show_main`、`--tray` 啟動維持隱藏）全部由我們自己的邏輯掌控，
/// 不能讓外掛在 `on_window_ready` 階段搶先動了顯示狀態。
pub fn flags() -> StateFlags {
    StateFlags::POSITION | StateFlags::SIZE | StateFlags::MAXIMIZED
}

/// 與 tauri.conf.json 主視窗的 minWidth/minHeight 同一份數字，改動要兩邊一起改
const MIN_WIDTH: u32 = 480;
const MIN_HEIGHT: u32 = 420;

/// 把還原尺寸夾在「不低於最小可用尺寸、不超過目前工作區」之間。
///
/// 外掛的 `WindowExt::restore_state` 對 POSITION 有做保護（存的矩形不與任何螢幕
/// 相交就跳過 `set_position`，讓 Tauri 的預設 `center` 生效），但 SIZE 完全沒有
/// 對應的處理——舊設定檔存的尺寸如果比現在的螢幕大（例如原本接 4K 螢幕、換到
/// 筆電內建小螢幕），視窗會被原封不動還原成超出工作區的大小。
///
/// 純函式，方便寫測試。工作區本身若比最小尺寸還小（罕見的小螢幕），保底維持
/// 最小尺寸——寧可讓視窗比工作區大一點，也不要縮到 UI 放不下。
pub fn clamp_restored_size(
    saved: (u32, u32),
    work_area: (u32, u32),
    min: (u32, u32),
) -> (u32, u32) {
    let clamp_dim = |v: u32, lo: u32, hi: u32| if hi < lo { lo } else { v.clamp(lo, hi) };
    (clamp_dim(saved.0, min.0, work_area.0), clamp_dim(saved.1, min.1, work_area.1))
}

/// setup 階段呼叫：外掛在 `on_window_ready`（早於我們的 `.setup` 閉包執行）已經
/// 把 SIZE 還原完了，這裡把還原後的尺寸依目前螢幕再校正一次。
///
/// 讀不到目前尺寸或螢幕資訊時什麼都不做——校正是錦上添花，不能讓它自己變成
/// 新的失敗點；讀得到但沒超界也不動，避免無謂觸發一次 Resized 事件。
pub fn correct_restored_size<R: Runtime>(win: &WebviewWindow<R>) {
    let Ok(size) = win.inner_size() else { return };
    let Ok(Some(monitor)) = win.current_monitor() else { return };
    let work = monitor.work_area().size;
    let (w, h) = clamp_restored_size(
        (size.width, size.height),
        (work.width, work.height),
        (MIN_WIDTH, MIN_HEIGHT),
    );
    if w != size.width || h != size.height {
        let _ = win.set_size(PhysicalSize::new(w, h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 存的尺寸就在工作區內：原封不動
    #[test]
    fn fits_within_the_work_area_is_left_alone() {
        assert_eq!(clamp_restored_size((800, 600), (1920, 1080), (480, 420)), (800, 600));
    }

    /// 換到比較小的螢幕（例如筆電內建 1366x768），舊設定存的是 4K 尺寸：縮到工作區大小
    #[test]
    fn oversized_saved_state_is_shrunk_to_the_work_area() {
        assert_eq!(clamp_restored_size((3840, 2160), (1366, 768), (480, 420)), (1366, 768));
    }

    /// 存的尺寸比最小可用尺寸還小：頂到最小值，不讓 UI 擠壞
    #[test]
    fn undersized_saved_state_is_never_shrunk_below_the_minimum() {
        assert_eq!(clamp_restored_size((300, 200), (1920, 1080), (480, 420)), (480, 420));
    }

    /// 極端小螢幕：工作區本身比最小尺寸還小，寧可讓視窗比工作區大也要保底最小尺寸
    #[test]
    fn a_work_area_smaller_than_the_minimum_still_keeps_the_minimum() {
        assert_eq!(clamp_restored_size((800, 600), (320, 240), (480, 420)), (480, 420));
    }

    /// 邊界值本身要保持不變，不能因為 clamp 的實作差一格
    #[test]
    fn exact_boundaries_are_kept_as_is() {
        assert_eq!(clamp_restored_size((480, 420), (1920, 1080), (480, 420)), (480, 420));
        assert_eq!(clamp_restored_size((1920, 1080), (1920, 1080), (480, 420)), (1920, 1080));
    }

    /// 寬高各自獨立夾範圍：只有其中一邊超界也要正確處理，不能互相影響
    #[test]
    fn width_and_height_are_clamped_independently() {
        assert_eq!(clamp_restored_size((3840, 600), (1920, 1080), (480, 420)), (1920, 600));
        assert_eq!(clamp_restored_size((800, 2160), (1920, 1080), (480, 420)), (800, 1080));
    }
}
