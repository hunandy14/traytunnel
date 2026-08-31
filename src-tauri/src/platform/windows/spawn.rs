//! 受監督 spawn：把子程序整棵樹綁進一個 Job Object。
//!
//! 內容是從 `ssh/tunnel.rs` 的 `spawn_ssh`／`test_connection` 原樣搬出來的那三行
//! Windows-only 動作——`creation_flags(CREATE_NO_WINDOW)`、`child.raw_handle()`、
//! `job.assign(..)`。tunnel 那邊只留參數組裝與生命週期邏輯。

use std::io;

use tokio::process::{Child, Command};

use super::winsys::Job;

/// CREATE_NO_WINDOW，避免主控台視窗一閃而過
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 一組子程序的看管者。丟掉它就等於收掉底下整棵程序樹
/// （Windows：Job Object 的 handle 一關，`KILL_ON_JOB_CLOSE` 就生效）。
#[derive(Debug)]
pub struct ProcessSupervisor(Job);

impl ProcessSupervisor {
    /// 先把看管者準備好，才去 spawn——順序反過來的話，spawn 與掛進去之間
    /// 會有一段「已經在跑但沒人管」的空窗。
    pub fn new() -> io::Result<ProcessSupervisor> {
        Ok(ProcessSupervisor(Job::new()?))
    }

    /// 依平台補齊 spawn 旗標、spawn，再把新程序掛到這個看管者底下。
    ///
    /// 掛不上去只記一行警告而不失敗：程序本身已經起來了，讓它跑總比為了
    /// 一個收尾機制把連線整條斷掉好。`log_context` 是那一行警告的前綴，
    /// 讓兩個呼叫點（監看迴圈與存檔前的連線測試）在日誌裡分得出來。
    pub fn spawn(&self, cmd: &mut Command, log_context: &str) -> io::Result<Child> {
        cmd.creation_flags(CREATE_NO_WINDOW);
        let child = cmd.spawn()?;
        if let Some(handle) = child.raw_handle() {
            if let Err(e) = self.0.assign(handle as isize) {
                log::warn!("{log_context}assign ssh to job object failed: {e}");
            }
        }
        Ok(child)
    }
}
