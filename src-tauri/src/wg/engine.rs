//! 把 device + stack + 監聽器組裝成一個可啟停的引擎實例（設計書 §1.7）。
//!
//! 這一層只認**機制**：`Forward` 綁 `serve_forward`、`Socks` 綁 `serve_socks5`。
//! `probeProxy` 不在這裡——它只決定 supervise 要不要排自測，封包怎麼走完全一樣。
//!
//! # 組裝順序（覆審打回 2026-08-24 後重做）
//!
//! [`spawn`] **立刻回傳事件通道**，真正的組裝在一個任務裡按這個順序跑：
//!
//! 1. 先綁全部列的監聽器並推 `Row(connecting)`——這一步必須排在任何引擎層
//!    狀態事件之前。反過來的話，`Engine(connected)` 會先進佇列、被 supervise
//!    攤成「每一條列 connected」，緊接著晚到的 `Row(connecting)` 又把它們壓回
//!    connecting，而 device 只在**變化時**推事件，於是快樂路徑上每一條列
//!    永遠卡在 connecting（W4.21 釘住）。
//! 2. MTU 探測。**同一個迴圈**自始至終獨佔 `device_events`，等握手、等回音的
//!    期間照樣即時把狀態事件往上送，不另開一份代班轉發。
//! 3. 依探測結果起 stack（smoltcp 的 MTU 是建構參數，起好之後改不了，所以
//!    探測只能排在它前面）。
//! 4. 轉入常駐的事件轉發。
//!
//! 第 1 步與第 3 步之間有一段窗口（最長 `HANDSHAKE_WAIT + PROBE_TIMEOUT`），
//! 那時本地埠已經在聽、stack 還沒起來：連進來的客戶端會把 `StackCmd` 排進
//! 通道裡等，stack 一起來就照順序接上。埠是開的，只是慢一點回答。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::state::status;

use super::{conf, device, mtu as pmtu, socks5, stack};

/// 引擎事件通道的深度。一條連線的列數是個位數，這裡只要不擋住組裝就夠
const EVENT_CHANNEL_DEPTH: usize = 64;

/// 探測窗裡先到的入站封包最多留幾顆。這一段最長不過幾秒，而且 stack 還沒起來
/// 就沒有任何連線在等資料——留一小截防的是「剛好卡在窗口邊緣的那幾顆」，
/// 不是拿來當緩衝區用的
const PREFILL_LIMIT: usize = 32;

/// 一條列要引擎替它做什麼。
///
/// **只有兩個 variant**——引擎這一層只認**機制**，而機制就是 §1.2 的那兩種。
/// `probeProxy` 不在這裡：它只決定 supervise 要不要排自測（§5.4），引擎不必
/// 知道，§1.3 的 ③ 與 ④ 走的是同一段程式碼。這正是新編碼的好處——不需要為了
/// 「其中一條會被探測」而在資料流上分岔。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowSpec {
    /// ③④ 轉發：本地埠 → 隧道內的固定目的地
    Forward { local: u16, remote: String },
    /// ⑤ 引擎自建的 SOCKS5 listener
    Socks { local: u16 },
}

impl RowSpec {
    /// 這一條列佔的本地埠。兩個 variant 都有一個，取法不該讓呼叫端 match
    pub fn local(&self) -> u16 {
        match self {
            RowSpec::Forward { local, .. } | RowSpec::Socks { local } => *local,
        }
    }
}

pub struct EngineSpec {
    /// 連線名，同時是引擎的身分與日誌前綴
    pub name: String,
    pub conf: conf::WgConf,
    /// 已經解析完的對端位址。
    ///
    /// **解析在 supervise 那一層做**（`wg::supervise`），不在這裡：DDNS 自癒
    /// 要「先解析、比對過再決定要不要重建」，那個決策點需要看得到上一輪用的是
    /// 哪一個位址。引擎只管照著連。
    pub endpoint: SocketAddr,
    /// 這一輪的 MTU 決策：已經定案的值，或「連上之後探一次」。
    ///
    /// 刻意不讓引擎自己去讀 `conf.mtu`：生效優先序（介面覆寫 ＞ conf 明寫 ＞
    /// 自動探測 ＞ 應用層預設）是設定層的事，由 `wg::plan_mtu` 算完再傳進來。
    pub mtu: pmtu::Plan,
    /// 0..N 條列。零條時 supervise 根本不會呼叫 [`spawn`]（§5.2）
    pub rows: Vec<(String, RowSpec)>,
}

/// 引擎自己的健康狀態。
///
/// **是個 enum 而不是 UI 字串**：supervise 的握手觀測（`wg::HandshakeWatch`）
/// 要對它做完備的 match，拿 `&'static str` 比對的話，新增一個狀態時編譯器
/// 一個字都不會說。翻成 exit-status 字彙是 [`EngineHealth::status`] 的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineHealth {
    /// 握手有效
    Connected,
    /// 握手陳舊或還沒握上
    Reconnecting,
    /// 不可恢復，supervise 會收掉這一輪重來
    Failed,
}

impl EngineHealth {
    /// 對應的 exit-status 字彙（§5.3 的零新事件：一個新字都沒有）
    pub fn status(self) -> &'static str {
        match self {
            EngineHealth::Connected => status::CONNECTED,
            EngineHealth::Reconnecting => status::RECONNECTING,
            EngineHealth::Failed => status::ERROR,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// 引擎自己的狀態（握手）。**不直接對外推事件**——supervise 收到後翻譯成
    /// 「底下每一條列的 exit-status」（§5.3 的零新事件）
    Engine(EngineHealth, Option<String>),
    /// 某一條列的狀態，餵給 `set_exit_status_of(local, ..)`
    Row(u16, &'static str, Option<String>),
    /// 這一輪 MTU 探測的結果。supervise 記日誌（等級由結果決定）並把它記在
    /// 本連線的執行期快取裡，重建時不再白探一次
    Mtu(pmtu::Probe),
    Log(String),
}

/// 起一顆引擎：綁 UDP、把組裝丟進背景任務、**立刻**回傳事件通道。
///
/// 只有「UDP 綁不到」會回 `Err`（埠被別人佔住是常見情況，該當場讓 supervise
/// 看到）。列綁不上不算引擎失敗——那一條進 `port_busy`，其他列照常跑（§5.2）。
pub async fn spawn(
    spec: EngineSpec,
    cancel: CancellationToken,
) -> Result<mpsc::Receiver<EngineEvent>, String> {
    let (events, rx) = mpsc::channel(EVENT_CHANNEL_DEPTH);
    let EngineSpec { name, conf, endpoint, mtu, rows } = spec;

    let device_handle = device::spawn(
        device::DeviceConfig {
            private_key: conf.private_key.0.clone(),
            peer_public_key: boringtun::x25519::PublicKey::from(conf.peer_public_key),
            preshared_key: conf.preshared_key,
            keepalive: conf.keepalive,
            endpoint,
            bind: bind_addr(&endpoint, conf.listen_port),
            stale_after: device::REJECT_AFTER,
            first_handshake_grace: device::FIRST_HANDSHAKE_GRACE,
        },
        cancel.clone(),
    )
    .map_err(|e| format!("[{name}] 綁不到 UDP 埠：{e}"))?;

    tokio::spawn(run(name, conf, mtu, rows, device_handle, events, cancel));
    Ok(rx)
}

/// 組裝與常駐轉發。順序見模組說明
async fn run(
    name: String,
    conf: conf::WgConf,
    plan: pmtu::Plan,
    rows: Vec<(String, RowSpec)>,
    device_handle: device::DeviceHandle,
    events: mpsc::Sender<EngineEvent>,
    cancel: CancellationToken,
) {
    let device::DeviceHandle { outbound, mut inbound, events: mut device_events, .. } =
        device_handle;

    // ① 先綁列。stack 還沒起來，所以指令通道要先自己建好
    let (cmd_tx, cmd_rx) = stack::command_channel();
    bind_rows(&name, rows, &cmd_tx, &events, &cancel).await;

    // ② 探測（Plan::Fixed 時整段跳過，一顆封包都不送）
    let (mtu, prefill) = match plan {
        pmtu::Plan::Fixed(mtu) => (mtu, Vec::new()),
        pmtu::Plan::Probe => {
            let io = ProbeIo {
                outbound: &outbound,
                inbound: &mut inbound,
                device_events: &mut device_events,
                events: &events,
            };
            let done =
                probe_phase(&conf, io, &cancel, pmtu::HANDSHAKE_WAIT, pmtu::PROBE_TIMEOUT).await;
            if done.outcome.is_warning() {
                log::warn!("[{name}] {}", done.outcome.log());
            } else {
                log::info!("[{name}] {}", done.outcome.log());
            }
            let mtu = done.outcome.mtu();
            if events.send(EngineEvent::Mtu(done.outcome)).await.is_err() {
                return;
            }
            (mtu, done.buffered)
        }
    };

    // ③ 起 stack，把探測窗裡先到的封包一起交給它
    stack::spawn_prewired(
        stack::StackConfig {
            // D2：位址一律以 /32、/128 掛，路由靠 default route
            addresses: conf
                .addresses
                .iter()
                .map(|n| {
                    let addr = smoltcp::wire::IpAddress::from(n.addr);
                    smoltcp::wire::IpCidr::new(addr, conf::IpNet::host_prefix(&n.addr))
                })
                .collect(),
            dns_servers: conf.dns.iter().map(|ip| smoltcp::wire::IpAddress::from(*ip)).collect(),
            mtu,
            allowed_ips: conf.allowed_ips.clone(),
            max_connections: stack::DEFAULT_MAX_CONNECTIONS,
            dns_timeout: super::dns::DEFAULT_TIMEOUT,
        },
        outbound,
        inbound,
        cmd_rx,
        prefill,
        cancel.clone(),
    );

    // ④ 常駐：device 的狀態訊號翻成引擎層的事件，supervise 再攤到各列
    loop {
        let event = tokio::select! {
            _ = cancel.cancelled() => break,
            e = device_events.recv() => match e { Some(e) => e, None => break },
        };
        if events.send(translate(event)).await.is_err() {
            break;
        }
    }
}

/// 逐條列綁監聽器並推 `Row(connecting)`。
///
/// 單一列綁不上（埠被佔）只讓那一條進 `port_busy`，其他列照常跑——這是與 ssh
/// 刻意不同的那一點：一條隧道底下有多條列，一條被佔沒有理由拖垮其他（§5.2）。
async fn bind_rows(
    name: &str,
    rows: Vec<(String, RowSpec)>,
    cmd: &mpsc::Sender<stack::StackCmd>,
    events: &mpsc::Sender<EngineEvent>,
    cancel: &CancellationToken,
) {
    for (row_name, row) in rows {
        let local = row.local();
        let listener = match tokio::net::TcpListener::bind((socks5::BIND_ADDR, local)).await {
            Ok(listener) => listener,
            Err(e) => {
                let _ = events.send(EngineEvent::Row(local, status::PORT_BUSY, None)).await;
                let _ = events
                    .send(EngineEvent::Log(format!("[{name}] {row_name} 綁不到埠 {local}：{e}")))
                    .await;
                continue;
            }
        };
        let cmd = cmd.clone();
        let row_cancel = cancel.clone();
        match row {
            RowSpec::Forward { remote, .. } => {
                tokio::spawn(async move {
                    socks5::serve_forward(listener, cmd, remote, row_cancel).await
                });
            }
            RowSpec::Socks { .. } => {
                tokio::spawn(async move { socks5::serve_socks5(listener, cmd, row_cancel).await });
            }
        }
        let _ = events.send(EngineEvent::Row(local, status::CONNECTING, None)).await;
    }
}

/// device 的狀態訊號 → 引擎層的事件
fn translate(event: device::DeviceEvent) -> EngineEvent {
    match event {
        device::DeviceEvent::HandshakeOk => EngineEvent::Engine(EngineHealth::Connected, None),
        device::DeviceEvent::HandshakeStale => {
            EngineEvent::Engine(EngineHealth::Reconnecting, None)
        }
        device::DeviceEvent::Fatal(msg) => EngineEvent::Engine(EngineHealth::Failed, Some(msg)),
    }
}

/// 探測那一段要用到的四條通道，打包起來只是為了不讓函式簽名長成一堵牆
pub(crate) struct ProbeIo<'a> {
    pub outbound: &'a mpsc::Sender<Vec<u8>>,
    pub inbound: &'a mut mpsc::Receiver<Vec<u8>>,
    pub device_events: &'a mut mpsc::Receiver<device::DeviceEvent>,
    pub events: &'a mpsc::Sender<EngineEvent>,
}

/// 探測階段的產物：結論，以及這段期間先到、還沒有人收的入站封包
pub(crate) struct Probed {
    pub outcome: pmtu::Probe,
    pub buffered: Vec<Vec<u8>>,
}

/// 等握手 → 送一顆填滿的 ICMP echo → 等回音。**這段期間 device 的狀態事件
/// 照常即時往上送**（同一個 select，不另開代班任務）。
///
/// 兩段等待都用 `sleep_until` 當 select 的其中一支，而不是把整段包進
/// `tokio::time::timeout`：包進去的話，逾時那一刻正在 `events.send(...)`
/// 的那一顆事件會連同整個 future 一起被丟掉——被吞掉的如果是 `HandshakeOk`，
/// 畫面就再也不會翻成 connected。送出事件的那幾行一律落在 select 的**完成
/// 分支裡**（select 選定之後才執行，不再被取消）。
///
/// 逾時的兩種語意刻意不同：等不到握手是 [`pmtu::Probe::Skipped`]（一顆探測
/// 封包都還沒送出去，什麼都沒量到），送出去了才沒回音才是
/// [`pmtu::Probe::Failed`]。
pub(crate) async fn probe_phase(
    conf: &conf::WgConf,
    io: ProbeIo<'_>,
    cancel: &CancellationToken,
    handshake_wait: Duration,
    probe_timeout: Duration,
) -> Probed {
    let ProbeIo { outbound, inbound, device_events, events } = io;
    let mut buffered: Vec<Vec<u8>> = Vec::new();

    let (src, dst) = match pmtu::target(&conf.addresses, &conf.dns, &conf.allowed_ips) {
        Ok(pair) => pair,
        Err(why) => return Probed { outcome: pmtu::Probe::Skipped(why), buffered },
    };

    // ---- 第一段：等握手
    let deadline = tokio::time::Instant::now() + handshake_wait;
    let mut handshook = false;
    while !handshook {
        tokio::select! {
            _ = cancel.cancelled() => {
                return Probed { outcome: pmtu::Probe::Skipped(pmtu::NO_HANDSHAKE), buffered };
            }
            _ = tokio::time::sleep_until(deadline) => break,
            packet = inbound.recv() => match packet {
                Some(packet) => stash(&mut buffered, packet),
                None => break,
            },
            event = device_events.recv() => {
                let Some(event) = event else { break };
                // 事件照樣往上送（不管它是哪一種），送完再決定這一段還等不等
                let next = Waiting::of(&event);
                // send 在 select 的完成分支裡，不在任何可取消的 timeout scope 內
                if events.send(translate(event)).await.is_err() {
                    return Probed {
                        outcome: pmtu::Probe::Skipped(pmtu::NO_HANDSHAKE),
                        buffered,
                    };
                }
                match next {
                    Waiting::Handshook => handshook = true,
                    Waiting::GiveUp => {
                        return Probed {
                            outcome: pmtu::Probe::Skipped(pmtu::NO_HANDSHAKE),
                            buffered,
                        };
                    }
                    Waiting::NotYet => {}
                }
            }
        }
    }
    if !handshook {
        return Probed { outcome: pmtu::Probe::Skipped(pmtu::NO_HANDSHAKE), buffered };
    }

    // ---- 第二段：送出探測封包，等它的回音
    if outbound.send(pmtu::echo_request(src, dst, pmtu::HIGH_MTU)).await.is_err() {
        return Probed { outcome: pmtu::Probe::Failed, buffered };
    }
    let deadline = tokio::time::Instant::now() + probe_timeout;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Probed { outcome: pmtu::Probe::Failed, buffered },
            _ = tokio::time::sleep_until(deadline) => {
                return Probed { outcome: pmtu::Probe::Failed, buffered };
            }
            packet = inbound.recv() => match packet {
                Some(packet) if pmtu::is_echo_reply(&packet, src, dst) => {
                    return Probed { outcome: pmtu::Probe::Ok, buffered };
                }
                Some(packet) => stash(&mut buffered, packet),
                None => return Probed { outcome: pmtu::Probe::Failed, buffered },
            },
            event = device_events.recv() => {
                let Some(event) = event else {
                    return Probed { outcome: pmtu::Probe::Failed, buffered };
                };
                if events.send(translate(event)).await.is_err() {
                    return Probed { outcome: pmtu::Probe::Failed, buffered };
                }
            }
        }
    }
}

/// 等握手的那一段收到一顆 device 事件之後，這一段還等不等。
///
/// 抽成一個 enum 是為了讓分類**只有一次 match**（而不是「一個 bool 記握上了、
/// 一個 bool 記致命錯誤」那種湊法：兩個布林湊得出四種組合，其中兩種無意義）
enum Waiting {
    /// 握上了，可以送探測封包了
    Handshook,
    /// device 掛了，這一輪不必再等
    GiveUp,
    /// 還沒有結論（例如中途翻了一次 stale），繼續等
    NotYet,
}

impl Waiting {
    fn of(event: &device::DeviceEvent) -> Self {
        match event {
            device::DeviceEvent::HandshakeOk => Waiting::Handshook,
            device::DeviceEvent::Fatal(_) => Waiting::GiveUp,
            device::DeviceEvent::HandshakeStale => Waiting::NotYet,
        }
    }
}

/// 探測窗裡先到的封包先留著，等 stack 起來再一起交給它。滿了就丟——
/// 這是 IP 層，而且這一段最長不過幾秒
fn stash(buffered: &mut Vec<Vec<u8>>, packet: Vec<u8>) {
    if buffered.len() < PREFILL_LIMIT {
        buffered.push(packet);
    }
}

/// UDP 要綁的本地位址：跟著端點的 IP 版本走，`ListenPort` 省略時交給 OS 配
fn bind_addr(endpoint: &SocketAddr, listen_port: u16) -> SocketAddr {
    match endpoint {
        SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), listen_port),
        SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), listen_port),
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
