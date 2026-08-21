//! 應用程式共用狀態，所有狀態變化都由這裡推事件給前端。
//!
//! 每個出口（以本地埠為唯一鍵）各自帶一份執行期狀態：連線狀態、自測結果、
//! 世代序號與 Job Object handle，彼此互不影響。

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_autostart::ManagerExt;

use crate::config::Config;
use crate::winsys::Job;

/// 活動日誌保留的行數上限
const LOG_CAPACITY: usize = 500;

pub const TRAY_ID: &str = "traytunnel-tray";
pub const MAIN_WINDOW: &str = "main";

/// 出口連線狀態，字面值即為 IPC 契約上的值
pub mod status {
    pub const STOPPED: &str = "stopped";
    pub const CONNECTING: &str = "connecting";
    pub const CONNECTED: &str = "connected";
    pub const RECONNECTING: &str = "reconnecting";
    pub const PORT_BUSY: &str = "port_busy";
    pub const ERROR: &str = "error";
}

/// 出口自測狀態
pub mod test_state {
    pub const TESTING: &str = "testing";
    pub const OK: &str = "ok";
    pub const FAIL: &str = "fail";
}

/// 事件：exit-status
#[derive(Debug, Clone, Serialize)]
pub struct ExitStatusPayload {
    pub local: u16,
    pub status: String,
    pub detail: Option<String>,
}

/// 事件：exit-test，同時也是 Snapshot 裡的 lastTest
#[derive(Debug, Clone, Serialize)]
pub struct ExitTestPayload {
    pub local: u16,
    pub state: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestView {
    pub state: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitView {
    pub name: String,
    pub local: u16,
    pub remote: String,
    pub enabled: bool,
    pub status: String,
    pub last_test: Option<TestView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub host: String,
    pub user: String,
    pub proxy_command: String,
    pub close_to_tray: bool,
    pub autostart: bool,
    pub exits: Vec<ExitView>,
    /// 活動日誌回放，順序由舊到新，內容與 log 事件的整行一致
    pub logs: Vec<String>,
}

/// 監看迴圈的佔位：位子有人就不發新號，避免同一個出口被起第二條 ssh。
/// 號碼是拿到位子之後才取的，沒搶到就不白燒一個世代序號。
fn claim_slot(slot: &mut Option<u64>, next: impl FnOnce() -> u64) -> Option<u64> {
    if slot.is_some() {
        return None;
    }
    let generation = next();
    *slot = Some(generation);
    Some(generation)
}

/// 退出的監看迴圈才有資格清位子，晚到的舊迴圈不能把新迴圈的位子清掉
fn release_slot(slot: &mut Option<u64>, generation: u64) {
    if *slot == Some(generation) {
        *slot = None;
    }
}

/// 推一行進環形緩衝，超過上限就丟掉最舊的，順序維持由舊到新
fn push_log_line(logs: &mut VecDeque<String>, line: String) {
    logs.push_back(line);
    while logs.len() > LOG_CAPACITY {
        logs.pop_front();
    }
}

/// 單一出口的執行期狀態
#[derive(Debug, Default)]
struct ExitRuntime {
    status: String,
    detail: Option<String>,
    last_test: Option<TestView>,
    /// 目前有效的世代序號，換號即代表舊的監看迴圈作廢；
    /// 號碼取自全域計數器，出口被刪掉又重建也不會撞號
    generation: u64,
    /// 目前活著的監看迴圈是哪一代，None 代表這個出口沒人在跑
    supervisor: Option<u64>,
    job: Option<(u64, Job)>,
}

impl ExitRuntime {
    fn new() -> Self {
        ExitRuntime { status: status::STOPPED.into(), ..Default::default() }
    }
}

pub struct AppState {
    pub app: AppHandle,
    /// 設定檔所在資料夾（執行檔同目錄）
    pub dir: PathBuf,
    cfg: Mutex<Config>,
    /// 環形緩衝，讓前端掛上監聽前（例如啟動當下）的日誌還能靠 Snapshot 補回來
    logs: Mutex<VecDeque<String>>,
    exits: Mutex<BTreeMap<u16, ExitRuntime>>,
    testing: Mutex<HashSet<u16>>,
    /// 全域世代計數器，發出去的號碼永不重複
    generation: AtomicU64,
    tray_hint_shown: AtomicBool,
    exiting: AtomicBool,
}

impl AppState {
    pub fn new(app: AppHandle, dir: PathBuf, cfg: Config) -> Self {
        let exits = cfg.forwards.iter().map(|f| (f.local, ExitRuntime::new())).collect();
        AppState {
            app,
            dir,
            cfg: Mutex::new(cfg),
            logs: Mutex::new(VecDeque::new()),
            exits: Mutex::new(exits),
            testing: Mutex::new(HashSet::new()),
            generation: AtomicU64::new(0),
            tray_hint_shown: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
        }
    }

    pub fn config(&self) -> Config {
        self.cfg.lock().unwrap().clone()
    }

    /// 就地改設定並落地存檔，回傳 Err 代表寫檔失敗（此時記憶體也不會被改動）
    pub fn update_config<F, T>(&self, edit: F) -> std::io::Result<T>
    where
        F: FnOnce(&mut Config) -> T,
    {
        let mut guard = self.cfg.lock().unwrap();
        let mut next = guard.clone();
        let out = edit(&mut next);
        crate::config::write_config(&self.dir, &next)?;
        *guard = next;
        drop(guard);
        self.sync_exits();
        Ok(out)
    }

    /// 設定裡新增或刪掉出口後，補齊／清掉對應的執行期狀態
    fn sync_exits(&self) {
        let ports: Vec<u16> = self.config().forwards.iter().map(|f| f.local).collect();
        let mut exits = self.exits.lock().unwrap();
        exits.retain(|p, _| ports.contains(p));
        for p in ports {
            exits.entry(p).or_insert_with(ExitRuntime::new);
        }
    }

    pub fn log(&self, msg: impl AsRef<str>) {
        let line = format!("{}  {}", chrono::Local::now().format("%H:%M:%S"), msg.as_ref());
        log::info!("{}", msg.as_ref());
        push_log_line(&mut self.logs.lock().unwrap(), line.clone());
        let _ = self.app.emit("log", line);
    }

    /// 更新某個出口的連線狀態並推事件；狀態沒變就不重複推。
    pub fn set_exit_status(&self, local: u16, status: &str, detail: Option<String>) {
        {
            let mut exits = self.exits.lock().unwrap();
            let rt = exits.entry(local).or_insert_with(ExitRuntime::new);
            if rt.status == status && rt.detail == detail {
                return;
            }
            rt.status = status.into();
            rt.detail = detail.clone();
        }
        let _ = self
            .app
            .emit("exit-status", ExitStatusPayload { local, status: status.into(), detail });
        self.refresh_tooltip();
    }

    pub fn exit_status(&self, local: u16) -> Option<String> {
        self.exits.lock().unwrap().get(&local).map(|r| r.status.clone())
    }

    pub fn is_connected(&self, local: u16) -> bool {
        self.exit_status(local).as_deref() == Some(status::CONNECTED)
    }

    /// 更新某個出口的自測狀態並推事件
    pub fn set_exit_test(&self, local: u16, state: &str, text: &str) {
        {
            let mut exits = self.exits.lock().unwrap();
            let rt = exits.entry(local).or_insert_with(ExitRuntime::new);
            rt.last_test = Some(TestView { state: state.into(), text: text.into() });
        }
        let _ = self.app.emit(
            "exit-test",
            ExitTestPayload { local, state: state.into(), text: text.into() },
        );
    }

    /// 出口斷線或停掉時把舊的自測結果清乾淨
    pub fn clear_exit_test(&self, local: u16) {
        let mut exits = self.exits.lock().unwrap();
        if let Some(rt) = exits.get_mut(&local) {
            rt.last_test = None;
        }
    }

    /// 標記某個埠開始測試，回傳 false 代表已經在測了
    pub fn begin_test(&self, local: u16) -> bool {
        self.testing.lock().unwrap().insert(local)
    }

    pub fn end_test(&self, local: u16) {
        self.testing.lock().unwrap().remove(&local);
    }

    /// 讓該出口進入新世代並騰出位子，舊的監看迴圈看到世代不符就會自行退出。
    /// 位子當場清掉，緊接著的 start 不必等舊迴圈醒來就能接手。
    pub fn next_generation(&self, local: u16) -> u64 {
        let next = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut exits = self.exits.lock().unwrap();
        let rt = exits.entry(local).or_insert_with(ExitRuntime::new);
        rt.generation = next;
        rt.supervisor = None;
        next
    }

    /// 搶下這個出口的監看位子，回傳 None 代表已經有一條線在跑，不要再起第二條
    pub fn claim_supervisor(&self, local: u16) -> Option<u64> {
        let counter = &self.generation;
        let mut exits = self.exits.lock().unwrap();
        let rt = exits.entry(local).or_insert_with(ExitRuntime::new);
        let claimed = claim_slot(&mut rt.supervisor, || {
            counter.fetch_add(1, Ordering::SeqCst) + 1
        });
        if let Some(generation) = claimed {
            rt.generation = generation;
        }
        claimed
    }

    /// 監看迴圈結束時歸還位子
    pub fn release_supervisor(&self, local: u16, generation: u64) {
        let mut exits = self.exits.lock().unwrap();
        if let Some(rt) = exits.get_mut(&local) {
            release_slot(&mut rt.supervisor, generation);
        }
    }

    pub fn generation(&self, local: u16) -> u64 {
        self.exits.lock().unwrap().get(&local).map(|r| r.generation).unwrap_or(0)
    }

    /// 世代還活著才算數，用來判斷監看迴圈要不要繼續
    pub fn generation_alive(&self, local: u16, generation: u64) -> bool {
        self.generation(local) == generation
    }

    pub fn store_job(&self, local: u16, generation: u64, job: Job) {
        let mut exits = self.exits.lock().unwrap();
        let rt = exits.entry(local).or_insert_with(ExitRuntime::new);
        rt.job = Some((generation, job));
    }

    /// 關掉 job handle，該出口的 ssh 程序樹一起結束
    pub fn kill_job(&self, local: u16) {
        let mut exits = self.exits.lock().unwrap();
        if let Some(rt) = exits.get_mut(&local) {
            let _ = rt.job.take();
        }
    }

    /// 只在世代相符時清掉 job，避免誤殺新的一輪連線
    pub fn kill_job_of(&self, local: u16, generation: u64) {
        let mut exits = self.exits.lock().unwrap();
        if let Some(rt) = exits.get_mut(&local) {
            if rt.job.as_ref().map(|(g, _)| *g) == Some(generation) {
                let _ = rt.job.take();
            }
        }
    }

    /// 收掉所有出口的 ssh 程序，離開程式時用
    pub fn kill_all_jobs(&self) {
        let mut exits = self.exits.lock().unwrap();
        for rt in exits.values_mut() {
            rt.generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            rt.supervisor = None;
            let _ = rt.job.take();
        }
    }

    pub fn tray_hint_shown(&self) -> bool {
        self.tray_hint_shown.swap(true, Ordering::SeqCst)
    }

    pub fn mark_tray_hint_shown(&self) {
        self.tray_hint_shown.store(true, Ordering::SeqCst);
    }

    pub fn is_exiting(&self) -> bool {
        self.exiting.load(Ordering::SeqCst)
    }

    pub fn mark_exiting(&self) {
        self.exiting.store(true, Ordering::SeqCst);
    }

    pub fn autostart(&self) -> bool {
        self.app.autolaunch().is_enabled().unwrap_or(false)
    }

    pub fn snapshot(&self) -> Snapshot {
        let cfg = self.config();
        let exits = self.exits.lock().unwrap();
        let views = cfg
            .forwards
            .iter()
            .map(|f| {
                let rt = exits.get(&f.local);
                ExitView {
                    name: f.name.clone(),
                    local: f.local,
                    remote: f.remote.clone(),
                    enabled: f.enabled,
                    status: rt
                        .map(|r| r.status.clone())
                        .unwrap_or_else(|| status::STOPPED.to_string()),
                    last_test: rt.and_then(|r| r.last_test.clone()),
                }
            })
            .collect();
        Snapshot {
            host: cfg.host,
            user: cfg.user,
            proxy_command: cfg.proxy_command,
            close_to_tray: cfg.close_to_tray,
            autostart: self.autostart(),
            exits: views,
            logs: self.logs.lock().unwrap().iter().cloned().collect(),
        }
    }

    /// 任何設定變更後全量推一次
    pub fn emit_config_changed(&self) {
        let _ = self.app.emit("config-changed", self.snapshot());
        self.refresh_tooltip();
    }

    /// 系統匣提示改成彙總，例如「Traytunnel - 2/2 connected」
    pub fn refresh_tooltip(&self) {
        let cfg = self.config();
        let exits = self.exits.lock().unwrap();
        let enabled: Vec<u16> = cfg.forwards.iter().filter(|f| f.enabled).map(|f| f.local).collect();
        let connected = enabled
            .iter()
            .filter(|p| exits.get(p).map(|r| r.status.as_str()) == Some(status::CONNECTED))
            .count();
        let text = if enabled.is_empty() {
            "Traytunnel - no exits enabled".to_string()
        } else {
            format!("Traytunnel - {}/{} connected", connected, enabled.len())
        };
        drop(exits);
        if let Some(tray) = self.app.tray_by_id(TRAY_ID) {
            let _ = tray.set_tooltip(Some(text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 啟動當下的日誌發生在前端掛上監聽之前，靠 Snapshot 的 logs 回放，
    /// 因此緩衝必須保住最新的 LOG_CAPACITY 行且順序是舊到新。
    #[test]
    fn log_buffer_keeps_newest_lines_in_order() {
        let mut logs = VecDeque::new();
        for i in 0..(LOG_CAPACITY + 100) {
            push_log_line(&mut logs, format!("line {i}"));
        }
        assert_eq!(logs.len(), LOG_CAPACITY);
        assert_eq!(logs.front().unwrap(), "line 100");
        assert_eq!(logs.back().unwrap(), &format!("line {}", LOG_CAPACITY + 99));
    }

    /// F2：start 對已經在跑的出口不能再起一條 ssh，否則舊的 ssh 還佔著埠，
    /// 新的監看迴圈會掃到自己人而誤報 port_busy
    #[test]
    fn second_claim_is_refused_while_one_is_running() {
        let mut slot = None;
        let mut seq = 0;
        let mut next = || {
            seq += 1;
            seq
        };
        assert_eq!(claim_slot(&mut slot, &mut next), Some(1));
        assert_eq!(claim_slot(&mut slot, &mut next), None);
        // 沒搶到就不該白燒世代序號
        assert_eq!(seq, 1);
    }

    #[test]
    fn released_slot_can_be_claimed_again() {
        let mut slot = None;
        assert_eq!(claim_slot(&mut slot, || 7), Some(7));
        release_slot(&mut slot, 7);
        assert_eq!(claim_slot(&mut slot, || 8), Some(8));
    }

    /// halt 之後舊迴圈才慢半拍退出，不能讓它把新迴圈的位子清掉
    #[test]
    fn stale_supervisor_cannot_release_a_newer_one() {
        let mut slot = None;
        claim_slot(&mut slot, || 1);
        slot = Some(2); // 舊的被作廢、新的已接手
        release_slot(&mut slot, 1);
        assert_eq!(slot, Some(2));
    }

    #[test]
    fn log_buffer_below_capacity_keeps_everything() {
        let mut logs = VecDeque::new();
        push_log_line(&mut logs, "a".into());
        push_log_line(&mut logs, "b".into());
        assert_eq!(logs.iter().cloned().collect::<Vec<_>>(), vec!["a", "b"]);
    }
}
