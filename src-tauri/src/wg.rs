//! WireGuard → 本地 SOCKS5（行程內使用者態隧道）的模組根與生命週期。
//!
//! 對外提供與 `ssh::tunnel` 完全對稱的一組動詞，內部維護每顆代理的監看迴圈，
//! **不含**任何協定細節（設計書 §1.1）。
//!
//! 目前整個模組只有骨架：型別與公開簽名到位讓 crate 編得過，內文一律
//! `todo!()`，行為由 W1～W7 的測試定義。

pub mod conf;
pub mod device;
pub mod dns;
pub mod engine;
pub mod socks5;
pub mod stack;

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::state::{status, AppState, Worker};
use crate::winsys::is_listening;

/// 引擎斷線後的重連間隔，與 `ssh::tunnel::RETRY` 同值同理由
pub const RETRY: Duration = Duration::from_secs(5);

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
        },
        cancel.clone(),
    ) {
        Ok(h) => h,
        Err(_) => return R::fail(BIND_FAILED),
    };

    let mut events = handle.events;
    let waited = tokio::time::timeout(TEST_TIMEOUT, async {
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
        Some(Plan { conf_path: crate::config::resolve_conf_path(&dir, &p.conf_path), rows, locals })
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
        let spec = engine::EngineSpec {
            name: conn.to_string(),
            conf,
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

        let stopped_by_engine = consume(state, conn, generation, &plan, &busy, &mut events).await;
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
async fn consume(
    state: &Arc<AppState>,
    conn: &str,
    generation: u64,
    plan: &Plan,
    busy: &[u16],
    events: &mut tokio::sync::mpsc::Receiver<engine::EngineEvent>,
) -> bool {
    loop {
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

#[cfg(test)]
#[path = "wg_live_tests.rs"]
mod live_tests;
