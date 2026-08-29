//! Activation policy 門面實作：把散在 `lib.rs` 三處的
//! `#[cfg(target_os = "macos")] app.set_activation_policy(...)` 收進這裡，
//! 讓呼叫端（`show_main`／`hide_to_tray`／`setup`）改叫平台門面，不必自己包
//! cfg。Windows 沒有「activation policy」這個概念（工作列圖示是系統內建，
//! 不需要動態前景／背景切換），對應的三支 no-op 直接寫在
//! `platform::mod`（見該檔「Activation policy」那一節），不必湊一份假的
//! Windows 實作進 `platform/windows`。
//!
//! 三支函式都吃 `&AppHandle`，統一走 `AppHandle::set_activation_policy`
//! （回 `Result`）；`initial_policy_for_tray_start` 在 `setup()` 內用的是
//! `&mut App`，呼叫端自己 `.handle()` 轉一次，門面這層不用再分兩種簽章。

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
pub fn initial_policy_for_tray_start(app: &AppHandle) {
    if let Err(e) = app.set_activation_policy(tauri::ActivationPolicy::Accessory) {
        log::warn!("could not switch to the Accessory activation policy: {e}");
    }
}
