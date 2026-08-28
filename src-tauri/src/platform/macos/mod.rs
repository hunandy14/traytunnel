//! macOS 實作。
//!
//! W1 只做搬移（整包 `todo!()` stub）；W3 依車道逐步補上實作。這裡的原則寫死不商量：
//!
//! * 型別與簽章要湊齊，`cargo check` 必須過——不然共用核心根本編不起來，
//!   後面的車道就無從開工；
//! * 還沒排到車道的部分一律 `todo!()` 或回明確的錯誤，**絕不回一個看起來正常的
//!   假值**。假值會讓「已經實作好了」這件事被誤判，然後在實機上以最難查的方式壞掉。
//!   唯一的例外是 [`paths::stem_marks_portable`] 與 [`paths::exe_toml_marks_portable`]，
//!   它們回 `false` 是 W3 的既定決議（macOS 不做可攜模式），不是佔位。
//!
//! W3-B（系統整合車道）已落地：`paths`（家目錄、可攜模式兩個觸發條件）、
//! `notify`（系統通知）、`sys` 的時間戳／開機自啟／`reveal_in_file_manager`。
//! C 車道也已落地：`sys` 的圖示尺寸／挑層。`sys` 的 `is_listening`、`spawn`、
//! `update` 分別是 A／A／W4 車道的範疇，仍是 stub。

mod notify;
mod paths;
mod spawn;
mod sys;
pub mod update;

pub use notify::{prepare_notifications, show_notification};
pub use paths::{exe_toml_marks_portable, home_dir, stem_marks_portable};
pub use spawn::ProcessSupervisor;
pub use sys::{
    autostart_enabled, disable_autostart, enable_autostart, is_listening, large_icon_size,
    local_time_hms, pick_icon_layer, read_autostart_command, reveal_in_file_manager,
    small_icon_size,
};
