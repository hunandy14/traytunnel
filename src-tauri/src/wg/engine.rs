//! 把 device + stack + 監聽器組裝成一個可啟停的引擎實例（設計書 §1.7）。
//!
//! 這一層只認**機制**：`Forward` 綁 `serve_forward`、`Socks` 綁 `serve_socks5`。
//! `probeProxy` 不在這裡——它只決定 supervise 要不要排自測，封包怎麼走完全一樣。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::state::status;

use super::{conf, device, mtu as pmtu, socks5, stack};

/// 引擎事件通道的深度。一條連線的列數是個位數，這裡只要不擋住組裝就夠
const EVENT_CHANNEL_DEPTH: usize = 64;

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
    /// 這顆引擎實際要用的隧道 MTU，**已經定案的那一個值**。
    ///
    /// 刻意不讓引擎自己去讀 `conf.mtu`：生效優先序（介面覆寫 ＞ conf 明寫 ＞
    /// [`conf::APP_DEFAULT_MTU`]）是設定層的事，由 `wg::effective_mtu` 算完再
    /// 傳進來，引擎這一層只負責照著設。
    ///
    /// `probe_mtu` 為真時這個值是**探測失敗的退路**（就是應用層預設 1280）。
    pub mtu: usize,
    /// 要不要自動探測路徑 MTU（由 `wg::should_probe_mtu` 決定：介面與 `.conf`
    /// 都沒指定時才為真，優先序因此不受影響）。詳見 [`super::mtu`]
    pub probe_mtu: bool,
    /// 0..N 條列。零條時 supervise 根本不會呼叫 [`spawn`]（§5.2）
    pub rows: Vec<(String, RowSpec)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// 引擎自己的狀態（握手）。**不直接對外推事件**——supervise 收到後翻譯成
    /// 「底下每一條列的 exit-status」（§5.3 的零新事件）
    Engine(&'static str, Option<String>),
    /// 某一條列的狀態，餵給 `set_exit_status_of(local, ..)`
    Row(u16, &'static str, Option<String>),
    Log(String),
}

/// 依序：解析端點 → 起 device → 起 stack → 逐條列綁監聽器。
///
/// `Forward` 綁 [`super::socks5::serve_forward`]、`Socks` 綁
/// [`super::socks5::serve_socks5`]。**單一列綁不上（埠被佔）只讓那一條進
/// `port_busy`，不讓整顆引擎失敗**；device／stack 起不來才回 `Err`，
/// 且已起來的部分會被 `cancel` 收乾淨。
pub async fn spawn(
    spec: EngineSpec,
    cancel: CancellationToken,
) -> Result<mpsc::Receiver<EngineEvent>, String> {
    let (events, rx) = mpsc::channel(EVENT_CHANNEL_DEPTH);
    let EngineSpec { name, conf, mtu, probe_mtu, rows } = spec;

    // 端點每次重連前重解一次，動態 DNS 的端點才跟得上。這是整個 wg/ 底下
    // 唯一一處會用到系統解析器的地方，而且它解的是隧道**外**的位址。
    let endpoint = device::resolve_endpoint(&conf.endpoint).await?;

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

    let device::DeviceHandle { outbound, mut inbound, events: mut device_events, .. } =
        device_handle;

    // MTU 必須在 stack 建起來之前就定案（smoltcp 的 MTU 是建構參數），
    // 所以自動探測排在這裡：device 已經在跑、stack 還沒有人在用 inbound
    let mtu = if probe_mtu {
        let outcome =
            run_mtu_probe(&conf, &outbound, &mut inbound, &mut device_events, &events, &cancel)
                .await;
        if outcome.is_warning() {
            log::warn!("[{name}] {}", outcome.log());
        } else {
            log::info!("[{name}] {}", outcome.log());
        }
        let _ = events.send(EngineEvent::Log(outcome.log())).await;
        outcome.mtu()
    } else {
        mtu
    };

    let stack = stack::spawn(
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
        cancel.clone(),
    );

    for (row_name, row) in rows {
        let local = row.local();
        let listener = match tokio::net::TcpListener::bind((socks5::BIND_ADDR, local)).await {
            Ok(listener) => listener,
            // 單一列綁不上只讓那一條進 port_busy，其他列照常跑（§5.2 與 ssh
            // 刻意不同的那一點：一條隧道底下有多條列，一條被佔沒有理由拖垮其他）
            Err(e) => {
                let _ = events.send(EngineEvent::Row(local, status::PORT_BUSY, None)).await;
                let _ = events
                    .send(EngineEvent::Log(format!("[{name}] {row_name} 綁不到埠 {local}：{e}")))
                    .await;
                continue;
            }
        };
        let cmd = stack.cmd.clone();
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

    // device 的狀態訊號翻成引擎層的事件；supervise 再把它攤到底下每一條列
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => break,
                e = device_events.recv() => match e { Some(e) => e, None => break },
            };
            if events.send(translate(event)).await.is_err() {
                break;
            }
        }
    });

    Ok(rx)
}

/// device 的狀態訊號 → 引擎層的事件。抽成函式是因為 MTU 探測期間要先代班
/// 轉一手：device 的事件只在**變化時**推一次，探測時吃掉的那一顆
/// `HandshakeOk` 沒補回去的話，畫面就再也不會翻成 connected
fn translate(event: device::DeviceEvent) -> EngineEvent {
    match event {
        device::DeviceEvent::HandshakeOk => EngineEvent::Engine(status::CONNECTED, None),
        device::DeviceEvent::HandshakeStale => EngineEvent::Engine(status::RECONNECTING, None),
        device::DeviceEvent::Fatal(msg) => EngineEvent::Engine(status::ERROR, Some(msg)),
    }
}

/// 自動 MTU 探測的完整一輪：挑目標 → 等握手 → 送一顆 1420 的 ICMP echo。
///
/// 握手還沒完成時封包只會被 boringtun 排進佇列，探了必逾時，所以要先等；
/// 等的期間看到的 device 事件**照樣往上送**（`translate`），畫面不會因為
/// 探測而慢一拍。等不到（逾時、Fatal、被取消）就當探測失敗，走保守的
/// [`pmtu::SAFE_MTU`]——這種時候隧道本來就還沒通，不值得再多等。
async fn run_mtu_probe(
    conf: &conf::WgConf,
    outbound: &mpsc::Sender<Vec<u8>>,
    inbound: &mut mpsc::Receiver<Vec<u8>>,
    device_events: &mut mpsc::Receiver<device::DeviceEvent>,
    events: &mpsc::Sender<EngineEvent>,
    cancel: &CancellationToken,
) -> pmtu::Probe {
    let (src, dst) = match pmtu::target(&conf.addresses, &conf.dns, &conf.allowed_ips) {
        Ok(pair) => pair,
        Err(why) => return pmtu::Probe::Skipped(why),
    };
    if !wait_handshake(device_events, events, cancel, pmtu::HANDSHAKE_WAIT).await {
        return pmtu::Probe::Failed;
    }
    pmtu::probe(outbound, inbound, src, dst, pmtu::PROBE_TIMEOUT).await
}

/// 等第一次握手完成，最多等 `patience`。期間收到的事件一律往上轉。
/// 回 true 代表握上了
async fn wait_handshake(
    device_events: &mut mpsc::Receiver<device::DeviceEvent>,
    events: &mpsc::Sender<EngineEvent>,
    cancel: &CancellationToken,
    patience: std::time::Duration,
) -> bool {
    let waited = tokio::time::timeout(patience, async {
        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => return false,
                e = device_events.recv() => match e { Some(e) => e, None => return false },
            };
            let handshook = event == device::DeviceEvent::HandshakeOk;
            let fatal = matches!(event, device::DeviceEvent::Fatal(_));
            if events.send(translate(event)).await.is_err() {
                return false;
            }
            if handshook {
                return true;
            }
            if fatal {
                return false;
            }
        }
    })
    .await;
    waited == Ok(true)
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
