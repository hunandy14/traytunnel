//! 應用程式共用狀態，所有狀態變化都由這裡推事件給前端。

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::config::Config;
use crate::winsys::Job;

/// 活動日誌保留的行數上限
const LOG_CAPACITY: usize = 500;

pub const TRAY_ID: &str = "traytunnel-tray";
pub const MAIN_WINDOW: &str = "main";
pub const SETTINGS_WINDOW: &str = "settings";

#[derive(Debug, Clone, Serialize)]
pub struct StatusPayload {
    pub text: String,
    /// muted / amber / accent / red，對應前端配色
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExitPayload {
    pub port: u16,
    /// idle / testing / ok / fail
    pub state: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub config: Config,
    pub status: StatusPayload,
    pub want_run: bool,
    pub connected: bool,
    pub logs: Vec<String>,
    pub exits: Vec<ExitPayload>,
    pub autostart: bool,
}

pub struct AppState {
    pub app: AppHandle,
    /// 設定檔所在資料夾（執行檔同目錄）
    pub dir: PathBuf,
    cfg: Mutex<Config>,
    logs: Mutex<VecDeque<String>>,
    status: Mutex<StatusPayload>,
    exits: Mutex<BTreeMap<u16, ExitPayload>>,
    testing: Mutex<HashSet<u16>>,
    want_run: AtomicBool,
    connected: AtomicBool,
    /// 隧道世代，遞增即代表舊的監看迴圈作廢
    generation: AtomicU64,
    job: Mutex<Option<(u64, Job)>>,
    tray_hint_shown: AtomicBool,
    exiting: AtomicBool,
}

impl AppState {
    pub fn new(app: AppHandle, dir: PathBuf, cfg: Config) -> Self {
        AppState {
            app,
            dir,
            cfg: Mutex::new(cfg),
            logs: Mutex::new(VecDeque::new()),
            status: Mutex::new(StatusPayload { text: "Starting...".into(), kind: "muted".into() }),
            exits: Mutex::new(BTreeMap::new()),
            testing: Mutex::new(HashSet::new()),
            want_run: AtomicBool::new(true),
            connected: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            job: Mutex::new(None),
            tray_hint_shown: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
        }
    }

    pub fn config(&self) -> Config {
        self.cfg.lock().unwrap().clone()
    }

    /// 更新設定並通知前端，出口卡片狀態由呼叫端決定要不要重設
    pub fn set_config(&self, cfg: Config) {
        *self.cfg.lock().unwrap() = cfg.clone();
        let _ = self.app.emit("config", cfg);
    }

    pub fn log(&self, msg: impl AsRef<str>) {
        let line = format!("{}  {}", chrono::Local::now().format("%H:%M:%S"), msg.as_ref());
        log::info!("{}", msg.as_ref());
        {
            let mut logs = self.logs.lock().unwrap();
            logs.push_back(line.clone());
            while logs.len() > LOG_CAPACITY {
                logs.pop_front();
            }
        }
        let _ = self.app.emit("log", line);
    }

    pub fn set_status(&self, text: &str, kind: &str) {
        let payload = StatusPayload { text: text.into(), kind: kind.into() };
        *self.status.lock().unwrap() = payload.clone();
        if let Some(tray) = self.app.tray_by_id(TRAY_ID) {
            let _ = tray.set_tooltip(Some(format!("Traytunnel - {text}")));
        }
        let _ = self.app.emit("status", payload);
    }

    pub fn set_exit(&self, port: u16, state: &str, text: &str) {
        let payload = ExitPayload { port, state: state.into(), text: text.into() };
        self.exits.lock().unwrap().insert(port, payload.clone());
        let _ = self.app.emit("exit", payload);
    }

    /// 依目前設定把所有出口卡片重設回未知狀態
    pub fn reset_exits(&self) {
        let ports: Vec<u16> = self.config().forwards.iter().map(|f| f.local).collect();
        self.exits.lock().unwrap().clear();
        for p in ports {
            self.set_exit(p, "idle", "-");
        }
    }

    /// 標記某個埠開始測試，回傳 false 代表已經在測了
    pub fn begin_test(&self, port: u16) -> bool {
        self.testing.lock().unwrap().insert(port)
    }

    pub fn end_test(&self, port: u16) {
        self.testing.lock().unwrap().remove(&port);
    }

    pub fn want_run(&self) -> bool {
        self.want_run.load(Ordering::SeqCst)
    }

    pub fn set_want_run(&self, on: bool) {
        self.want_run.store(on, Ordering::SeqCst);
        let _ = self.app.emit("run-state", on);
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub fn set_connected(&self, on: bool) {
        self.connected.store(on, Ordering::SeqCst);
    }

    pub fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn store_job(&self, generation: u64, job: Job) {
        *self.job.lock().unwrap() = Some((generation, job));
    }

    /// 關掉 job handle，整棵 ssh 程序樹一起結束
    pub fn kill_job(&self) {
        let _ = self.job.lock().unwrap().take();
    }

    /// 只在世代相符時清掉 job，避免誤殺新的一輪連線
    pub fn kill_job_of(&self, generation: u64) {
        let mut guard = self.job.lock().unwrap();
        if guard.as_ref().map(|(g, _)| *g) == Some(generation) {
            let _ = guard.take();
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

    pub fn snapshot(&self, autostart: bool) -> Snapshot {
        Snapshot {
            config: self.config(),
            status: self.status.lock().unwrap().clone(),
            want_run: self.want_run(),
            connected: self.connected(),
            logs: self.logs.lock().unwrap().iter().cloned().collect(),
            exits: self.exits.lock().unwrap().values().cloned().collect(),
            autostart,
        }
    }
}
