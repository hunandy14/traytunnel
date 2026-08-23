//! smoltcp 介面與 poll 迴圈（設計書 §1.4）。目前只有骨架。

use std::time::Duration;

use smoltcp::wire::IpEndpoint;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{conf, dns};

/// 每顆引擎的連線上限預設值
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;

/// 每條連線 rx/tx 各配的 TCP 緩衝（onetun 是 64 KiB，這裡減半）
pub const SOCKET_BUFFER: usize = 32 * 1024;

/// 虛擬埠的配置範圍
pub const VPORT_RANGE: std::ops::RangeInclusive<u16> = 1024..=60999;

/// 每條連線上下行通道的深度（反壓的來源）
pub const CONN_CHANNEL_DEPTH: usize = 4;

/// 一條虛擬連線的兩端通道
pub struct Conn {
    pub port: VirtualPort,
    /// 上行：呼叫端 → 隧道
    pub tx: mpsc::Sender<bytes::Bytes>,
    /// 下行：隧道 → 呼叫端
    pub rx: mpsc::Receiver<bytes::Bytes>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// 對端 RST／連不上
    Refused,
    /// 逾時
    Timeout,
    /// 目的地不在 AllowedIPs 內
    NotAllowed,
    /// 目的地的 IP 版本本隧道沒有位址
    NoRoute,
    /// 虛擬埠或連線數用罄
    Exhausted,
}

pub enum StackCmd {
    Connect {
        dst: IpEndpoint,
        reply: oneshot::Sender<Result<Conn, ConnectError>>,
    },
    /// 隧道內 DNS 查詢，socks5h 語意的落點
    Resolve {
        name: String,
        reply: oneshot::Sender<Result<Vec<std::net::IpAddr>, dns::ResolveError>>,
    },
    /// 測試用（並為未來的反向轉發預留）：在隧道內位址上開一個被動監聽
    Listen {
        endpoint: IpEndpoint,
        accept: mpsc::Sender<Conn>,
    },
    Close {
        port: VirtualPort,
    },
}

pub struct StackConfig {
    pub addresses: Vec<smoltcp::wire::IpCidr>,
    pub dns_servers: Vec<smoltcp::wire::IpAddress>,
    pub mtu: usize,
    pub allowed_ips: Vec<conf::IpNet>,
    pub max_connections: usize,
    /// DNS 查詢逾時。
    ///
    /// 設計書 §1.5 寫死 5 秒，但 §5 W5.5 要求可注入以縮短測試，因此做成參數。
    pub dns_timeout: Duration,
}

pub fn spawn(
    _cfg: StackConfig,
    _outbound: mpsc::Sender<Vec<u8>>,
    _inbound: mpsc::Receiver<Vec<u8>>,
    _cancel: CancellationToken,
) -> StackHandle {
    todo!("W4.*：smoltcp 介面與 poll 迴圈")
}

pub struct StackHandle {
    pub cmd: mpsc::Sender<StackCmd>,
    pub join: tokio::task::JoinHandle<()>,
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub struct VirtualPort(pub(crate) u16);

/// W4.2 專用的回歸開關：把 D2 的 default route 關掉。
///
/// `Medium::Ip` 下 `has_neighbor()` 等同「`route()` 有回值」，沒有 default route
/// 的話 TCP 的 SYN 會**靜默地**送不出去。那一行是整份設計最容易被順手「簡化」
/// 掉又不會馬上壞的一行，因此把它做成測試可切換的旗標來釘住。
#[cfg(test)]
pub(crate) static SKIP_DEFAULT_ROUTE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
