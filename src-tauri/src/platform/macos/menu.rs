//! macOS 標準應用程式選單：App／Edit／Window 三個最小集。
//!
//! 為什麼非做不可：WKWebView 的輸入框要吃到 Cmd+C／Cmd+V／Cmd+A 這類快捷鍵，
//! 前提是選單列上真的掛著對應的項目——macOS 的鍵盤快捷鍵是透過選單系統的
//! key equivalent 分派的，不是直接進 webview 的 responder chain。純系統匣
//! 常駐、完全沒有選單列的 App，即使視窗有焦點，輸入框裡也按不動剪下／
//! 複製／貼上／全選（Edit 選單），也沒有 Cmd+W／Cmd+M（Window 選單）。
//!
//! Quit 刻意**不用** `PredefinedMenuItem::quit`：那顆項目點下去的瞬間直接
//! 呼叫 `exit(0)`，不會走過我們自己的收尾（`kill_all_jobs`——ssh／
//! WireGuard 的子行程需要被明確殺掉，不然就是孤兒行程留在系統裡）。這是
//! tauri 上游已知且判定 wontfix 的限制（tauri-apps/tauri#7586：討論串裡
//! 官方回覆 `MenuItem::Quit` 就是直接 `exit(0)`，沒有任何事件可以攔）。
//! 因此這裡改用一顆一般 `MenuItem`，掛標準的 Cmd+Q（macOS 使用者的預期
//! 沒有變，只是繞開了上游那個洞），事件走 `App::on_menu_event` 轉呼叫端
//! 自己的結束流程，語意對齊系統匣選單既有的 Exit（見 `lib.rs::do_exit`）。

use tauri::menu::{AboutMetadataBuilder, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Wry};

use crate::appicon;

/// Quit 項目的 id，`lib.rs` 的 `App::on_menu_event` 靠它辨認要不要呼叫
/// `do_exit`（其餘項目全是 `PredefinedMenuItem`，原生行為，不必路由）。
pub const QUIT_ID: &str = "app-quit";

/// About 面板的內容。版本一律讀執行期的 `package_info()`（`Cargo.toml` 的
/// `version`，發版時 `scripts/bump.mjs` 早已跟 `package.json` 對過），不寫死
/// 字串——不然每次發版都多一個要記得同步、忘了就跟著舊版號的地方。
/// copyright 是 `Cargo.toml` `authors` 欄位既有的資料退一步湊出來的，Cargo
/// 沒有專門的 copyright 欄位；`authors` 欄位本身也一併帶上（macOS 的 About
/// 面板不吃這欄，見 tauri `AboutMetadata` 文件的平台相容表，但 Windows／
/// Linux 用得到，帶著不吃虧）。圖示用 `appicon::window_icon()`——跟主視窗、
/// 工作列共用同一份內嵌 ICO 挑層，不必另外湊一份圖檔。
fn about_metadata(app: &AppHandle) -> tauri::menu::AboutMetadata<'static> {
    let pkg = app.package_info();
    let authors: Vec<String> =
        pkg.authors.split(':').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    let copyright = (!authors.is_empty()).then(|| format!("© {}", authors.join(", ")));
    AboutMetadataBuilder::new()
        .name(Some("Traytunnel".to_string()))
        .version(Some(pkg.version.to_string()))
        .copyright(copyright)
        .authors((!authors.is_empty()).then_some(authors))
        .icon(appicon::window_icon())
        .build()
}

/// 建一份最小集選單：App（About／Quit）、Edit（Undo/Redo/Cut/Copy/Paste/
/// Select All）、Window（Minimize/Close）。除了 Quit，其餘一律用
/// `PredefinedMenuItem`——原生行為、原生快捷鍵，不必自己刻 accelerator。
pub fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let about = PredefinedMenuItem::about(app, None, Some(about_metadata(app)))?;
    let quit_sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, QUIT_ID, "Quit Traytunnel", true, Some("Cmd+Q"))?;
    let app_items: Vec<&dyn IsMenuItem<Wry>> = vec![&about, &quit_sep, &quit];
    let app_menu = Submenu::with_items(app, "Traytunnel", true, &app_items)?;

    let undo = PredefinedMenuItem::undo(app, None)?;
    let redo = PredefinedMenuItem::redo(app, None)?;
    let edit_sep1 = PredefinedMenuItem::separator(app)?;
    let cut = PredefinedMenuItem::cut(app, None)?;
    let copy = PredefinedMenuItem::copy(app, None)?;
    let paste = PredefinedMenuItem::paste(app, None)?;
    let edit_sep2 = PredefinedMenuItem::separator(app)?;
    let select_all = PredefinedMenuItem::select_all(app, None)?;
    let edit_items: Vec<&dyn IsMenuItem<Wry>> =
        vec![&undo, &redo, &edit_sep1, &cut, &copy, &paste, &edit_sep2, &select_all];
    let edit_menu = Submenu::with_items(app, "Edit", true, &edit_items)?;

    let minimize = PredefinedMenuItem::minimize(app, None)?;
    let close_window = PredefinedMenuItem::close_window(app, None)?;
    let window_items: Vec<&dyn IsMenuItem<Wry>> = vec![&minimize, &close_window];
    let window_menu = Submenu::with_items(app, "Window", true, &window_items)?;

    Menu::with_items(app, &[&app_menu, &edit_menu, &window_menu])
}
