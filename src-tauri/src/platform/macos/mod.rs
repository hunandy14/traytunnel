//! macOS 實作——**目前整包都是 stub**。
//!
//! W1 只做搬移，真正的實作是 W3 的事。這裡的原則寫死不商量：
//!
//! * 型別與簽章要湊齊，`cargo check` 必須過——不然共用核心根本編不起來，
//!   後面的車道就無從開工；
//! * 執行面一律 `todo!()` 或回明確的錯誤，**絕不回一個看起來正常的假值**。
//!   假值會讓「已經實作好了」這件事被誤判，然後在實機上以最難查的方式壞掉。
//!   唯一的例外是 [`paths::stem_marks_portable`]，它回 `false` 是 W3 的既定決議
//!   （macOS 不做可攜模式），不是佔位。

mod notify;
mod paths;
mod spawn;
mod sys;
pub mod update;

pub use notify::{prepare_notifications, show_notification};
pub use paths::{home_dir, stem_marks_portable};
pub use spawn::ProcessSupervisor;
pub use sys::{
    autostart_enabled, disable_autostart, enable_autostart, is_listening, large_icon_size,
    local_time_hms, pick_icon_layer, read_autostart_command, reveal_in_file_manager,
    small_icon_size,
};
