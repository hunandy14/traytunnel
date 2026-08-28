mod appicon;
mod commands;
mod config;
mod exits;
// 平台抽象層：Windows／macOS 專屬的東西全在這底下，共用核心只准走
// `crate::platform::*` 這個門面（子模組不是 pub，走不進去）
mod platform;
mod ssh;
mod state;
mod traymenu;
mod watchdog;
// WireGuard → 本地 SOCKS5（行程內使用者態隧道）
mod wg;
mod winstate;

use std::sync::Arc;

// tunnel 搬進 ssh 模組後在此轉口，讓既有呼叫端維持 `tunnel::...` 不必改路徑
pub use ssh::tunnel;

use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};

// 更新整條路由平台提供（封裝格式綁死在各自的安裝器上），這裡轉個名字，
// 底下的呼叫端維持 `update::...` 不必改
use crate::platform::update;

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

/// 系統匣氣泡通知。掛名（Windows 的 AUMID）與實際怎麼彈都在平台層，
/// 這裡只決定「掛在誰名下、標題寫什麼」。
fn balloon(app: &AppHandle, body: &str) {
    platform::show_notification(&app.config().identifier, "Traytunnel", body);
}

fn hide_to_tray(state: &Shared) {
    if let Some(w) = state.app.get_webview_window(MAIN_WINDOW) {
        let _ = w.hide();
    }
    if state.take_tray_hint() {
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

/// 開機自啟自癒：登記的那一行命令未指向目前執行檔時，於啟動時重寫一次。
/// 涵蓋路徑失效與非本程式寫入的殘留格式——這兩種情況下 toggle 都會顯示 ON，
/// 實際卻啟動不到這支程式。
fn heal_autostart(app: &AppHandle, state: &Shared) {
    let name = state::autostart_name(app);
    if !platform::autostart_enabled(&name) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let current = platform::read_autostart_command(&name).unwrap_or_default().to_lowercase();
    if current.contains(exe.to_string_lossy().to_lowercase().as_str()) {
        return;
    }
    match platform::enable_autostart(&name, &exe) {
        Ok(()) => state.log("autostart entry refreshed"),
        Err(e) => state.log(format!("autostart entry refresh failed: {e}")),
    }
}

/// AUMID 自註冊：AUMID 字串就是 tauri.conf.json 的 identifier，必須完全一致，
/// 通知外掛在正式部署路徑下用的也是它。
fn prepare_notifications(app: &AppHandle) -> Vec<String> {
    let aumid = app.config().identifier.clone();
    let product =
        app.config().product_name.clone().unwrap_or_else(|| app.package_info().name.clone());
    let Ok(exe) = std::env::current_exe() else {
        return vec!["could not resolve the executable path for notifications".into()];
    };
    platform::prepare_notifications(&aumid, &product, &exe)
}

// ---------------------------------------------------------------- 進入點

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // ---------------------------------------------------------------- 更新交棒
    //
    // 這一段**必須是整支程式的第一件事**，位置本身就是規格：
    //
    // * 排在 single-instance 外掛註冊之前。那顆外掛一初始化就把具名互斥鎖拿在
    //   手上，而 NSIS 的靜默安裝會去找還活著的舊行程並把它關掉；我們在還沒拿
    //   任何鎖的時候就 spawn 完安裝程式並 `exit(0)`，整段互相等待完全不會發生。
    // * 排在任何 UI 之前。使用者不該看見一個一閃就消失的視窗。
    //
    // 有就緒的更新時這一行不會回來（行程直接退出），所以它前面不可以放任何
    // 有副作用的初始化。**不要把它往後搬。**
    //
    // 回來的是要補進活動日誌的行——AppState 這時還不存在，先收著，setup 裡再記。
    let update_notes = update::apply_pending_at_startup(is_tray_start());

    tauri::Builder::default()
        // single-instance 必須第一個註冊：第二個實例只負責喚醒主視窗
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }))
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
        // 原生檔案選擇器，只給 pick_wg_conf 用（Q3 裁決採用）
        .plugin(tauri_plugin_dialog::init())
        // 記住主視窗位置／大小，重啟不歸零置中。旗標不含 VISIBLE，
        // 還原完全不碰顯示狀態，理由見 winstate 模組開頭的說明
        .plugin(tauri_plugin_window_state::Builder::new().with_state_flags(winstate::flags()).build())
        // 更新外掛只在 Rust 側用（設定與公鑰讀 tauri.conf.json 的 plugins.updater），
        // 前端一律走我們自己的指令，不開它的 JS 權限
        .plugin(tauri_plugin_updater::Builder::new().build())
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
            commands::upsert_wg_proxy,
            commands::delete_wg_proxy,
            commands::set_wg_enabled,
            commands::upsert_wg_socks,
            commands::inspect_conf,
            commands::test_wg_conf,
            commands::pick_wg_conf,
            commands::test_exit,
            commands::test_connection,
            commands::set_close_to_tray,
            commands::set_autostart,
            commands::get_config_path,
            commands::open_config_dir,
            commands::set_automatic_updates,
            commands::check_for_updates_now,
            commands::install_update,
            commands::apply_update,
            commands::open_release_page,
            commands::open_releases_page,
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

            // 暫存區裡那份就緒的更新要在**畫系統匣之前**認回來，
            // 「Restart to update」才會從第一次畫的時候就在選單上，
            // 而不是等到下一次狀態變動才冒出來。
            //
            // 什麼時候真的會撈到東西：`apply_pending_at_startup` 有一條會「留著
            // 標記不套用」的路——已經有另一個實例在跑（第二實例不可以把第一實例
            // 裝掉）。那次啟動就是靠這裡把更新撈回狀態的。
            update::restore_staged(&shared);

            build_tray(&handle, &shared)?;

            if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                // 工作列的視窗按鈕吃的是 SM_CXICON（175% 下 56px），codegen 給的是
                // ICO 第一層 16px，得自己挑層再設一次才不會被 GDI 放大而模糊
                match appicon::window_icon() {
                    Some(icon) => {
                        if let Err(e) = win.set_icon(icon) {
                            log::warn!("could not set the window icon: {e}");
                        }
                    }
                    None => log::warn!("no window icon layer available, keeping the default"),
                }

                // 外掛在這個 setup 閉包跑之前就已經把 POSITION／SIZE 還原完了
                // （window 是在 Tauri 內部呼叫 setup 之前就依 tauri.conf.json 建好的），
                // 這裡補校正一次：舊設定存的尺寸可能比目前螢幕大，位置也可能落在
                // 已經拔掉的那台螢幕上
                winstate::correct_restored_geometry(&win);

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
            for note in update_notes {
                shared.log(note);
            }
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
                // 這條路徑自己已經彈過一顆通知，順帶把「關到系統匣」那顆一次性
                // 提示領掉，避免使用者第一次按 X 時再被通知一次
                let _ = shared.take_tray_hint();
                balloon(&handle, "Started in the system tray. Double-click the tray icon to open.");
            } else {
                show_main(&handle);
            }

            // enabled 的出口開機就自己連，兩型連線都算。先記一行「要連幾條」：
            // 沒有它就分不出「自動連線根本沒被觸發」與「觸發了但一條都沒起來」
            shared.log(format!(
                "starting {} enabled exit(s)",
                shared.with_config(|c| c.enabled_locals().len())
            ));
            tunnel::start_enabled(&shared);
            wg::start_enabled(&shared);
            // 十幾秒後複查一次，該在跑卻沒在跑的自己補踢一腳
            watchdog::spawn(&shared);
            // 更新檢查排在最後：它自己先睡幾秒，啟動路徑上不佔任何時間
            update::spawn_checker(&shared);
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
        traymenu::ID_RECONNECT_ALL => {
            tunnel::reconnect_all(st);
            wg::reconnect_running(st);
        }
        // 已經下載好的更新，現在就套用。
        //
        // 丟到 blocking 執行緒上：`apply_now` 要把十幾 MB 的安裝檔整個讀進來
        // 算一次 SHA-256，而選單事件是在主執行緒上處理的，同步做等於讓整個
        // 系統匣（連同主視窗）卡住那幾百毫秒。成功的話那條路不會回來。
        traymenu::ID_APPLY_UPDATE => {
            let st = st.clone();
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(e) = update::apply_now(&st) {
                    st.log(format!("update failed: {e}"));
                }
            });
        }
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
            } else if let Some(name) = id.strip_prefix(traymenu::WG_RECONNECT_PREFIX) {
                // wg 連線沒有代表性的埠，選單 id 帶的是連線名（§5.6）
                if st.with_config(|c| c.wg_proxy(name).is_some()) {
                    st.log_from(name, "reconnecting...");
                    st.reload_wg_confs();
                    wg::restart(st, name);
                } else {
                    st.log(format!("no such WireGuard connection: {name}"));
                    st.refresh_tray();
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
    let ready = shared.staged_version();
    let model = traymenu::menu_model(&shared.source_views(), &shared.wg_views(), ready.as_deref());
    let menu = traymenu::build(app, &model)?;

    // 挑不到層就退回 codegen 內建的圖示；連那個都沒有時寧可先把系統匣建起來
    // 也不要讓整支程式 panic，圖示之後照樣可以補
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
