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
pub struct SourceView {
    pub name: String,
    pub host: String,
    pub user: String,
    pub proxy_command: String,
    pub exits: Vec<ExitView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub close_to_tray: bool,
    pub autostart: bool,
    pub sources: Vec<SourceView>,
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

/// 組一行日誌：`HH:mm:ss  [源名] 訊息`，app 級事件不帶源名。
fn format_log(source: Option<&str>, msg: &str) -> String {
    let ts = chrono::Local::now().format("%H:%M:%S");
    match source {
        Some(s) => format!("{ts}  [{s}] {msg}"),
        None => format!("{ts}  {msg}"),
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
    /// 這次執行生效的設定檔完整路徑，由 config::config_location() 解析而來；
    /// 全程式的回寫、備份與「開啟設定資料夾」都以它為準
    pub path: PathBuf,
    cfg: Mutex<Config>,
    /// 環形緩衝，讓前端掛上監聽前（例如啟動當下）的日誌還能靠 Snapshot 補回來
    logs: Mutex<VecDeque<String>>,
    exits: Mutex<BTreeMap<u16, ExitRuntime>>,
    testing: Mutex<HashSet<u16>>,
    /// 全域世代計數器，發出去的號碼永不重複
    generation: AtomicU64,
    tray_hint_shown: AtomicBool,
    exiting: AtomicBool,
    /// 設定檔壞掉又備份不出來時會被拉起來，之後一律拒絕回寫
    read_only: AtomicBool,
}

impl AppState {
    pub fn new(app: AppHandle, path: PathBuf, cfg: Config) -> Self {
        let exits = cfg.locals().into_iter().map(|p| (p, ExitRuntime::new())).collect();
        AppState {
            app,
            path,
            cfg: Mutex::new(cfg),
            logs: Mutex::new(VecDeque::new()),
            exits: Mutex::new(exits),
            testing: Mutex::new(HashSet::new()),
            generation: AtomicU64::new(0),
            tray_hint_shown: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            read_only: AtomicBool::new(false),
        }
    }

    /// 把設定切成唯讀，之後每一次 `update_config` 都會直接回 Err。
    /// 只有「設定檔壞掉且備份不出來」時會走到這裡，見 `LoadOutcome::read_only`。
    pub fn mark_read_only(&self) {
        self.read_only.store(true, Ordering::SeqCst);
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only.load(Ordering::SeqCst)
    }

    /// 唯讀地看一眼設定：閉包帶進鎖裡跑，整份 Config 不必複製。
    ///
    /// 絕大多數呼叫點只是問一個布林或抄幾個埠號，卻要為此深拷貝整份設定
    /// （源、出口、字串全部），而狀態一變就會連推好幾次事件，複製量相當可觀。
    ///
    /// 閉包裡不可以再碰任何會鎖 cfg 的方法（`update_config`、`with_config`
    /// 自己），否則當場自我死鎖。要跨鎖持有的話，在閉包裡複製需要的那幾筆
    /// 出來就好——目前全程式沒有任何一處真的需要整份 owned 設定。
    pub fn with_config<T>(&self, f: impl FnOnce(&Config) -> T) -> T {
        f(&self.cfg.lock().unwrap())
    }

    /// 就地改設定並落地存檔，回傳 Err 代表寫檔失敗（此時記憶體也不會被改動）。
    /// 唯讀模式下一律回 Err，絕不拿預設值去輾使用者那份救不回來的原檔。
    pub fn update_config<F, T>(&self, edit: F) -> std::io::Result<T>
    where
        F: FnOnce(&mut Config) -> T,
    {
        if self.is_read_only() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the config file is unreadable and could not be backed up, \
                 settings are read-only until it is fixed",
            ));
        }
        let mut guard = self.cfg.lock().unwrap();
        let mut next = guard.clone();
        let out = edit(&mut next);
        crate::config::write_config_at(&self.path, &next)?;
        *guard = next;
        drop(guard);
        self.sync_exits();
        Ok(out)
    }

    /// 設定裡新增或刪掉出口後，補齊／清掉對應的執行期狀態。
    ///
    /// 丟掉的那些 `ExitRuntime` 會連同它持有的 Job handle 一起 drop，被刪掉的
    /// 出口那條 ssh 程序樹當場就收掉了；刪除流程因此可以先存檔再停線，
    /// 不必為了收程序而搶在存檔之前 halt。
    fn sync_exits(&self) {
        let ports = self.with_config(|c| c.locals());
        let mut exits = self.exits.lock().unwrap();
        exits.retain(|p, _| ports.contains(p));
        for p in ports {
            exits.entry(p).or_insert_with(ExitRuntime::new);
        }
    }

    /// app 級事件的日誌，不帶源名
    pub fn log(&self, msg: impl AsRef<str>) {
        self.push_log(format_log(None, msg.as_ref()));
    }

    /// 某個源底下發生的事，行首多一段 `[源名]`
    pub fn log_from(&self, source: &str, msg: impl AsRef<str>) {
        self.push_log(format_log(Some(source), msg.as_ref()));
    }

    /// 依本地埠自動補上所屬源的名字；出口已經被刪掉時退回 app 級格式
    pub fn log_exit(&self, local: u16, msg: impl AsRef<str>) {
        match self.with_config(|c| c.source_name_of(local).map(str::to_string)) {
            Some(src) => self.log_from(&src, msg),
            None => self.log(msg),
        }
    }

    fn push_log(&self, line: String) {
        log::info!("{line}");
        push_log_line(&mut self.logs.lock().unwrap(), line.clone());
        let _ = self.app.emit("log", line);
    }

    /// 更新某個出口的連線狀態並推事件；狀態沒變就不重複推。
    ///
    /// 只更新既存的出口：出口一旦被刪掉，執行期狀態也跟著被 `sync_exits` 清掉，
    /// 這時晚到的狀態更新若順手把項目補回來，就會生出一個設定裡根本不存在的
    /// 幽靈出口，之後每次 `source_views` 都得靠設定過濾才看不見它。
    pub fn set_exit_status(&self, local: u16, status: &str, detail: Option<String>) {
        {
            let mut exits = self.exits.lock().unwrap();
            let Some(rt) = exits.get_mut(&local) else {
                return;
            };
            if rt.status == status && rt.detail == detail {
                return;
            }
            rt.status = status.into();
            rt.detail = detail.clone();
        }
        let _ = self
            .app
            .emit("exit-status", ExitStatusPayload { local, status: status.into(), detail });
        self.refresh_tray();
    }

    pub fn exit_status(&self, local: u16) -> Option<String> {
        self.exits.lock().unwrap().get(&local).map(|r| r.status.clone())
    }

    pub fn is_connected(&self, local: u16) -> bool {
        self.exit_status(local).as_deref() == Some(status::CONNECTED)
    }

    /// 目前連線中：只要不是 stopped（含尚未有紀錄）就算，涵蓋 connecting／
    /// reconnecting／port_busy／error 這幾個過渡狀態，讓重接掃得到它們。
    pub fn is_running(&self, local: u16) -> bool {
        !matches!(self.exit_status(local).as_deref(), None | Some(status::STOPPED))
    }

    /// 更新某個出口的自測狀態並推事件。與 `set_exit_status` 同理，
    /// 只更新既存的出口，不讓已刪掉的埠靠一次晚到的自測結果復活
    pub fn set_exit_test(&self, local: u16, state: &str, text: &str) {
        {
            let mut exits = self.exits.lock().unwrap();
            let Some(rt) = exits.get_mut(&local) else {
                return;
            };
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

    /// 每個源與其出口的當下樣貌，Snapshot 與系統匣選單共用這一份算法。
    ///
    /// 鎖序是 cfg → exits，全程式只有這裡同時持有兩把，不會反向配對。
    pub fn source_views(&self) -> Vec<SourceView> {
        self.with_config(|cfg| {
        let exits = self.exits.lock().unwrap();
        cfg.sources
            .iter()
            .map(|s| SourceView {
                name: s.name.clone(),
                host: s.host.clone(),
                user: s.user.clone(),
                proxy_command: s.proxy_command.clone(),
                exits: s
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
                    .collect(),
            })
            .collect()
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            close_to_tray: self.with_config(|c| c.close_to_tray),
            autostart: self.autostart(),
            sources: self.source_views(),
            logs: self.logs.lock().unwrap().iter().cloned().collect(),
        }
    }

    /// 任何設定變更後全量推一次
    pub fn emit_config_changed(&self) {
        let _ = self.app.emit("config-changed", self.snapshot());
        self.refresh_tray();
    }

    /// 系統匣的提示文字與右鍵選單都跟著狀態走，狀態一變就整份重算。
    ///
    /// 鎖紀律：先取快照（鎖在 `source_views` 裡取完就放掉），之後只碰快照，
    /// 真正碰 tray 的動作在背景執行緒上做，絕不持鎖呼叫系統匣。
    /// 號碼牌緊貼快照配出，中間不插任何事，才不會有「號碼新、快照舊」的交錯。
    pub fn refresh_tray(&self) {
        let sources = self.source_views();
        let seq = crate::traymenu::next_seq();
        crate::traymenu::refresh(&self.app, &sources, seq);
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

    /// restart_exit＝halt 後立刻 start：halt 當場把位子清掉，所以新的 start
    /// 不必等舊迴圈醒來就搶得到，而晚退出的舊迴圈也不能把它的位子還掉
    #[test]
    fn restart_hands_the_slot_over_without_a_race() {
        let mut slot = None;
        let mut seq = 0;
        let mut next = || {
            seq += 1;
            seq
        };
        assert_eq!(claim_slot(&mut slot, &mut next), Some(1));
        // halt：遞增世代並當場騰出位子
        slot = None;
        // 緊接著的 start 立刻接手，中間不會有第二條 ssh 並存
        assert_eq!(claim_slot(&mut slot, &mut next), Some(2));
        // 舊迴圈這時才醒來退出
        release_slot(&mut slot, 1);
        assert_eq!(slot, Some(2));
        // 位子還被佔著，重複的 start 依舊會被擋下
        assert_eq!(claim_slot(&mut slot, &mut next), None);
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

    /// 日誌行格式：源底下的事件帶 [源名]，app 級事件不帶
    #[test]
    fn log_line_carries_source_name_only_when_it_has_one() {
        let with = format_log(Some("hk"), "exit-a : up");
        assert!(with.ends_with("  [hk] exit-a : up"), "{with}");
        let without = format_log(None, "Traytunnel started");
        assert!(without.ends_with("  Traytunnel started"), "{without}");
        assert!(!without.contains('['));
        // 時間戳仍是 HH:mm:ss，長度固定 8
        assert_eq!(with.split("  ").next().unwrap().len(), 8);
    }

    #[test]
    fn log_buffer_below_capacity_keeps_everything() {
        let mut logs = VecDeque::new();
        push_log_line(&mut logs, "a".into());
        push_log_line(&mut logs, "b".into());
        assert_eq!(logs.iter().cloned().collect::<Vec<_>>(), vec!["a", "b"]);
    }
}
