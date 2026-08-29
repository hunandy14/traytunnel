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

use tauri::tray::TrayIconBuilder;
// Windows 專屬的點擊行為（雙擊左鍵開主視窗）才用得到這兩個型別，
// macOS 分支整個不碰它們——見 build_tray 的 cfg 分支（D4：macOS 點擊一律開選單）
#[cfg(windows)]
use tauri::tray::{MouseButton, TrayIconEvent};
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

/// 視窗要出現之前先把 activation policy 切回 Regular：選單列／Dock 圖示
/// 才會跟著視窗一起接管（純系統匣的 Accessory 沒有選單列，也不進 Dock／
/// Cmd+Tab）。刻意排在 `w.show()` 之前而不是之後——Apple Developer Forums
/// 對這顆 API 的長年迴響是「切完 policy 立刻操作視窗容易撞到第一次開啟
/// 閃一下」，讓 AppKit 先吃到 policy 變更、視窗操作晚一步跟上，比兩者
/// 同一瞬間做完更穩。
fn show_main(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    if let Err(e) = app.set_activation_policy(tauri::ActivationPolicy::Regular) {
        log::warn!("could not switch to the Regular activation policy: {e}");
    }
    if let Some(w) = app.get_webview_window(MAIN_WINDOW) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

// ------------------------------------------------- 白屏診斷（macOS）
//
// 白屏這個症狀在日誌裡本來完全沒有痕跡：視窗開出來、紅綠燈畫得好好的，只有
// webview 內容是一片白，Rust 這一側從頭到尾不會有任何一行不對勁。追這個症狀
// 已經耗掉三輪（#62 的 vite base、#63 的 content process 自癒），每一輪都卡在
// 同一件事上——**沒有辦法從日誌分辨「前端根本沒載進來」與「載進來但沒畫出來」**。
//
// 下面兩樣東西只寫日誌，不改任何行為，目的就是把這條分界線畫進 traytunnel.log：
//
// 1. 啟動時記一行 webview 實際的 URL。`tauri build` 產出的正式版一定是
//    `tauri://localhost`（內嵌前端）；直接 `cargo build` 產出的執行檔則是
//    `http://localhost:1420`（`build.devUrl`），單獨執行時 Vite 沒在跑，畫面
//    **必然**一片白。README「建置」章節寫了這件事，但日誌看不出來，於是每次
//    有人拿 `cargo build` 的執行檔驗 UI 就會重新踩一次。
// 2. 寬限時間內沒有任何一次 page load 完成就記一行 warn，並把 URL 一起帶上。
//    涵蓋上面那條 dev 路徑，也涵蓋正式版自訂協定真的取不到資源的情形。
//
// 與底下 `on_web_content_process_terminate` 的 warn 是同一套思路：使用者再回報
// 白屏時，traytunnel.log 要能一行定位是哪一種成因。
//
// 只記日誌對「拿到裸執行檔卻不知道要另外跑 Vite」的人幫助有限——他們十之八九
// 不會去翻 traytunnel.log，只會看到一片白就回報成 bug。寬限時間到、確認是
// dev URL（`build.devUrl`）又真的沒有任何 page load 時，額外把空白的 webview
// `navigate` 到一個內嵌好說明文字的 `data:` URL，把「這是預期行為」直接畫在
// 畫面上。
//
// 選 `navigate` 而不是 `eval`：實測過 `eval`／`eval_with_callback`，兩者在這個
// 情境下完全沒有作用（不報錯、callback 也不會觸發）——原因是 wry 的 WKWebView
// 後端把 `evaluateJavaScript` 呼叫閘在 `pending_scripts` 佇列後面，只有
// `didCommitNavigation`（wry-0.55.1 `src/wkwebview/navigation.rs` 的
// `did_commit_navigation`）才會把佇列真的送進 webview 執行；`http://localhost:1420`
// 連線被拒絕是在 provisional navigation 階段就失敗，從來不會走到
// `didCommitNavigation`，所以佇列裡的 script 永遠停在排隊狀態，`eval` 形同
// 沒打中。`navigate` 是全新的一次導航請求，不吃這個佇列，`data:` URL 也不需要
// 任何網路連線就能被 WKWebView 直接當成一份完整文件載入，兩邊實測都能穩定畫出來。
// 也因此完全不影響 `tauri build` 產物：正式版走 `tauri://localhost` 自訂協定，
// 這裡的 URL 判斷直接跳過。
#[cfg(target_os = "macos")]
static PAGE_LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 前端載入的寬限時間。正式版走的是本機自訂協定、dev 走的是本機 http，兩邊
/// 都遠遠用不到這麼久；訂得寬是為了讓那行 warn 只在真的載不到時才出現。
#[cfg(target_os = "macos")]
const PAGE_LOAD_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// 空白 webview 要 `navigate` 過去的說明頁。深色底、繁體中文，讓拿到裸執行檔的
/// 人一看畫面就知道這是預期行為，不必先去翻日誌。只在下面
/// `watch_first_page_load` 判斷「dev URL 且寬限時間內沒有任何 page load」時
/// 才會被組成 `data:` URL 用掉。
#[cfg(target_os = "macos")]
const DEV_BUILD_NOTICE_HTML: &str = r##"<!doctype html>
<html lang="zh-Hant">
<head>
<meta charset="utf-8" />
<style>
  html, body {
    margin: 0;
    min-height: 100vh;
    background: #1e1e1e;
    color: #e6e6e6;
    font-family: -apple-system, BlinkMacSystemFont, "PingFang TC", "Helvetica Neue", sans-serif;
  }
  body { display: flex; align-items: center; justify-content: center; }
  .card { max-width: 520px; padding: 32px; line-height: 1.8; text-align: left; }
  h1 { font-size: 20px; margin: 0 0 12px; color: #ffb454; }
  p { margin: 0 0 12px; }
  ul { margin: 0 0 12px; padding-left: 20px; }
  .hint { margin: 0; opacity: 0.7; font-size: 13px; }
</style>
</head>
<body>
  <div class="card">
    <h1>這是開發用建置</h1>
    <p>
      這支執行檔由「cargo build」直接產生，沒有內嵌前端，等了 5 秒仍未偵測到
      任何畫面載入，代表 Vite 開發伺服器沒有在跑。
    </p>
    <p style="color:#9cdcfe;">正確的驗證方式：</p>
    <ul>
      <li>執行「npm run dev」（會一併啟動 Vite）</li>
      <li>或改用「tauri build」產出的正式版 App（不需要 Vite）</li>
    </ul>
    <p class="hint">（這則說明只在裸執行檔又沒有開發伺服器時出現，正式產物不受影響）</p>
  </div>
</body>
</html>
"##;

/// 記下主 webview 的來源，並在寬限時間後複查一次有沒有真的載完；載不完又是
/// dev URL 的話，順手把說明頁 `navigate` 進空白的 webview。
#[cfg(target_os = "macos")]
fn watch_first_page_load<R: tauri::Runtime>(win: &tauri::WebviewWindow<R>) {
    let url = win.url().map(|u| u.to_string()).unwrap_or_else(|_| "<unknown>".to_string());
    log::info!("main webview url: {url}");
    let win = win.clone();
    std::thread::spawn(move || {
        std::thread::sleep(PAGE_LOAD_GRACE);
        if PAGE_LOADED.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        log::warn!(
            "the main webview has not finished loading {url} after {}s, the window will be blank; \
             a binary from a plain `cargo build` points at build.devUrl and needs `npm run web:dev` \
             running alongside it, build with `tauri build` for one that stands alone",
            PAGE_LOAD_GRACE.as_secs()
        );
        // 只在確認是 build.devUrl（`http://localhost:1420`）又真的沒有任何
        // page load 時才 navigate；正式版的 `tauri://localhost` 不會走進這裡，
        // 對 `tauri build` 產物零影響。
        if url.starts_with("http://localhost:1420") {
            use base64::Engine as _;
            let encoded = base64::engine::general_purpose::STANDARD.encode(DEV_BUILD_NOTICE_HTML);
            let data_url = format!("data:text/html;charset=utf-8;base64,{encoded}");
            match tauri::Url::parse(&data_url) {
                Ok(notice_url) => {
                    if let Err(e) = win.navigate(notice_url) {
                        log::warn!(
                            "could not navigate the blank webview to the dev-build notice: {e}"
                        );
                    }
                }
                Err(e) => log::warn!("could not build the dev-build notice data: URL: {e}"),
            }
        }
    });
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
    // 視窗收起來就回 Accessory：Dock 圖示與選單列跟著消失，回到純系統匣常駐
    // 的樣子，跟 show_main 的 Regular 對稱。
    #[cfg(target_os = "macos")]
    if let Err(e) = state.app.set_activation_policy(tauri::ActivationPolicy::Accessory) {
        log::warn!("could not switch to the Accessory activation policy: {e}");
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

// ------------------------------------------------- 退出掛鉤（macOS）
//
// Windows 上這一整段沒有對應語意：`ProcessSupervisor` 在那邊是一個 Job Object，
// `KILL_ON_JOB_CLOSE` 是**核心**的保證——行程無論怎麼消失（正常退出、當掉、
// 被工作管理員結束、登出），核心都會把整個 job 收乾淨。
//
// macOS 的 `ProcessSupervisor` 收尾靠的是 `Drop`，那是使用者空間的程式碼，
// 而 `ssh -N` 是最不容易自己死掉的東西（stdin 是 null、父行程死了被 launchd
// 收養、SIGPIPE 被忽略）。只要有一條退出路徑跑不到 `do_exit`，那條隧道的 ssh
// 就會留下來繼續握著 `-L` 的本地埠，下一次啟動每一列都 `PORT_BUSY`、而重連是
// 無退避無上限的五秒一輪——症狀是「app 永遠連不上，重開機才好」。
//
// 底下兩支各補一條 `do_exit` 到不了的路徑，第三條（SIGKILL／當機）沒辦法在
// 當下補救，由 `platform::sweep_supervised_leftovers` 在下一次啟動收屍。

/// 事件迴圈真的要結束了（`RunEvent::Exit`）——最後一次收拾程序樹的機會。
///
/// 補的是 **Dock 圖示的 Quit 與登出／關機**：那兩個走的是 AppleEvent，
/// AppKit 呼叫 `applicationWillTerminate:`，tao 在那裡發出 `LoopDestroyed`
/// （tao-0.35.3 `platform_impl/macos/app_delegate.rs`），tauri 把它轉成
/// `RunEvent::Exit`（tauri-runtime-wry-2.11.4 `lib.rs`）；接著 NSApplication
/// 直接把行程結束掉，managed 的 `Arc<AppState>` 永遠不會被 drop，
/// `ProcessSupervisor` 的 `Drop` 當然也不會跑。
///
/// 選單列自己的 Quit 不走這條——那一項的 id 是我們自訂的
/// [`platform::MENU_QUIT_ID`]，事件路由到 `do_exit`（見 setup）。這裡用
/// `is_exiting()` 擋掉已經收過尾的情形，`do_exit` 自己觸發的 `RunEvent::Exit`
/// 因此不會再殺第二次。
#[cfg(target_os = "macos")]
fn kill_jobs_on_final_exit(app: &AppHandle, event: &tauri::RunEvent) {
    if !matches!(event, tauri::RunEvent::Exit) {
        return;
    }
    let Some(state) = app.try_state::<Shared>() else {
        // setup 都還沒跑完（或已經拆掉），沒有東西需要收
        return;
    };
    if state.is_exiting() {
        return; // do_exit 已經收過尾了
    }
    log::warn!(
        "the event loop is exiting without going through do_exit (Dock Quit or logout), \
         killing every supervised process tree now"
    );
    state.mark_exiting();
    state.kill_all_jobs();
}

/// 掛上 SIGTERM／SIGHUP／SIGINT 的收尾。
///
/// 補的是 `kill <pid>`（預設就是 SIGTERM）、launchd 在登出或 `launchctl bootout`
/// 時對 job 送的 SIGTERM、以及終端機的 Ctrl+C 與掛斷。這幾條的預設動作都是
/// 「行程當場消失」，連 `RunEvent::Exit` 都不會發出來。
///
/// 收尾內容就是 `do_exit`（收程序樹 → 請事件迴圈退出），這裡跑在 signal-hook
/// 開的那條普通執行緒上，不是訊號處理常式裡，所以拿鎖、寫日誌、呼叫
/// `AppHandle::exit` 都是安全的（見 `platform::install_termination_handler`）。
///
/// 後面那段寬限＋硬退出是給「事件迴圈根本沒有回應」準備的：那時候
/// `AppHandle::exit` 送出去的訊息沒人處理，行程會就這樣掛著，而送訊號的人
/// （使用者、launchd）等的是它結束。程序樹在 `do_exit` 那一行就已經收乾淨了，
/// 所以這裡直接離開不會留下任何東西。
#[cfg(target_os = "macos")]
fn install_signal_exit(state: &Shared) {
    /// 從「已經收完程序樹」到「還是不肯結束」之間給事件迴圈的寬限。
    const GRACE: std::time::Duration = std::time::Duration::from_secs(3);

    let st = state.clone();
    let installed = platform::install_termination_handler(move |name| {
        log::warn!("received {name}, killing every supervised process tree before exiting");
        st.log(format!("received {name}, exiting"));
        do_exit(&st);
        std::thread::spawn(|| {
            std::thread::sleep(GRACE);
            log::warn!(
                "the event loop did not exit within {}s of the signal; leaving the hard way \
                 (every supervised process tree is already gone)",
                GRACE.as_secs()
            );
            std::process::exit(0);
        });
    });
    if let Err(e) = installed {
        log::warn!("could not install the termination signal handler: {e}");
    }
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

    let builder = tauri::Builder::default()
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
        .plugin(
            tauri_plugin_window_state::Builder::new().with_state_flags(winstate::flags()).build(),
        )
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
        ]);

    // WKWebView 的渲染跑在獨立於本體行程的系統行程（com.apple.WebKit.WebContent），
    // 這個行程可以被系統獨立回收——真正的觸發條件是系統記憶體壓力（jetsam 式回收），
    // 不是單純隱藏視窗就會發生；但沒有可見視窗、又切到 Accessory（見 show_main／
    // hide_to_tray 那組動態 activation policy）的 app，它的 WebContent 行程在記憶體
    // 壓力發生時會是優先被犧牲的對象。行程死掉後 WKWebView 不會自己重新載入，
    // 畫面會一直卡在白屏，直到使用者手動整個重開 app。
    //
    // tauri 有官方掛鉤能偵測這個事件並在這裡自救：`Builder::on_web_content_process_terminate`
    // （macOS／iOS 專屬，已核對存在於我們釘死的 tauri-v2.11.5 標籤，
    // crates/tauri/src/app.rs 第 1798 行；底層是 wry 的 wkwebview/navigation.rs
    // 把 WKUIDelegate 的 webContentProcessDidTerminate: 轉呼叫上來）。tauri 本身
    // 不會自動重載，要自己呼叫 `Webview::reload`；先記一筆 warn 才有辦法從日誌
    // 回頭確認這件事真的發生過——使用者若再遇到白屏，第一步就是查
    // traytunnel.log 有沒有這行，有就是這個機制，沒有就要往別的方向查。
    // 有任何一次 page load 走完就把旗標立起來，`watch_first_page_load` 的複查
    // 靠它決定要不要記那行 warn（見上面那段說明）
    #[cfg(target_os = "macos")]
    let builder = builder.on_page_load(|_webview, payload| {
        if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
            PAGE_LOADED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    });

    #[cfg(target_os = "macos")]
    let builder = builder.on_web_content_process_terminate(|webview| {
        log::warn!(
            "webview content process terminated (label: {}), reloading to self-heal",
            webview.label()
        );
        if let Err(e) = webview.reload() {
            log::warn!("could not reload the webview after content process termination: {e}");
        }
    });

    builder
        .setup(|app| {
            // 純 tray 常駐：不要 Dock 圖示、不要出現在 Cmd+Tab 切換器。traytunnel
            // 是系統匣工具，沒有「一般 App」該有的存在感（對應 Windows 沒有工作列
            // 圖示、只在系統匣的既有行為）。要趁還沒建視窗、建系統匣之前定調，
            // 免得使用者先看到一閃而過的 Dock 圖示。
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

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

            // macOS 的程序樹收尾（Windows 由 Job Object 的 KILL_ON_JOB_CLOSE 全包，
            // 這裡整段不存在）。兩件事都要卡在這個位置：
            //
            // * 收屍必須在 single-instance 外掛確定「我們是唯一實例」之後——
            //   第二個實例是在 `Builder::build()` 裡就 `process::exit(0)` 的，
            //   走不到這個 setup 閉包，所以站在這裡就等於站在那道閘後面；
            //   若在它前面掃，會把**還在服役**的那個實例的 ssh 全部砍掉。
            // * 收屍也必須在任何一條隧道 spawn 之前（`tunnel::start_enabled`
            //   在本函式最後），上一輪殘留的 ssh 才來得及把埠讓出來。
            // * 訊號處理要趁早掛：掛好之前收到的 SIGTERM 走的是預設動作
            //   （行程當場消失），那正是要補的那條路。
            #[cfg(target_os = "macos")]
            {
                platform::sweep_supervised_leftovers();
                install_signal_exit(&shared);
            }

            // 暫存區裡那份就緒的更新要在**畫系統匣之前**認回來，
            // 「Restart to update」才會從第一次畫的時候就在選單上，
            // 而不是等到下一次狀態變動才冒出來。
            //
            // 什麼時候真的會撈到東西：`apply_pending_at_startup` 有一條會「留著
            // 標記不套用」的路——已經有另一個實例在跑（第二實例不可以把第一實例
            // 裝掉）。那次啟動就是靠這裡把更新撈回狀態的。
            update::restore_staged(&shared);

            build_tray(&handle, &shared)?;

            // macOS 標準選單列（App／Edit／Window）：沒有這份選單，WKWebView
            // 的輸入框連 Cmd+C／Cmd+V／Cmd+A 都按不動（macOS 的快捷鍵走選單系統
            // 分派，不是直接進 responder chain），見 platform::macos::menu 模組
            // 開頭的說明。Quit 項目自訂了 id，事件路由到這裡呼叫 `do_exit`，
            // 語意對齊系統匣選單既有的 Exit（`PredefinedMenuItem::quit` 會直接
            // `exit(0)`，繞過 `kill_all_jobs`，同一份模組註解裡也記了原因）。
            #[cfg(target_os = "macos")]
            {
                let menu = platform::build_menu(&handle)?;
                app.set_menu(menu)?;
                let quit_state = shared.clone();
                app.on_menu_event(move |_app_handle, event| {
                    if event.id().as_ref() == platform::MENU_QUIT_ID {
                        do_exit(&quit_state);
                    }
                });
            }

            if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                // 白屏診斷：webview 到底載到哪一份前端、有沒有載成功（見模組上方那段）
                #[cfg(target_os = "macos")]
                watch_first_page_load(&win);

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
        // `build` + `App::run(callback)` 而不是 `Builder::run(context)`：後者是
        // 前者外加一個空的事件回呼（tauri-2.11.5 `app.rs` 的
        // `Builder::run` 就是 `self.build(context)?.run(|_, _| {})`），而我們需要
        // 那個回呼——macOS 的 Dock Quit 與登出只會發出 `RunEvent::Exit`，
        // 不會經過任何一條我們自己的退出路徑。行為上兩者其餘完全相同。
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|_app, _event| {
            #[cfg(target_os = "macos")]
            kill_jobs_on_final_exit(_app, &_event);
        });
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

    // 圖示來源分平台：Windows 從內嵌 ICO 挑層（一字不動）；macOS 要一份純黑＋透明
    // 的 template PNG，彩色 ICO 硬套 icon_as_template 只會走樣。兩邊都挑不到層時
    // 一路退回 codegen 內建的圖示；連那個都沒有時寧可先把系統匣建起來也不要讓整支
    // 程式 panic，圖示之後照樣可以補
    #[cfg(windows)]
    let icon = appicon::tray_icon().or_else(|| app.default_window_icon().cloned());
    #[cfg(target_os = "macos")]
    let icon = appicon::tray_icon_template()
        .or_else(appicon::tray_icon)
        .or_else(|| app.default_window_icon().cloned());
    if icon.is_none() {
        log::warn!("no tray icon available, building the tray without one");
    }
    let st = shared.clone();
    let mut builder = TrayIconBuilder::with_id(TRAY_ID);
    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }
    // macOS 依明暗模式（與選取狀態）自動套色只認 template image；Windows 沒有這個
    // 概念，這個呼叫在 Windows 上是 no-op，但仍限定在 macOS 分支讓意圖清楚
    #[cfg(target_os = "macos")]
    {
        builder = builder.icon_as_template(true);
    }
    builder = builder
        .tooltip("Traytunnel")
        .menu(&menu)
        .on_menu_event(move |app, event| on_tray_menu(app, &st, event.id().as_ref()));

    // 點擊行為（D4）：Windows 維持現行「左鍵開主視窗、右鍵開選單」一字不動；
    // macOS 採平台慣例——左右鍵一律開選單，開主視窗只留在選單的「Open window」項
    // （traymenu::menu_model 一律附上這一項，見 traymenu.rs），不額外接雙擊處理常式
    #[cfg(windows)]
    {
        builder = builder.show_menu_on_left_click(false).on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = event {
                show_main(tray.app_handle());
            }
        });
    }
    #[cfg(target_os = "macos")]
    {
        // 與預設值相同，寫出來是為了讓這個平台分支自我說明，不必回頭翻
        // tray-icon 的 default() 才知道 mac 這邊「什麼都沒做」其實是刻意的
        builder = builder.show_menu_on_left_click(true);
    }

    builder.build(app)?;
    Ok(())
}
