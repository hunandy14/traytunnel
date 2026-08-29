//! macOS 實作。
//!
//! W1 只做搬移（整包 `todo!()` stub）；W3／W4 依車道逐步補上實作。這裡的原則寫死
//! 不商量：
//!
//! * 型別與簽章要湊齊，`cargo check` 必須過——不然共用核心根本編不起來，
//!   後面的車道就無從開工；
//! * 還沒排到車道的部分一律 `todo!()` 或回明確的錯誤，**絕不回一個看起來正常的
//!   假值**。假值會讓「已經實作好了」這件事被誤判，然後在實機上以最難查的方式壞掉。
//!   唯一的例外是 [`paths::stem_marks_portable`] 與 [`paths::exe_toml_marks_portable`]，
//!   它們回 `false` 是 W3 的既定決議（macOS 不做可攜模式），不是佔位。
//!
//! W3-B（系統整合車道）已落地：`paths`（家目錄、可攜模式兩個觸發條件）、
//! `notify`（系統通知）、`sys` 的時間戳／開機自啟／`reveal_in_file_manager`／`open_url`。
//! A 車道也已落地：`sys` 的 `is_listening`、`spawn`（`ProcessSupervisor`）。
//! C 車道也已落地：`sys` 的圖示尺寸／挑層。
//! W4-U 車道也已落地：`update` 整條路（走 tauri-plugin-updater 的 `.app` bundle
//! 原地替換）。其中三支暫存相關的函式是**語意正確的 no-op** 而不是佔位——macOS
//! 的更新是即時下載安裝制，沒有 NSIS 那種暫存交棒，理由整段寫在 [`update`] 的
//! 模組說明裡。
//!
//! 視窗風格車道也已落地：`menu`（App／Edit／Window 標準選單，`build_menu`／
//! `MENU_QUIT_ID` 進了門面）。這是刻意**不對稱**的門面項目——Windows 沒有
//! 「activation policy」「app 選單列」這兩個概念，門面上這兩行整段
//! `#[cfg(target_os = "macos")]`，Windows 編譯時連符號都不存在，不必湊一份
//! 假的 Windows 實作。動態 activation policy（Accessory／Regular 切換）
//! 直接呼叫 tauri 的 `AppHandle::set_activation_policy`，屬於一行等級的
//! 外部 API 呼叫，跟既有的初始 policy 設定同一種寫法，留在 `lib.rs` 裡
//! `#[cfg(target_os = "macos")]` 內聯，沒有另外進這裡。

mod menu;
mod notify;
mod paths;
mod spawn;
mod sys;
pub mod update;

pub use menu::{build as build_menu, QUIT_ID as MENU_QUIT_ID};
pub use notify::{prepare_notifications, show_notification};
pub use paths::{exe_toml_marks_portable, home_dir, stem_marks_portable};
pub use spawn::ProcessSupervisor;
pub use sys::{
    autostart_enabled, disable_autostart, enable_autostart, is_listening, large_icon_size,
    local_time_hms, read_autostart_command, reveal_in_file_manager, small_icon_size,
};
