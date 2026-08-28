//! 受監督 spawn 的 stub。
//!
//! W3：macOS 沒有 Job Object，對應物是行程群組——spawn 前
//! `pre_exec` 呼叫 `setsid()`／`setpgid()`，收尾時對整個 group 送 SIGTERM／SIGKILL。
//! 這件事一定要做：ssh 的 ProxyCommand 會再生出孫程序，只殺 ssh 會留孤兒。

use std::io;

use tokio::process::{Child, Command};

/// 一組子程序的看管者。丟掉它就等於收掉底下整棵程序樹。
#[derive(Debug)]
pub struct ProcessSupervisor(());

impl ProcessSupervisor {
    /// 刻意 `todo!()` 而不是回一個什麼都不做的空殼：空殼會讓 tunnel 照常
    /// spawn 出 ssh，然後在沒有人負責收屍的情況下留下孤兒程序。
    pub fn new() -> io::Result<ProcessSupervisor> {
        todo!("W3: macOS 的行程群組看管尚未實作")
    }

    pub fn spawn(&self, _cmd: &mut Command, _log_context: &str) -> io::Result<Child> {
        todo!("W3: macOS 的受監督 spawn 尚未實作")
    }
}

/// 「丟掉它就等於收掉整棵程序樹」是這個型別的契約，不是實作細節——呼叫端
/// （`tunnel::test_connection` 的 `drop(job)`）靠的就是它。macOS 這邊先把位子
/// 佔著，W3 補上「對整個行程群組送 SIGTERM，逾時再 SIGKILL」。
///
/// 目前 `new()` 是 `todo!()`，所以實際上永遠不會有實例走到這裡。
impl Drop for ProcessSupervisor {
    fn drop(&mut self) {}
}
