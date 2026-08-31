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
    platform::enter_foreground(app);
    if let Some(w) = app.get_webview_window(MAIN_WINDOW) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        // 視窗藏著的時候被系統回收掉 content process 的話，reload 被延到
        // 這裡才做——理由見 WEBVIEW_NEEDS_RELOAD 那段。
        reload_if_pending(&w, "show");
    }
}

// ------------------------------------------------- content process 回收後的自癒
//
// WKWebView 的渲染跑在獨立的系統行程（com.apple.WebKit.WebContent），系統在
// 記憶體壓力下可以把它單獨回收掉；行程死了 WKWebView 不會自己重載，畫面就
// 一直卡在白屏。tauri 的 `on_web_content_process_terminate` 掛鉤能接到這件事，
// 我們在那裡呼叫 `Webview::reload` 自救。
//
// **但不可以無條件立刻 reload。** 觸發這顆掛鉤的前提就是系統正在缺記憶體，
// 而被優先犧牲的又正是「沒有可見視窗、切到 Accessory」的 app（見
// `show_main`／`hide_to_tray` 那組動態 activation policy）。視窗藏著的時候
// 立刻 reload 等於馬上重新生一個 WebContent 行程出來畫一份沒有人在看的頁面
// ——記憶體壓力沒有解除，系統再回收一次，掛鉤再 reload 一次，一路轉圈，
// 把本來只是「下次打開要重載」的小事變成持續燒 CPU 與記憶體的迴圈。
//
// 分兩條路：
//
// * **視窗可見**：真的有人在看那片白屏，立刻 reload，這是原本的行為。
// * **視窗藏著**：只立旗標（外加一行 warn），什麼都不畫。等視窗真的回到使用者
//   面前才 reload，那時他本來就在等畫面，重生一個 WebContent 行程是划算的。
//
// 欠下的那次 reload 有**兩個**還款點，缺一不可，兩邊都走同一支
// [`reload_if_pending`]（底下的 [`take_pending_reload`] 是 `swap(false)`，
// 保證只還一次）：
//
// * `show_main`——系統匣的 Open window、第二實例喚醒、Windows 的雙擊圖示。
// * 主視窗的 `WindowEvent::Focused(true)`（見 `setup` 裡那顆處理常式）——
//   補的是**最小化**這一格。`is_visible()` 對最小化的視窗回 false，所以那時
//   被回收只會立旗標；但從 Dock 圖示或 Mission Control 還原一扇最小化的視窗
//   完全不經過 `show_main`（它沒有被 `hide_to_tray` 收起來過，policy 一直是
//   Regular）。只靠 `show_main` 的話那次 reload 永遠還不掉。
static WEBVIEW_NEEDS_RELOAD: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 回收發生時該怎麼辦。抽成純函式是為了能單獨測——這條分支就是上面那段
/// 說明的全部內容，而它本身跟 webview 無關。
///
/// 掛 `cfg(target_os = "macos")` 不是因為這條規則有平台特性，是因為
/// `on_web_content_process_terminate` 這顆掛鉤本身只有 macOS／iOS 有
/// （Windows 的 WebView2 沒有對應事件），Windows 上連呼叫端都不存在。旗標與
/// [`take_pending_reload`] 反而是跨平台的：`show_main` 與視窗的 `Focused(true)`
/// 兩邊都會問一次，Windows 上那顆旗標永遠是 false，行為零變化。
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReloadPlan {
    /// 有人在看，立刻重載。
    Now,
    /// 沒人在看，立旗標等下次前景化。
    Defer,
}

#[cfg(target_os = "macos")]
fn plan_reload_after_terminate(window_visible: bool) -> ReloadPlan {
    if window_visible {
        ReloadPlan::Now
    } else {
        ReloadPlan::Defer
    }
}

/// 領取「欠一次 reload」的旗標：有的話回 true 並就地清掉，重複呼叫只會生效
/// 一次（`show_main` 每次開窗都會問，不能每次都重載）。
///
/// 收旗標的參數而不是直接讀 [`WEBVIEW_NEEDS_RELOAD`]，是為了讓測試能拿自己的
/// 一顆來測，不必碰行程全域狀態。
fn take_pending_reload(flag: &std::sync::atomic::AtomicBool) -> bool {
    flag.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// 欠下的那次 reload 的**還款動作**：領得到旗標就重載，領不到就什麼都不做。
///
/// 兩個還款點（`show_main` 與主視窗的 `Focused(true)`）逐字做同一件事，只差
/// warn 那一行的字尾，所以抽在這裡；`why` 就是那個字尾（`"show"`／`"focus"`），
/// 使用者回報時看得出是哪一條路把畫面救回來的。
fn reload_if_pending(win: &tauri::WebviewWindow, why: &str) {
    if !take_pending_reload(&WEBVIEW_NEEDS_RELOAD) {
        return;
    }
    log::info!("reloading the webview that was reclaimed while hidden");
    if let Err(e) = win.reload() {
        log::warn!("could not reload the webview on {why}: {e}");
    }
}

// ------------------------------------------------- 白屏診斷（前端就緒複查）
//
// 白屏這個症狀在日誌裡本來完全沒有痕跡：視窗開出來（macOS 紅綠燈畫得好好的，
// Windows 自繪標題列也一樣），只有 webview 內容是一片白，Rust 這一側從頭到尾
// 不會有任何一行不對勁。追這個症狀已經耗掉三輪（#62 的 vite base、#63 的
// content process 自癒），每一輪都卡在同一件事上——**沒有辦法從日誌分辨
// 「前端根本沒載進來」與「載進來但沒畫出來」**。這條診斷本身跟平台無關：
// `tauri-codegen` 在 dev 模式又設了 `build.devUrl` 時嵌的是空資產，裸
// `cargo build` 產出的執行檔在 Windows／macOS 兩邊會踩到同一種白屏，因此不
// 用 `#[cfg(target_os = "macos")]` 圈住。
//
// 下面兩樣東西只寫日誌，不改任何行為，目的就是把這條分界線畫進 traytunnel.log：
//
// 1. 啟動時記一行 webview 實際的 URL。`tauri build` 產出的正式版一定是
//    `tauri://localhost`（內嵌前端）；直接 `cargo build` 產出的執行檔則是
//    `build.devUrl`（tauri.conf.json 目前設的是 `http://localhost:1420`），
//    單獨執行時 Vite 沒在跑，畫面**必然**一片白。README「建置」章節寫了這件
//    事，但日誌看不出來，於是每次有人拿 `cargo build` 的執行檔驗 UI 就會
//    重新踩一次。
// 2. 寬限時間內前端一次都沒有回報就緒就記一行 warn，並把 URL 一起帶上。
//    涵蓋上面那條 dev 路徑，也涵蓋前端載進來了卻沒能跑起來的情形。
//
// 與 macOS 專屬的 `on_web_content_process_terminate` 那個 warn 是同一套
// 思路：使用者再回報白屏時，traytunnel.log 要能一行定位是哪一種成因（那顆
// 掛鉤本身是 macOS／iOS 專屬的 tauri API，沒有 Windows 對應項，維持
// `#[cfg(target_os = "macos")]`）。
//
// 只記日誌對「拿到裸執行檔卻不知道要另外跑 Vite」的人幫助有限——他們十之八九
// 不會去翻 traytunnel.log，只會看到一片白就回報成 bug。寬限時間到、確認是
// dev URL（`build.devUrl`）又真的沒有前端回報就緒時，額外把空白的 webview
// `navigate` 到一個內嵌好說明文字的 `data:` URL，把「這是預期行為」直接畫在
// 畫面上。
//
// ## 守衛為什麼是「前端自己回報」，不是 page-load 事件
//
// 上一版的守衛是 `Builder::on_page_load` 立起來的 `Started`／`Finished` 兩顆
// 旗標：看到 `Finished` 就算載成功，看到 `Started` 就算「dev server 有回應、
// 只是還沒畫完」，兩者都不覆蓋畫面。這條守衛在 macOS 的 WKWebView 上成立，
// 在 Windows 的 WebView2 上**不成立**：連線被拒絕時 WebView2 不是讓導航失敗，
// 而是**自己畫一頁錯誤頁**——那一頁是真的載入了，`Started` 與 `Finished` 都
// 會照發。`PAGE_LOAD_FINISHED` 於是恆為真，複查提早 return，說明頁在 Windows
// 上永遠不會出現。原本註解宣稱的「平台中立」是假的：同一份程式碼在兩個平台
// 上做的是兩件不同的事，而且壞掉的那一邊沒有任何訊號。
//
// 換成前端就緒信標之後就不再依賴 webview 後端的導航語意：`main.ts` 的啟動鏈
// 跑完會 invoke 一次 `frontend_ready`（見 `commands::frontend_ready`），那是
// **我們自己的前端真的跑起來了**的唯一證據——WebView2 的錯誤頁、WKWebView
// 的空白頁都不可能發出這個 invoke。兩個平台這才真的走同一套語意。
//
// 選 `navigate` 而不是 `eval`：實測過 `eval`／`eval_with_callback`，兩者在
// macOS 的 WKWebView 後端完全沒有作用（不報錯、callback 也不會觸發）——原因
// 是 wry 的 WKWebView 後端把 `evaluateJavaScript` 呼叫閘在 `pending_scripts`
// 佇列後面，只有 `didCommitNavigation`（wry-0.55.1
// `src/wkwebview/navigation.rs` 的 `did_commit_navigation`）才會把佇列真的
// 送進 webview 執行；dev URL 連線被拒絕是在 provisional navigation 階段就
// 失敗，從來不會走到 `didCommitNavigation`，所以佇列裡的 script 永遠停在
// 排隊狀態，`eval` 形同沒打中。`navigate` 是全新的一次導航請求，不吃這個
// 佇列，`data:` URL 也不需要任何網路連線就能被直接當成一份完整文件載入，
// 兩邊實測都能穩定畫出來；`navigate` 本身是 tauri 的跨平台 API，不是
// WKWebView 專屬能力，Windows 走同一份程式碼。

/// 前端就緒信標。`main.ts` 的啟動鏈跑完會 invoke `frontend_ready`
/// （`commands::frontend_ready` → [`mark_frontend_ready`]），這裡是唯一落點。
static FRONTEND_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `commands::frontend_ready` 的落點：前端說它已經跑起來了。
///
/// 記一行 info 是刻意的——它與上面那行 `main webview url:` 湊成一對，使用者
/// 回報白屏時，traytunnel.log 有沒有這一行就是「前端根本沒載進來」與「載進來
/// 但沒畫出來」的分界線，而那正是整段診斷存在的理由。
///
/// 但**只在 false → true 那一次記**：這支指令是前端每載入一次就叫一次，而
/// 頁面重載不只使用者按得到——`WEBVIEW_NEEDS_RELOAD` 的自癒、content process
/// 被回收後的重生都會再跑一輪啟動鏈。旗標本來就冪等（第二次起 `swap` 回
/// `true`，狀態不變），日誌不該跟著重複；那行字要維持「這次執行前端起來過」
/// 的分界線語意，不是計次器。
pub(crate) fn mark_frontend_ready() {
    if !FRONTEND_READY.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::info!("the frontend reported ready");
    }
}

/// 前端回報就緒的寬限時間。dev 走的是本機 http，遠遠用不到這麼久；訂得寬是
/// 為了讓那行 warn 只在真的載不到時才出現。
///
/// 跟著整條複查一起掛 `cfg(dev)`：正式產物不跑這條路，留著只會是一個沒有人
/// 用的常數（`tauri build` 會如實報 dead_code）。
#[cfg(dev)]
const PAGE_LOAD_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// 寬限時間到時的四種結局。抽成純函式（[`verdict`]）是為了讓它可以單獨測——
/// 這是整段診斷唯一真的有分支的地方，其餘都是 I/O。
#[cfg(dev)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadVerdict {
    /// 前端回報過就緒，什麼都不必做。
    Ready,
    /// 沒就緒，但 webview 指的不是 dev server：只記一行 warn。這種情況畫面上
    /// 是什麼我們並不知道（可能正在慢慢載），不該擅自覆蓋掉。
    WarnOnly,
    /// 沒就緒、指著 `build.devUrl`，而那個 host:port **敲得到**：dev server
    /// 就在那裡，只是還沒把第一份模組送到（Vite 冷機時 pre-bundling ＋ 抓幾十
    /// 個 ESM 模組很容易超過寬限時間）。記一行 warn 就好，**永不接管畫面**。
    WaitForDevServer,
    /// 沒就緒、指著 `build.devUrl`，而那個 host:port **連不上**：這才是真的
    /// 「裸執行檔配沒開的 Vite」。記 warn 並把說明頁蓋上去。
    ShowDevNotice,
}

/// 寬限時間到之後要做什麼，只看三顆布林。
///
/// 第三顆 `dev_server_reachable` 是對 `build.devUrl` 的 host:port 敲一次 TCP
/// 的結果（見 [`dev_server_reachable`]）。加這一顆是因為原本的兩顆布林會把
/// **正常的 `npm run dev`** 誤判成「沒有開發伺服器」：Vite 冷機第一次啟動要
/// 做 dependency pre-bundling 再送幾十個 ESM 模組，超過 5 秒是常態，而一旦
/// 被 `navigate` 到說明頁，之後前端就算送出 `frontend_ready` 也回不去，HMR
/// 一併死掉——開發者只能重開。多敲這一次 TCP 之後，**只有 dev server 真的
/// 不在**（連線被拒／逾時）才會接管，與 `tauri dev` CLI 自己等 dev server
/// 的判準同一套語意。
///
/// 前兩格（`frontend_ready` 為真，或根本不是 dev URL）的結論與探測無關，
/// 呼叫端因此可以在那兩格直接傳 `false` 省下那一次連線，不必真的去敲。
#[cfg(dev)]
fn verdict(frontend_ready: bool, is_dev_url: bool, dev_server_reachable: bool) -> LoadVerdict {
    if frontend_ready {
        LoadVerdict::Ready
    } else if !is_dev_url {
        LoadVerdict::WarnOnly
    } else if dev_server_reachable {
        LoadVerdict::WaitForDevServer
    } else {
        LoadVerdict::ShowDevNotice
    }
}

/// 「等到寬限時間都沒等到前端」那行 warn。兩種結局（[`LoadVerdict::WarnOnly`]
/// 與 [`LoadVerdict::ShowDevNotice`]）共用同一段字，抽出來才不會哪天只改一邊。
#[cfg(dev)]
fn blank_window_warning(url_text: &str) -> String {
    format!(
        "the frontend has not reported ready {}s after start (webview url {url_text}), the \
         window will be blank; a binary from a plain `cargo build` points at build.devUrl and \
         needs `npm run web:dev` running alongside it, build with `tauri build` for one that \
         stands alone",
        PAGE_LOAD_GRACE.as_secs()
    )
}

/// 敲一次 dev server 的 host:port，看它在不在。連得上就是在。
///
/// 只用 TCP 連線、不發 HTTP 請求：要回答的問題是「有沒有人在那個埠上聽」，
/// 三次交握就已經給完答案；發 GET 反而要處理 Vite 對未知路徑的各種回應。
///
/// 位址由 `Url` 的 host 與 port 現組（`port_or_known_default` 讓沒寫埠的
/// `http://…` 也有 80 可用），不寫死 `127.0.0.1:1420`——`build.devUrl` 是設定，
/// 不是規格。`to_socket_addrs` 一併涵蓋兩種寫法：字面位址（含 IPv6 的
/// `[::1]` 方括號形式）直接 parse，主機名則走系統解析。
///
/// 解析結果可能同時有 IPv4 與 IPv6（`localhost` 就是），**逐個試到第一個連得
/// 上為止**：Vite 預設只綁一族，只試第一個會把「綁在另一族」誤判成沒開。
/// 逾時給 1 秒——對象是本機，連得上的話遠遠用不到；這個數字只是「不要在
/// 一個沒人聽的位址上無限等下去」的上限。
#[cfg(dev)]
fn dev_server_reachable(dev_url: Option<&tauri::Url>) -> bool {
    use std::net::ToSocketAddrs as _;

    const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

    let Some(url) = dev_url else {
        return false;
    };
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        log::debug!("build.devUrl ({url}) has no host:port to probe");
        return false;
    };
    let addrs = match format!("{host}:{port}").to_socket_addrs() {
        Ok(addrs) => addrs,
        Err(e) => {
            log::debug!("could not resolve the dev server address {host}:{port}: {e}");
            return false;
        }
    };
    addrs.into_iter().any(|addr| std::net::TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok())
}

/// 空白 webview 要 `navigate` 過去的說明頁。深色底、繁體中文，讓拿到裸執行檔的
/// 人一看畫面就知道這是預期行為，不必先去翻日誌。
///
/// `cfg(dev)` 與 `tauri::is_dev()` 是同一個述詞（兩者同源於 tauri 的
/// `custom-protocol` feature：tauri-build 依 `DEP_TAURI_DEV` 設這個 cfg 別名，
/// `tauri::is_dev()` 則是 `!cfg!(feature = "custom-protocol")`），所以正式產物
/// 裡這份字串連同整條複查一起**根本不會被編進去**，不是靠執行期分支繞過。
#[cfg(dev)]
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
      這支執行檔由「cargo build」直接產生，沒有內嵌前端，等了 5 秒仍未收到
      前端的就緒回報，代表 Vite 開發伺服器沒有在跑。
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

/// 記下主 webview 的來源，並在寬限時間後複查一次前端有沒有真的跑起來；沒有、
/// 又指著 dev URL、**而且那個 dev server 敲不到**的話，才把說明頁 `navigate`
/// 進空白的 webview。
///
/// 那道 TCP 探測是接管前的最後一道閘（見 [`verdict`]）：`npm run dev` 冷機時
/// Vite 的 pre-bundling 超過寬限時間是常態，少了這道閘，正常的開發流程會被
/// 說明頁劫持，而且回不去。
///
/// `dev_url` 的單一來源是 `app.config().build.dev_url`（呼叫端傳進來，見
/// `setup()`），不在這裡寫死 `"http://localhost:1420"`——那是目前
/// tauri.conf.json 的值，不是規格；改成比對 `Url::origin()`，不寫死字串也
/// 不受路徑／查詢字串影響。
///
/// 那行 `main webview url:` 無論哪種建置都要記（它是白屏回報的第一條線索）；
/// 複查本身只有 dev 建置才有意義，正式產物走的是內嵌資產的自訂協定，比對不上
/// 任何 dev_url，整段連同說明頁的 HTML 一起編不進去（見
/// [`DEV_BUILD_NOTICE_HTML`] 的說明：`cfg(dev)` 就是 `tauri::is_dev()`）。
fn watch_first_page_load<R: tauri::Runtime>(
    win: &tauri::WebviewWindow<R>,
    dev_url: Option<tauri::Url>,
) {
    let actual_url = win.url();
    let url_text =
        actual_url.as_ref().map(|u| u.to_string()).unwrap_or_else(|_| "<unknown>".to_string());
    log::info!("main webview url: {url_text}");

    #[cfg(not(dev))]
    let _ = (actual_url, dev_url, url_text);

    #[cfg(dev)]
    {
        // 有沒有指向 dev server 這件事在啟動當下就能判斷完，搬進 async 區塊前先
        // 算好，區塊裡不必再重新 parse 一次字串。
        let is_dev_url = match (&actual_url, &dev_url) {
            (Ok(actual), Some(dev)) => actual.origin() == dev.origin(),
            _ => false,
        };
        let win = win.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(PAGE_LOAD_GRACE).await;
            let ready = FRONTEND_READY.load(std::sync::atomic::Ordering::Relaxed);
            // 探測只在「沒就緒又指著 dev server」那一格才有意義（另外兩格的
            // 結論與它無關，見 `verdict` 的說明），`&&` 的短路正好省掉那一次
            // 連線。`connect_timeout` 是同步阻塞的（最多 1 秒 × 解析出來的
            // 位址數），丟進阻塞執行緒池，不要佔著 tokio 的工作執行緒。
            let dev_server_alive = !ready
                && is_dev_url
                && tauri::async_runtime::spawn_blocking(move || {
                    dev_server_reachable(dev_url.as_ref())
                })
                .await
                .unwrap_or(false);
            match verdict(ready, is_dev_url, dev_server_alive) {
                LoadVerdict::Ready => {}
                // dev server 就在那裡，只是還沒把第一份模組送過來。**不接管**
                // ——蓋上說明頁之後前端再送 `frontend_ready` 也回不去，HMR 一起
                // 死掉，開發者只能重開；等它自己載完才是對的。
                LoadVerdict::WaitForDevServer => log::warn!(
                    "the frontend has not reported ready {}s after start, but the dev server at \
                     {url_text} answers a TCP connect: it is probably still pre-bundling, \
                     leaving the webview alone",
                    PAGE_LOAD_GRACE.as_secs()
                ),
                // 畫面上是什麼我們並不知道（可能正在慢慢載），不擅自覆蓋。
                LoadVerdict::WarnOnly => log::warn!("{}", blank_window_warning(&url_text)),
                // 指著 dev server、那個埠又沒人在聽：這才是「裸執行檔配沒開的
                // Vite」，把說明頁蓋上去。
                LoadVerdict::ShowDevNotice => {
                    log::warn!("{}", blank_window_warning(&url_text));
                    use base64::Engine as _;
                    let encoded =
                        base64::engine::general_purpose::STANDARD.encode(DEV_BUILD_NOTICE_HTML);
                    let data_url = format!("data:text/html;charset=utf-8;base64,{encoded}");
                    match tauri::Url::parse(&data_url) {
                        Ok(notice_url) => {
                            if let Err(e) = win.navigate(notice_url) {
                                log::warn!(
                                    "could not navigate the blank webview to the dev-build \
                                     notice: {e}"
                                );
                            }
                        }
                        Err(e) => log::warn!("could not build the dev-build notice data: URL: {e}"),
                    }
                }
            }
        });
    }
}

/// 系統匣氣泡通知。掛名（Windows 的 AUMID）與實際怎麼彈都在平台層，
/// 這裡只決定「掛在誰名下、標題寫什麼」。
fn balloon(app: &AppHandle, body: &str) {
    platform::show_notification(&app.config().identifier, "Traytunnel", body);
}

/// 通知裡「怎麼重新打開視窗」那句尾巴。
///
/// 手勢本身（雙擊圖示／從選單挑 Open window）由
/// [`platform::TRAY_OPEN_GESTURE_HINT`] 提供，這裡只接尾巴。**兩個平台各抄一份
/// 完整句子是不行的**：那句話描述的是 `build_tray` 的點擊政策，兩處分開放就會
/// 漂——改了政策忘了改文案，通知就會教使用者做一個做不到的手勢，而且沒有任何
/// 東西擋得住。門面常數與點擊政策同源（見門面那段說明），這裡拿到的一定是對的。
fn reopen_hint() -> String {
    format!("{} to reopen.", platform::TRAY_OPEN_GESTURE_HINT)
}

/// 同 [`reopen_hint`]，給「本來就沒開過視窗」那條路徑用（`--tray` 啟動）。
fn open_hint() -> String {
    format!("{} to open.", platform::TRAY_OPEN_GESTURE_HINT)
}

fn hide_to_tray(state: &Shared) {
    if let Some(w) = state.app.get_webview_window(MAIN_WINDOW) {
        let _ = w.hide();
    }
    // 視窗收起來就回 Accessory：Dock 圖示與選單列跟著消失，回到純系統匣常駐
    // 的樣子，跟 show_main 的 Regular 對稱。
    platform::retire_to_tray(&state.app);
    if state.take_tray_hint() {
        balloon(&state.app, &format!("Closed to tray, still running. {}", reopen_hint()));
    }
}

fn do_exit(state: &Shared) {
    state.shutdown();
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
    state.shutdown();
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
/// 三道保險，順序本身就是規格：
///
/// 1. **寬限計時器要在 `do_exit` 之前起跑**，而且是從「收到訊號」那一刻起算，
///    不是從「收尾做完」起算。送訊號的人（使用者、launchd 的登出流程）等的是
///    行程結束，`do_exit` 自己卡住（例如某把鎖被別的執行緒抓著不放）也一樣要
///    有人把行程帶走；排在後面的話這個計時器根本不會被建立。
/// 2. **`do_exit` 用 `catch_unwind` 包住**。`kill_all_jobs` 裡有好幾處
///    `.lock().unwrap()`，鎖一旦中毒（別的執行緒持鎖時 panic）這裡就會跟著
///    panic；沒有這層包裝的話 panic 會直接把訊號執行緒炸掉，`signals.forever()`
///    的迴圈就沒了——之後每一顆 SIGTERM 都被 signal-hook 的 handler 吃掉、
///    卻再也沒有人處理，行程從此殺不死（`kill` 沒反應，只剩 `kill -9`）。
///    `AssertUnwindSafe` 在這裡是誠實的：唯一跨越邊界的是 `Arc<AppState>`，
///    而它內部的狀態就算被 panic 留在半途，我們接下來也只是要退出而已。
/// 3. **第二次訊號直接硬退出**（`130` 是 shell 對 `SIGINT` 終止的慣例碼）。
///    保留「按不動就再按一次 Ctrl+C」這個所有人都有的肌肉記憶，不必等寬限跑完。
///
///    這一條的前提是**前一顆訊號已經被處理完**：`signals.forever()` 是一條
///    迴圈，`on_signal` 沒回來就不會取下一顆訊號。所以第一顆卡在 `do_exit`
///    裡面時，再按幾次 Ctrl+C 都不會走到這個分支——那種情況由 (1) 的寬限
///    計時器收場，它是獨立的執行緒，不受這條迴圈影響。兩者各管一種卡法：
///    這一條管「收尾做完了但事件迴圈不理人」，計時器管「連收尾本身都卡住」。
///
/// ## 硬退出這條路會少做什麼
///
/// `process::exit` 不會發 `RunEvent::Exit`，所以兩顆外掛的收尾都不會跑，
/// 明列在這裡免得日後有人以為它是無損的：
///
/// * **single-instance 的 `/tmp/<id>_si.sock` 會留在磁碟上。** 無害：那顆
///   socket 沒有人在 listen，下一次啟動 `connect` 會拿到 `ConnectionRefused`，
///   外掛自己就把它清掉再重新 listen（見該外掛 `platform_impl/macos.rs`）。
/// * **這一次的視窗位置／大小不會被存下來**，下次開窗回到上一次存過的幾何。
///
/// 刻意**不**在這裡補一句「存一下視窗狀態」：`tauri-plugin-window-state` 的
/// `save_window_state` 要去問每一扇視窗的位置與大小，那些呼叫會被派回主執行緒
/// ——而走到硬退出這條路的前提就是主執行緒已經不回應了，補這一句只會讓收尾
/// 卡在同一個地方，把「至少會結束」也一起賠掉。
#[cfg(target_os = "macos")]
fn install_signal_exit(state: &Shared) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 從**收到訊號**起算，給收尾與事件迴圈的寬限。
    const GRACE: std::time::Duration = std::time::Duration::from_secs(3);

    static SIGNALS_SEEN: AtomicUsize = AtomicUsize::new(0);

    let st = state.clone();
    let installed = platform::install_termination_handler(move |name| {
        if SIGNALS_SEEN.fetch_add(1, Ordering::SeqCst) > 0 {
            log::warn!("received {name} again, leaving immediately");
            std::process::exit(130);
        }
        log::warn!("received {name}, killing every supervised process tree before exiting");

        // (1) 先起跑再收尾——理由見上面那段
        std::thread::spawn(|| {
            std::thread::sleep(GRACE);
            log::warn!("still here {}s after the signal; leaving the hard way", GRACE.as_secs());
            std::process::exit(0);
        });

        // (2) 收尾 panic 不可以把這條訊號執行緒帶走
        let st = st.clone();
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            st.log(format!("received {name}, exiting"));
            do_exit(&st);
        }))
        .is_err()
        {
            log::error!(
                "the shutdown path panicked while handling {name} (a poisoned lock?); \
                 the grace timer will take the process down"
            );
        }
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
///
/// 「目前執行檔的路徑本身就不該被登記」這一格由 `platform::enable_autostart`
/// 自己擋（macOS 的 App Translocation：從 dmg／`~/Downloads` 直接開啟時
/// `current_exe()` 是一條這次執行才存在的隨機掛載點路徑，覆寫進去等於把使用者
/// 原本好好的自啟弄壞）。這裡刻意不重複那道判斷、也不加任何平台 cfg：拒絕會以
/// `Err` 回來，走下面既有的「refresh failed」那一支，訊息原樣進活動日誌。
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

// ---------------------------------------------------------------- 前端平台旗標
//
// `<html data-platform="...">` 是前端唯一的平台視覺分歧點（見 styles.css 的
// `[data-platform="macos"]` 規則：mac 上藏自繪的 −/× 換原生紅綠燈、幫紅綠燈
// 留左上角空間）。舊做法是 vite.config.ts 的 htmlPlatformPlugin 在**建置期**
// 依 Node 的 `process.platform` 蓋進 `dist/index.html`——建置機的 OS 跟執行機
// 保證一致這個前提，只在「每次都重新建置」時成立；`dist` 產物一旦跨機重用
// （例如把 CI 產物搬到別台機器、或重複利用舊建置目錄），蓋進去的值就是錯的，
// 而且沒有任何訊號會告訴你錯了。
//
// 改成**執行期**由 Rust 端決定：值來自 `cfg(target_os = "macos")`，跟執行機
// 保證一致，不必像 `@tauri-apps/plugin-os` 那樣引入新依賴或執行期偵測。用官方
// 的 webview initialization script（`tauri::plugin::Builder::js_init_script`，
// 語意等同各別 webview 的 `initialization_script`：在全域物件建立後、任何頁面
// 自己的 script 執行之前跑）在頁面載入前把值寫進 `<html data-platform>`，
// vite.config.ts 的 htmlPlatformPlugin 與 index.html 的佔位字串因此可以整段
// 刪掉。
//
// 「全域物件建好了」是這顆 API **唯一**的保證，`<html>` 在不在是另一回事，
// 兩個 webview 後端在這一點上不一樣——腳本因此不能只寫一行直接設。整段理由
// 與作法在 `PLATFORM_FLAG_INIT_JS` 的說明。
//
// 值直接用 `std::env::consts::OS`，不自己抄一組 `cfg` 常數：那組常數要抄的
// 正是編譯目標本身，而標準函式庫已經有一份**由編譯器填的**同義字串，兩者
// 不可能不一致。手抄版多了兩行 `cfg`、多了一個「哪天加第三個平台會忘記補」
// 的洞，換不到任何東西。目前兩個目標的值分別是 `"macos"` 與 `"windows"`，
// 與原本手抄的兩個字面值逐字相同，styles.css 的
// `[data-platform="macos"]` 選擇器不必動。
const PLATFORM_FLAG: &str = std::env::consts::OS;

/// 把 [`PLATFORM_FLAG`] 寫進 `<html data-platform>` 的初始化腳本模板
/// （`__PLATFORM__` 由 [`platform_flag_init_script`] 換掉）。
///
/// ## 為什麼不能只寫一行 `document.documentElement.dataset.platform = …`
///
/// 初始化腳本在兩個平台跑的時機**不一樣**，而舊寫法只在其中一邊成立：
///
/// * **WKWebView**（macOS，wry 走 `WKUserScript` 的 `AtDocumentStart`）：
///   文件的 `<html>` 元素這時已經建好了，直接設就會中。
/// * **WebView2**（Windows，wry 走 `AddScriptToExecuteOnDocumentCreated`）：
///   那顆 API 保證的是「global object 建好了」，**HTML 還沒開始解析**，
///   `document.documentElement` 是 `null`——舊寫法在這裡拋 TypeError，
///   Windows 於是永遠沒有 `data-platform`，與 index.html／這個模組宣稱的
///   「執行期保證一致、不存在沒有屬性的 frame」正好相反。
///
/// 所以先試直接設；設不到就掛一顆 `MutationObserver` 監聽 `document` 的
/// `childList`，`<html>` 一被建出來就補寫並 `disconnect()`。
///
/// **不用 `DOMContentLoaded`**：那要等整份文件解析完，中間 CSS 已經套完一輪，
/// mac 上會先畫出一組自繪的 −/×（`[data-platform="macos"]` 那條規則還沒生效）
/// 再閃掉——正是要避免的 FOUC。MutationObserver 在 `<html>` 出現的那個
/// microtask 就補上，早於任何樣式套用。
const PLATFORM_FLAG_INIT_JS: &str = r#"(function () {
  var flag = "__PLATFORM__";
  function stamp() {
    var el = document.documentElement;
    if (!el) { return false; }
    el.dataset.platform = flag;
    return true;
  }
  if (stamp()) { return; }
  var observer = new MutationObserver(function () {
    if (stamp()) { observer.disconnect(); }
  });
  observer.observe(document, { childList: true });
})();"#;

/// 初始化腳本的成品。抽成函式是為了讓 [`PLATFORM_FLAG`] 真的被換進去這件事
/// 測得到——模板裡留著沒換掉的佔位字串會是一個完全無聲的錯誤。
fn platform_flag_init_script() -> String {
    PLATFORM_FLAG_INIT_JS.replace("__PLATFORM__", PLATFORM_FLAG)
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

    // ---------------------------------------------------------------- GUI 啟動的 PATH
    //
    // macOS 專屬。launchd 給 GUI 行程（Finder 雙擊、`open`、我們的 LaunchAgent）
    // 的 `PATH` 只有 `/usr/bin:/bin:/usr/sbin:/sbin`，Homebrew 裝的 `cloudflared`
    // （ssh `ProxyCommand` 的預設值）不在裡面，於是 GUI 啟動的實例每一條隧道都在
    // `sh: cloudflared: not found` 上失敗。整段理由與作法在
    // `platform::macos::sys` 的「GUI 啟動的 PATH」那一節。
    //
    // **位置也是規格**：`set_var` 改的是行程共用的環境區塊，必須在任何執行緒生出來
    // 之前（`tauri::Builder` 之前）跑完。排在更新交棒之後不衝突——macOS 沒有那套
    // 暫存交棒，上面那一行在這個平台是語意正確的 no-op。
    //
    // Windows 不需要也沒有這支函式（GUI 行程本來就繼承使用者 `PATH`），因此門面
    // 上整段 cfg，這裡給一個空的清單讓底下的日誌重播不必分平台。
    #[cfg(target_os = "macos")]
    let path_notes = platform::fix_gui_launch_path();
    #[cfg(not(target_os = "macos"))]
    let path_notes: Vec<String> = Vec::new();

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
        // 前端平台旗標（見上方 PLATFORM_FLAG 與 PLATFORM_FLAG_INIT_JS 說明）：
        // 頁面解析前把 data-platform 寫進 <html>，取代建置期蓋章。這是一個只帶
        // init script、沒有 invoke handler 的迷你外掛，不是真的要接 JS 那一側的
        // 訊息——`tauri::plugin::Builder` 本身就是官方 API，不算新依賴。
        .plugin(
            tauri::plugin::Builder::<_, ()>::new("platform-flag")
                .js_init_script(platform_flag_init_script())
                .build(),
        )
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
            commands::frontend_ready,
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
    //
    // **reload 的時機由視窗可不可見決定**，不是無條件立刻做：觸發這顆掛鉤的
    // 前提就是系統缺記憶體，藏著的時候立刻重生一個 WebContent 行程去畫沒有人
    // 在看的頁面，只會被系統再回收一次，一路轉圈。規則整段寫在
    // `plan_reload_after_terminate` 上面。
    #[cfg(target_os = "macos")]
    let builder = builder.on_web_content_process_terminate(|webview| {
        let visible = webview
            .window()
            .is_visible()
            // 問不到就當作看得見：立刻重載最多是多花一次重生，判成藏著卻其實
            // 有人在看的話，那片白屏會一直留到使用者自己去點系統匣才好。
            .unwrap_or(true);
        match plan_reload_after_terminate(visible) {
            ReloadPlan::Now => {
                log::warn!(
                    "webview content process terminated (label: {}), reloading to self-heal",
                    webview.label()
                );
                if let Err(e) = webview.reload() {
                    log::warn!(
                        "could not reload the webview after content process termination: {e}"
                    );
                }
            }
            ReloadPlan::Defer => {
                log::warn!(
                    "webview content process terminated (label: {}) while the window was hidden, \
                     deferring the reload until the window is shown again",
                    webview.label()
                );
                WEBVIEW_NEEDS_RELOAD.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
    });

    builder
        .setup(|app| {
            // 純 tray 常駐：不要 Dock 圖示、不要出現在 Cmd+Tab 切換器。traytunnel
            // 是系統匣工具，沒有「一般 App」該有的存在感（對應 Windows 沒有工作列
            // 圖示、只在系統匣的既有行為）。要趁還沒建視窗、建系統匣之前定調，
            // 免得使用者先看到一閃而過的 Dock 圖示。Windows 是 no-op（見
            // `platform::initial_policy_for_tray_start` 的門面說明）。
            //
            // 這一行看起來「太晚」而且確實太晚：setup 跑在事件迴圈的
            // `RuntimeRunEvent::Ready` 上，比 tao 的
            // `applicationDidFinishLaunching:` 還後面，所以 `--tray` 啟動時
            // Dock 圖示會先閃一格（約 50–100ms）才消失。本輪 review（M1）把
            // 這件事當成回歸查過，三個時機全部實測——搬到唯一真正更早的位置
            // （`build()` 與 `run()` 之間）確實能把那一格消掉，代價卻是讓 tao
            // 無條件執行的 `activateIgnoringOtherApps` 生效並**永久搶走鍵盤
            // 焦點**，比原本更糟。**維持原狀**，數據與結論全記在
            // `platform::macos::policy` 模組開頭。
            platform::initial_policy_for_tray_start(app.handle());

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
                watch_first_page_load(&win, app.config().build.dev_url.clone());

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
                let focus_target = win.clone();
                win.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        if !st.is_exiting() {
                            api.prevent_close();
                            close_main(&st);
                        }
                    }
                    // 視窗重新拿到焦點時把欠下的那次 reload 還掉。
                    //
                    // `show_main` 已經有一份同樣的檢查，這一條補的是**繞過
                    // `show_main` 的那條路**：`is_visible()` 對**最小化**的視窗
                    // 回 false，content process 若在那時被回收就只會立旗標
                    // （見 `WEBVIEW_NEEDS_RELOAD` 那段）；而從 Dock 圖示或
                    // Mission Control 還原一扇最小化的視窗**不會經過我們任何
                    // 一支函式**——它沒有被 `hide_to_tray` 收起來過，policy 也
                    // 一直是 Regular，`show_main` 根本不會被呼叫。少了這一條，
                    // 那次 reload 就永遠還不掉，畫面一路留白到使用者去系統匣
                    // 點「Open window」為止（舊碼在這一格是立刻 reload，這是
                    // 延後 reload 之後才開出來的窄縫）。
                    //
                    // 兩條路各領一次不會重複 reload：領旗標的 `take_pending_reload`
                    // 是 `swap(false)`，`show_main` 先領走的話這裡拿到的就是 false。
                    //
                    // 用 `matches!` 併成一個條件、不寫成 match arm 加 guard：
                    // `reload_if_pending` 會改狀態（它領旗標），藏進 match guard
                    // 就變成「條件沒過但旗標已經被領走」，那是最不該放在 guard
                    // 裡的那種副作用。
                    if matches!(event, WindowEvent::Focused(true)) {
                        reload_if_pending(&focus_target, "focus");
                    }
                });
            }

            shared.refresh_tray();
            shared.log("Traytunnel started");
            // PATH 修正跑在 logger 裝上之前（見 `run()` 開頭），它的話要在這裡才記得到
            for note in path_notes {
                shared.log(note);
            }
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
                balloon(&handle, &format!("Started in the system tray. {}", open_hint()));
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
///
/// tauri 把 `TrayIconBuilder::on_menu_event` 與 `App::on_menu_event` 註冊進同一份
/// 全域監聽清單（`TrayIcon::on_menu_event` 官方文件原話：「called for any menu
/// event, whether it is coming from this window, another window or from the tray
/// icon menu」），所以 macOS 選單列的 Cmd+Q（`platform::MENU_QUIT_ID`）也會呼叫
/// 到這裡——早退避免落進下面的 catch-all，誤記一行「unhandled tray menu id」；
/// quit 本身已經由 `setup()` 裡的 `app.on_menu_event` 處理過（呼叫 `do_exit`）。
fn on_tray_menu(app: &AppHandle, st: &Shared, id: &str) {
    #[cfg(target_os = "macos")]
    if id == platform::MENU_QUIT_ID {
        return;
    }
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

    // 圖示與「它是不是 template image」一起由平台門面決定（`platform::tray_icon`）。
    // 兩件事必須同源：舊做法在這裡分兩段 cfg——一段挑圖（macOS 先試 template PNG，
    // 解不開退回彩色 ICO），另一段無條件 `icon_as_template(true)`——退路一旦踩到就
    // 會拿彩色圖去套 template，AppKit 只讀 alpha 重畫剪影，系統匣上是一團走樣的
    // 黑影。門面回 `(Image, bool)` 之後這個分岔在型別上就不成立了，這三段 cfg
    // 也跟著消失。挑不到任何圖時寧可先把系統匣建起來，也不要讓整支程式 panic。
    let icon = platform::tray_icon(app);
    if icon.is_none() {
        log::warn!("no tray icon available, building the tray without one");
    }
    let st = shared.clone();
    let mut builder = TrayIconBuilder::with_id(TRAY_ID);
    if let Some((icon, template)) = icon {
        // 依明暗模式（與選取狀態）自動套色只認 template image，這是 macOS 專屬的
        // 概念；Windows 恆 false，這個呼叫在那邊本來就是 no-op
        builder = builder.icon(icon).icon_as_template(template);
    }
    builder = builder
        .tooltip("Traytunnel")
        .menu(&menu)
        .on_menu_event(move |app, event| on_tray_menu(app, &st, event.id().as_ref()));

    // 點擊行為（D4）：Windows 維持現行「左鍵開主視窗、右鍵開選單」一字不動；
    // macOS 採平台慣例——左右鍵一律開選單，開主視窗只留在選單的「Open window」項
    // （traymenu::menu_model 一律附上這一項，見 traymenu.rs），不額外接雙擊處理常式。
    //
    // **這兩段 cfg 與 `platform::TRAY_OPEN_GESTURE_HINT` 綁定**：通知裡教使用者
    // 做的手勢就是這裡設定的政策。改了這裡就要去改那個常數（各平台的
    // `trayicon` 子模組），否則通知會教一個做不到的手勢。
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// 白屏複查的判斷（M13）：**唯一**讓說明頁蓋上去的組合是「前端沒回報就緒」
    /// 且「webview 指著 build.devUrl」且「那個 dev server 敲不到」。
    ///
    /// 舊版守衛看的是 `on_page_load` 的 Started／Finished，而 WebView2 對連線
    /// 失敗的錯誤頁也照發那兩顆事件——說明頁在 Windows 上因此永遠不會出現。
    /// 換成前端自己 invoke 的就緒信標之後，兩個平台走的才是同一套語意；這條
    /// 測試釘住的就是那張真值表。
    ///
    /// 第三顆輸入（TCP 探測結果）是 CORE-1 補的：少了它，冷機時 Vite
    /// pre-bundling 超過 5 秒的**正常** `npm run dev` 會被說明頁劫持，而且
    /// 之後前端回報就緒也回不去。這裡直接注入布林，測試本身不連任何線。
    #[cfg(dev)]
    #[test]
    fn dev_notice_only_when_the_frontend_is_silent_and_the_dev_server_is_gone() {
        // 前端回報就緒 → 不管是不是 dev URL、dev server 在不在，都不必做任何事
        for is_dev_url in [true, false] {
            for reachable in [true, false] {
                assert_eq!(verdict(true, is_dev_url, reachable), LoadVerdict::Ready);
            }
        }
        // 沒回報就緒但不是 dev URL（例如正式協定真的取不到資源）→ 只記一行
        // warn，畫面上是什麼我們不知道，不擅自覆蓋。探測結果在這一格不影響結論
        assert_eq!(verdict(false, false, false), LoadVerdict::WarnOnly);
        assert_eq!(verdict(false, false, true), LoadVerdict::WarnOnly);
        // 沒回報就緒、指著 dev server，而 dev server 敲得到 → 它只是還在
        // pre-bundling，永遠不接管
        assert_eq!(verdict(false, true, true), LoadVerdict::WaitForDevServer);
        // 沒回報就緒、指著 dev server，那個埠又沒人在聽 → 這才是要蓋說明頁的
        // 那一格（裸執行檔配沒開的 Vite）
        assert_eq!(verdict(false, true, false), LoadVerdict::ShowDevNotice);
    }

    /// 探測本身的兩道退場：沒有 dev_url、或那個 URL 沒有 host 可以敲，
    /// 一律回 false（＝當作 dev server 不在）。
    ///
    /// 真的去連線的那一格不在這裡測——那要靠一個真的在聽的埠，屬於整合層；
    /// 這條只釘住「拿不到位址時不會 panic，也不會誤判成連得上」。
    #[cfg(dev)]
    #[test]
    fn the_dev_server_probe_gives_up_without_a_host() {
        assert!(!dev_server_reachable(None), "沒有 dev_url 就沒有東西可以敲");
        let no_host = tauri::Url::parse("data:text/html,hi").expect("data: URL 要 parse 得起來");
        assert!(!dev_server_reachable(Some(&no_host)), "data: URL 沒有 host:port 可以敲");
    }

    /// content process 被回收時的處置（M14）：可見才立刻重載。
    ///
    /// 藏著的時候立刻 reload 會馬上重生一個 WebContent 行程去畫沒有人在看的
    /// 頁面——而觸發這顆掛鉤的前提正是系統缺記憶體，於是它再被回收、再 reload，
    /// 一路轉圈。這條測試釘住「藏著就只立旗標」。
    #[cfg(target_os = "macos")]
    #[test]
    fn a_hidden_window_defers_the_reload() {
        assert_eq!(plan_reload_after_terminate(true), ReloadPlan::Now);
        assert_eq!(plan_reload_after_terminate(false), ReloadPlan::Defer);
    }

    /// 欠下的那次 reload 只還一次：`show_main` 每次開窗都會問這顆旗標，
    /// 沒有「拿走即清掉」的話，回收過一次之後每次開窗都會白重載一遍。
    #[test]
    fn the_pending_reload_flag_is_taken_exactly_once() {
        let flag = AtomicBool::new(false);
        assert!(!take_pending_reload(&flag), "沒欠就不該回 true");

        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(take_pending_reload(&flag), "欠了一次要領得到");
        assert!(!take_pending_reload(&flag), "領過就該清掉，不可以每次開窗都重載");
    }

    /// 前端旗標的預設是「還沒就緒」，`mark_frontend_ready` 之後就是就緒，
    /// 而且**重複叫是冪等的**——頁面重載（使用者自己、或
    /// `WEBVIEW_NEEDS_RELOAD` 的自癒）會讓前端再叫一次，狀態不能被搞亂，
    /// 那行 info 也只該在第一次出現（見 `mark_frontend_ready` 的說明）。
    ///
    /// 這顆旗標是行程全域的，所以只在這一條測試裡碰它（`verdict` 那條走的是
    /// 純函式，不依賴全域狀態）。
    #[test]
    fn the_frontend_ready_beacon_flips_the_flag() {
        assert!(!FRONTEND_READY.load(std::sync::atomic::Ordering::Relaxed));
        mark_frontend_ready();
        assert!(FRONTEND_READY.load(std::sync::atomic::Ordering::Relaxed));
        mark_frontend_ready();
        assert!(FRONTEND_READY.load(std::sync::atomic::Ordering::Relaxed));
    }

    /// 前端平台旗標就是編譯目標本身，不再手抄一組 cfg 常數（M18）。
    /// styles.css 的 `[data-platform="macos"]` 選擇器吃的是這個值。
    #[test]
    fn the_platform_flag_matches_the_build_target() {
        #[cfg(target_os = "macos")]
        assert_eq!(PLATFORM_FLAG, "macos");
        #[cfg(windows)]
        assert_eq!(PLATFORM_FLAG, "windows");
    }

    /// 初始化腳本真的把旗標換進去了，而且**不會只寫一行直接設**（CORE-2）。
    ///
    /// WebView2 的 `AddScriptToExecuteOnDocumentCreated` 在 HTML 解析前就跑，
    /// 那時 `document.documentElement` 是 null；少了 `MutationObserver` 那條
    /// 退路，Windows 上這行腳本會拋 TypeError、`data-platform` 永遠不存在，
    /// 而且完全無聲。這條測試釘住兩件事：佔位字串換掉了，退路還在。
    #[test]
    fn the_platform_flag_init_script_survives_a_null_document_element() {
        let js = platform_flag_init_script();
        assert!(!js.contains("__PLATFORM__"), "佔位字串沒被換掉，前端會拿到一個假的平台名");
        assert!(js.contains(&format!("\"{PLATFORM_FLAG}\"")), "腳本裡要有這個編譯目標的旗標");
        assert!(js.contains("MutationObserver"), "documentElement 為 null 時要有退路");
        assert!(js.contains("disconnect"), "補寫完要把 observer 收掉");
    }

    /// 通知裡那句手勢提示由平台門面常數接出來，兩種語境共用同一份描述（M18）。
    /// 文案與改動前逐字相同——這條測試釘的就是「沒有因為改結構而動到使用者
    /// 看得到的字」。
    #[test]
    fn the_reopen_hints_read_exactly_as_before() {
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                reopen_hint(),
                "Choose \"Open window\" from the tray icon's menu to reopen."
            );
            assert_eq!(open_hint(), "Choose \"Open window\" from the tray icon's menu to open.");
        }
        #[cfg(windows)]
        {
            assert_eq!(reopen_hint(), "Double-click the tray icon to reopen.");
            assert_eq!(open_hint(), "Double-click the tray icon to open.");
        }
    }
}
