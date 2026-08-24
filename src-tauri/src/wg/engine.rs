//! 把 device + stack + 監聽器組裝成一個可啟停的引擎實例（設計書 §1.7）。
//!
//! 這一層只認**機制**：`Forward` 綁 `serve_forward`、`Socks` 綁 `serve_socks5`。
//! `probeProxy` 不在這裡——它只決定 supervise 要不要排自測，封包怎麼走完全一樣。
//!
//! # 組裝順序
//!
//! 起 stack → 綁全部列的監聽器並推 `Row(connecting)` → 轉入常駐的事件轉發。
//!
//! **綁列必須排在任何引擎層狀態事件之前**。反過來的話，`Engine(connected)`
//! 會先進佇列、被 supervise 攤成「每一條列 connected」，緊接著晚到的
//! `Row(connecting)` 又把它們壓回 connecting，而 device 只在**變化時**推事件，
//! 於是快樂路徑上每一條列永遠卡在 connecting（W4.20 釘住）。這也是為什麼
//! device 的事件在組裝完成之前**一顆都不轉發**：轉發只發生在最後那個常駐迴圈，
//! 順序因此是結構保證的，不是靠時序運氣。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::state::status;

use super::{conf, device, socks5, stack};

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
    /// 已經解析完的對端位址。
    ///
    /// **解析在 supervise 那一層做**（`wg::supervise`），不在這裡：DDNS 自癒
    /// 要「先解析、比對過再決定要不要重建」，那個決策點需要看得到上一輪用的是
    /// 哪一個位址。引擎只管照著連。
    pub endpoint: SocketAddr,
    /// 這顆引擎實際要用的隧道 MTU，**已經定案的那一個值**。
    ///
    /// 刻意不讓引擎自己去讀 `conf.mtu`：生效優先序（介面覆寫 ＞ conf 明寫 ＞
    /// [`conf::APP_DEFAULT_MTU`]）是設定層的事，由 `wg::effective_mtu` 算完再
    /// 傳進來，引擎這一層只負責照著設。
    pub mtu: usize,
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
    Log(String),
}

/// 依序：起 device → 起 stack → 逐條列綁監聽器 → 常駐轉發。
///
/// `Forward` 綁 [`super::socks5::serve_forward`]、`Socks` 綁
/// [`super::socks5::serve_socks5`]。**單一列綁不上（埠被佔）只讓那一條進
/// `port_busy`，不讓整顆引擎失敗**；device 起不來（UDP 埠被佔）才回 `Err`，
/// 且已起來的部分會被 `cancel` 收乾淨。
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

    let device::DeviceHandle { outbound, inbound, events: mut device_events, .. } = device_handle;

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

    bind_rows(&name, rows, &stack.cmd, &events, &cancel).await;

    // device 的狀態訊號翻成引擎層的事件；supervise 再把它攤到底下每一條列。
    // **轉發只發生在這裡**，所以它必定排在上面那些 Row 事件之後
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => break,
                e = device_events.recv() => match e { Some(e) => e, None => break },
            };
            // send 落在 select 的完成分支裡，不在任何可取消的 scope 內：
            // 被取消掉的如果是一顆 HandshakeOk，畫面就再也不會翻成 connected
            if events.send(translate(event)).await.is_err() {
                break;
            }
        }
    });

    Ok(rx)
}

/// 逐條列綁監聽器並推 `Row(connecting)`。
///
/// 單一列綁不上（埠被佔）只讓那一條進 `port_busy`，其他列照常跑——這是與 ssh
/// 刻意不同的那一點：一條隧道底下有多條列，一條被佔沒有理由拖垮其他（§5.2）。
///
/// **每綁一條之前先看一眼取消權杖**：這一輪如果已經被 halt 掉了（使用者按了
/// 中斷、或設定改了觸發重建），再綁下去就是替一棵已經死掉的任務樹佔住本地埠，
/// 而下一輪起來時會發現自己的埠被「自己」佔著，變成 port_busy（R5）。
async fn bind_rows(
    name: &str,
    rows: Vec<(String, RowSpec)>,
    cmd: &mpsc::Sender<stack::StackCmd>,
    events: &mpsc::Sender<EngineEvent>,
    cancel: &CancellationToken,
) {
    for (row_name, row) in rows {
        if cancel.is_cancelled() {
            return;
        }
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

/// UDP 要綁的本地位址：跟著端點的 IP 版本走，`listen_port` 為 0 時交給 OS 配
pub(crate) fn bind_addr(endpoint: &SocketAddr, listen_port: u16) -> SocketAddr {
    match endpoint {
        SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), listen_port),
        SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), listen_port),
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
