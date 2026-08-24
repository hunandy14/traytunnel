//! WireGuard → 本地 SOCKS5（行程內使用者態隧道）的模組根與生命週期。
//!
//! 對外提供與 `ssh::tunnel` 完全對稱的一組動詞，內部維護每顆引擎的監看迴圈，
//! **不含**任何協定細節（設計書 §1.1）。
//!
//! 與 ssh 的唯一結構性差異是**身分**：ssh 一個出口就是一條連線，身分是本地埠；
//! wg 一條隧道底下有 0..N 條列，沒有哪個埠代表得了它，身分因此是連線的 `name`
//! （§5.2）。各列的執行期狀態仍住在同一份以本地埠為鍵的表裡，D5 不受影響。

pub mod conf;
pub mod device;
pub mod dns;
pub mod engine;
pub mod socks5;
pub mod stack;

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::state::{status, AppState, Worker};
use crate::winsys::is_listening;

/// 引擎斷線後的重連間隔，與 `ssh::tunnel::RETRY` 同值同理由
pub const RETRY: Duration = Duration::from_secs(5);

/// reconnecting 連續卡這麼久，就把整顆引擎重建一次。
///
/// **動機是 DDNS**：`[Peer] Endpoint` 寫的是主機名時，`resolve_endpoint` 只在
/// `engine::spawn` 那一刻跑一次。家裡的寬頻換了 IP（或對端的 DDNS 紀錄更新）
/// 之後，引擎會抱著一個已經沒人在聽的舊位址一直重送握手，**永遠不會自己好**
/// ——使用者掛機一整天回來看到的就是一條紅著的隧道，非得手動重連不可。
/// 重建引擎是唯一會重跑端點解析的路徑（§5.2 的生命週期）。
///
/// 60 秒是「網路只是抖一下」與「端點真的搬家了」之間的分界：WireGuard 自己的
/// 重試（`REKEY_TIMEOUT` 5 秒）在這段時間內已經試過十幾次，還沒起來就不是抖動。
pub const RECONNECT_REBUILD_AFTER: Duration = Duration::from_secs(60);

/// 進入 reconnecting 時記的那一行。stale 路徑以前是完全靜默的，
/// 使用者只看得到一個變色的點，日誌裡卻查不到任何線索
const HANDSHAKE_RETRY_LOG: &str = "handshake not completed, retrying";

/// 卡在 reconnecting 太久、要重建引擎時記的那一行
const REBUILD_LOG: &str = "rebuilding engine to re-resolve endpoint";

/// 埠佔用預檢的複查間隔，與 `ssh::tunnel::PORT_GRACE` 同值同理由
pub const PORT_GRACE: Duration = Duration::from_millis(500);

/// 一輪連線的取消權杖包裝。
///
/// Drop 時 cancel()，於是 `state.rs` 既有的 `rt.job.take()` 語意（拿走即殺掉）
/// 一字不改就同時涵蓋 ssh 的 Job 與 wg 的任務樹（設計書 §4.2）。
pub struct CancelGuard(pub CancellationToken);

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// 引擎監看迴圈的巡檢間隔，與 `ssh::tunnel::POLL` 同值同理由。
///
/// 這裡不是拿來輪詢狀態的（狀態由 `EngineEvent` 推過來），只是讓卡在
/// `events.recv()` 的迴圈每隔一段時間醒來看一眼自己的世代還算不算數。
const POLL: Duration = Duration::from_millis(2000);

/// `.conf` 測試的總上限，與 `ssh::tunnel` 的連線測試同值同理由
pub const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// `test_conf` 的失敗訊息。**一律是固定字串**：訊息會出現在對話框與活動日誌上，
/// 帶進端點主機名、位址、DNS 或金鑰就等於把 `.conf` 的內容漏出去（U2 的紅線）。
const ENDPOINT_UNRESOLVED: &str = "無法解析 [Peer] Endpoint 的位址";
const BIND_FAILED: &str = "綁不到本地 UDP 埠";
const HANDSHAKE_TIMEOUT: &str = "握手逾時：15 秒內沒有收到對端回應";
const DEVICE_STOPPED: &str = "隧道在握手完成前就結束了";

/// 確保這條 wg 連線有一條引擎在跑；已經有就 no-op。
///
/// 語意與 `ssh::tunnel::start` 一模一樣，包含「不會另起第二條」。
///
/// **身分是連線的 `name` 而不是某個埠**：一條連線有 0..N 條列（§1.2），
/// 沒有哪個埠代表得了它。底下一條啟用的列都沒有時直接 no-op，不起引擎（§5.2）。
pub fn start(state: &Arc<AppState>, conn: &str) {
    if !state.with_config(|c| should_run_engine(c, conn)) {
        return; // 零列或全部停用：沒有東西會用到這條隧道（§5.2）
    }
    // 壞掉的 `.conf` 拒絕啟動。放它進去的話畫面會出現一個規格上不存在的狀態：
    // 連線的點是紅的（confError），底下的列卻一條條變綠
    if let Some(err) = state.wg_conf_error(conn) {
        state.log_from(conn, format!("cannot start: {err}"));
        return;
    }
    let Some(generation) = state.wg_claim_supervisor(conn) else {
        return; // 已經有一顆引擎在跑
    };
    let st = state.clone();
    let name = conn.to_string();
    tauri::async_runtime::spawn(async move {
        supervise(&st, &name, generation).await;
        st.wg_release_supervisor(&name, generation);
    });
}

/// 停掉這條連線：遞增世代讓監看迴圈作廢，取消 CancellationToken 收掉整棵任務樹
/// （引擎 + 所有列的監聽器）。不動設定裡的 enabled。
pub fn halt(state: &Arc<AppState>, conn: &str) {
    // 引擎那一份先換號：worker 被 take 走的那一刻 CancelGuard 就 drop 了
    state.wg_next_generation(conn);
    // 底下**每一條列**各推一次 stopped（W6.5／W6.16）。連線層不推任何新事件
    // ——卡片標題的狀態是前端由各列彙總出來的（§5.3 的零新事件）
    for local in state.with_config(|c| halted_locals(c, conn)) {
        state.next_generation(local);
        state.clear_exit_test(local);
        state.set_exit_status(local, status::STOPPED, None);
    }
}

/// halt 後立刻 start，套用最新的 .conf 與列清單。
pub fn restart(state: &Arc<AppState>, conn: &str) {
    halt(state, conn);
    start(state, conn);
}

/// 起／停單一列。`start_exit`／`stop_exit` 打在 wg 的列上時走這裡。
///
/// 實作是**重建整顆引擎**，不是熱插拔那一條列的監聽器：列清單是在
/// `engine::spawn` 當下綁定的，要熱插拔就得讓引擎對外開一組 add／remove
/// 介面並把 `StackHandle` 一路留著。WireGuard 重新握手是幾百毫秒的事，
/// 比起維護那套機制與它帶來的一整類競態，重連一次是明顯更小的代價。
pub fn start_row(state: &Arc<AppState>, local: u16) {
    rebuild_owner(state, local);
}

pub fn halt_row(state: &Arc<AppState>, local: u16) {
    rebuild_owner(state, local);
}

/// 這條列所屬的連線重建一次。呼叫端負責先把 `enabled` 寫進設定，
/// 重建時的列清單自然就是新的那一份
fn rebuild_owner(state: &Arc<AppState>, local: u16) {
    let Some(conn) = state.with_config(|c| c.wg_proxy_of(local).map(|p| p.name.clone())) else {
        return;
    };
    restart(state, &conn);
}

/// 所有 enabled 的 wg 連線都拉起來（程式啟動與 start_all 都走這裡）
pub fn start_enabled(state: &Arc<AppState>) {
    for conn in connection_names(state) {
        start(state, &conn);
    }
}

/// 停掉所有 wg 連線
pub fn halt_all(state: &Arc<AppState>) {
    for conn in connection_names(state) {
        halt(state, &conn);
    }
}

/// 重接目前有列在跑的 wg 連線；整條都停著的維持停著
pub fn reconnect_running(state: &Arc<AppState>) {
    for conn in connection_names(state) {
        let running = state
            .with_config(|c| halted_locals(c, &conn))
            .into_iter()
            .any(|local| state.is_running(local));
        if running {
            restart(state, &conn);
        }
    }
}

fn connection_names(state: &Arc<AppState>) -> Vec<String> {
    state.with_config(|c| c.wg_proxies.iter().map(|p| p.name.clone()).collect())
}

/// 存檔前的 .conf 驗證＋真握手測試，回傳型別直接沿用 ssh 那一個。
///
/// 流程：解析 .conf → 綁一個臨時 UDP → 送 handshake initiation → 等握手完成
/// → 立刻拆掉。總上限 15 秒（與 `ssh::tunnel::TEST_TIMEOUT` 同值同理由）。
///
/// 兩件事情特別要緊：
///
/// * 解析就失敗時**完全不綁 UDP、不連外**（W9.3），錯誤訊息與 `inspect_conf`
///   是同一句；
/// * 臨時引擎在回傳前一定拆乾淨（W9.1／W9.2），不留任何背景任務。UDP 一律讓
///   OS 配埠，不用 `.conf` 裡的 `ListenPort`——那是正式連線要用的埠，
///   測試不可以把它搶走。
pub async fn test_conf(conf_path: &std::path::Path) -> crate::ssh::tunnel::TestConnectionResult {
    test_conf_within(conf_path, TEST_TIMEOUT).await
}

/// [`test_conf`] 的可注入逾時版。
///
/// 上限是規格的一部分（15 秒），但「逾時會失敗而不是誤報成功」這條性質不該
/// 要一個 15 秒的測試才驗得到（W9.2／W9.4），因此門檻做成參數。
pub(crate) async fn test_conf_within(
    conf_path: &std::path::Path,
    timeout: Duration,
) -> crate::ssh::tunnel::TestConnectionResult {
    use crate::ssh::tunnel::TestConnectionResult as R;

    let conf = match conf::load(conf_path) {
        Ok(c) => c,
        Err(e) => return R::fail(e),
    };
    let Ok(endpoint) = device::resolve_endpoint(&conf.endpoint).await else {
        return R::fail(ENDPOINT_UNRESOLVED);
    };

    let cancel = CancellationToken::new();
    // guard 在函式結束（含 early return 與逾時）時 drop，整棵任務樹一起收掉
    let _guard = CancelGuard(cancel.clone());
    let handle = match device::spawn(
        device::DeviceConfig {
            private_key: conf.private_key.0.clone(),
            peer_public_key: boringtun::x25519::PublicKey::from(conf.peer_public_key),
            preshared_key: conf.preshared_key,
            keepalive: conf.keepalive,
            endpoint,
            bind: unspecified_bind(&endpoint),
            stale_after: device::REJECT_AFTER,
            first_handshake_grace: device::FIRST_HANDSHAKE_GRACE,
        },
        cancel.clone(),
    ) {
        Ok(h) => h,
        Err(_) => return R::fail(BIND_FAILED),
    };

    let mut events = handle.events;
    let waited = tokio::time::timeout(timeout, async {
        loop {
            match events.recv().await {
                Some(device::DeviceEvent::HandshakeOk) => return Some(true),
                // PresharedKey 不符時對端根本不會回應，最後就是逾時（W9.4）
                Some(_) => continue,
                None => return None,
            }
        }
    })
    .await;

    match waited {
        Ok(Some(true)) => R::ok(),
        Ok(Some(false)) | Ok(None) => R::fail(DEVICE_STOPPED),
        Err(_) => R::fail(HANDSHAKE_TIMEOUT),
    }
}

/// 臨時測試用的 UDP 本地位址：版本跟著端點走，埠一律交給 OS 配
fn unspecified_bind(endpoint: &std::net::SocketAddr) -> std::net::SocketAddr {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    match endpoint {
        SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    }
}

/// 只解析不連線，給編輯面板顯示「這份 conf 裡有什麼」。
///
/// **不握手、不解析主機名**（W1.33）：端點寫一個解不出來的名字也照樣回摘要，
/// 那是重連時才要做的事。錯誤訊息與 [`conf::parse`] 是同一句（W1.34），金鑰
/// 一個位元組都不會出現在裡面。
pub fn inspect_conf(conf_path: &std::path::Path) -> Result<conf::ConfSummary, String> {
    conf::load(conf_path).map(|c| c.summary())
}

/// 握手歲數 → exit-status 字彙的映射（設計書 §4.2 的門檻表，W6.4）。
///
/// `None`→connecting；`Some(< REJECT_AFTER)`→connected；否則 reconnecting。
pub fn status_for_handshake(age: Option<Duration>) -> &'static str {
    match age {
        None => status::CONNECTING,
        Some(d) if d < device::REJECT_AFTER => status::CONNECTED,
        // 門檻一到就老實顯示 reconnecting：寧可早一點，也不要讓使用者盯著一個
        // 假的 connected 而流量石沉大海
        Some(_) => status::RECONNECTING,
    }
}

/// 這條連線停掉時，要一併壓成 stopped 的所有本地埠（底下每一條列各一次）。
pub fn halted_locals(cfg: &crate::config::Config, conn: &str) -> Vec<u16> {
    cfg.wg_proxy(conn).map(|p| p.forwards.iter().map(|f| f.local).collect()).unwrap_or_default()
}

/// 這條連線現在該啟動哪些列（W6.11）：**連線 enabled 且列 enabled**。
///
/// 連線層與列層是兩個獨立的意圖，`AND` 起來才是「這條列現在該不該跑」（§5.5）。
pub fn rows_to_start(cfg: &crate::config::Config, conn: &str) -> Vec<u16> {
    cfg.wg_proxy(conn)
        .filter(|p| p.enabled)
        .map(|p| p.forwards.iter().filter(|f| f.enabled).map(|f| f.local).collect())
        .unwrap_or_default()
}

/// 要不要替這條連線起一顆引擎（§5.2 的啟停條件，W6.8／W6.14）。
///
/// 零列或全部停用的連線不需要跑一顆 WireGuard——沒有任何東西會用到它，
/// 留一顆空轉的引擎只是白白吃著 UDP 埠與一個計時器。
pub fn should_run_engine(cfg: &crate::config::Config, conn: &str) -> bool {
    !rows_to_start(cfg, conn).is_empty()
}

/// 這條連線這一輪要用的隧道 MTU：**介面覆寫 ＞ `.conf` 明寫 ＞ 應用層預設**。
///
/// 三態各有各的理由，缺一不可：
///
/// * `override_mtu`（`WgProxy.mtu`）是使用者在連線表單上填的值。他之所以會去填，
///   通常正是因為 conf 給的（或預設的）那個值在他的線路上會黑洞，所以它最大。
///   **這不會改寫 `.conf` 檔**——那份檔案是別的工具產出的，我們只讀不寫。
/// * `conf_mtu` 是 `.conf` 明寫的 `[Interface] MTU`。對端管理員特意寫了值就照做。
/// * 兩者都沒有才輪到 [`conf::APP_DEFAULT_MTU`]。
///
/// 純函式，沒有 IO：這條優先序是規格本身，測試直接釘它而不必起一顆引擎。
pub fn effective_mtu(override_mtu: Option<usize>, conf_mtu: Option<usize>) -> usize {
    override_mtu.or(conf_mtu).unwrap_or(conf::APP_DEFAULT_MTU)
}

/// 這一輪引擎的握手觀測：狀態變化時該記哪一行日誌，以及 reconnecting
/// 卡了多久該重建引擎（DDNS 自癒，見 [`RECONNECT_REBUILD_AFTER`]）。
///
/// 抽成一顆**不碰 IO、`now` 由呼叫端傳進來**的小狀態機，時間相關的規格因此
/// 測得到而不必真的坐等 60 秒（W6.19～W6.21）。
pub(crate) struct HandshakeWatch {
    /// 「握手花了多久」的計時起點：引擎剛起來，或上一次翻進 reconnecting 的那一刻
    round_started: Instant,
    /// 目前這一段 reconnecting 從什麼時候開始；None 代表現在不在 reconnecting
    reconnecting_since: Option<Instant>,
    /// reconnecting 連續超過這麼久就該重建（production 是 [`RECONNECT_REBUILD_AFTER`]）
    rebuild_after: Duration,
    phase: Phase,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Phase {
    /// 這一輪還沒有任何結論
    Waiting,
    Ok,
    Reconnecting,
}

impl HandshakeWatch {
    pub(crate) fn new(now: Instant, rebuild_after: Duration) -> Self {
        HandshakeWatch {
            round_started: now,
            reconnecting_since: None,
            rebuild_after,
            phase: Phase::Waiting,
        }
    }

    /// 吃一顆引擎狀態，回傳「要記的那一行日誌」（沒變化就回 None）。
    ///
    /// 只有**變化**才記：device 那一層已經去抖過（只在 `Reported` 改變時推事件），
    /// 這裡再擋一次是為了 port_busy／error 之類的狀態夾在中間時不重複刷屏。
    pub(crate) fn on_status(&mut self, st: &'static str, now: Instant) -> Option<String> {
        match st {
            status::CONNECTED if self.phase != Phase::Ok => {
                self.phase = Phase::Ok;
                self.reconnecting_since = None;
                let ms = now.saturating_duration_since(self.round_started).as_millis();
                Some(format!("handshake ok in {ms}ms"))
            }
            status::RECONNECTING if self.phase != Phase::Reconnecting => {
                self.phase = Phase::Reconnecting;
                self.reconnecting_since = Some(now);
                // 下一次握手成功要從「掉線的這一刻」起算，而不是從引擎啟動起算
                self.round_started = now;
                Some(HANDSHAKE_RETRY_LOG.to_string())
            }
            _ => None,
        }
    }

    /// reconnecting 是不是已經卡超過門檻了
    pub(crate) fn overdue(&self, now: Instant) -> bool {
        self.reconnecting_since
            .is_some_and(|since| now.saturating_duration_since(since) >= self.rebuild_after)
    }
}

/// 引擎狀態 → 底下各列的狀態（W6.9）。
///
/// 「埠被佔住只影響那一條列」是與 ssh 不同的地方，而且是刻意的：ssh 一個出口
/// 就是一條連線，埠被佔就整條起不來；WG 一條隧道底下有多條列，其中一條的埠
/// 被佔沒有理由拖垮其他列（§5.2）。
pub fn row_statuses(rows: &[u16], engine: &'static str, busy: &[u16]) -> Vec<(u16, &'static str)> {
    rows.iter()
        .map(|local| (*local, if busy.contains(local) { status::PORT_BUSY } else { engine }))
        .collect()
}

/// `set_wg_enabled` 會做的事，依序（W6.13）。
///
/// 抽成一串步驟才測得到「存檔成功才動引擎」與 `apply_enabled` 那條刻意的
/// 不對稱：連接時先推事件再拉線（介面立刻看得到 connecting），中斷時先停線
/// 再推事件（不會出現「已停用但還連著」的那一瞬）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WgEnabledStep {
    EmitConfigChanged,
    StartEngine(String),
    HaltEngine(String),
}

/// * `saved` 為 false（存檔失敗）：引擎維持原狀，只推一次 `emit_config_changed`
///   把樂觀翻過去的開關拉回真值（沿用 `commands.rs::apply_enabled` 的通則）。
/// * `on` 為 true 但底下零條 enabled 的列：設定寫入成功，但**引擎不啟動**（W6.14）。
pub fn wg_enabled_steps(
    conn: &str,
    on: bool,
    saved: bool,
    has_enabled_row: bool,
) -> Vec<WgEnabledStep> {
    use WgEnabledStep::*;
    if !saved {
        // 設定沒存成功＝這次操作等於沒發生，引擎一動都不動；只把樂觀翻過去的
        // 開關拉回真值（沿用 `commands.rs::apply_enabled` 的通則）
        return vec![EmitConfigChanged];
    }
    match (on, has_enabled_row) {
        // 連接：先推事件再拉線，介面立刻看得到 connecting
        (true, true) => vec![EmitConfigChanged, StartEngine(conn.into())],
        // 設定寫進去了，但沒有任何列要跑，就不留一顆空轉的 WireGuard（§5.2）
        (true, false) => vec![EmitConfigChanged],
        // 中斷：先停線再推事件，不會出現「已停用但還連著」的那一瞬
        (false, _) => vec![HaltEngine(conn.into()), EmitConfigChanged],
    }
}

// ---------------------------------------------------------------- 監看迴圈

/// 這一輪引擎要跑的東西。設定被改掉時整輪重來，不做增量更新
struct Plan {
    conf_path: std::path::PathBuf,
    /// 使用者在連線表單上填的 MTU 覆寫值，沒填就是 None（見 [`effective_mtu`]）
    mtu: Option<usize>,
    /// 只含 enabled 的列，`socks` 已排在前（§5.3）
    rows: Vec<(String, engine::RowSpec)>,
    locals: Vec<u16>,
}

fn plan(state: &Arc<AppState>, conn: &str) -> Option<Plan> {
    use crate::config::RowKind;
    let dir = state.path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
    state.with_config(|c| {
        let p = c.wg_proxy(conn)?;
        if !p.enabled {
            return None;
        }
        let rows: Vec<(String, engine::RowSpec)> = crate::config::ordered_rows(&p.forwards)
            .into_iter()
            .filter(|f| f.enabled)
            .map(|f| {
                let spec = match f.kind {
                    RowKind::Socks => engine::RowSpec::Socks { local: f.local },
                    RowKind::Forward => engine::RowSpec::Forward {
                        local: f.local,
                        remote: f.remote.clone().unwrap_or_default(),
                    },
                };
                (f.name.clone(), spec)
            })
            .collect();
        let locals = rows.iter().map(|(_, r)| r.local()).collect();
        Some(Plan {
            conf_path: crate::config::resolve_conf_path(&dir, &p.conf_path),
            mtu: p.mtu,
            rows,
            locals,
        })
    })
}

/// 分段等待，中途世代作廢就立刻回 false（與 `ssh::tunnel::wait_alive` 同構）
async fn wait_alive(state: &Arc<AppState>, conn: &str, generation: u64, total: Duration) -> bool {
    let slice = Duration::from_millis(250);
    let mut waited = Duration::ZERO;
    while waited < total {
        tokio::time::sleep(slice).await;
        waited += slice;
        if !state.wg_generation_alive(conn, generation) {
            return false;
        }
    }
    true
}

/// 引擎的狀態攤到底下每一條列（§5.3 的零新事件）。
///
/// 被佔埠的那幾條列不跟著走——那是與 ssh 刻意不同的地方（§5.2）。
fn spread(
    state: &Arc<AppState>,
    generation: u64,
    locals: &[u16],
    busy: &[u16],
    engine_status: &'static str,
    detail: Option<String>,
) {
    for (local, st) in row_statuses(locals, engine_status, busy) {
        // 離開 connected 就把自測顯示與協定快取一起收掉（§1.5）
        if st != status::CONNECTED {
            state.clear_exit_test(local);
        }
        state.set_exit_status_of(local, generation, st, detail.clone());
    }
}

/// 逐條列預檢埠佔用，含 `PORT_GRACE` 複查。某一條被佔只影響那一條（§5.2）
async fn busy_rows(locals: &[u16]) -> Vec<u16> {
    let mut busy: Vec<u16> = locals.iter().copied().filter(|l| is_listening(*l)).collect();
    if !busy.is_empty() {
        // 自己的舊監聽器剛被收掉時，埠可能還殘留幾百毫秒才真的放掉
        tokio::time::sleep(PORT_GRACE).await;
        busy.retain(|l| is_listening(*l));
    }
    busy
}

/// 單一 wg 連線的監看迴圈，與 `ssh::tunnel::supervise` 同構（§2.1 的對照表）。
///
/// 每一次狀態寫入都走 `set_exit_status_of` 帶著**引擎這一代**的號碼：wg 沒有
/// 「每條列一個監看迴圈」，各列的狀態都是由這一條迴圈代寫的，守門要比的自然是
/// 引擎的世代，所以每一輪開頭先用 `adopt_rows` 把號碼發給底下每一條列。
async fn supervise(state: &Arc<AppState>, conn: &str, generation: u64) {
    loop {
        if !state.wg_generation_alive(conn, generation) {
            return;
        }
        // 連線被刪掉、被關掉、或一條啟用的列都不剩：收工，不留空轉的引擎
        let Some(plan) = plan(state, conn) else {
            return;
        };
        if plan.rows.is_empty() {
            return;
        }
        state.adopt_rows(&plan.locals, generation);

        let conf = match conf::load(&plan.conf_path) {
            Ok(c) => c,
            Err(e) => {
                // 訊息來自 conf::parse，裡面不含任何金鑰（W1.29／U2）
                spread(state, generation, &plan.locals, &[], status::ERROR, Some(e.clone()));
                state.log_from(conn, format!("{e}, retrying in 5s"));
                if !wait_alive(state, conn, generation, RETRY).await {
                    return;
                }
                continue;
            }
        };
        for w in &conf.warnings {
            state.log_from(conn, w);
        }
        if conf.keepalive.is_none() {
            state.log_from(conn, "沒有 PersistentKeepalive，NAT 後方的連線可能在閒置後斷掉");
        }

        let busy = busy_rows(&plan.locals).await;
        if !state.wg_generation_alive(conn, generation) {
            return;
        }
        for local in &busy {
            state.set_exit_status_of(
                *local,
                generation,
                status::PORT_BUSY,
                Some(format!("Local port {local} is already in use by another process.")),
            );
            state.log_from(conn, format!("local port {local} busy, retrying in 5s"));
        }
        // 還沒有任何握手：狀態就是 `status_for_handshake(None)`（§5.2 的門檻表）
        spread(state, generation, &plan.locals, &busy, status_for_handshake(None), None);

        let cancel = CancellationToken::new();
        let mtu = effective_mtu(plan.mtu, conf.mtu);
        // 只有真的覆寫了才留一行：這正是「我設了 1400，它到底有沒有生效」那個
        // 問題的答案，而 MTU 黑洞的症狀（網頁載一半）本來就很難自己看出來
        if plan.mtu.is_some() {
            state.log_from(conn, format!("MTU overridden to {mtu}"));
        }
        let spec = engine::EngineSpec {
            name: conn.to_string(),
            conf,
            mtu,
            rows: plan.rows.iter().filter(|(_, r)| !busy.contains(&r.local())).cloned().collect(),
        };
        let mut events = match engine::spawn(spec, cancel.clone()).await {
            Ok(rx) => rx,
            Err(e) => {
                spread(state, generation, &plan.locals, &busy, status::ERROR, Some(e.clone()));
                state.log_from(conn, format!("{e}, retrying in 5s"));
                if !wait_alive(state, conn, generation, RETRY).await {
                    return;
                }
                continue;
            }
        };
        // spawn 與 store 之間有一段窄窗口，halt 可能剛好插進來換掉世代；
        // 世代不符時 worker 就在 store_worker 裡 drop，剛起來的任務樹當場收乾淨
        state.wg_store_worker(conn, generation, Worker::Wg(CancelGuard(cancel)));

        let stopped_by_engine =
            consume(state, conn, generation, &plan, &busy, &mut events, RECONNECT_REBUILD_AFTER)
                .await;
        state.wg_kill_worker_of(conn, generation);
        if !stopped_by_engine {
            return; // 世代已經被作廢，狀態由 halt 那一側負責
        }
        if !wait_alive(state, conn, generation, RETRY).await {
            return;
        }
    }
}

/// 消費引擎事件直到它結束或世代作廢。回 true 代表「引擎自己停了，該重連」。
///
/// `rebuild_after` 就是 [`RECONNECT_REBUILD_AFTER`]，做成參數只為了測試檯注得進
/// 一個短值（正式路徑上唯一的呼叫端傳的就是那個常數）。
async fn consume(
    state: &Arc<AppState>,
    conn: &str,
    generation: u64,
    plan: &Plan,
    busy: &[u16],
    events: &mut tokio::sync::mpsc::Receiver<engine::EngineEvent>,
    rebuild_after: Duration,
) -> bool {
    let mut watch = HandshakeWatch::new(Instant::now(), rebuild_after);
    loop {
        // 卡在 reconnecting 太久：端點的位址可能已經漂走了（DDNS），而重解析
        // 只發生在 engine::spawn，所以唯一的自癒手段就是把這一輪收掉重來
        if watch.overdue(Instant::now()) {
            state.log_from(conn, REBUILD_LOG);
            return true;
        }
        let event = tokio::select! {
            // 卡在 recv() 的時候也要定期醒來看一眼自己還算不算數
            _ = tokio::time::sleep(POLL) => {
                if state.wg_generation_alive(conn, generation) {
                    continue;
                }
                return false;
            }
            e = events.recv() => match e {
                Some(e) => e,
                // 通道關了＝引擎那棵任務樹沒了
                None => return true,
            },
        };
        if !state.wg_generation_alive(conn, generation) {
            return false;
        }
        match event {
            engine::EngineEvent::Log(line) => state.log_from(conn, line),
            // 單一列自己的狀態（目前只有綁不到埠會走這裡）
            engine::EngineEvent::Row(local, st, detail) => {
                state.set_exit_status_of(local, generation, st, detail);
            }
            engine::EngineEvent::Engine(st, detail) => {
                spread(state, generation, &plan.locals, busy, st, detail);
                // 握手成功花了多久、什麼時候翻進 reconnecting：以前這兩件事在
                // 日誌裡完全是空白的，使用者只看得到一個變色的點
                if let Some(line) = watch.on_status(st, Instant::now()) {
                    state.log_from(conn, line);
                }
                if st == status::CONNECTED {
                    // 只有 should_probe 為真的列才排自測（§5.2 的狀態機）
                    for local in probed_rows_of(state, conn) {
                        if !busy.contains(&local) {
                            crate::tunnel::probe_exit(state, local);
                        }
                    }
                }
                if st == status::ERROR {
                    // Fatal：收掉引擎，5 秒後整組重起
                    state.log_from(conn, "engine failed, retrying in 5s");
                    return true;
                }
            }
        }
    }
}

/// 這條連線底下要被探測的列（`kind = socks` 或 `probeProxy = true`，§1.3）
fn probed_rows_of(state: &Arc<AppState>, conn: &str) -> Vec<u16> {
    state.with_config(|c| {
        c.wg_proxy(conn)
            .map(|p| {
                p.forwards
                    .iter()
                    .filter(|f| f.enabled && crate::config::should_probe(f.kind, f.probe_proxy))
                    .map(|f| f.local)
                    .collect()
            })
            .unwrap_or_default()
    })
}

#[cfg(test)]
#[path = "wg_tests.rs"]
mod tests;

/// 刪除流程與 `.conf` 驗證／選檔 IPC 的測試——§6 的 W6.17～W6.23 與 W9 系列。
///
/// 與 `wg_tests.rs` 分開掛：那一份是前一棒的紅燈存證，這一輪一個字都沒動。
#[cfg(test)]
#[path = "wg_ipc_tests.rs"]
mod ipc_tests;

#[cfg(test)]
#[path = "wg_live_tests.rs"]
mod live_tests;
