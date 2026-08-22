mod appicon;
mod aumid;
mod commands;
mod config;
mod exits;
mod state;
mod traymenu;
mod tunnel;
mod winsys;

use std::sync::Arc;

use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_winrt_notification::{IconCrop, Toast};

use config::{Config, LoadOutcome};
use state::{AppState, MAIN_WINDOW, TRAY_ID};

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
    if state.with_config(|c| c.close_to_tray) {
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
            commands::get_state,
            commands::start_exit,
            commands::stop_exit,
            commands::restart_exit,
            commands::start_source,
            commands::stop_source,
            commands::start_all,
            commands::stop_all,
            commands::upsert_source,
            commands::delete_source,
            commands::upsert_forward,
            commands::delete_forward,
            commands::test_exit,
            commands::set_close_to_tray,
            commands::set_autostart,
            commands::get_config_path,
            commands::open_config_dir,
            commands::window_close,
            commands::window_minimize,
            commands::exit_app,
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
            // 壞檔又備份不出來時，原檔是使用者僅存的一份，這次執行一律不准回寫。
            // 要趕在任何存檔路徑（含系統匣、自啟自癒）跑起來之前拉閘。
            if outcome.read_only() {
                shared.mark_read_only();
            }
            app.manage(shared.clone());

            build_tray(&handle, &shared)?;

            if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                // 工作列的視窗按鈕吃的是 SM_CXICON（175% 下 56px），codegen 給的是
                // ICO 第一層 16px，得自己挑層再設一次才不會被 GDI 放大成一團糊
                match appicon::window_icon() {
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
                LoadOutcome::CreateFailed { error, .. } => {
                    shared.log(format!("config file could not be created ({error}), using defaults"));
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
                            shared.log("settings are read-only this session, fix the config file to save again");
                            balloon(
                                &handle,
                                "Config file could not be read and no backup could be written. Settings are read-only this session.",
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
                if commands::require_source(st, name) {
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
    match st.with_config(|c| c.forward(local).map(|f| f.enabled)) {
        Some(enabled) => commands::set_exit_enabled(st, local, !enabled),
        // 選單比設定舊了（出口已經被刪掉），重建一次讓它跟上
        None => {
            st.log(format!("port {local} : no such exit"));
            st.refresh_tray();
        }
    }
}

/// 有任何出口 enabled 就是 Stop all，全停時就是 Start all
fn toggle_all(st: &Shared) {
    let all_stopped = st.with_config(|c| c.enabled_locals().is_empty());
    commands::set_all_enabled(st, all_stopped);
}

fn build_tray(app: &AppHandle, shared: &Shared) -> tauri::Result<()> {
    let menu = traymenu::build(app, &traymenu::menu_model(&shared.source_views()))?;

    // 挑不到層就退回 codegen 內建的圖示；連那個都沒有時寧可讓系統匣先長出來
    // 也不要 panic 掉整支程式，圖示之後照樣可以補
    let icon = appicon::tray_icon().or_else(|| app.default_window_icon().cloned());
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
