//! 應用程式共用狀態，所有狀態變化都由這裡推事件給前端。
//!
//! 每個出口（以本地埠為唯一鍵）各自帶一份執行期狀態：連線狀態、自測結果、
//! 世代序號與 Job Object handle，彼此互不影響。

use std::collections::{BTreeMap, HashMap, VecDeque};
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

/// 事件：exit-test。
///
/// `result` 是 None 時代表「把這個出口的自測顯示清掉」，斷線、停用、重接都會發。
///
/// 用 `flatten` 而不是多包一層物件，是為了向後相容：Some 序列化出來就是
/// `{local, state, text}`，與這個事件原本的形狀一模一樣，讀結果的那一端不必改；
/// None 只剩 `{local}`，state／text 讀出來是 undefined，語意上正是「沒有結果」。
#[derive(Debug, Clone, Serialize)]
pub struct ExitTestPayload {
    pub local: u16,
    #[serde(flatten)]
    pub result: Option<TestView>,
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

/// 有新版可用時的資訊：同時是 Snapshot 的 `update` 欄位與 `update-available`
/// 事件的內容。沒有新版（或還沒查過、檢查失敗）時一律是 None，介面就不顯示那一列。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// 遠端公告的新版本號，不帶 v
    pub version: String,
    /// true＝安裝版，可以就地下載安裝；false＝可攜／單檔版，只能開瀏覽器讓使用者自己換
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub close_to_tray: bool,
    pub autostart: bool,
    /// 實際生效的值（設定檔沒寫時已經照模式決定好了），設定頁的開關直接吃它
    pub check_for_updates: bool,
    pub sources: Vec<SourceView>,
    /// 活動日誌回放，順序由舊到新，內容與 log 事件的整行一致
    pub logs: Vec<String>,
    /// 背景檢查發現的新版，沒有就是 null（介面靠它決定要不要顯示更新列）
    pub update: Option<UpdateInfo>,
}

/// 監看迴圈的佔位：位子有人就不發新號，避免同一個出口被起第二條 ssh。
/// 號碼在取得位子之後才配發，未取得時不消耗世代序號。
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

/// 一次自測的憑證：這個出口的**連線世代**加上**自測期號**。
///
/// 只靠連線世代是不夠的。世代只在 halt／restart 時換號，而 ssh 自己掛掉時
/// 監看迴圈是內圈 break 之後在同一代裡重跑一輪——連線換了、世代沒換，
/// 上一條連線發出去、還在路上的那份探測（`probe` 最久要 12 秒）就會通過
/// 世代檢查，把舊連線的出口 IP 寫成新連線的自測結果。
///
/// 自測期號補上這個落差：換號的唯一入口是 [`AppState::clear_exit_test`]，
/// 而斷線、停用、重接都會經過那裡。兩個號碼合起來當一張憑證，呼叫端只要
/// 原樣帶著它走，不必自己記得該比哪幾個號。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestToken {
    generation: u64,
    epoch: u64,
}

/// 搶下自測佔位，回傳 false 代表「同一張憑證真的還在測」，這次不要再發一份探測。
///
/// 憑證不同時**搶佔**而不是拒絕：位子上那份是上一輪連線留下的在途探測，
/// 它的結果已經被憑證判死，沒有理由讓一個註定被丟掉的探測把新的自測擋在門外
/// ——擋掉的話這個出口就再也不會有自測結果，只能等使用者自己按。
fn claim_test(slots: &mut HashMap<u16, TestToken>, local: u16, token: TestToken) -> bool {
    if slots.get(&local) == Some(&token) {
        return false;
    }
    slots.insert(local, token);
    true
}

/// 探測結束時歸還佔位，只還得掉自己那一張：被搶佔過的舊探測晚一步跑完，
/// 不可以把接手者的佔位清掉，否則下一份探測會與接手者並存。
fn release_test(slots: &mut HashMap<u16, TestToken>, local: u16, token: TestToken) {
    if slots.get(&local) == Some(&token) {
        slots.remove(&local);
    }
}

/// 快照裡該不該帶著自測結果：只有 connected 的出口才帶。
///
/// 自測結果講的是「這條線通到哪裡」，線一斷它就不再是事實。前端對事件流已經
/// 是這個規矩（狀態一離開 connected 就把顯示清掉），快照這邊要是照舊把
/// `last_test` 原樣送出去，任何一次 config-changed 都會把前端剛清掉的舊字
/// 回灌回畫面上——使用者看到的會是一個斷線的出口配著上一輪的出口 IP。
///
/// 擋在這裡而不是在每個狀態轉換點各清一次：`source_views` 是快照與系統匣共用的
/// 唯一出口，一道規則就涵蓋所有路徑，之後新增狀態轉換也不會有人忘了補。
fn visible_test(status: &str, last_test: Option<TestView>) -> Option<TestView> {
    if status == status::CONNECTED {
        last_test
    } else {
        None
    }
}

/// 就地寫一筆連線狀態，回傳「有沒有真的變」。有守門與無守門的兩條寫入路徑
/// 共用這一份「相同就不推事件」的規則，兩邊不會各判各的。
fn write_status(rt: &mut ExitRuntime, status: &str, detail: &Option<String>) -> bool {
    if rt.status == status && rt.detail == *detail {
        return false;
    }
    rt.status = status.into();
    rt.detail = detail.clone();
    true
}

/// 開機自啟登錄值的名稱。沿用 productName（沒有就退回套件名），與先前
/// tauri-plugin-autostart 寫進去的那一份同名，升級的使用者不必重設。
pub fn autostart_name(app: &AppHandle) -> String {
    app.config().product_name.clone().unwrap_or_else(|| app.package_info().name.clone())
}

/// 組一行日誌：`HH:mm:ss  [源名] 訊息`，app 級事件不帶源名。
fn format_log(source: Option<&str>, msg: &str) -> String {
    let ts = crate::winsys::local_time_hms();
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
#[derive(Debug)]
struct ExitRuntime {
    status: String,
    detail: Option<String>,
    last_test: Option<TestView>,
    /// 目前有效的世代序號，換號即代表舊的監看迴圈作廢；
    /// 號碼取自全域計數器，出口被刪掉又重建也不會撞號
    generation: u64,
    /// 目前有效的自測期號，換號即代表在途的探測結果不算數了。
    /// 與 generation 分開的理由見 [`TestToken`]
    test_epoch: u64,
    /// 目前活著的監看迴圈是哪一代，None 代表這個出口沒人在跑
    supervisor: Option<u64>,
    job: Option<(u64, Job)>,
}

impl ExitRuntime {
    /// 這個出口當下的自測憑證
    fn token(&self) -> TestToken {
        TestToken { generation: self.generation, epoch: self.test_epoch }
    }
}

/// 手寫而不是 derive：新項目的 status 一定要是 stopped。
/// derive 出來的空字串不是任何一個合法狀態，而 `is_running` 的判斷是
/// 「不是 stopped 就算在跑」，空字串一旦外流就會讓沒起來的出口顯示成運行中。
impl Default for ExitRuntime {
    fn default() -> Self {
        ExitRuntime {
            status: status::STOPPED.into(),
            detail: None,
            last_test: None,
            generation: 0,
            test_epoch: 0,
            supervisor: None,
            job: None,
        }
    }
}

pub struct AppState {
    pub app: AppHandle,
    /// 這次執行生效的設定檔完整路徑，由 config::config_location() 解析而來；
    /// 全程式的回寫、備份與「開啟設定資料夾」都以它為準
    pub path: PathBuf,
    /// 這次是不是可攜模式（設定檔就在執行檔旁邊），同樣來自 config_location()。
    /// 目前只有「檢查更新」這一項的預設值跟著它走
    portable: bool,
    cfg: Mutex<Config>,
    /// 環形緩衝，讓前端掛上監聽前（例如啟動當下）的日誌還能靠 Snapshot 補回來
    logs: Mutex<VecDeque<String>>,
    exits: Mutex<BTreeMap<u16, ExitRuntime>>,
    /// 正在自測的埠，值是那份探測拿在手上的憑證。互斥比的是憑證而不只是埠，
    /// 舊連線留下的在途探測才擋不住新一輪的自測
    testing: Mutex<HashMap<u16, TestToken>>,
    /// 全域世代計數器，發出去的號碼永不重複
    generation: AtomicU64,
    tray_hint_shown: AtomicBool,
    exiting: AtomicBool,
    /// 設定檔壞掉又備份不出來時會被拉起來，之後一律拒絕回寫
    read_only: AtomicBool,
    /// 背景更新檢查的結果，None 代表目前沒有新版可用
    update: Mutex<Option<UpdateInfo>>,
}

impl AppState {
    pub fn new(app: AppHandle, path: PathBuf, portable: bool, cfg: Config) -> Self {
        let exits = cfg.locals().into_iter().map(|p| (p, ExitRuntime::default())).collect();
        AppState {
            app,
            path,
            portable,
            cfg: Mutex::new(cfg),
            logs: Mutex::new(VecDeque::new()),
            exits: Mutex::new(exits),
            testing: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
            tray_hint_shown: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            read_only: AtomicBool::new(false),
            update: Mutex::new(None),
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
    /// 自己），否則會當場造成自我死鎖。要跨鎖持有的話，在閉包裡複製需要的那幾筆
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
            exits.entry(p).or_default();
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
        // emit 只要 Serialize + Clone，&str 就夠：先借出去推事件，再把整個
        // String 讓給環形緩衝，一行日誌從頭到尾只配置一次
        let _ = self.app.emit("log", line.as_str());
        push_log_line(&mut self.logs.lock().unwrap(), line);
    }

    /// 對既存出口的執行期狀態就地改一筆，回傳 None 代表這個出口已經不在了。
    ///
    /// 執行期狀態的項目只由 `AppState::new` 與 `sync_exits` 依設定建立，
    /// 其餘地方一律只改既存項：晚到的狀態更新若順手把項目補回來，就會生出
    /// 設定裡根本不存在的幽靈出口。
    fn with_exit_mut<T>(&self, local: u16, f: impl FnOnce(&mut ExitRuntime) -> T) -> Option<T> {
        self.exits.lock().unwrap().get_mut(&local).map(f)
    }

    /// 更新某個出口的連線狀態並推事件；狀態沒變就不重複推。
    ///
    /// 只更新既存的出口：出口一旦被刪掉，執行期狀態也跟著被 `sync_exits` 清掉，
    /// 這時晚到的狀態更新若順手把項目補回來，就會生出一個設定裡根本不存在的
    /// 幽靈出口，之後每次 `source_views` 都得靠設定過濾才看不見它。
    ///
    /// 這一版**不看世代**，給的是「無論如何都要寫下去」的呼叫端：halt 要把狀態
    /// 壓成 stopped 正是在遞增世代之後，指令層改的也都是當下這一刻的事實。
    /// 監看迴圈那種「算出來的時候還算數、寫下去時可能已經過期」的狀態一律走
    /// [`set_exit_status_of`]。
    pub fn set_exit_status(&self, local: u16, status: &str, detail: Option<String>) {
        let changed = self.with_exit_mut(local, |rt| write_status(rt, status, &detail));
        if changed == Some(true) {
            self.announce_status(local, status, detail);
        }
    }

    /// 世代守門版的狀態寫入：世代不符就整筆丟掉，連事件都不推。
    ///
    /// 監看迴圈算出一個狀態到真正寫進去之間，中間隔著 `is_listening`、`spawn`、
    /// 甚至 `with_config` 搶 cfg 鎖的等待——那段時間足夠讓 halt 插進來把世代換掉。
    /// 舊迴圈晚到的那一手若照寫，會把 halt 剛壓下去的 stopped 蓋回 connected：
    /// 出口實際上已經停了，介面卻顯示連著，而且沒有任何後續事件會把它糾正回來
    /// （舊迴圈下一圈就退出了），只能等使用者自己再按一次。連帶的傷害是
    /// 「Reconnect all」靠 `is_running` 挑對象，會把這個假的 running 出口重新拉起來。
    ///
    /// 比對與寫入必須在**同一次 exits 鎖**內完成，否則「先問世代、再寫狀態」
    /// 中間一樣有窗口，守門等於沒設。要不要推事件也在鎖裡決定好，
    /// 但事件與系統匣刷新一律等放掉鎖之後才做（`refresh_tray` 會再取這把鎖）。
    pub fn set_exit_status_of(
        &self,
        local: u16,
        generation: u64,
        status: &str,
        detail: Option<String>,
    ) {
        let changed = {
            let mut exits = self.exits.lock().unwrap();
            match exits.get_mut(&local) {
                Some(rt) if rt.generation == generation => write_status(rt, status, &detail),
                _ => false,
            }
        };
        if changed {
            self.announce_status(local, status, detail);
        }
    }

    /// 狀態真的變了之後的收尾：推事件並重算系統匣。
    /// 呼叫時**不可以**持有 exits 鎖，`refresh_tray` 會再取一次。
    fn announce_status(&self, local: u16, status: &str, detail: Option<String>) {
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

    /// 更新某個出口的自測狀態並推事件，憑證不符就整筆丟掉。
    ///
    /// 與 `set_exit_status_of` 同一套規矩：比對與寫入在同一次 exits 鎖內完成，
    /// 中途被 halt／restart／斷線重連換掉憑證的探測寫不進去。順帶保留原本
    /// 「只更新既存的出口」的性質，已刪掉的埠不會靠一次晚到的自測結果復活。
    pub fn set_exit_test_of(&self, local: u16, token: TestToken, state: &str, text: &str) {
        let view = TestView { state: state.into(), text: text.into() };
        let written = {
            let mut exits = self.exits.lock().unwrap();
            match exits.get_mut(&local) {
                Some(rt) if rt.token() == token => {
                    rt.last_test = Some(view.clone());
                    true
                }
                _ => false,
            }
        };
        if written {
            let _ = self.app.emit("exit-test", ExitTestPayload { local, result: Some(view) });
        }
    }

    /// 出口斷線或停掉時把舊的自測結果清乾淨，並讓在途的探測就地作廢。
    ///
    /// 換期號這一手對「ssh 自己掛掉、監看迴圈在同一代裡重跑一輪」特別要緊：
    /// 那條路不換連線世代，沒有期號的話上一條連線發出去的探測會通過世代檢查，
    /// 把舊連線的出口 IP 寫成新連線的自測結果。
    pub fn clear_exit_test(&self, local: u16) {
        let counter = &self.generation;
        let had = self.with_exit_mut(local, |rt| {
            rt.test_epoch = counter.fetch_add(1, Ordering::SeqCst) + 1;
            rt.last_test.take().is_some()
        });
        // 本來就沒有結果可清就不推事件：斷線重連每 5 秒會走一次這裡，
        // 沒有這道閘的話會一直送出內容相同的空事件
        if had == Some(true) {
            let _ = self.app.emit("exit-test", ExitTestPayload { local, result: None });
        }
    }

    /// 取這個出口當下的自測憑證。一次鎖把兩個號碼一起讀出來，
    /// 中間不會被插進一半（讀到舊世代配新期號那種不存在的組合）。
    pub fn test_token(&self, local: u16) -> TestToken {
        self.exits
            .lock()
            .unwrap()
            .get(&local)
            .map(ExitRuntime::token)
            .unwrap_or(TestToken { generation: 0, epoch: 0 })
    }

    /// 憑證還算不算數，用來決定要不要把探測結果寫回去
    pub fn test_alive(&self, local: u16, token: TestToken) -> bool {
        self.exits.lock().unwrap().get(&local).is_some_and(|rt| rt.token() == token)
    }

    /// 標記某個埠開始測試，回傳 false 代表同一張憑證真的還在測
    pub fn begin_test(&self, local: u16, token: TestToken) -> bool {
        claim_test(&mut self.testing.lock().unwrap(), local, token)
    }

    /// 探測跑完歸還佔位，只還得掉自己那一張
    pub fn end_test(&self, local: u16, token: TestToken) {
        release_test(&mut self.testing.lock().unwrap(), local, token);
    }

    /// 讓該出口進入新世代並騰出位子，舊的監看迴圈看到世代不符就會自行退出。
    /// 位子當場清掉，緊接著的 start 不必等舊迴圈醒來就能接手。
    pub fn next_generation(&self, local: u16) {
        let counter = &self.generation;
        self.with_exit_mut(local, |rt| {
            rt.generation = counter.fetch_add(1, Ordering::SeqCst) + 1;
            rt.supervisor = None;
        });
    }

    /// 搶下這個出口的監看位子，回傳 None 代表已經有一條線在跑，不要再起第二條
    pub fn claim_supervisor(&self, local: u16) -> Option<u64> {
        let counter = &self.generation;
        self.with_exit_mut(local, |rt| {
            let claimed =
                claim_slot(&mut rt.supervisor, || counter.fetch_add(1, Ordering::SeqCst) + 1);
            if let Some(generation) = claimed {
                rt.generation = generation;
            }
            claimed
        })
        .flatten()
    }

    /// 監看迴圈結束時歸還位子
    pub fn release_supervisor(&self, local: u16, generation: u64) {
        self.with_exit_mut(local, |rt| release_slot(&mut rt.supervisor, generation));
    }

    pub fn generation(&self, local: u16) -> u64 {
        self.exits.lock().unwrap().get(&local).map(|r| r.generation).unwrap_or(0)
    }

    /// 世代還活著才算數，用來判斷監看迴圈要不要繼續
    pub fn generation_alive(&self, local: u16, generation: u64) -> bool {
        self.generation(local) == generation
    }

    /// 記下這一輪連線的 job handle，**只在世代還是自己那一代時**才收下。
    ///
    /// spawn 與 store_job 之間有一段窄窗口：halt／restart 可能剛好插進來遞增世代，
    /// 新的監看迴圈也已經 spawn 完並存好自己的 job。這時舊迴圈晚到的這一手若照存，
    /// 會把新世代的 job 蓋掉——被蓋掉的那個 handle 一 drop，剛接起來的連線當場被殺，
    /// 留下的反而是舊世代那條沒人管的 ssh。
    ///
    /// 世代不符（或出口已經被刪掉）時 job 就在這裡 drop：handle 關閉，
    /// 那條剛 spawn 出來、已經沒有人要的 ssh 連同 ProxyCommand 的孫程序一起收乾淨。
    pub fn store_job(&self, local: u16, generation: u64, job: Job) {
        self.with_exit_mut(local, |rt| {
            if rt.generation == generation {
                rt.job = Some((generation, job));
            }
        });
    }

    /// 關掉 job handle，該出口的 ssh 程序樹一起結束
    pub fn kill_job(&self, local: u16) {
        self.with_exit_mut(local, |rt| rt.job.take());
    }

    /// 只在世代相符時清掉 job，避免誤殺新的一輪連線
    pub fn kill_job_of(&self, local: u16, generation: u64) {
        self.with_exit_mut(local, |rt| {
            if rt.job.as_ref().map(|(g, _)| *g) == Some(generation) {
                rt.job.take();
            }
        });
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

    /// 領取「關到系統匣」那顆一次性提示：第一次呼叫回 true 並就地作廢，
    /// 之後一律回 false。
    ///
    /// 命名用 take_ 前綴是為了標示它會改狀態：實作是 swap(true)，不是單純查詢，
    /// 呼叫一次提示就被領走。已經自己彈過通知的路徑（例如 --tray 啟動）
    /// 直接呼叫它把提示領掉即可。
    pub fn take_tray_hint(&self) -> bool {
        !self.tray_hint_shown.swap(true, Ordering::SeqCst)
    }

    pub fn is_exiting(&self) -> bool {
        self.exiting.load(Ordering::SeqCst)
    }

    pub fn mark_exiting(&self) {
        self.exiting.store(true, Ordering::SeqCst);
    }

    pub fn autostart(&self) -> bool {
        crate::winsys::autostart_enabled(&autostart_name(&self.app))
    }

    /// 這次執行要不要檢查更新：設定檔沒寫的話，一般模式開、可攜模式關
    pub fn checks_for_updates(&self) -> bool {
        self.with_config(|c| c.checks_for_updates(self.portable))
    }

    /// 記下背景檢查的結果並推事件；跟上次一樣就不重推。
    ///
    /// 每 24 小時會再查一次，同一個新版本重複推的話，設定頁那一列會無謂重畫，
    /// 也讓事件流看起來像真的又發生了什麼事。
    pub fn set_update(&self, info: Option<UpdateInfo>) {
        {
            let mut slot = self.update.lock().unwrap();
            if *slot == info {
                return;
            }
            *slot = info.clone();
        }
        let _ = self.app.emit("update-available", info);
    }

    pub fn update_info(&self) -> Option<UpdateInfo> {
        self.update.lock().unwrap().clone()
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
                            let status = rt
                                .map(|r| r.status.clone())
                                .unwrap_or_else(|| status::STOPPED.to_string());
                            ExitView {
                                name: f.name.clone(),
                                local: f.local,
                                remote: f.remote.clone(),
                                enabled: f.enabled,
                                last_test: visible_test(
                                    &status,
                                    rt.and_then(|r| r.last_test.clone()),
                                ),
                                status,
                            }
                        })
                        .collect(),
                })
                .collect()
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot_with(self.source_views())
    }

    /// 已經算好 `source_views` 的呼叫端走這裡，不要再算一次
    fn snapshot_with(&self, sources: Vec<SourceView>) -> Snapshot {
        Snapshot {
            close_to_tray: self.with_config(|c| c.close_to_tray),
            autostart: self.autostart(),
            check_for_updates: self.checks_for_updates(),
            sources,
            logs: self.logs.lock().unwrap().iter().cloned().collect(),
            update: self.update_info(),
        }
    }

    /// 任何設定變更後全量推一次。
    ///
    /// 前端的 Snapshot 與系統匣選單吃的是同一份 `source_views`，算一次就好：
    /// 系統匣只讀（`refresh` 當場把它轉成選單模型），讀完再把那一份讓給要
    /// 序列化的 Snapshot。兩個接收端彼此獨立，先後順序不影響結果。
    pub fn emit_config_changed(&self) {
        let sources = self.source_views();
        self.refresh_tray_with(&sources);
        let _ = self.app.emit("config-changed", self.snapshot_with(sources));
    }

    /// 系統匣的提示文字與右鍵選單都跟著狀態走，狀態一變就整份重算。
    ///
    /// 鎖紀律：先取快照（鎖在 `source_views` 裡取完就放掉），之後只碰快照，
    /// 真正碰 tray 的動作在背景執行緒上做，絕不持鎖呼叫系統匣。
    /// 號碼牌緊貼快照配出，中間不插任何事，才不會有「號碼新、快照舊」的交錯。
    pub fn refresh_tray(&self) {
        self.refresh_tray_with(&self.source_views());
    }

    /// 已經算好 `source_views` 的呼叫端走這裡，不要再算一次
    fn refresh_tray_with(&self, sources: &[SourceView]) {
        let seq = crate::traymenu::next_seq();
        crate::traymenu::refresh(&self.app, sources, seq);
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

    /// start 對已經在跑的出口不能再起一條 ssh，否則舊的 ssh 還佔著埠，
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
        // 未取得位子時不該消耗世代序號
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

    /// halt 之後舊迴圈較晚才退出，不能讓它把新迴圈的位子清掉
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

    /// 有結果時序列化出來的形狀不可以變：state／text 要平鋪在 local 旁邊，
    /// 與這個事件原本的樣子一致，收結果的那一端不必為了清除語意改寫
    #[test]
    fn a_test_result_still_goes_out_flat() {
        let payload = ExitTestPayload {
            local: 1080,
            result: Some(TestView {
                state: test_state::OK.into(),
                text: "1.2.3.4  Taipei, TW".into(),
            }),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["local"], 1080);
        assert_eq!(json["state"], test_state::OK);
        assert_eq!(json["text"], "1.2.3.4  Taipei, TW");
    }

    /// 清除是同一個事件的另一種形狀：只剩 local，沒有 state／text
    #[test]
    fn a_cleared_test_carries_no_result_fields() {
        let json = serde_json::to_value(ExitTestPayload { local: 1080, result: None }).unwrap();
        assert_eq!(json["local"], 1080);
        assert!(json.get("state").is_none(), "清除事件不可以帶著空字串假裝有結果");
        assert!(json.get("text").is_none());
    }

    /// 快照只在 connected 時帶自測結果，否則斷線後的任何一次 config-changed
    /// 都會把前端剛清掉的舊出口 IP 回灌回畫面上
    #[test]
    fn a_snapshot_only_carries_the_test_while_connected() {
        let view = || Some(TestView { state: test_state::OK.into(), text: "1.2.3.4".into() });
        assert!(visible_test(status::CONNECTED, view()).is_some());
        for s in [
            status::STOPPED,
            status::CONNECTING,
            status::RECONNECTING,
            status::PORT_BUSY,
            status::ERROR,
        ] {
            assert!(visible_test(s, view()).is_none(), "{s} 不該帶著上一輪的自測結果");
        }
    }

    fn token(generation: u64, epoch: u64) -> TestToken {
        TestToken { generation, epoch }
    }

    /// 同一輪連線裡重複按自測要擋下來，否則一個埠會同時飛出好幾份探測
    #[test]
    fn the_same_token_cannot_start_a_second_probe() {
        let mut slots = HashMap::new();
        assert!(claim_test(&mut slots, 1080, token(1, 1)));
        assert!(!claim_test(&mut slots, 1080, token(1, 1)));
    }

    /// 重接之後憑證換了新的，這時位子上那份是註定被丟掉的舊探測——
    /// 必須讓新的自測搶佔，不然這個出口在舊探測跑完之前都測不了
    #[test]
    fn a_newer_token_preempts_a_stale_probe() {
        let mut slots = HashMap::new();
        assert!(claim_test(&mut slots, 1080, token(1, 1)));
        assert!(claim_test(&mut slots, 1080, token(2, 5)), "換了憑證要搶得到位子");
        assert_eq!(slots.get(&1080), Some(&token(2, 5)));
    }

    /// ssh 自己掛掉、監看迴圈在同一代裡重跑一輪：世代沒變，只有自測期號變了，
    /// 這一樣要算成新的一輪，舊探測不能擋著
    #[test]
    fn a_new_epoch_alone_is_enough_to_preempt() {
        let mut slots = HashMap::new();
        assert!(claim_test(&mut slots, 1080, token(7, 1)));
        assert!(claim_test(&mut slots, 1080, token(7, 2)));
    }

    /// 被搶佔的舊探測晚一步跑完，不可以把接手者的位子清掉
    #[test]
    fn a_preempted_probe_cannot_release_the_new_slot() {
        let mut slots = HashMap::new();
        claim_test(&mut slots, 1080, token(1, 1));
        claim_test(&mut slots, 1080, token(2, 2));
        release_test(&mut slots, 1080, token(1, 1));
        assert_eq!(slots.get(&1080), Some(&token(2, 2)), "舊的還不掉新的位子");
        release_test(&mut slots, 1080, token(2, 2));
        assert!(!slots.contains_key(&1080), "自己那一張還得掉");
    }

    /// 位子是逐埠獨立的，一個出口在測不會擋到另一個
    #[test]
    fn test_slots_are_per_exit() {
        let mut slots = HashMap::new();
        assert!(claim_test(&mut slots, 1080, token(1, 1)));
        assert!(claim_test(&mut slots, 1083, token(1, 1)));
        release_test(&mut slots, 1080, token(1, 1));
        assert_eq!(slots.get(&1083), Some(&token(1, 1)));
    }

    #[test]
    fn log_buffer_below_capacity_keeps_everything() {
        let mut logs = VecDeque::new();
        push_log_line(&mut logs, "a".into());
        push_log_line(&mut logs, "b".into());
        assert_eq!(logs.iter().cloned().collect::<Vec<_>>(), vec!["a", "b"]);
    }
}
