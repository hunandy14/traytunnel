//! boringtun 封包泵（設計書 §1.3）。
//!
//! `Tunn` 只被本任務碰，因此完全不需要 Mutex。目前只有骨架。

use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// WireGuard 的 `REJECT_AFTER_TIME`：握手超過這個歲數就不能再算 connected
pub const REJECT_AFTER: Duration = Duration::from_secs(180);

/// 計時器 tick 間隔（onetun 是 1ms 空轉，這裡固定 250ms）
pub const TIMER_TICK: Duration = Duration::from_millis(250);

/// 引擎對外的狀態訊號，supervise 迴圈靠它決定 exit-status。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    /// 握手完成（第一次，或重新協商成功）
    HandshakeOk,
    /// 超過 `stale_after` 仍無成功握手
    HandshakeStale,
    /// 不可恢復：UDP 綁定失敗、端點解析失敗
    Fatal(String),
}

pub struct DeviceConfig {
    pub private_key: boringtun::x25519::StaticSecret,
    pub peer_public_key: boringtun::x25519::PublicKey,
    pub preshared_key: Option<[u8; 32]>,
    pub keepalive: Option<u16>,
    /// 已解析完的對端位址
    pub endpoint: SocketAddr,
    /// `0.0.0.0:listen_port` 或 `[::]:listen_port`
    pub bind: SocketAddr,
    /// 握手陳舊門檻。
    ///
    /// 設計書 §1.3 寫死 `REJECT_AFTER`，但 §5 W4.12 要求可注入，否則那條測試
    /// 就是一個 200 秒的測試。預設值仍是 [`REJECT_AFTER`]。
    pub stale_after: Duration,
}

/// 起 device 任務。任務在 `cancel` 被取消或所有 sender 掉光時結束。
pub fn spawn(_cfg: DeviceConfig, _cancel: CancellationToken) -> std::io::Result<DeviceHandle> {
    todo!("W4.*：boringtun 封包泵")
}

pub struct DeviceHandle {
    /// stack → device：待加密的 IP 封包
    pub outbound: mpsc::Sender<Vec<u8>>,
    /// device → stack：已解密的 IP 封包
    pub inbound: mpsc::Receiver<Vec<u8>>,
    pub events: mpsc::Receiver<DeviceEvent>,
    pub join: tokio::task::JoinHandle<()>,
    /// 綁定完成後 UDP socket 的實際本地位址（`listen_port = 0` 時測試檯要靠它互連）
    pub local_addr: SocketAddr,
}

/// `[Peer] Endpoint` 的主機名解析。
///
/// **整個 `wg/` 底下唯一允許呼叫系統解析器的地方**（設計書 §2.2 的三道洩漏防線
/// 之一，由 W1.31 的 grep 型測試釘住）：端點是隧道外的位址，本來就必須用系統
/// 解析器，而且要每次重連前重解一次，動態 DNS 的端點才跟得上。
pub async fn resolve_endpoint(_endpoint: &str) -> Result<SocketAddr, String> {
    todo!("以 tokio::net::lookup_host 解析 [Peer] Endpoint")
}

/// 測試觀測點：送進 UDP socket 的封包數。
///
/// W4.8（AllowedIPs 擋下時不得有任何封包出去）與 W5.6（沒有 DNS 伺服器時不得
///有任何 DNS 封包出去）都要斷言「一個位元組都沒送出去」，只靠黑箱看不出來。
#[cfg(test)]
pub(crate) static UDP_TX_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
