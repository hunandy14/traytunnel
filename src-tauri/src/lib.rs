mod config;
mod exits;
mod state;
mod tunnel;
mod winsys;

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_notification::NotificationExt;

use config::{Config, LoadOutcome};
use state::{AppState, Snapshot, MAIN_WINDOW, SETTINGS_WINDOW, TRAY_ID};

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
    state.set_want_run(false);
    tunnel::stop(state);
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

/// 視窗高度隨出口卡片數量變動，公式沿用原版版面
fn apply_window_size(app: &AppHandle, forwards: usize) {
    let cards = std::cmp::max(10, 68 * forwards as i32 - 10);
    let height = 322 + cards;
    if let Some(w) = app.get_webview_window(MAIN_WINDOW) {
        let _ = w.set_size(LogicalSize::new(464.0, height as f64));
    }
}

// ---------------------------------------------------------------- 前端指令

#[tauri::command]
fn get_state(app: AppHandle, state: State<'_, Shared>) -> Snapshot {
    let autostart = app.autolaunch().is_enabled().unwrap_or(false);
    state.snapshot(autostart)
}

#[tauri::command]
fn toggle_run(state: State<'_, Shared>) {
    let st = state.inner().clone();
    if st.want_run() {
        st.set_want_run(false);
        tunnel::stop(&st);
        st.log("tunnel stopped");
        st.set_status("Stopped", "muted");
        st.reset_exits();
    } else {
        st.set_want_run(true);
        tunnel::start(&st);
    }
}

#[tauri::command]
fn retest(state: State<'_, Shared>) {
    let st = state.inner().clone();
    if st.connected() {
        tunnel::start_exit_tests(&st);
    } else {
        st.log("not connected, cannot test");
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

/// 設定視窗在 tauri.conf.json 就建好但預設隱藏，開關只是 show/hide，
/// 每次打開先通知頁面重新載入目前的設定值。
#[tauri::command]
fn open_settings(app: AppHandle) -> Result<(), String> {
    let w = app
        .get_webview_window(SETTINGS_WINDOW)
        .ok_or_else(|| "settings window is missing".to_string())?;
    let _ = w.emit("settings-open", ());
    w.show().map_err(|e| e.to_string())?;
    let _ = w.unminimize();
    let _ = w.set_focus();
    Ok(())
}

#[tauri::command]
fn close_settings(app: AppHandle) {
    if let Some(w) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = w.hide();
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsInput {
    host: String,
    user: String,
    proxy_command: String,
    /// 多行文字，每行「name local remote」
    forwards: String,
}

#[tauri::command]
fn save_config(app: AppHandle, state: State<'_, Shared>, input: SettingsInput) -> Result<(), String> {
    let st = state.inner().clone();
    let forwards = config::parse_forward_lines(&input.forwards).map_err(|bad| {
        format!("Invalid forward line:\n{bad}\n\nExpected:  name  localPort  remoteHost:remotePort")
    })?;
    if input.host.trim().is_empty() || input.user.trim().is_empty() || forwards.is_empty() {
        return Err("Host, User and at least one forward are required.".into());
    }
    let cfg = Config {
        host: input.host.trim().to_string(),
        user: input.user.trim().to_string(),
        proxy_command: input.proxy_command.trim().to_string(),
        close_to_tray: st.config().close_to_tray,
        forwards,
    };
    config::write_config(&st.dir, &cfg).map_err(|e| format!("Failed to save settings:\n{e}"))?;
    let count = cfg.forwards.len();
    st.set_config(cfg);
    st.reset_exits();
    apply_window_size(&app, count);
    st.log("config saved, restarting tunnel");
    tunnel::restart(&st);
    Ok(())
}

#[tauri::command]
fn set_close_to_tray(state: State<'_, Shared>, on: bool) -> Result<(), String> {
    let st = state.inner().clone();
    let mut cfg = st.config();
    cfg.close_to_tray = on;
    config::write_config(&st.dir, &cfg).map_err(|e| format!("Failed to save settings:\n{e}"))?;
    st.set_config(cfg);
    st.log(if on { "close hides to tray" } else { "close exits app" });
    Ok(())
}

#[tauri::command]
fn set_autostart(app: AppHandle, state: State<'_, Shared>, on: bool) -> Result<(), String> {
    let st = state.inner().clone();
    let result = if on { app.autolaunch().enable() } else { app.autolaunch().disable() };
    result.map_err(|e| format!("Failed to change autostart:\n{e}"))?;
    st.log(if on { "autostart enabled" } else { "autostart disabled" });
    Ok(())
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
            toggle_run,
            retest,
            window_close,
            window_minimize,
            exit_app,
            open_settings,
            close_settings,
            save_config,
            set_close_to_tray,
            set_autostart,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let dir = exe_dir();
            let outcome = config::load_from_dir(&dir);
            let cfg = outcome.config().clone();
            let shared: Shared = Arc::new(AppState::new(handle.clone(), dir, cfg.clone()));
            app.manage(shared.clone());

            build_tray(&handle, &shared)?;
            apply_window_size(&handle, cfg.forwards.len());

            // 設定視窗只藏不關，之後才能再打開
            if let Some(win) = app.get_webview_window(SETTINGS_WINDOW) {
                let w = win.clone();
                let st = shared.clone();
                win.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        if !st.is_exiting() {
                            api.prevent_close();
                            let _ = w.hide();
                        }
                    }
                });
            }

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

            shared.reset_exits();
            shared.log("Traytunnel started");
            match &outcome {
                LoadOutcome::Created(_) => {
                    shared.log("config created with defaults, open Settings to edit");
                }
                LoadOutcome::Migrated(_) => {
                    shared.log("config migrated from traytunnel.json");
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

            tunnel::start(&shared);
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
