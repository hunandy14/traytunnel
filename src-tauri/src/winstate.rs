//! 主視窗位置／大小記憶：tauri-plugin-window-state 的旗標選擇，
//! 以及外掛沒做但自己得補的一層防護。
//!
//! 外掛本身只在 `RunEvent::Exit` 落地寫檔（見它的 `on_event`），CloseRequested
//! 只更新記憶體快取，因此與我們 prevent_close＋hide_to_tray 的視窗事件處理並不衝突：
//! 一般關到系統匣不會誤觸落地，真正退出（`app.exit(0)`）時 Exit 事件照樣會發。
//!
//! 就地更新那條路兩個平台的收尾不一樣，但都不必再多掛一層 hook：
//!
//! * **Windows**：`update::apply_now` 起完 NSIS 安裝程式之後是直接
//!   `std::process::exit(0)`，繞過 `RunEvent::Exit`，外掛的存檔 hook 不會跑
//!   ——那正是「更新後視窗歸零置中」的成因。所以那一支在 spawn 成功之後
//!   **自己呼叫一次** `save_window_state`（見 `platform/windows/update.rs`）。
//! * **macOS**：`update::install` 換完 bundle 走的是 `AppHandle::restart()`，
//!   而它在非主執行緒上是「請事件迴圈正常退出、退完再重新執行自己」，
//!   `RunEvent::Exit` 照發、外掛照存，因此那一條**不需要**補這一手。

use tauri::{PhysicalPosition, PhysicalSize, Runtime, WebviewWindow};
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

/// 視窗要算「還抓得到」，至少要有這麼寬、這麼高落在工作區內。
///
/// 高度取一列標題列的高度：抓得到標題列就搬得動視窗，使用者自己救得回來。
/// 寬度取一段足以按住拖曳的距離——只露出十幾像素的邊，滑鼠實際上點不到。
/// 兩者都刻意訂得保守：這道校正要處理的是「視窗掉到螢幕外面」，
/// 不是替使用者決定視窗該擺哪裡，靠邊放的習慣不該被它動到。
const VISIBLE_MIN_WIDTH: u32 = 120;
const VISIBLE_MIN_HEIGHT: u32 = 32;

/// 視窗矩形，(x, y) 是左上角。位置可以是負的（多螢幕時主螢幕左邊那幾台）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// 一維上兩段區間的重疊長度
fn overlap(a_start: i32, a_len: u32, b_start: i32, b_len: u32) -> u32 {
    let lo = a_start.max(b_start);
    let hi = (a_start + a_len as i32).min(b_start + b_len as i32);
    (hi - lo).max(0) as u32
}

/// 把一維座標拉回可見範圍：放得下就夾在區間內（保住使用者原本的相對位置，
/// 只做最小平移），放不下就靠齊工作區的起點——那樣至少左上角在畫面上，
/// 標題列與視窗控制鈕都抓得到。置中反而會把上緣推到畫面外。
fn pull_into(pos: i32, len: u32, work_start: i32, work_len: u32) -> i32 {
    if len >= work_len {
        return work_start;
    }
    pos.clamp(work_start, work_start + (work_len - len) as i32)
}

/// 還原後的視窗要不要搬位置，要的話搬到哪裡；不必動就回 None。
///
/// 外掛的 `restore_state` 對 POSITION 是有做保護，但它問的是「存起來的位置
/// 落在哪個螢幕上」——只要**一個角**落在某台螢幕的範圍內就算數，而且比的是
/// 螢幕全域而不是工作區。於是這些情形全都通得過它那一關，視窗卻是還原到
/// 使用者搆不著的地方：接了副螢幕時把視窗拖到右下角、拔掉副螢幕再開；
/// 存的時候視窗大半在螢幕外；工作列或全螢幕的工作列位置吃掉了那個角。
/// 結果是標題列整條在畫面外，滑鼠抓不到，只能去改設定檔或用鍵盤搬。
///
/// 純函式，判斷規則靠測試釘住，不必開真的視窗去試。
/// 垂直方向刻意**不**看整片重疊面積，只看視窗頂端那一條有沒有落在工作區裡。
/// 標題列是唯一能把視窗拖回來的把手，它在畫面外就等於救不回來——比螢幕還大的
/// 視窗正是這種情形：看得見的面積大得很，頂端卻在畫面上方外面。
pub fn corrected_position(win: Rect, work: Rect) -> Option<(i32, i32)> {
    // 視窗比門檻還小的時候只要求它自己那麼多，否則永遠滿足不了門檻而每次都被搬
    let need_w = VISIBLE_MIN_WIDTH.min(win.w);
    let bar_h = VISIBLE_MIN_HEIGHT.min(win.h);
    let seen_w = overlap(win.x, win.w, work.x, work.w);
    let seen_bar = overlap(win.y, bar_h, work.y, work.h);
    if seen_w >= need_w && seen_bar >= bar_h {
        return None;
    }
    Some((pull_into(win.x, win.w, work.x, work.w), pull_into(win.y, win.h, work.y, work.h)))
}

/// setup 階段呼叫：外掛在 `on_window_ready`（早於我們的 `.setup` 閉包執行）已經
/// 把 POSITION／SIZE 還原完了，這裡把還原後的幾何依目前螢幕再校正一次。
///
/// 順序是先尺寸後位置，不可以顛倒：尺寸校正會把視窗縮進工作區，縮完之後
/// 與工作區的相交關係就變了，先算位置等於拿一份即將作廢的矩形在算。
///
/// 讀不到目前幾何或螢幕資訊時什麼都不做——校正是錦上添花，不能讓它自己變成
/// 新的失敗點；讀得到但沒超界也不動，避免無謂觸發一次 Resized／Moved 事件。
pub fn correct_restored_geometry<R: Runtime>(win: &WebviewWindow<R>) {
    correct_restored_size(win);
    correct_restored_position(win);
}

fn correct_restored_size<R: Runtime>(win: &WebviewWindow<R>) {
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

/// 位置用的是 outer 幾何：可見與否看的是整個視窗框（含標題列與陰影邊），
/// 不是網頁內容的那一塊。
fn correct_restored_position<R: Runtime>(win: &WebviewWindow<R>) {
    let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) else { return };
    let Ok(Some(monitor)) = win.current_monitor() else { return };
    let area = monitor.work_area();
    let rect = Rect { x: pos.x, y: pos.y, w: size.width, h: size.height };
    let work =
        Rect { x: area.position.x, y: area.position.y, w: area.size.width, h: area.size.height };
    if let Some((x, y)) = corrected_position(rect, work) {
        let _ = win.set_position(PhysicalPosition::new(x, y));
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

    // ---------------------------------------------------------------- 位置校正

    /// 1920x1080、工作列佔掉底下 40px 的一般單螢幕
    fn work() -> Rect {
        Rect { x: 0, y: 0, w: 1920, h: 1040 }
    }

    fn win(x: i32, y: i32) -> Rect {
        Rect { x, y, w: 800, h: 600 }
    }

    /// 好端端待在工作區裡的視窗一步都不能動——這道校正不是來替使用者
    /// 決定視窗該擺哪裡的
    #[test]
    fn a_window_inside_the_work_area_is_left_alone() {
        assert_eq!(corrected_position(win(100, 100), work()), None);
        // 剛好貼齊四邊也算在裡面
        assert_eq!(corrected_position(win(0, 0), work()), None);
        assert_eq!(corrected_position(win(1120, 440), work()), None);
    }

    /// 靠邊放但還露得夠多：使用者自己這樣放的，不要雞婆搬回來
    #[test]
    fn a_mostly_visible_window_near_the_edge_is_kept() {
        // 右邊只剩 200px 在畫面上，超過門檻的 120
        assert_eq!(corrected_position(win(1720, 200), work()), None);
        // 下緣掉出工作區（例如被工作列蓋住一截）不算問題，標題列還在
        assert_eq!(corrected_position(win(300, 900), work()), None);
    }

    /// 看得見的面積再大都不算數，判斷的是標題列在不在：上緣掉出畫面之後，
    /// 這個視窗露出 200px 高，卻連拖都拖不動
    #[test]
    fn a_large_visible_area_does_not_excuse_a_hidden_title_bar() {
        assert_eq!(corrected_position(win(300, -400), work()), Some((300, 0)));
    }

    /// 拔掉副螢幕的經典情形：存的位置整個在主螢幕右邊，還原後完全看不到。
    /// 拉回來時只做最小平移，y 沒問題就不動 y
    #[test]
    fn a_window_off_to_the_right_is_pulled_back() {
        assert_eq!(corrected_position(win(2600, 300), work()), Some((1120, 300)));
    }

    /// 標題列在畫面上方外面：抓不到就搬不動，這正是要救的那一種
    #[test]
    fn a_title_bar_above_the_screen_is_pulled_down() {
        // 只剩 10px 高露在工作區裡，低於一列標題列
        assert_eq!(corrected_position(win(300, -590), work()), Some((300, 0)));
    }

    /// 只露出一條細邊也不算數：那幾像素滑鼠實際上點不到
    #[test]
    fn a_sliver_at_the_edge_is_not_enough() {
        assert_eq!(corrected_position(win(-780, 200), work()), Some((0, 200)));
    }

    /// 負座標的工作區（副螢幕排在主螢幕左邊）：拉回的是那台螢幕的範圍，
    /// 不是硬拉到原點
    #[test]
    fn a_work_area_at_negative_coordinates_is_handled() {
        let left = Rect { x: -1920, y: -200, w: 1920, h: 1040 };
        assert_eq!(corrected_position(win(-4000, 0), left), Some((-1920, 0)));
        assert_eq!(corrected_position(win(-1800, 0), left), None);
    }

    /// 視窗比工作區還大（尺寸校正之後照理不會發生，但不能因此算錯）：
    /// 靠齊工作區起點，讓左上角連同標題列與控制鈕都在畫面上。
    /// 這裡置中反而會把上緣推到畫面外
    #[test]
    fn a_window_larger_than_the_work_area_is_aligned_to_its_origin() {
        let big = Rect { x: -500, y: -500, w: 2400, h: 1400 };
        let small = Rect { x: 0, y: 0, w: 1366, h: 728 };
        assert_eq!(corrected_position(big, small), Some((0, 0)));
    }

    /// 門檻是對視窗自身尺寸取小的：極小的視窗要求整個可見，
    /// 否則它永遠滿足不了門檻，每次啟動都會被搬一次
    #[test]
    fn a_tiny_window_is_measured_against_its_own_size() {
        let tiny = Rect { x: 100, y: 100, w: 60, h: 20 };
        assert_eq!(corrected_position(tiny, work()), None);
        // 真的掉出去了才搬
        assert_eq!(corrected_position(Rect { x: -100, y: 100, ..tiny }, work()), Some((0, 100)));
    }

    /// 兩軸各自判斷、也各自拉回：只有一邊出問題時另一邊維持原位
    #[test]
    fn both_axes_are_corrected_together_when_both_are_off() {
        assert_eq!(corrected_position(win(3000, -900), work()), Some((1120, 0)));
    }
}
