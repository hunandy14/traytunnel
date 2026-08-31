//! Activation policy 門面實作（Windows 側）：Windows 沒有「activation
//! policy」這個概念——工作列圖示與視窗系統選單本來就是系統內建，不需要
//! macOS 那種 Dock／選單列動態前景／背景切換——三支全是 no-op，純粹是為了
//! 湊齊 `crate::platform` 門面清單的同名同簽章項目，讓 `lib.rs` 的呼叫端
//! （`show_main`／`hide_to_tray`／`setup`）不必為了平台差異各自包一段
//! `#[cfg(target_os = "macos")]`。對應的 macOS 實作在
//! `platform::macos::policy`。

use tauri::AppHandle;

/// Windows 沒有 activation policy 這回事，no-op。
pub fn enter_foreground(_app: &AppHandle) {}

/// Windows 沒有 activation policy 這回事，no-op。
pub fn retire_to_tray(_app: &AppHandle) {}

/// Windows 沒有 activation policy 這回事，no-op。
pub fn initial_policy_for_tray_start(_app: &AppHandle) {}
