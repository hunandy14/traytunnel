//! 受監督 spawn：Unix 行程群組（process group）版本。對照組是
//! `platform/windows/spawn.rs` 的 Job Object。
//!
//! macOS 沒有 Job Object，對應物是行程群組：spawn 前用 `process_group(0)`
//! 讓子程序自成一個新群組（新群組的 pgid 就等於子程序自己的 pid，這是
//! `process_group(0)` 的定義，不必事後另外查詢），子程序自己再 spawn 出來的
//! 孫程序預設會**繼承同一個 pgid**（除非孫程序自己又呼叫 `setpgid`／`setsid`，
//! 契約測試 §2(ii) 的 `sh -c "sleep N & wait"` 沒有這麼做，這正是「一群」的
//! 前提）。Drop 時對這個 pgid 送 `SIGKILL`，一次收掉整棵樹，不必知道底下究竟
//! 有幾層、生了幾支——這件事一定要做：ssh 的 ProxyCommand 會再生出孫程序，
//! 只殺 ssh 本身會留下孤兒。
//!
//! 直接送 `SIGKILL`、不先 `SIGTERM` 給對方收拾的機會：`Drop::drop` 是同步的，
//! 沒有地方能 await「等一下看它有沒有自己退」，呼叫端（`tunnel::test_connection`
//! 的 `drop(job)`）要的就是「這一行過去之後，程序樹保證已經死了」這個更強的
//! 保證，契約測試 §2(i)(ii) 驗的正是這個。

use std::io;
use std::sync::Mutex;

use tokio::process::{Child, Command};

/// 一組子程序的看管者。丟掉它就等於收掉底下整棵程序樹
/// （對每一個記下來的 pgid 送 `SIGKILL`）。
#[derive(Debug)]
pub struct ProcessSupervisor {
    /// 正常情況下只會有一筆：呼叫端一個 supervisor 對一次 `spawn`。型別留
    /// `Vec` 是因為簽章上不禁止呼叫多次——`spawn` 拿的是 `&self`，Drop 得對得起
    /// 這個簽章允許的所有用法，不能假設呼叫端只會叫一次。
    pgids: Mutex<Vec<i32>>,
}

impl ProcessSupervisor {
    /// 先把看管者準備好，才去 spawn——順序反過來的話，spawn 與掛進去之間
    /// 會有一段「已經在跑但沒人管」的空窗。
    pub fn new() -> io::Result<ProcessSupervisor> {
        Ok(ProcessSupervisor { pgids: Mutex::new(Vec::new()) })
    }

    /// 讓子程序 spawn 前自成一個新的行程群組，spawn 之後記下它的 pgid。
    ///
    /// 呼叫端已經設好的 stdio（見契約測試：`Stdio::null()`／`Stdio::piped()`）
    /// 一律原樣尊重，這裡只多加 `process_group(0)` 這一道旗標，不碰其他任何
    /// 已經在 `cmd` 上設定好的東西。
    ///
    /// `log_context` 語意比照 Windows 版：只是為了兩個呼叫點（監看迴圈與存檔前
    /// 的連線測試）在日誌裡分得出來的前綴。Windows 版在「掛進 Job Object失敗」
    /// 時只 warn 不失敗，因為程序本身已經起來了；macOS 這邊沒有對應的「掛失敗」
    /// 分支——`process_group(0)` 是 spawn 前就設定好的旗標，`spawn()` 一旦成功，
    /// pgid 必定等於子程序自己的 pid，不會有「起來了但沒記到 pgid」的中間狀態。
    /// 這裡仍保留 `log_context` 參數只是讓兩個平台的簽章、呼叫端維持一致。
    pub fn spawn(&self, cmd: &mut Command, _log_context: &str) -> io::Result<Child> {
        cmd.process_group(0);
        let child = cmd.spawn()?;
        if let Some(pid) = child.id() {
            let mut pgids = self.pgids.lock().unwrap_or_else(|e| e.into_inner());
            pgids.push(pid as i32);
        }
        Ok(child)
    }
}

/// 「丟掉它就等於收掉整棵程序樹」是這個型別的契約，不是實作細節——呼叫端
/// （`tunnel::test_connection` 的 `drop(job)`）靠的就是它。
impl Drop for ProcessSupervisor {
    fn drop(&mut self) {
        // Drop 不可以 panic：鎖萬一中毒（某次 `spawn` 在持鎖期間 panic，理論上
        // 不會發生，因為鎖住的區間裡沒有任何會 panic 的呼叫，但這裡不賭這件事）
        // 一樣要能繼續往下送 signal，不能讓中毒直接炸穿 Drop。
        let pgids = match self.pgids.lock() {
            Ok(mut guard) => std::mem::take(&mut *guard),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        for pgid in pgids {
            // 對整個群組送 SIGKILL，一次收掉子程序與它底下所有繼承同一個 pgid
            // 的孫程序。`ESRCH`（群組裡已經沒有任何程序）當正常收尾看待，不是
            // 錯誤——子程序自己先退掉、或整棵樹早就自然結束的情況都會走到這裡，
            // 這時候「殺不到東西」正是我們要的結果，不必當一回事往外 warn。
            let rc = unsafe { libc::kill(-pgid, libc::SIGKILL) };
            if rc != 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ESRCH) {
                    log::warn!("kill process group {pgid} failed: {err}");
                }
            }
        }
    }
}
