mod config;
mod exits;
mod state;
mod tunnel;
mod winsys;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_notification::NotificationExt;

use config::{Config, Forward, LoadOutcome};
use state::{AppState, Snapshot, MAIN_WINDOW, TRAY_ID};

type Shared = Arc<AppState>;

/// 啟動參數帶 -Tray/--tray 代表直接隱藏到系統匣
fn is_tray_start() -> bool {
    std::env::args().skip(1).any(|a| {
        let a = a.trim_start_matches('-').to_ascii_lowercase();
        a == "tray"
    })
}

/// 設定檔與執行檔放在一起，維持可攜性
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN_WINDOW) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn balloon(app: &AppHandle, body: &str) {
    let _ = app.notification().builder().title("Traytunnel").body(body).show();
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

/// 設定檔寫入失敗一律讓使用者看得到，且記憶體狀態不會被改掉
fn report_save_error(state: &Shared, e: std::io::Error) {
    state.log(format!("failed to save settings: {e}"));
}

// ---------------------------------------------------------------- 前端指令

#[tauri::command]
fn get_state(state: State<'_, Shared>) -> Snapshot {
    state.snapshot()
}

/// 連接單一出口：記住使用者的選擇（enabled=true）後再拉線
#[tauri::command]
fn start_exit(state: State<'_, Shared>, local: u16) {
    let st = state.inner().clone();
    if st.config().forward(local).is_none() {
        st.log(format!("port {local} : no such exit"));
        return;
    }
    if let Err(e) = st.update_config(|c| {
        if let Some(f) = c.forward_mut(local) {
            f.enabled = true;
        }
    }) {
        report_save_error(&st, e);
        return;
    }
    st.emit_config_changed();
    tunnel::start(&st, local);
}

/// 中斷單一出口：enabled=false 並持久化，重開程式也不會自己連回來
#[tauri::command]
fn stop_exit(state: State<'_, Shared>, local: u16) {
    let st = state.inner().clone();
    if st.config().forward(local).is_none() {
        st.log(format!("port {local} : no such exit"));
        return;
    }
    if let Err(e) = st.update_config(|c| {
        if let Some(f) = c.forward_mut(local) {
            f.enabled = false;
        }
    }) {
        report_save_error(&st, e);
        return;
    }
    tunnel::halt(&st, local);
    st.emit_config_changed();
}

#[tauri::command]
fn start_all(state: State<'_, Shared>) {
    let st = state.inner().clone();
    if let Err(e) = st.update_config(|c| {
        for f in c.forwards.iter_mut() {
            f.enabled = true;
        }
    }) {
        report_save_error(&st, e);
        return;
    }
    st.emit_config_changed();
    tunnel::start_enabled(&st);
}

#[tauri::command]
fn stop_all(state: State<'_, Shared>) {
    let st = state.inner().clone();
    if let Err(e) = st.update_config(|c| {
        for f in c.forwards.iter_mut() {
            f.enabled = false;
        }
    }) {
        report_save_error(&st, e);
        return;
    }
    tunnel::halt_all(&st);
    st.emit_config_changed();
}

/// 存全域連線設定，回傳 None 代表成功
#[tauri::command]
fn save_global(
    state: State<'_, Shared>,
    host: String,
    user: String,
    proxy_command: String,
) -> Option<String> {
    let st = state.inner().clone();
    if let Some(err) = config::validate_global(&host, &user) {
        return Some(err);
    }
    if let Err(e) = st.update_config(|c| {
        c.host = host.trim().to_string();
        c.user = user.trim().to_string();
        c.proxy_command = proxy_command.trim().to_string();
    }) {
        let msg = format!("Failed to save settings:\n{e}");
        report_save_error(&st, e);
        return Some(msg);
    }
    st.emit_config_changed();
    st.log("connection settings saved, restarting running exits");
    // 新的 host/user/proxyCommand 只有重接才會生效
    tunnel::restart_running(&st);
    None
}

/// 新增或編輯出口，originalLocal 為 None 代表新增；回傳 None 代表成功
#[tauri::command]
fn upsert_forward(
    state: State<'_, Shared>,
    original_local: Option<u16>,
    name: String,
    local: u16,
    remote: String,
) -> Option<String> {
    let st = state.inner().clone();
    let cfg = st.config();
    let name = name.trim().to_string();
    let remote = remote.trim().to_string();
    if let Some(err) =
        config::validate_forward(&cfg.forwards, original_local, &name, local, &remote)
    {
        return Some(err);
    }

    // 編輯運行中的出口要先停掉舊的那條線（換埠時舊埠也才會放掉）；
    // 新增的出口比照設定檔缺省值視為 enabled，加完就直接連
    let was_enabled = match original_local {
        Some(orig) => cfg.forward(orig).map(|f| f.enabled).unwrap_or(false),
        None => true,
    };
    if let Some(orig) = original_local {
        tunnel::halt(&st, orig);
    }

    if let Err(e) = st.update_config(|c| match original_local {
        Some(orig) => {
            if let Some(f) = c.forward_mut(orig) {
                f.name = name.clone();
                f.local = local;
                f.remote = remote.clone();
            }
        }
        None => c.forwards.push(Forward {
            name: name.clone(),
            local,
            remote: remote.clone(),
            enabled: true,
        }),
    }) {
        let msg = format!("Failed to save settings:\n{e}");
        report_save_error(&st, e);
        return Some(msg);
    }

    st.emit_config_changed();
    st.log(match original_local {
        Some(_) => format!("{name} updated"),
        None => format!("{name} added"),
    });
    if was_enabled {
        tunnel::start(&st, local);
    }
    None
}

/// 刪出口，運行中的先停掉
#[tauri::command]
fn delete_forward(state: State<'_, Shared>, local: u16) {
    let st = state.inner().clone();
    let Some(name) = st.config().forward(local).map(|f| f.name.clone()) else {
        st.log(format!("port {local} : no such exit"));
        return;
    };
    tunnel::halt(&st, local);
    if let Err(e) = st.update_config(|c| c.forwards.retain(|f| f.local != local)) {
        report_save_error(&st, e);
        return;
    }
    st.emit_config_changed();
    st.log(format!("{name} deleted"));
}

#[tauri::command]
fn test_exit(state: State<'_, Shared>, local: u16) {
    tunnel::test_exit(&state.inner().clone(), local);
}

#[tauri::command]
fn test_all(state: State<'_, Shared>) {
    tunnel::test_connected(&state.inner().clone());
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
            start_all,
            stop_all,
            save_global,
            upsert_forward,
            delete_forward,
            test_exit,
            test_all,
            set_close_to_tray,
            set_autostart,
            window_close,
            window_minimize,
            exit_app,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let dir = exe_dir();
            let outcome = config::load_from_dir(&dir);
            let cfg: Config = outcome.config().clone();
            let shared: Shared = Arc::new(AppState::new(handle.clone(), dir, cfg.clone()));
            app.manage(shared.clone());

            build_tray(&handle, &shared)?;

            // 主視窗關閉請求（例如 Alt+F4）也走 closeToTray 規則
            if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
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

            shared.refresh_tooltip();
            shared.log("Traytunnel started");
            match &outcome {
                LoadOutcome::Created(_) => {
                    shared.log("config created with defaults, open Settings to edit");
                }
                LoadOutcome::Broken { backup, error, .. } => {
                    shared.log(format!("config unreadable ({error}), using defaults"));
                    match backup {
                        Some(path) => {
                            shared.log(format!(
                                "broken config kept at {}",
                                path.file_name()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or(config::BROKEN_NAME)
                            ));
                            balloon(
                                &handle,
                                "Config file could not be parsed. A backup was saved as traytunnel.toml.broken and defaults are in use.",
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

fn build_tray(app: &AppHandle, shared: &Shared) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open window", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "exit", "Exit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    let st = shared.clone();
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Traytunnel")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open" => show_main(app),
            // 系統匣的 Exit 一律真的退出
            "exit" => do_exit(&st),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}
