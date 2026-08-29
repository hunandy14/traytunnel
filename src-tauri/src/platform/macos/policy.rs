//! Activation policy 門面實作：把散在 `lib.rs` 三處的
//! `#[cfg(target_os = "macos")] app.set_activation_policy(...)` 收進這裡，
//! 讓呼叫端（`show_main`／`hide_to_tray`／`setup`）改叫平台門面，不必自己包
//! cfg。Windows 沒有「activation policy」這個概念（工作列圖示是系統內建，
//! 不需要動態前景／背景切換），對應的三支 no-op 在
//! `platform::windows::policy`，同名同簽章。
//!
//! 三支函式都吃 `&AppHandle`，統一走 `AppHandle::set_activation_policy`
//! （回 `Result`）；`initial_policy_for_tray_start` 在 `setup()` 內用的是
//! `&mut App`，呼叫端自己 `.handle()` 轉一次，門面這層不用再分兩種簽章。
//!
//! # 為什麼 `initial_policy_for_tray_start` 也吃 `&AppHandle`：三個時機的實測
//!
//! 這一段是本輪 review 的 M1 留下來的**否證紀錄**，寫在這裡是為了不要有人
//! 再走一次同樣的路。當時的假設是：R3 把這一支從 `App::set_activation_policy`
//! （`&mut App`）改成 `AppHandle::set_activation_policy` 是一次回歸，因為前者
//! 寫的是 tao 的 aux state（`applicationDidFinishLaunching:` 會拿去套用的那份
//! 預設值），後者只是往事件迴圈丟一則 `Message::SetActivationPolicy`
//! （tauri-runtime-wry-2.11.4 `lib.rs` 第 2736 行），排在 launch 後面。
//!
//! 假設的前半是對的，結論是錯的——**因為 `setup` 根本不在那個區間裡**。
//! tauri 不是在 `Builder::build()` 裡跑 setup 的，而是等事件迴圈發出
//! `RuntimeRunEvent::Ready` 才跑（tauri-2.11.5 `app.rs` 的
//! `make_run_event_loop_callback`），而 `App::run()` 一開頭就把 runtime
//! `take()` 走了。於是 setup 裡的 `App::set_activation_policy` 只會落進它自己
//! 的 `else` 分支，退回 `AppHandle` 那條排隊路。**R3 前後在行為上完全等價，
//! 那不是回歸。**
//!
//! 三個時機都實測了（`--tray` 啟動，隔離 HOME 與 identifier，
//! `lsappinfo info <pid>` 每 50ms 取樣一次，另外用
//! `NSWorkspace.frontmostApplication` 對焦點取樣）：
//!
//! ```text
//!                     lsappinfo type 序列                 NSWorkspace 前景 app
//! setup + AppHandle   BackgroundOnly → Foreground → UIElement   不變（沒搶焦點）
//! setup + &mut App    BackgroundOnly → Foreground → UIElement   不變（與上一列逐格相同）
//! build 與 run 之間   BackgroundOnly → UIElement                **變成 traytunnel**
//! ```
//!
//! 第三列確實把那一格 `Foreground`（Dock 圖示閃一下，約 50–100ms）消掉了，
//! 代價卻更大：tao 的 `AppState::launched` 在套完 policy 之後**無條件**呼叫
//! `activateIgnoringOtherApps(true)`（tao-0.35.3
//! `platform_impl/macos/app_state.rs`，aux 的 `activate_ignoring_other_apps`
//! 預設就是 `true`，tauri 沒有轉出來的介面可以關掉它）。policy 在那一刻若
//! 已經是 `Accessory`，這次 activate 會成功，app 就**一直**是前景（實測 12
//! 秒都沒有還回去）——使用者原本在打字的視窗當場失去焦點，而 Accessory 又不
//! 進 Cmd+Tab，只能自己去點一下別的視窗才拿得回來。policy 若還是預設的
//! `Regular`（前兩列），這次 activate 不會生效，焦點原封不動。
//!
//! 拿「50ms 的 Dock 圖示」換「永久搶走鍵盤焦點」是虧的，所以**維持原狀**：
//! 呼叫端仍在 `setup` 裡用 `AppHandle` 呼叫。
//!
//! 順帶記一筆：`tauri build` 出來的 `.app` 那一格 `Foreground` 本來就消不掉。
//! 有 bundle 時 LaunchServices 是在行程 check-in 時**照 `Info.plist` 註冊**
//! 的，比我們任何一行程式碼都早；要拿掉只能在 bundle 設定裡加 `LSUIElement`
//! ——那會連一般啟動（要開視窗那條）一起改掉，是另一個決定，不在這裡處理。
//! 實測（同一份執行檔外面包一層最小 bundle）兩種寫法都會先出現一格
//! `Foreground`。

use tauri::AppHandle;

/// 視窗要出現之前先把 activation policy 切回 Regular：選單列／Dock 圖示
/// 才會跟著視窗一起接管（純系統匣的 Accessory 沒有選單列，也不進 Dock／
/// Cmd+Tab）。刻意排在 `w.show()` 之前而不是之後——Apple Developer Forums
/// 對這顆 API 的長年迴響是「切完 policy 立刻操作視窗容易撞到第一次開啟
/// 閃一下」，讓 AppKit 先吃到 policy 變更、視窗操作晚一步跟上，比兩者
/// 同一瞬間做完更穩。
pub fn enter_foreground(app: &AppHandle) {
    if let Err(e) = app.set_activation_policy(tauri::ActivationPolicy::Regular) {
        log::warn!("could not switch to the Regular activation policy: {e}");
    }
}

/// 視窗收起來就回 Accessory：Dock 圖示與選單列跟著消失，回到純系統匣常駐
/// 的樣子，跟 `enter_foreground` 對稱。
pub fn retire_to_tray(app: &AppHandle) {
    if let Err(e) = app.set_activation_policy(tauri::ActivationPolicy::Accessory) {
        log::warn!("could not switch to the Accessory activation policy: {e}");
    }
}

/// 啟動時的起始 policy：純 tray 常駐，不要 Dock 圖示、不要出現在 Cmd+Tab
/// 切換器。traytunnel 是系統匣工具，沒有「一般 App」該有的存在感（對應
/// Windows 沒有工作列圖示、只在系統匣的既有行為）。要趁還沒建視窗、建系統匣
/// 之前定調，免得使用者先看到一閃而過的 Dock 圖示——不論這次啟動最後是
/// `-tray` 常駐還是直接開窗，都先落在 Accessory，開窗那條路徑會再由
/// `enter_foreground` 切回 Regular（見 `lib.rs::show_main` 的呼叫順序）。
///
/// **不要為了「早一點生效」把它搬到 `build()` 與 `run()` 之間**（那是唯一能
/// 寫進 tao aux state 的位置）：那樣確實會少掉一格 `Foreground`，但會讓
/// launch 時的 `activateIgnoringOtherApps` 生效並永久搶走鍵盤焦點。三個時機的
/// 實測數據與結論見模組開頭。
pub fn initial_policy_for_tray_start(app: &AppHandle) {
    if let Err(e) = app.set_activation_policy(tauri::ActivationPolicy::Accessory) {
        log::warn!("could not switch to the Accessory activation policy: {e}");
    }
}
