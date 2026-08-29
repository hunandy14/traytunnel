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
//!
//! ## Drop 涵蓋不到的那些退出路徑
//!
//! Windows 的 Job Object 有 `KILL_ON_JOB_CLOSE`，那是**核心**的保證；這裡的
//! Drop 是**使用者空間的程式碼**，行程被 `SIGKILL`／當掉／被 `kill <pid>` 的
//! 預設 SIGTERM 帶走時一次都不會跑。差額由三道防線補起來，缺一不可：
//!
//! | 退出路徑 | 誰接住 |
//! |---|---|
//! | 正常退出（系統匣 Exit、選單 Quit、關閉鈕） | `lib.rs::do_exit` → `kill_all_jobs` |
//! | Dock 的 Quit、登出／關機的 AppleEvent | `lib.rs` 的 `RunEvent::Exit` 掛鉤 |
//! | `SIGTERM`（`kill <pid>`、launchd）／`SIGHUP`／`SIGINT`（Ctrl+C） | [`install_termination_handler`] |
//! | `SIGKILL`、真正的當機 | 下一次啟動的 [`super::pgids::sweep_leftovers`] |
//!
//! 前三條都是「還來得及跑我們自己的程式碼」，最後一條沒有——那一格只能靠把
//! pgid 寫在磁碟上、下次啟動回頭收屍，理由與誤殺的防線都寫在 [`super::pgids`]。

use std::io;
use std::sync::Mutex;

use tokio::process::{Child, Command};

use super::pgids;

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
    /// `log_context` 語意比照 Windows 版：兩個呼叫點（監看迴圈與存檔前的連線
    /// 測試）在日誌裡分得出來的前綴，也是這裡「記不到 pgid」那一行 warn 的前綴。
    /// Windows 版在「掛進 Job Object 失敗」時只 warn 不失敗，因為程序本身已經
    /// 起來了；macOS 這邊剛 spawn 成功的當下 `child.id()` 理論上一定是
    /// `Some`——`tokio::process::Child::id` 只有在子程序被 poll 到結束、pid
    /// 已經回收之後才會變成 `None`，而這裡連第一次 poll 都還沒發生——所以
    /// `None` 分支實務上不該被打到。保留它、而不是假設它不會發生，是不讓
    /// 「萬一這個保證哪天變了」變成一個悄悄不記 pgid、Drop 也悄悄殺不到東西的
    /// 靜默失敗：程序本身已經起來了，讓它跑總比為了收尾機制炸掉呼叫端好，
    /// 但至少要在日誌上留一筆讓人查得到。
    pub fn spawn(&self, cmd: &mut Command, log_context: &str) -> io::Result<Child> {
        cmd.process_group(0);
        // 命令列要在 spawn **之前**取：spawn 之後 `cmd` 照樣讀得到，但先取好
        // 才不會在「spawn 成功了、記登記簿時卻少一半資訊」之間留下順序上的疑問。
        let command = pgids::command_line(cmd.as_std());
        let child = cmd.spawn()?;
        match child.id() {
            Some(pid) => {
                let mut pgids_guard = self.pgids.lock().unwrap_or_else(|e| e.into_inner());
                pgids_guard.push(pid as i32);
                // 鎖放掉再寫檔：登記簿是磁碟 I/O，沒有理由讓它拖著這把鎖
                drop(pgids_guard);
                // 磁碟上的登記簿：這一份**只**為了「本行程被 SIGKILL／當掉，
                // Drop 沒機會跑」那一格存在，見模組開頭的表與 `pgids` 的說明。
                //
                // 這裡有一個關不起來的競態窗口：`cmd.spawn()` 回來到這一行寫完
                // 檔案之間若被 SIGKILL，那個群組就沒有被登記過，下一次啟動的
                // 收屍也認不得它。這是**本質**的——pgid 就是子程序的 pid，
                // 而 pid 要 `spawn()` 回來才知道，沒有辦法「先登記再 spawn」。
                // 窗口只有幾百微秒（一次 write + rename），而且方向是漏殺，
                // 不是誤殺；真要關掉它得改成「先寫一筆佔位、拿到 pid 再補上」，
                // 那會把「登記簿裡的每一筆都對應一個真的群組」這個讓收屍敢動手
                // 的前提換成一堆語意不明的半成品條目，划不來。
                pgids::register(pid as i32, &command);
            }
            None => {
                log::warn!(
                    "{log_context}spawned child has no pid; its process group cannot be tracked, so the supervisor will not be able to kill it later"
                );
            }
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
            // 的孫程序。下界保護（`kill(-0)` 是自砍、`kill(-1)` 是無差別廣播）
            // 與「`ESRCH` 當正常收尾」都在 `pgids::kill_group` 裡，兩個呼叫端
            // （這裡與啟動時的收屍）共用同一份。
            pgids::kill_group(pgid);
            // 殺完才退登記：順序反過來的話，正好在這中間被 SIGKILL 就會留下
            // 一個「已經從登記簿消失、卻還活著」的群組，那正是收屍要救的那一格
            pgids::unregister(pgid);
        }
    }
}

// ---------------------------------------------------------------- 終止訊號

/// 掛上 `SIGTERM`／`SIGHUP`／`SIGINT` 的處理，收到任何一個就呼叫 `on_signal`。
///
/// 涵蓋的退出路徑：`kill <pid>`（預設就是 SIGTERM）、launchd 在登出／
/// `launchctl bootout` 時對 job 送的 SIGTERM、終端機的 Ctrl+C 與掛斷。這幾條路
/// 的預設動作都是「行程當場消失」，`Drop`／`RunEvent::Exit` 一個都不會跑，
/// 於是 ssh 程序樹整棵變成孤兒——這支函式就是為了把那一格補起來。
///
/// ## 為什麼是 signal-hook 而不是自己 `sigaction`
///
/// 訊號處理常式能做的事被 async-signal-safety 卡得極死：不能配置記憶體、不能
/// 取任何一把可能已經被中斷的鎖（我們的收尾要碰 tokio 執行期與好幾把
/// `Mutex`，在 handler 裡碰它們是教科書等級的死鎖）、連 `println!` 都不行。
/// signal-hook 的作法是社群標準解：它真正掛進核心的那支 handler 只做「往一支
/// pipe 寫一個位元組」這件 async-signal-safe 的事，其餘全部在**一條普通的
/// 執行緒**上處理。於是傳進來的 `on_signal` 跑在正常的執行緒環境裡，
/// 想拿鎖、想寫日誌、想呼叫 `AppHandle::exit` 都沒問題。
///
/// 這條執行緒刻意不保留 join handle：它的生命週期就是整支程式，
/// 沒有任何一個地方需要「把訊號處理收回來」。
pub fn install_termination_handler<F>(mut on_signal: F) -> io::Result<()>
where
    F: FnMut(&'static str) + Send + 'static,
{
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGTERM, SIGHUP, SIGINT])?;
    std::thread::Builder::new().name("traytunnel-signals".into()).spawn(move || {
        for signal in signals.forever() {
            on_signal(signal_name(signal));
        }
    })?;
    Ok(())
}

/// 日誌用的名字。只認得我們自己掛的那三個，其餘不該出現。
fn signal_name(signal: i32) -> &'static str {
    match signal {
        signal_hook::consts::SIGTERM => "SIGTERM",
        signal_hook::consts::SIGHUP => "SIGHUP",
        signal_hook::consts::SIGINT => "SIGINT",
        _ => "an unexpected signal",
    }
}
