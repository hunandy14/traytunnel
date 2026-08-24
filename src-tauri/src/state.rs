//! 應用程式共用狀態，所有狀態變化都由這裡推事件給前端。
//!
//! 每個出口（以本地埠為唯一鍵）各自帶一份執行期狀態：連線狀態、自測結果、
//! 世代序號與 Job Object handle，彼此互不影響。

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestView {
    pub state: String,
    pub text: String,
    /// 識別出的代理協定，給 UI 徽章用（`"socks5"`／`"http"`）。
    ///
    /// 識別不出來時是 None，而且**該鍵整個不出現**——送一個空字串等於叫前端
    /// 畫一顆空白徽章（W8.27）。舊的讀取端讀到 undefined 不受影響（§5.3）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

impl TestView {
    /// 沒有協定資訊的自測結果——ssh 那條既有路徑與所有「還沒識別」的情況都用它
    pub fn plain(state: impl Into<String>, text: impl Into<String>) -> Self {
        TestView { state: state.into(), text: text.into(), protocol: None }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitView {
    pub name: String,
    pub local: u16,
    /// `socks` 列沒有目的地（§1.3 ⑤）
    pub remote: Option<String>,
    /// `"forward"` | `"socks"`——前端據此在兩段的交界處插區段標題（§1.4）
    pub kind: String,
    /// 前端據此決定要不要留徽章／出口 IP 的位置（§5.3）
    pub probe_proxy: bool,
    pub enabled: bool,
    pub status: String,
    pub last_test: Option<TestView>,
}

/// 一條 wg 連線在快照裡的樣貌（§5.3）。
///
/// 機密邊界：`endpoint`／`addresses`／`dns`／`allowed_ips` **不是機密**（在任何
/// WireGuard 客戶端的介面上都看得到，而且使用者需要它們才知道轉發的 `remote`
/// 該怎麼寫）。**唯一的機密是 `PrivateKey` 與 `PresharedKey`**，它們不進
/// `Config`、不進 `Snapshot`、不進任何 `Serialize`、不進任何日誌。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WgProxyView {
    pub name: String,
    pub conf_path: String,
    pub enabled: bool,
    /// 使用者填的 MTU 覆寫值，沒填就是 None——編輯面板要把現值帶回欄位裡
    pub mtu: Option<usize>,
    /// `.conf` 讀不到／解析不過時的訊息，讀得到就是 None
    pub conf_error: Option<String>,
    /// 卡片副標要顯示的東西（U4）
    pub endpoint: String,
    pub addresses: Vec<String>,
    pub dns: Vec<String>,
    pub allowed_ips: Vec<String>,
    /// 這條連線底下的列，`socks` 列已排在前（§1.4：「SOCKS5」區段在上）
    pub exits: Vec<ExitView>,
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
    /// 「Automatic updates」開關實際生效的值（設定檔沒寫時已經照模式決定好了），
    /// 設定頁的開關直接吃它
    pub automatic_updates: bool,
    pub sources: Vec<SourceView>,
    /// wg 連線。對舊前端是相容的加法：沒有就是空陣列
    pub wg_proxies: Vec<WgProxyView>,
    /// 活動日誌回放，順序由舊到新，內容與 log 事件的整行一致
    pub logs: Vec<String>,
    /// 背景檢查發現的新版，沒有就是 null（介面靠它決定要不要顯示更新列）
    pub update: Option<UpdateInfo>,
    /// 已經下載好、等下一次啟動安裝的那一版版本號（不帶 v），沒有就是 null。
    /// 介面與系統匣靠它決定要不要給「Restart to update」
    pub pending_update: Option<String>,
    /// 知道有新版、但下載失敗了正在退避等重試。介面靠它把「正在下載」的轉圈
    /// 換成一句誠實的「下載失敗，之後會再試」
    pub update_stalled: bool,
}

/// 監看迴圈的佔位：位子有人就不發新號，避免同一個出口被起第二條 ssh。
/// 號碼在取得位子之後才配發，未取得時不消耗世代序號。
pub(crate) fn claim_slot(slot: &mut Option<u64>, next: impl FnOnce() -> u64) -> Option<u64> {
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

/// 監看位子的租約：`Drop` 時把位子還掉。
///
/// 原本是「`supervise().await` 之後手動 `release_supervisor`」。那一行只要
/// 沒跑到，這個出口的位子就永遠佔著，而 `start` 看到有人佔位就直接 no-op
/// ——結果是這條隧道再也起不來，連看門狗都救不了（它問的正是「位子在不在」）。
/// 沒跑到的路徑不只一條：監看迴圈裡任何一處 panic、或有人在 `.await` 之後
/// 加了一個 early return。
///
/// 交給 `Drop` 就不必再依賴「記得寫那一行」：任務正常結束、提早 return、
/// panic 展開，位子都會回來。世代守門仍然在 `release_slot` 裡，
/// 晚到的舊租約還是清不掉新迴圈的位子。
pub struct SupervisorSeat {
    state: Arc<AppState>,
    who: SeatOwner,
    generation: u64,
}

enum SeatOwner {
    /// ssh 出口，身分是本地埠
    Exit(u16),
    /// wg 連線，身分是連線名（§5.2）
    Wg(String),
}

impl SupervisorSeat {
    /// 這一輪的世代號。監看迴圈每一次寫狀態都要帶著它（守門用）
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// 搶下某個 ssh 出口的監看位子，搶到就拿一張會自己歸還的租約。
/// 回傳 None 代表已經有一條線在跑，不要再起第二條。
pub fn claim_exit_seat(state: &Arc<AppState>, local: u16) -> Option<SupervisorSeat> {
    let generation = state.claim_supervisor(local)?;
    Some(SupervisorSeat { state: state.clone(), who: SeatOwner::Exit(local), generation })
}

/// 搶下某條 wg 連線的監看位子，語意與 [`claim_exit_seat`] 完全一致
pub fn claim_wg_seat(state: &Arc<AppState>, conn: &str) -> Option<SupervisorSeat> {
    let generation = state.wg_claim_supervisor(conn)?;
    Some(SupervisorSeat { state: state.clone(), who: SeatOwner::Wg(conn.to_string()), generation })
}

impl Drop for SupervisorSeat {
    fn drop(&mut self) {
        match &self.who {
            SeatOwner::Exit(local) => self.state.release_supervisor(*local, self.generation),
            SeatOwner::Wg(conn) => self.state.wg_release_supervisor(conn, self.generation),
        }
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
pub(crate) fn visible_test(status: &str, last_test: Option<TestView>) -> Option<TestView> {
    if status == status::CONNECTED {
        last_test
    } else {
        None
    }
}

/// `clear_exit_test` 的完整作用：自測顯示與**協定識別快取**一起清掉（W8.25）。
///
/// 兩者的作廢時機完全一致（§1.5）：斷線、停用、重接都會經過這裡。分開清的話，
/// 遲早會出現「線斷了、徽章還掛著上一輪識別出來的協定」那種畫面。
pub(crate) fn cleared_test_state(
    _last_test: Option<TestView>,
    _detected: Option<crate::exits::ProxyProtocol>,
) -> (Option<TestView>, Option<crate::exits::ProxyProtocol>) {
    // 兩個欄位一起歸零。收兩個參數不是為了看它們的值，而是為了讓呼叫端在
    // 型別上就交不出「只清一個」的寫法——`clear_exit_test` 的兩行賦值改成
    // 一次解構，就不會有人日後只更新其中一行。
    (None, None)
}

/// 一條列 + 它的執行期狀態 → 一筆 `ExitView`。ssh 與 wg 共用同一份組裝。
fn exit_view(f: &crate::config::Forward, exits: &BTreeMap<u16, ExitRuntime>) -> ExitView {
    let rt = exits.get(&f.local);
    let status = rt.map(|r| r.status.clone()).unwrap_or_else(|| status::STOPPED.to_string());
    ExitView {
        name: f.name.clone(),
        local: f.local,
        remote: f.remote.clone(),
        kind: match f.kind {
            crate::config::RowKind::Forward => "forward".into(),
            crate::config::RowKind::Socks => "socks".into(),
        },
        probe_proxy: f.probe_proxy,
        enabled: f.enabled,
        last_test: visible_test(&status, rt.and_then(|r| r.last_test.clone())),
        status,
    }
}

/// 一條連線底下的列，**`socks` 列一律排在 `forward` 列之前**（§5.3／W3.40）。
///
/// 順序由後端保證、前端只在交界處插區段標題——不交給前端各自排，否則系統匣與
/// 主視窗會排出兩種順序。SSH 連線只會有 `forward` 列，這條排序對它是恆等式。
fn row_views(
    forwards: &[crate::config::Forward],
    exits: &BTreeMap<u16, ExitRuntime>,
) -> Vec<ExitView> {
    crate::config::ordered_rows(forwards).into_iter().map(|f| exit_view(f, exits)).collect()
}

/// 設定 + 執行期狀態 → 每個源與其出口的當下樣貌。
/// 兩把鎖都由呼叫端持著，這裡只做純粹的組裝。
fn build_views(cfg: &Config, exits: &BTreeMap<u16, ExitRuntime>) -> Vec<SourceView> {
    cfg.sources
        .iter()
        .map(|s| SourceView {
            name: s.name.clone(),
            host: s.host.clone(),
            user: s.user.clone(),
            proxy_command: s.proxy_command.clone(),
            exits: row_views(&s.forwards, exits),
        })
        .collect()
}

/// 設定 + 執行期狀態 + `.conf` 摘要快取 → 每條 wg 連線的當下樣貌（§5.3）。
///
/// `.conf` 的內容從快取拿而不是當場讀檔：這一支跟著每一次狀態變化跑（系統匣
/// 也吃同一份），連線一多就會變成每秒好幾次的磁碟讀取。快取由 `sync_wg_confs`
/// 在設定變動時重整。
fn build_wg_views(
    cfg: &Config,
    exits: &BTreeMap<u16, ExitRuntime>,
    confs: &HashMap<String, Result<crate::wg::conf::ConfSummary, String>>,
) -> Vec<WgProxyView> {
    cfg.wg_proxies
        .iter()
        .map(|p| {
            let summary = confs.get(&p.name);
            let ok = summary.and_then(|r| r.as_ref().ok());
            WgProxyView {
                name: p.name.clone(),
                conf_path: p.conf_path.clone(),
                enabled: p.enabled,
                mtu: p.mtu,
                conf_error: summary.and_then(|r| r.as_ref().err()).cloned(),
                // 這四項在任何 WireGuard 客戶端的介面上都看得到，而且使用者需要
                // 它們才知道轉發列的 remote 該怎麼寫——不是機密（§5.3）
                endpoint: ok.map(|c| c.endpoint.clone()).unwrap_or_default(),
                addresses: ok.map(|c| c.addresses.clone()).unwrap_or_default(),
                dns: ok.map(|c| c.dns.clone()).unwrap_or_default(),
                allowed_ips: ok.map(|c| c.allowed_ips.clone()).unwrap_or_default(),
                exits: row_views(&p.forwards, exits),
            }
        })
        .collect()
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

/// 世代守門版的狀態寫入：世代不符就**一個欄位都不動**並回 false。
///
/// 守門本身抽成純函式才測得到。這道判斷是 `set_exit_status_of` 唯一的防線，
/// 而它防的是一條要靠時序才重現得出來的競態——寫進 `AppState` 裡的話，
/// 測試就得生出一個真的 AppHandle，等於這道守門永遠沒有測試護著，
/// 被誰順手拿掉也不會有人發現。
fn guarded_write_status(
    rt: &mut ExitRuntime,
    generation: u64,
    status: &str,
    detail: &Option<String>,
) -> bool {
    if rt.generation != generation {
        return false;
    }
    write_status(rt, status, detail)
}

/// 憑證守門版的自測寫入：憑證不符就**一個欄位都不動**並回 false。
/// 抽成純函式的理由同 [`guarded_write_status`]。
fn guarded_write_test(rt: &mut ExitRuntime, token: TestToken, view: &TestView) -> bool {
    if rt.token() != token {
        return false;
    }
    rt.last_test = Some(view.clone());
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
struct ExitRuntime {
    status: String,
    detail: Option<String>,
    last_test: Option<TestView>,
    /// 協定識別的結果快取（§1.5）。**執行期，不落設定檔**：設定檔只記使用者填的
    /// 東西，協定是觀察到的事實。作廢時機與自測憑證完全一致（見 `clear_exit_test`）
    detected: Option<crate::exits::ProxyProtocol>,
    /// 目前有效的世代序號，換號即代表舊的監看迴圈作廢；
    /// 號碼取自全域計數器，出口被刪掉又重建也不會撞號
    generation: u64,
    /// 目前有效的自測期號，換號即代表在途的探測結果不算數了。
    /// 與 generation 分開的理由見 [`TestToken`]
    test_epoch: u64,
    /// 目前活著的監看迴圈是哪一代，None 代表這個出口沒人在跑
    supervisor: Option<u64>,
    /// 這一輪連線持有的「殺得掉的東西」。型別是 [`Worker`] 而不是 `Job`，
    /// 於是 `rt.job.take()`（拿走即殺掉）同時涵蓋 ssh 的程序樹與 wg 的任務樹
    job: Option<(u64, Worker)>,
}

/// 一輪連線持有的「殺得掉的東西」（設計書 §4.2）。
///
/// ssh 是 Job Object handle、wg 是一棵 tokio 任務樹的 CancellationToken，
/// 兩者都靠 Drop 收尾，所以 `rt.job.take()`（拿走即殺掉）那條既有語意
/// 一字不改就同時涵蓋兩種，`store_job`／`kill_job_of`／`kill_all_jobs`
/// 的世代守門與那幾條競態論證也完全不用重寫。
///
pub(crate) enum Worker {
    // 兩個欄位都**只為了 Drop 而持有**，沒有讀取端是刻意的：handle 一關，
    // 整棵 ssh 程序樹（含 ProxyCommand 的孫程序）就結束；權杖一取消，
    // 整棵 wg 任務樹（引擎 + 所有列的監聽器）就結束
    Ssh(#[allow(dead_code)] Job),
    Wg(#[allow(dead_code)] crate::wg::CancelGuard),
}

/// `store_job` 的世代守門，抽成純函式才測得到（W6.2）。
///
/// 世代相符才收下並回 true；不符時 `worker` 就在這裡 drop——那條剛 spawn 出來、
/// 已經沒有人要的連線（ssh 程序樹或 wg 任務樹）當場被收乾淨。
///
/// 「不符時不可以蓋掉既有的那一份」也是契約的一部分：新世代的 worker 可能已經
/// 就位，舊迴圈晚到的這一手若照存，被蓋掉的那個 handle 一 drop，剛接起來的連線
/// 當場被殺，留下的反而是舊世代那條沒人管的。
pub(crate) fn store_worker(
    slot: &mut Option<(u64, Worker)>,
    rt_generation: u64,
    generation: u64,
    worker: Worker,
) -> bool {
    if rt_generation != generation {
        // worker 在這一行結束時 drop：Job handle 關閉／CancellationToken 取消
        drop(worker);
        return false;
    }
    *slot = Some((generation, worker));
    true
}

/// `kill_all_jobs` 的核心：收掉所有 worker，回報要寫成 stopped 的埠（W6.3）。
///
/// 本來就沒人在跑的那些埠不回報——呼叫端據此決定要不要推事件，沒有這道閘就會
/// 對著一整排早就 stopped 的出口再推一次一模一樣的事件。
pub(crate) fn drain_workers(slots: &mut BTreeMap<u16, Option<(u64, Worker)>>) -> Vec<u16> {
    let mut stopped = Vec::new();
    for (local, slot) in slots.iter_mut() {
        if slot.take().is_some() {
            stopped.push(*local);
        }
    }
    stopped
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
            detected: None,
            generation: 0,
            test_epoch: 0,
            supervisor: None,
            job: None,
        }
    }
}

/// 一條 wg 連線的執行期狀態。
///
/// 引擎的身分是**連線的 name**（§5.2）：一條連線有 0..N 條列，沒有哪一個埠有
/// 資格代表整條隧道。各列自己的執行期狀態仍住在以 u16 為鍵的 `exits` 裡，D5 不變。
#[derive(Default)]
struct WgEngineRuntime {
    generation: u64,
    supervisor: Option<u64>,
    /// 引擎那棵任務樹的取消權杖，拿走即收掉（含所有列的監聽器）
    worker: Option<(u64, Worker)>,
}

/// 設定檔所在資料夾，`wgProxies.confPath` 的相對路徑基準（W3.19）
fn config_dir(path: &std::path::Path) -> &std::path::Path {
    path.parent().unwrap_or_else(|| std::path::Path::new("."))
}

/// 把每條 wg 連線的 `.conf` 讀一遍，成功存摘要、失敗存訊息。
///
/// **不握手、不解析主機名**——這是給編輯面板與卡片副標看的唯讀摘要，
/// 金鑰一個位元組都不在其中（`ConfSummary` 的欄位就那幾個）。
fn read_wg_confs(
    cfg: &Config,
    dir: &std::path::Path,
) -> HashMap<String, Result<crate::wg::conf::ConfSummary, String>> {
    cfg.wg_proxies
        .iter()
        .map(|p| {
            let path = crate::config::resolve_conf_path(dir, &p.conf_path);
            (p.name.clone(), crate::wg::inspect_conf(&path))
        })
        .collect()
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
    /// 每條 wg 連線一份引擎執行期狀態，鍵是連線名（§5.2）
    wg_engines: Mutex<HashMap<String, WgEngineRuntime>>,
    /// 每條 wg 連線的 `.conf` 摘要，讀不到／解析不過時存錯誤訊息。
    /// 設定一變就重整（見 `sync_wg_confs`），快照與系統匣都吃它，不各自讀檔
    wg_confs: Mutex<HashMap<String, Result<crate::wg::conf::ConfSummary, String>>>,
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
    /// 已經下載好、等下一次啟動才安裝的那一版，None 代表暫存區是空的
    pending: Mutex<Option<crate::update::Pending>>,
    /// 「知道有新版，但下載失敗了、正在退避等下一次試」。
    ///
    /// 沒有這一格的話，介面只看得到「有新版」與「有東西就緒」兩個事實，
    /// 於是「有新版但還沒就緒」一律被畫成轉圈的 Downloading…——網路壞掉時
    /// 那顆 spinner 會轉上一整天，而它宣稱的事情根本沒有在發生。
    update_stalled: AtomicBool,
}

impl AppState {
    pub fn new(app: AppHandle, path: PathBuf, cfg: Config) -> Self {
        let exits = cfg.locals().into_iter().map(|p| (p, ExitRuntime::default())).collect();
        let wg_confs = read_wg_confs(&cfg, config_dir(&path));
        let wg_engines: HashMap<String, WgEngineRuntime> =
            cfg.wg_proxies.iter().map(|p| (p.name.clone(), WgEngineRuntime::default())).collect();
        AppState {
            app,
            path,
            cfg: Mutex::new(cfg),
            logs: Mutex::new(VecDeque::new()),
            exits: Mutex::new(exits),
            wg_engines: Mutex::new(wg_engines),
            wg_confs: Mutex::new(wg_confs),
            testing: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
            tray_hint_shown: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            read_only: AtomicBool::new(false),
            update: Mutex::new(None),
            pending: Mutex::new(None),
            update_stalled: AtomicBool::new(false),
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
        self.update_config_checked(|c| Ok::<T, Infallible>(edit(c))).map(|r| match r {
            Ok(v) => v,
            // 閉包回的是 Infallible，這一支永遠到不了
            Err(never) => match never {},
        })
    }

    /// 與 `update_config` 相同，但閉包有否決權：回 Err 就當作這次操作沒發生
    /// ——不寫檔、記憶體不動，錯誤原樣交回呼叫端。
    ///
    /// 唯一性那種「要看過整份設定才知道」的驗證必須在閉包裡再做一次。指令層的
    /// 標準流程是「先 with_config 讀一份來驗，驗過了再 update_config 寫下去」，
    /// 而這兩步之間 cfg 鎖是放開的：兩個同時進來的新增可以雙雙通過驗證，
    /// 再一前一後把兩筆同名的源（或同一個本地埠）push 進去。閉包裡這一次
    /// 重驗是在鎖裡做的，成本只是再走一遍幾個字串比較。
    pub fn update_config_checked<F, T, E>(&self, edit: F) -> std::io::Result<Result<T, E>>
    where
        F: FnOnce(&mut Config) -> Result<T, E>,
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
        let out = match edit(&mut next) {
            Ok(v) => v,
            // 被否決：next 是一份還沒公開過的複本，直接丟掉就等於什麼都沒發生
            Err(e) => return Ok(Err(e)),
        };
        crate::config::write_config_at(&self.path, &next)?;
        *guard = next;
        drop(guard);
        self.sync_exits();
        Ok(Ok(out))
    }

    /// 設定裡新增或刪掉出口後，補齊／清掉對應的執行期狀態。
    ///
    /// 丟掉的那些 `ExitRuntime` 會連同它持有的 Job handle 一起 drop，被刪掉的
    /// 出口那條 ssh 程序樹當場就收掉了；刪除流程因此可以先存檔再停線，
    /// 不必為了收程序而搶在存檔之前 halt。
    fn sync_exits(&self) {
        let ports = self.with_config(|c| c.locals());
        {
            let mut exits = self.exits.lock().unwrap();
            exits.retain(|p, _| ports.contains(p));
            for p in ports {
                exits.entry(p).or_default();
            }
        }
        self.sync_wg_engines();
        self.sync_wg_confs();
    }

    /// 設定裡刪掉一條 wg 連線後，把它的引擎執行期狀態一併清掉。
    ///
    /// 丟掉的那份 `WgEngineRuntime` 會連同 `CancelGuard` 一起 drop，那顆引擎的
    /// 整棵任務樹（含所有列的監聽器）當場收掉——與 `sync_exits` 對 ssh 那條
    /// 「刪掉出口就等於收掉程序樹」的性質一致，刪除流程因此可以先存檔再停線。
    fn sync_wg_engines(&self) {
        let names =
            self.with_config(|c| c.wg_proxies.iter().map(|p| p.name.clone()).collect::<Vec<_>>());
        let mut engines = self.wg_engines.lock().unwrap();
        engines.retain(|name, _| names.contains(name));
        // 補齊，與 `sync_exits` 一樣：項目的生死只由這一支決定，其他地方一律
        // 只改既存項（見 `with_engine_mut`）
        for name in names {
            engines.entry(name).or_default();
        }
    }

    /// 重讀每條 wg 連線的 `.conf` 摘要。設定一變就跑一次（改了 confPath、
    /// 新增／刪除連線都算），快照與系統匣之後都只讀快取，不再碰磁碟。
    fn sync_wg_confs(&self) {
        let fresh = self.with_config(|c| read_wg_confs(c, config_dir(&self.path)));
        *self.wg_confs.lock().unwrap() = fresh;
    }

    /// 從磁碟重讀 `.conf` 摘要並全量推一次——外部檔案被改過（或使用者按下重新
    /// 連線）時，畫面上那幾行唯讀資訊才跟得上
    pub fn reload_wg_confs(&self) {
        self.sync_wg_confs();
    }

    /// 這條 wg 連線的 `.conf` 解析錯誤，讀得過就是 None。
    /// **壞 conf 的連線不准啟動**——引擎沒有東西可以拿去建隧道。
    pub fn wg_conf_error(&self, conn: &str) -> Option<String> {
        self.wg_confs.lock().unwrap().get(conn).and_then(|r| r.as_ref().err()).cloned()
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
    /// 這一版**不看世代**。目前唯一的呼叫端是 `tunnel::halt`，而它正是在遞增
    /// 世代之後才把狀態壓成 stopped——帶著守門反而寫不進去。監看迴圈那種
    /// 「算出來的時候還算數、寫下去時可能已經過期」的狀態一律走
    /// [`set_exit_status_of`]；日後要是有新的呼叫端，先確認它寫的真的是
    /// 「當下這一刻的事實」，不然預設就該用有守門的那一版。
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
            exits
                .get_mut(&local)
                .is_some_and(|rt| guarded_write_status(rt, generation, status, &detail))
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
        self.write_exit_test_of(local, token, TestView::plain(state, text));
    }

    /// 帶協定徽章的版本（§5.3 的 `TestView.protocol`）。識別不出來時 `protocol`
    /// 是 None，序列化後那個鍵整個不出現——送空字串等於叫前端畫一顆空白徽章。
    pub fn set_exit_test_view_of(&self, local: u16, token: TestToken, view: TestView) {
        self.write_exit_test_of(local, token, view);
    }

    fn write_exit_test_of(&self, local: u16, token: TestToken, view: TestView) {
        let written = {
            let mut exits = self.exits.lock().unwrap();
            exits.get_mut(&local).is_some_and(|rt| guarded_write_test(rt, token, &view))
        };
        if written {
            let _ = self.app.emit("exit-test", ExitTestPayload { local, result: Some(view) });
        }
    }

    /// 這條列的協定識別快取（§1.5）。命中就不必再跑一次 `detect`（W8.24）
    pub fn detected_protocol(&self, local: u16) -> Option<crate::exits::ProxyProtocol> {
        self.exits.lock().unwrap().get(&local).and_then(|rt| rt.detected)
    }

    /// 記下識別結果，但**只在憑證還算數時**——不然一份對舊連線做的識別會被寫進
    /// 新連線的快取，徽章就一直掛著另一台伺服器的協定
    pub fn set_detected_protocol(
        &self,
        local: u16,
        token: TestToken,
        protocol: crate::exits::ProxyProtocol,
    ) {
        self.with_exit_mut(local, |rt| {
            if rt.token() == token {
                rt.detected = Some(protocol);
            }
        });
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
            let had = rt.last_test.is_some();
            // 自測顯示與協定識別快取一起歸零（§1.5／W8.25）。一次解構而不是兩行
            // 賦值，日後就不會有人只更新其中一行
            (rt.last_test, rt.detected) = cleared_test_state(rt.last_test.take(), rt.detected);
            had
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

    /// 這個出口的監看位子上有沒有人。
    ///
    /// **只讀，不搶位子**——看門狗要問的正是「有沒有」，拿 `claim_supervisor`
    /// 去問等於自己把位子占走，之後真正的監看迴圈反而起不來。
    pub fn has_supervisor(&self, local: u16) -> bool {
        self.with_exit_mut(local, |rt| rt.supervisor.is_some()).unwrap_or(false)
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

    /// 讓這幾條列跟著引擎的世代走。
    ///
    /// wg 沒有「每條列一個監看迴圈」——底下所有列的狀態都由引擎那一條迴圈代寫，
    /// 所以 `set_exit_status_of` 要比對的自然是**引擎的世代**。每一輪開頭發一次
    /// 號碼，halt 換掉引擎世代之後，舊迴圈晚到的那一手就寫不進去了。
    pub fn adopt_rows(&self, locals: &[u16], generation: u64) {
        let mut exits = self.exits.lock().unwrap();
        for local in locals {
            if let Some(rt) = exits.get_mut(local) {
                rt.generation = generation;
            }
        }
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
    pub(crate) fn store_job(&self, local: u16, generation: u64, worker: Worker) {
        self.with_exit_mut(local, |rt| {
            let rt_generation = rt.generation;
            store_worker(&mut rt.job, rt_generation, generation, worker);
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

    /// 收掉所有出口的 ssh 程序，離開程式時用。
    ///
    /// 程序收掉了，狀態也要跟著寫成 stopped：這個函式不只跑在「馬上就要 exit」
    /// 的路上，就地更新那條路是在交棒給安裝程式之前呼叫它，安裝失敗時程式還會
    /// 留在原地——狀態沒改的話，介面與系統匣會停在「connected」，而背後那些
    /// ssh 早就沒了。
    ///
    /// **死鎖警告**：這裡不可以呼叫 `set_exit_status`，它會經由 `with_exit_mut`
    /// 再取一次同一把 exits 鎖，當場自我死鎖。所以在 guard 裡直接改 `rt.status`
    /// 並把要通知的埠蒐集起來，放掉 guard 之後才推事件；`refresh_tray` 也一樣
    /// （它會取 cfg 與 exits 兩把鎖），而且整批只重算一次——系統匣本來就是
    /// 整份重建，逐埠各刷一次只是白做工。
    pub fn kill_all_jobs(&self) {
        // wg 的引擎不住在 exits 裡（它的身分是連線名），要另外收一次；
        // 各列的監聽器是那棵任務樹的一部分，跟著 CancelGuard 一起走。
        // **只收 worker、不刪項目**：這一支也跑在「交棒給安裝程式之前」那條路上，
        // 安裝失敗時程式還留在原地，項目被刪掉的話之後就再也 claim 不到位子了
        {
            let mut engines = self.wg_engines.lock().unwrap();
            for rt in engines.values_mut() {
                rt.generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
                rt.supervisor = None;
                rt.worker.take();
            }
        }
        let mut stopped = Vec::new();
        {
            let mut exits = self.exits.lock().unwrap();
            // 先把每個出口的 worker 摘出來交給 drain_workers 統一收掉：「拿走即
            // 殺掉」那條語意只有一份實作，ssh 的程序樹與 wg 的任務樹都涵蓋（W6.3）。
            // 它回報的是「真的收掉了東西」的埠，這裡用不到——底下要把**每一個**
            // 出口都壓成 stopped，不只是有 worker 的那些
            let mut slots: BTreeMap<u16, Option<(u64, Worker)>> =
                exits.iter_mut().map(|(local, rt)| (*local, rt.job.take())).collect();
            drop(drain_workers(&mut slots));
            for (local, rt) in exits.iter_mut() {
                rt.generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
                rt.supervisor = None;
                if write_status(rt, status::STOPPED, &None) {
                    stopped.push(*local);
                }
            }
        }
        if stopped.is_empty() {
            return;
        }
        for local in stopped {
            let _ = self.app.emit(
                "exit-status",
                ExitStatusPayload { local, status: status::STOPPED.into(), detail: None },
            );
        }
        self.refresh_tray();
    }

    // ------------------------------------------------------------ wg 引擎的世代守門
    //
    // 與 ssh 出口那一組（claim_supervisor／next_generation／store_job）逐點對稱，
    // 只是鍵從 u16 換成連線名——引擎的身分是連線，不是某個埠（§5.2）。

    /// 對既存的那一份引擎狀態就地改一筆，回傳 None 代表這條連線已經不在了。
    ///
    /// 與 `with_exit_mut` 同一條紀律：項目**只由 `sync_wg_engines` 依設定建立**，
    /// 其餘地方一律只改既存項。晚到的更新若順手把項目補回來，就會生出一條設定
    /// 裡根本不存在的幽靈連線，而它要撐到下一次設定變動才會被清掉。
    fn with_engine_mut<T>(
        &self,
        conn: &str,
        f: impl FnOnce(&mut WgEngineRuntime) -> T,
    ) -> Option<T> {
        self.wg_engines.lock().unwrap().get_mut(conn).map(f)
    }

    /// 這條連線的監看位子上有沒有人。理由同 [`AppState::has_supervisor`]：只讀，不搶
    pub fn wg_has_supervisor(&self, conn: &str) -> bool {
        self.with_engine_mut(conn, |rt| rt.supervisor.is_some()).unwrap_or(false)
    }

    /// 搶下這條連線的監看位子，回傳 None 代表已經有一顆引擎在跑（或連線不在了）
    pub fn wg_claim_supervisor(&self, conn: &str) -> Option<u64> {
        let counter = &self.generation;
        self.with_engine_mut(conn, |rt| {
            let claimed =
                claim_slot(&mut rt.supervisor, || counter.fetch_add(1, Ordering::SeqCst) + 1);
            if let Some(generation) = claimed {
                rt.generation = generation;
            }
            claimed
        })
        .flatten()
    }

    pub fn wg_release_supervisor(&self, conn: &str, generation: u64) {
        self.with_engine_mut(conn, |rt| release_slot(&mut rt.supervisor, generation));
    }

    /// 換世代並當場騰出位子，順手收掉那棵任務樹。
    /// 緊接著的 start 不必等舊迴圈醒來就能接手（與 ssh 的 halt 同一套）
    pub fn wg_next_generation(&self, conn: &str) {
        let counter = &self.generation;
        self.with_engine_mut(conn, |rt| {
            rt.generation = counter.fetch_add(1, Ordering::SeqCst) + 1;
            rt.supervisor = None;
            rt.worker.take();
        });
    }

    pub fn wg_generation_alive(&self, conn: &str, generation: u64) -> bool {
        self.wg_engines.lock().unwrap().get(conn).map(|rt| rt.generation) == Some(generation)
    }

    /// 記下這一輪引擎的任務樹，只在世代還是自己那一代時才收下（W6.2 的守門）
    pub(crate) fn wg_store_worker(&self, conn: &str, generation: u64, worker: Worker) {
        self.with_engine_mut(conn, |rt| {
            let rt_generation = rt.generation;
            store_worker(&mut rt.worker, rt_generation, generation, worker);
        });
    }

    /// 只在世代相符時收掉任務樹，避免誤殺新的一輪
    pub fn wg_kill_worker_of(&self, conn: &str, generation: u64) {
        self.with_engine_mut(conn, |rt| {
            if rt.worker.as_ref().map(|(g, _)| *g) == Some(generation) {
                rt.worker.take();
            }
        });
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

    /// 這次執行要不要自動更新。設定檔沒寫的話兩種模式都算開，
    /// 理由見 `config::DEFAULT_AUTOMATIC_UPDATES`
    pub fn checks_for_updates(&self) -> bool {
        self.with_config(|c| c.checks_for_updates())
    }

    /// 記下背景檢查的結果並推事件；跟上次一樣就不重推，回傳值就是「這次有沒有變」。
    ///
    /// 每 24 小時會再查一次，同一個新版本重複推的話，設定頁那一列會無謂重畫，
    /// 也讓事件流看起來像真的又發生了什麼事。呼叫端拿回傳值決定要不要在活動
    /// 日誌記那一行「偵測到新版」——同一個理由，一版只記一次。
    pub fn set_update(&self, info: Option<UpdateInfo>) -> bool {
        {
            let mut slot = self.update.lock().unwrap();
            if *slot == info {
                return false;
            }
            *slot = info.clone();
        }
        let _ = self.app.emit("update-available", info);
        true
    }

    pub fn update_info(&self) -> Option<UpdateInfo> {
        self.update.lock().unwrap().clone()
    }

    /// 記下暫存區裡那份就緒的更新；跟上次一樣就不重推。
    ///
    /// 變了就全量推一次：設定頁那顆鈕與系統匣的「Restart to update」都吃這一份，
    /// 而它們平常是靠 config-changed 更新的，這裡沿用同一條路就不必再多一種事件。
    pub fn set_staged(&self, pending: Option<crate::update::Pending>) {
        {
            let mut slot = self.pending.lock().unwrap();
            if *slot == pending {
                return;
            }
            *slot = pending;
        }
        self.emit_config_changed();
    }

    /// 就緒的那一版版本號（不帶 v），暫存區空的就是 None
    pub fn staged_version(&self) -> Option<String> {
        self.pending.lock().unwrap().as_ref().map(|p| p.version.clone())
    }

    /// 記下「下載卡住了／又動起來了」；值沒變就不重推。
    ///
    /// 這一格是介面用來分辨「正在下載」與「下載失敗、等著重試」的唯一依據，
    /// 所以每一次嘗試開始時要清掉、失敗時要設起來，兩邊都不能漏。
    pub fn set_update_stalled(&self, stalled: bool) {
        if self.update_stalled.swap(stalled, Ordering::SeqCst) != stalled {
            self.emit_config_changed();
        }
    }

    pub fn update_stalled(&self) -> bool {
        self.update_stalled.load(Ordering::SeqCst)
    }

    /// 每個源與其出口的當下樣貌，Snapshot 與系統匣選單共用這一份算法。
    ///
    /// 鎖序是 cfg → exits，全程式只有這裡同時持有兩把，不會反向配對。
    pub fn source_views(&self) -> Vec<SourceView> {
        self.with_config(|cfg| build_views(cfg, &self.exits.lock().unwrap()))
    }

    /// 每條 wg 連線與其列的當下樣貌，Snapshot 與系統匣選單共用這一份算法
    pub fn wg_views(&self) -> Vec<WgProxyView> {
        self.with_config(|cfg| {
            build_wg_views(cfg, &self.exits.lock().unwrap(), &self.wg_confs.lock().unwrap())
        })
    }

    /// 取快照的同時配一張系統匣套用號碼牌，兩者在**同一次 exits 鎖內**完成。
    ///
    /// 號碼牌的用途是讓晚算出來的快照永遠贏過早算出來的（`traymenu::refresh`
    /// 會拿它跟全域計數器比，比輸就整份丟掉）。快照與號碼之間要是放掉了鎖，
    /// 這個保證就不成立：兩條執行緒可以在「A 取完快照、還沒配號」時交錯，
    /// 讓 A 拿到比較大的號碼卻載著比較舊的快照，於是 B 那份新的先被貼上去、
    /// 又被 A 那份舊的蓋掉，系統匣就這樣停在過期的狀態直到下一次狀態變化。
    fn views_with_seq(&self) -> (Vec<SourceView>, Vec<WgProxyView>, Option<String>, u64) {
        let ready = self.staged_version();
        self.with_config(|cfg| {
            let exits = self.exits.lock().unwrap();
            let views = build_views(cfg, &exits);
            let wg = build_wg_views(cfg, &exits, &self.wg_confs.lock().unwrap());
            (views, wg, ready, crate::traymenu::next_seq())
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot_with(self.source_views(), self.wg_views())
    }

    /// 已經算好那兩份檢視的呼叫端走這裡，不要再算一次
    fn snapshot_with(&self, sources: Vec<SourceView>, wg_proxies: Vec<WgProxyView>) -> Snapshot {
        Snapshot {
            close_to_tray: self.with_config(|c| c.close_to_tray),
            autostart: self.autostart(),
            automatic_updates: self.checks_for_updates(),
            sources,
            wg_proxies,
            logs: self.logs.lock().unwrap().iter().cloned().collect(),
            update: self.update_info(),
            pending_update: self.staged_version(),
            update_stalled: self.update_stalled(),
        }
    }

    /// 任何設定變更後全量推一次。
    ///
    /// 前端的 Snapshot 與系統匣選單吃的是同一份 `source_views`，算一次就好：
    /// 系統匣只讀（`refresh` 當場把它轉成選單模型），讀完再把那一份讓給要
    /// 序列化的 Snapshot。兩個接收端彼此獨立，先後順序不影響結果。
    pub fn emit_config_changed(&self) {
        let (sources, wg, ready, seq) = self.views_with_seq();
        crate::traymenu::refresh(&self.app, &sources, &wg, ready.as_deref(), seq);
        let _ = self.app.emit("config-changed", self.snapshot_with(sources, wg));
    }

    /// 系統匣的提示文字與右鍵選單都跟著狀態走，狀態一變就整份重算。
    ///
    /// 鎖紀律：先取快照與號碼牌（鎖在 `views_with_seq` 裡取完就放掉），之後只碰
    /// 快照，真正碰 tray 的動作在背景執行緒上做，絕不持鎖呼叫系統匣。
    pub fn refresh_tray(&self) {
        let (sources, wg, ready, seq) = self.views_with_seq();
        crate::traymenu::refresh(&self.app, &sources, &wg, ready.as_deref(), seq);
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
            result: Some(TestView::plain(test_state::OK, "1.2.3.4  Taipei, TW")),
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
        let view = || Some(TestView::plain(test_state::OK, "1.2.3.4"));
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

    // ------------------------------------------------------------ 世代／憑證守門

    /// halt 之後的樣子：世代已經被換掉，狀態壓成 stopped，自測清乾淨
    fn halted(generation: u64, epoch: u64) -> ExitRuntime {
        ExitRuntime {
            status: status::STOPPED.into(),
            detail: None,
            last_test: None,
            detected: None,
            generation,
            test_epoch: epoch,
            supervisor: None,
            job: None,
        }
    }

    /// 守門的核心契約：世代不符時是**零寫入**，不是「寫了但不推事件」。
    ///
    /// 這正是那條競態的形狀——halt 已經把出口壓成 stopped 並換了世代，
    /// 舊監看迴圈晚一步把 connected 交上來。放它進去的話出口會顯示連著、
    /// 實際上線已經停了，而且再也沒有事件會來糾正。
    #[test]
    fn a_stale_generation_writes_nothing() {
        let mut rt = halted(8, 1);
        assert!(!guarded_write_status(&mut rt, 7, status::CONNECTED, &None), "回 false");
        assert_eq!(rt.status, status::STOPPED, "狀態不可以被舊迴圈蓋掉");
        assert_eq!(rt.detail, None);
    }

    /// detail 也在守門範圍內：只擋 status 不擋 detail 的話，
    /// 出口會顯示 stopped 卻掛著上一輪的錯誤訊息
    #[test]
    fn a_stale_generation_does_not_touch_the_detail_either() {
        let mut rt = ExitRuntime {
            status: status::PORT_BUSY.into(),
            detail: Some("Local port 1080 is already in use.".into()),
            generation: 8,
            ..Default::default()
        };
        assert!(!guarded_write_status(&mut rt, 7, status::ERROR, &Some("boom".into())));
        assert_eq!(rt.status, status::PORT_BUSY);
        assert_eq!(rt.detail.as_deref(), Some("Local port 1080 is already in use."));
    }

    /// 世代相符時照常寫進去——守門不能嚴到把當代的更新也擋掉
    #[test]
    fn the_current_generation_still_writes_through() {
        let mut rt = halted(8, 1);
        assert!(guarded_write_status(&mut rt, 8, status::CONNECTED, &None));
        assert_eq!(rt.status, status::CONNECTED);
    }

    /// 世代相符但值沒變時回 false，走的是「不重推事件」那條規則，
    /// 與守門擋下來是兩回事
    #[test]
    fn an_unchanged_status_reports_no_change_even_when_the_generation_matches() {
        let mut rt = halted(8, 1);
        assert!(!guarded_write_status(&mut rt, 8, status::STOPPED, &None));
        assert_eq!(rt.status, status::STOPPED);
    }

    /// 自測寫入的守門同樣要零寫入：重接之後 last_test 是空的，
    /// 舊探測的結果不可以把上一條連線的出口 IP 填回來
    #[test]
    fn a_stale_token_writes_no_test_result() {
        let mut rt = halted(8, 2);
        let view = TestView::plain(test_state::OK, "1.2.3.4  Taipei, TW");
        assert!(!guarded_write_test(&mut rt, token(7, 2), &view), "世代不符要擋下");
        assert_eq!(rt.last_test, None);
        // 世代對、期號不對（ssh 自己掛掉後在同一代裡重連）一樣要擋下
        assert!(!guarded_write_test(&mut rt, token(8, 1), &view), "期號不符要擋下");
        assert_eq!(rt.last_test, None);
    }

    /// 舊憑證也不可以覆蓋掉當代已經寫進去的結果
    #[test]
    fn a_stale_token_cannot_overwrite_a_current_result() {
        let mut rt = halted(8, 2);
        let fresh = TestView::plain(test_state::OK, "5.6.7.8");
        assert!(guarded_write_test(&mut rt, token(8, 2), &fresh));
        let stale = TestView::plain(test_state::FAIL, "no response");
        assert!(!guarded_write_test(&mut rt, token(8, 1), &stale));
        assert_eq!(rt.last_test.as_ref(), Some(&fresh), "當代的結果要留著");
    }

    /// 憑證相符時照常寫進去
    #[test]
    fn a_current_token_writes_the_test_result_through() {
        let mut rt = halted(8, 2);
        let view = TestView::plain(test_state::OK, "1.2.3.4");
        assert!(guarded_write_test(&mut rt, token(8, 2), &view));
        assert_eq!(rt.last_test.as_ref(), Some(&view));
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
