//! Windows 實作。介面清單見 [`crate::platform`]，本檔只負責湊齊那些名字。
//!
//! `winsys`／`aumid`／`update` 是原封不動從 `src/` 搬進來的既有模組（只改了
//! 模組路徑與 import），`spawn`／`notify`／`paths` 則是把原本散在 `ssh/tunnel.rs`、
//! `lib.rs`、`config.rs` 裡的 Windows 呼叫收攏進來的那幾件。

mod aumid;
mod notify;
mod paths;
mod spawn;
pub mod update;
mod winsys;

pub use notify::{prepare_notifications, show_notification};
pub use paths::{exe_toml_marks_portable, home_dir, stem_marks_portable};
pub use spawn::ProcessSupervisor;
pub use winsys::{
    autostart_enabled, disable_autostart, enable_autostart, is_listening, large_icon_size,
    local_time_hms, pick_icon_layer, read_run_value as read_autostart_command,
    reveal_in_explorer as reveal_in_file_manager, small_icon_size,
};
