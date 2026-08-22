mod aumid;
mod config;
mod exits;
mod state;
mod traymenu;
mod tunnel;
mod winsys;

use std::io::Cursor;
use std::sync::Arc;

use tauri::image::Image;
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_winrt_notification::{IconCrop, Toast};

use config::{Config, LoadOutcome, Source};
use state::{AppState, Snapshot, MAIN_WINDOW, TRAY_ID};

type Shared = Arc<AppState>;

/// 啟動參數帶 -Tray/--tray 代表直接隱藏到系統匣
fn is_tray_start() -> bool {
    std::env::args().skip(1).any(|a| {
        let a = a.trim_start_matches('-').to_ascii_lowercase();
        a == "tray"
    })
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN_WINDOW) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 系統匣氣泡通知：直接組 Toast 而不是走 tauri-plugin-notification 的 builder，
/// 因為那個 builder 在 Windows 分支沒接圖示（icon()/attachment() 都到不了 notify-rust
/// 底層），要讓 toast 內文左側出現大 logo（appLogoOverride）只有 Toast::icon() 這條路。
fn balloon(app: &AppHandle, body: &str) {
    let aumid = app.config().identifier.clone();
    let mut toast = Toast::new(&aumid).title("Traytunnel").text1(body);
    if let Some(icon) = aumid::icon_file_path(&aumid) {
        toast = toast.icon(&icon, IconCrop::Square, "Traytunnel");
    }
    if let Err(e) = toast.show() {
        log::warn!("failed to show toast notification: {e}");
    }
}

fn hide_to_tray(state: &Shared) {
    if let Some(w) = state.app.get_webview_window(MAIN_WINDOW) {
        let _ = w.hide();
    }
    if !state.tray_hint_shown() {
        balloon(&state.app, "Closed to tray, still running. Double-click the tray icon to reopen.");
    }
}

fn do_exit(state: &Shared) {
    state.mark_exiting();
    state.kill_all_jobs();
    state.app.exit(0);
}

/// 關閉鈕行為由 closeToTray 決定
fn close_main(state: &Shared) {
    if state.config().close_to_tray {
        hide_to_tray(state);
    } else {
        do_exit(state);
    }
}

/// 開機自啟自癒：舊版 PowerShell 留下的 Run 登錄項會讓 toggle 顯示 ON 卻其實
/// 啟動不到這支程式，啟動時發現登錄值沒指向目前的執行檔就重寫一次。
fn heal_autostart(app: &AppHandle, state: &Shared) {
    if !app.autolaunch().is_enabled().unwrap_or(false) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let exe = exe.to_string_lossy().to_lowercase();
    let name = app
        .config()
        .product_name
        .clone()
        .unwrap_or_else(|| app.package_info().name.clone());
    let current = winsys::read_run_value(&name).unwrap_or_default().to_lowercase();
    if current.contains(&exe) {
        return;
    }
    match app.autolaunch().enable() {
        Ok(()) => state.log("autostart entry refreshed"),
        Err(e) => state.log(format!("autostart entry refresh failed: {e}")),
    }
}

/// AUMID 自註冊：AUMID 字串就是 tauri.conf.json 的 identifier，必須完全一致，
/// 通知外掛在正式部署路徑下用的也是它。
fn prepare_notifications(app: &AppHandle) -> Vec<String> {
    let aumid = app.config().identifier.clone();
    let product = app
        .config()
        .product_name
        .clone()
        .unwrap_or_else(|| app.package_info().name.clone());
    let Ok(exe) = std::env::current_exe() else {
        return vec!["could not resolve the executable path for notifications".into()];
    };
    aumid::prepare(&aumid, &product, &exe)
}

/// 設定檔寫入失敗一律讓使用者看得到，且記憶體狀態不會被改掉
fn report_save_error(state: &Shared, e: std::io::Error) {
    state.log(format!("failed to save settings: {e}"));
}

// ---------------------------------------------------------------- 前端指令

#[tauri::command]
fn get_state(state: State<'_, Shared>) -> Snapshot {
    state.snapshot()
}

/// 出口不存在時記一行就回，回傳 false 代表沒有這個出口
fn require_exit(st: &Shared, local: u16) -> bool {
    if st.config().forward(local).is_none() {
        st.log(format!("port {local} : no such exit"));
        return false;
    }
    true
}

/// 源不存在時記一行就回
fn require_source(st: &Shared, name: &str) -> bool {
    if st.config().source(name).is_none() {
        st.log(format!("no such source: {name}"));
        return false;
    }
    true
}

/// 連接單一出口：記住使用者的選擇（enabled=true）後再拉線。
/// 前端指令與系統匣選單共用這裡，不繞 invoke。
fn enable_exit(st: &Shared, local: u16) {
    if !require_exit(st, local) {
        return;
    }
    if let Err(e) = st.update_config(|c| {
        if let Some(f) = c.forward_mut(local) {
            f.enabled = true;
        }
    }) {
        report_save_error(st, e);
        // 存檔失敗代表 enabled 沒改成，但系統匣的勾選已經被原生選單自己翻掉了，
        // 重建一次把它拉回設定裡的真值
        st.refresh_tray();
        return;
    }
    st.emit_config_changed();
    tunnel::start(st, local);
}

/// 中斷單一出口：enabled=false 並持久化，重開程式也不會自己連回來
fn disable_exit(st: &Shared, local: u16) {
    if !require_exit(st, local) {
        return;
    }
    if let Err(e) = st.update_config(|c| {
        if let Some(f) = c.forward_mut(local) {
            f.enabled = false;
        }
    }) {
        report_save_error(st, e);
        // 同上：勾選已被原生選單翻掉，設定卻沒改成，重建把它拉回真值
        st.refresh_tray();
        return;
    }
    tunnel::halt(st, local);
    st.emit_config_changed();
}

#[tauri::command]
fn start_exit(state: State<'_, Shared>, local: u16) {
    enable_exit(&state.inner().clone(), local);
}

#[tauri::command]
fn stop_exit(state: State<'_, Shared>, local: u16) {
    disable_exit(&state.inner().clone(), local);
}

/// 重接單一出口：halt 後立刻 start，套用最新設定。
/// 停用中的出口按重接視同要它連起來，順手把 enabled 補成 true。
#[tauri::command]
fn restart_exit(state: State<'_, Shared>, local: u16) {
    let st = state.inner().clone();
    if !require_exit(&st, local) {
        return;
    }
    let enabled = st.config().forward(local).map(|f| f.enabled).unwrap_or(false);
    if !enabled {
        if let Err(e) = st.update_config(|c| {
            if let Some(f) = c.forward_mut(local) {
                f.enabled = true;
            }
        }) {
            report_save_error(&st, e);
            return;
        }
        st.emit_config_changed();
    }
    st.log_exit(local, format!("port {local} : restarting"));
    tunnel::restart(&st, local);
}

/// 連接一個源底下全部的出口
#[tauri::command]
fn start_source(state: State<'_, Shared>, name: String) {
    let st = state.inner().clone();
    if !require_source(&st, &name) {
        return;
    }
    if let Err(e) = st.update_config(|c| {
        if let Some(s) = c.source_mut(&name) {
            for f in s.forwards.iter_mut() {
                f.enabled = true;
            }
        }
    }) {
        report_save_error(&st, e);
        return;
    }
    st.emit_config_changed();
    tunnel::start_source(&st, &name);
}

/// 中斷一個源底下全部的出口
#[tauri::command]
fn stop_source(state: State<'_, Shared>, name: String) {
    let st = state.inner().clone();
    if !require_source(&st, &name) {
        return;
    }
    if let Err(e) = st.update_config(|c| {
        if let Some(s) = c.source_mut(&name) {
            for f in s.forwards.iter_mut() {
                f.enabled = false;
            }
        }
    }) {
        report_save_error(&st, e);
        return;
    }
    tunnel::halt_source(&st, &name);
    st.emit_config_changed();
}

/// 全部連接：跨源把 enabled 全開再拉線
fn enable_all(st: &Shared) {
    if let Err(e) = st.update_config(set_all_enabled(true)) {
        report_save_error(st, e);
        return;
    }
    st.emit_config_changed();
    tunnel::start_enabled(st);
}

/// 全部中斷
fn disable_all(st: &Shared) {
    if let Err(e) = st.update_config(set_all_enabled(false)) {
        report_save_error(st, e);
        return;
    }
    tunnel::halt_all(st);
    st.emit_config_changed();
}

#[tauri::command]
fn start_all(state: State<'_, Shared>) {
    enable_all(&state.inner().clone());
}

#[tauri::command]
fn stop_all(state: State<'_, Shared>) {
    disable_all(&state.inner().clone());
}

fn set_all_enabled(on: bool) -> impl FnOnce(&mut Config) {
    move |c: &mut Config| {
        for s in c.sources.iter_mut() {
            for f in s.forwards.iter_mut() {
                f.enabled = on;
            }
        }
    }
}

/// 新增或編輯連線源，originalName 為 None 代表新增；回傳 None 代表成功。
/// 改到連線欄位時會重接這個源底下運行中的出口。
#[tauri::command]
fn upsert_source(
    state: State<'_, Shared>,
    original_name: Option<String>,
    name: String,
    host: String,
    user: String,
    proxy_command: String,
) -> Option<String> {
    let st = state.inner().clone();
    let cfg = st.config();
    let name = name.trim().to_string();
    let host = host.trim().to_string();
    let user = user.trim().to_string();
    let proxy_command = proxy_command.trim().to_string();
    if let Some(err) =
        config::validate_source(&cfg.sources, original_name.as_deref(), &name, &host, &user)
    {
        return Some(err);
    }

    // 連線欄位有沒有真的變，決定要不要把這個源的出口重接一輪
    let changed = match original_name.as_deref().and_then(|n| cfg.source(n)) {
        Some(old) => old.host != host || old.user != user || old.proxy_command != proxy_command,
        None => false,
    };

    let target = name.clone();
    if let Err(e) = st.update_config(|c| match original_name.as_deref() {
        Some(orig) => {
            if let Some(s) = c.source_mut(orig) {
                s.name = target.clone();
                s.host = host.clone();
                s.user = user.clone();
                s.proxy_command = proxy_command.clone();
            }
        }
        // 新的源底下還沒有任何出口
        None => c.sources.push(Source {
            name: target.clone(),
            host: host.clone(),
            user: user.clone(),
            proxy_command: proxy_command.clone(),
            forwards: Vec::new(),
        }),
    }) {
        let msg = format!("Failed to save settings:\n{e}");
        report_save_error(&st, e);
        return Some(msg);
    }

    st.emit_config_changed();
    st.log_from(
        &name,
        match original_name {
            Some(_) => "source updated",
            None => "source added",
        },
    );
    if changed {
        st.log_from(&name, "connection settings changed, restarting running exits");
        tunnel::restart_running_in_source(&st, &name);
    }
    None
}

/// 刪源，底下的出口先全部停掉；刪到零源也是允許的
#[tauri::command]
fn delete_source(state: State<'_, Shared>, name: String) {
    let st = state.inner().clone();
    if !require_source(&st, &name) {
        return;
    }
    tunnel::halt_source(&st, &name);
    if let Err(e) = st.update_config(|c| c.sources.retain(|s| s.name != name)) {
        report_save_error(&st, e);
        return;
    }
    st.emit_config_changed();
    st.log(format!("source {name} deleted"));
}

/// 新增或編輯出口，originalLocal 為 None 代表新增；回傳 None 代表成功。
/// source 是這個出口要掛進去的源，編輯時也可以藉此把出口搬到別的源。
#[tauri::command]
fn upsert_forward(
    state: State<'_, Shared>,
    source: String,
    original_local: Option<u16>,
    name: String,
    local: u16,
    remote: String,
) -> Option<String> {
    let st = state.inner().clone();
    let cfg = st.config();
    if cfg.source(&source).is_none() {
        return Some(format!("no such connection: {source}"));
    }
    // 新增的出口比照設定檔缺省值視為 enabled，加完就直接連；編輯則沿用原本的選擇
    let was_enabled = match original_local {
        Some(orig) => cfg.forward(orig).map(|f| f.enabled).unwrap_or(false),
        None => true,
    };
    // 正規化與驗證都在 config 那邊做完，這裡只負責把它給的那一筆原樣存下去
    let prepared =
        config::prepare_forward(&cfg.sources, original_local, &name, local, &remote, was_enabled);
    let forward = match prepared {
        Ok(f) => f,
        Err(err) => return Some(err),
    };
    let name = forward.name.clone();

    // 編輯運行中的出口要先停掉舊的那條線（換埠或換源時舊埠也才會放掉）
    if let Some(orig) = original_local {
        tunnel::halt(&st, orig);
    }

    if let Err(e) = st.update_config(|c| {
        if let Some(orig) = original_local {
            // 先從原本的源拔掉，再掛進目標源，同源編輯也走同一條路
            for s in c.sources.iter_mut() {
                s.forwards.retain(|f| f.local != orig);
            }
        }
        if let Some(s) = c.source_mut(&source) {
            s.forwards.push(forward.clone());
        }
    }) {
        let msg = format!("Failed to save settings:\n{e}");
        report_save_error(&st, e);
        return Some(msg);
    }

    st.emit_config_changed();
    st.log_from(
        &source,
        match original_local {
            Some(_) => format!("{name} updated"),
            None => format!("{name} added"),
        },
    );
    if was_enabled {
        tunnel::start(&st, local);
    }
    None
}

/// 刪出口，運行中的先停掉
#[tauri::command]
fn delete_forward(state: State<'_, Shared>, local: u16) {
    let st = state.inner().clone();
    let cfg = st.config();
    let Some((src, f)) = cfg.locate(local) else {
        st.log(format!("port {local} : no such exit"));
        return;
    };
    let (sname, fname) = (src.name.clone(), f.name.clone());
    tunnel::halt(&st, local);
    if let Err(e) = st.update_config(|c| {
        for s in c.sources.iter_mut() {
            s.forwards.retain(|f| f.local != local);
        }
    }) {
        report_save_error(&st, e);
        return;
    }
    st.emit_config_changed();
    st.log_from(&sname, format!("{fname} deleted"));
}

#[tauri::command]
fn test_exit(state: State<'_, Shared>, local: u16) {
    tunnel::test_exit(&state.inner().clone(), local);
}

#[tauri::command]
fn set_close_to_tray(state: State<'_, Shared>, on: bool) -> Result<(), String> {
    let st = state.inner().clone();
    st.update_config(|c| c.close_to_tray = on)
        .map_err(|e| format!("Failed to save settings:\n{e}"))?;
    st.emit_config_changed();
    st.log(if on { "close hides to tray" } else { "close exits app" });
    Ok(())
}

#[tauri::command]
fn set_autostart(app: AppHandle, state: State<'_, Shared>, on: bool) -> Result<(), String> {
    let st = state.inner().clone();
    let result = if on { app.autolaunch().enable() } else { app.autolaunch().disable() };
    result.map_err(|e| format!("Failed to change autostart:\n{e}"))?;
    st.log(if on { "autostart enabled" } else { "autostart disabled" });
    st.emit_config_changed();
    Ok(())
}

/// 這次執行實際生效的設定檔完整路徑，設定頁的 About 直接顯示它
#[tauri::command]
fn get_config_path(state: State<'_, Shared>) -> String {
    state.path.to_string_lossy().into_owned()
}

/// 在檔案總管裡開啟設定檔所在資料夾，並選中設定檔本身
#[tauri::command]
fn open_config_dir(state: State<'_, Shared>) {
    let st = state.inner().clone();
    if let Err(e) = winsys::reveal_in_explorer(&st.path) {
        st.log(format!("could not open the config folder: {e}"));
    }
}

#[tauri::command]
fn window_close(state: State<'_, Shared>) {
    close_main(&state.inner().clone());
}

#[tauri::command]
fn window_minimize(app: AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN_WINDOW) {
        let _ = w.minimize();
    }
}

#[tauri::command]
fn exit_app(state: State<'_, Shared>) {
    do_exit(&state.inner().clone());
}

// ---------------------------------------------------------------- 進入點

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // single-instance 必須第一個註冊：第二個實例只負責喚醒主視窗
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--tray"]),
        ))
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("traytunnel".into()),
                    }),
                ])
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            get_state,
            start_exit,
            stop_exit,
            restart_exit,
            start_source,
            stop_source,
            start_all,
            stop_all,
            upsert_source,
            delete_source,
            upsert_forward,
            delete_forward,
            test_exit,
            set_close_to_tray,
            set_autostart,
            get_config_path,
            open_config_dir,
            window_close,
            window_minimize,
            exit_app,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            // 通知掛名要在任何 UI／toast 之前處理掉
            let aumid_notes = prepare_notifications(&handle);
            // 設定檔位置只解析這一次，之後的讀寫與備份都跟著這個結果走
            let loc = config::config_location();
            let outcome = config::load_from_path(&loc.path);
            let cfg: Config = outcome.config().clone();
            let shared: Shared =
                Arc::new(AppState::new(handle.clone(), loc.path.clone(), cfg.clone()));
            app.manage(shared.clone());

            build_tray(&handle, &shared)?;

            if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                // 工作列的視窗按鈕吃的是 SM_CXICON（175% 下 56px），codegen 給的是
                // ICO 第一層 16px，得自己挑層再設一次才不會被 GDI 放大成一團糊
                match window_icon() {
                    Some(icon) => {
                        if let Err(e) = win.set_icon(icon) {
                            log::warn!("could not set the window icon: {e}");
                        }
                    }
                    None => log::warn!("no window icon layer available, keeping the default"),
                }

                // 主視窗關閉請求（例如 Alt+F4）也走 closeToTray 規則
                let st = shared.clone();
                win.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        if !st.is_exiting() {
                            api.prevent_close();
                            close_main(&st);
                        }
                    }
                });
            }

            shared.refresh_tray();
            shared.log("Traytunnel started");
            shared.log(format!(
                "config: {}{}",
                loc.path.display(),
                if loc.portable { " (portable, next to the executable)" } else { "" }
            ));
            for note in aumid_notes {
                shared.log(note);
            }
            match &outcome {
                LoadOutcome::Created(_) => {
                    shared.log("config created with defaults, open Settings to edit");
                }
                LoadOutcome::Migrated(cfg) => {
                    shared.log(format!(
                        "config migrated to the multi-source format ({} source(s))",
                        cfg.sources.len()
                    ));
                }
                LoadOutcome::Broken { backup, error, .. } => {
                    shared.log(format!("config unreadable ({error}), using defaults"));
                    match backup {
                        Some(path) => {
                            // 可攜模式與家目錄模式的檔名不同，訊息一律用實際檔名
                            let name = config::file_name_of(path);
                            shared.log(format!("broken config kept at {name}"));
                            balloon(
                                &handle,
                                &format!(
                                    "Config file could not be parsed. A backup was saved as {name} and defaults are in use."
                                ),
                            );
                        }
                        None => {
                            shared.log("config left untouched, no backup could be written");
                            balloon(
                                &handle,
                                "Config file could not be read. It was left untouched and defaults are in use.",
                            );
                        }
                    }
                }
                LoadOutcome::Loaded(_) => {}
            }

            heal_autostart(&handle, &shared);

            if is_tray_start() {
                shared.mark_tray_hint_shown();
                balloon(&handle, "Started in the system tray. Double-click the tray icon to open.");
            } else {
                show_main(&handle);
            }

            // enabled 的出口開機就自己連
            tunnel::start_enabled(&shared);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 多層 ICO 直接內嵌，才不必依賴磁碟上有沒有圖示檔。
/// 系統匣與視窗圖示都從這一顆挑層，assets/gen-tray-icons.py 產生。
const APP_ICO: &[u8] = include_bytes!("../icons/icon.ico");

/// 從內嵌的多層 ICO 裡挑最接近 `want` 的一層，解成 RGBA。
///
/// Tauri codegen 的 `default_window_icon()` 只取 ICO 的第一層固定尺寸（我們的第一層
/// 是 16px），高 DPI 時交給 GDI 拉伸就會糊掉（tauri#14596、#9335），所以系統匣與主
/// 視窗都自己挑層再明確設定。
fn ico_layer(want: u32, purpose: &str) -> Option<Image<'static>> {
    let dir = ico::IconDir::read(Cursor::new(APP_ICO)).ok()?;
    let sizes: Vec<u32> = dir.entries().iter().map(|e| e.width()).collect();
    let idx = winsys::pick_icon_layer(&sizes, want)?;
    let img = dir.entries()[idx].decode().ok()?;
    log::info!("{purpose} icon: system wants {want}px, using the {}px layer", img.width());
    Some(Image::new_owned(img.rgba_data().to_vec(), img.width(), img.height()))
}

/// 系統匣圖示：照 SM_CXSMICON（100% 是 16、175% 是 28）挑層
fn tray_icon() -> Option<Image<'static>> {
    ico_layer(winsys::small_icon_size().0, "tray")
}

/// 主視窗圖示：照 SM_CXICON（100% 是 32、175% 是 56）挑層。
/// 這是工作列的視窗按鈕與 Alt+Tab 取的尺寸。
fn window_icon() -> Option<Image<'static>> {
    ico_layer(winsys::large_icon_size().0, "window")
}

#[cfg(test)]
mod app_icon_tests {
    use super::*;

    /// 內嵌的 ICO 必須含系統匣（SM_CXSMICON）與視窗（SM_CXICON）常用的整數縮放
    /// 尺寸，少了哪一層就會退回讓 GDI 拉伸而糊掉。125%／150%／175% 的 40／48／56
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
        let (small, _) = winsys::small_icon_size();
        let (large, _) = winsys::large_icon_size();
        assert!(small >= 16, "SM_CXSMICON = {small}");
        assert!(large >= small, "SM_CXICON {large} 不該小於 SM_CXSMICON {small}");
        assert!(tray_icon().is_some(), "系統匣挑不到層");
        assert!(window_icon().is_some(), "視窗挑不到層");
    }
}

/// 系統匣選單的事件路由：id 前綴決定要做什麼，一律呼叫內部函式，不繞 invoke
fn on_tray_menu(app: &AppHandle, st: &Shared, id: &str) {
    match id {
        traymenu::ID_OPEN => show_main(app),
        // 系統匣的 Exit 一律真的退出
        traymenu::ID_EXIT => do_exit(st),
        traymenu::ID_ALL_TOGGLE => toggle_all(st),
        traymenu::ID_RECONNECT_ALL => tunnel::reconnect_all(st),
        // 狀態行是停用的，照理點不到，真收到也是什麼都不做
        traymenu::ID_STATUS => {}
        _ => {
            if let Some(local) = id.strip_prefix(traymenu::EXIT_PREFIX).and_then(|p| p.parse().ok())
            {
                toggle_exit(st, local);
            } else if let Some(name) = id.strip_prefix(traymenu::SRC_RECONNECT_PREFIX) {
                if require_source(st, name) {
                    tunnel::reconnect_source(st, name);
                }
            } else {
                log::warn!("unhandled tray menu id: {id}");
            }
        }
    }
}

/// 勾選＝設定裡的 enabled，所以點一下就是反過來
fn toggle_exit(st: &Shared, local: u16) {
    match st.config().forward(local) {
        Some(f) if f.enabled => disable_exit(st, local),
        Some(_) => enable_exit(st, local),
        // 選單比設定舊了（出口已經被刪掉），重建一次讓它跟上
        None => {
            st.log(format!("port {local} : no such exit"));
            st.refresh_tray();
        }
    }
}

/// 有任何出口 enabled 就是 Stop all，全停時就是 Start all
fn toggle_all(st: &Shared) {
    if st.config().enabled_locals().is_empty() {
        enable_all(st);
    } else {
        disable_all(st);
    }
}

fn build_tray(app: &AppHandle, shared: &Shared) -> tauri::Result<()> {
    let menu = traymenu::build(app, &traymenu::menu_model(&shared.source_views()))?;

    // 挑不到層就退回 codegen 內建的圖示；連那個都沒有時寧可讓系統匣先長出來
    // 也不要 panic 掉整支程式，圖示之後照樣可以補
    let icon = tray_icon().or_else(|| app.default_window_icon().cloned());
    if icon.is_none() {
        log::warn!("no tray icon available, building the tray without one");
    }
    let st = shared.clone();
    let mut builder = TrayIconBuilder::with_id(TRAY_ID);
    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }
    builder
        .tooltip("Traytunnel")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| on_tray_menu(app, &st, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}
