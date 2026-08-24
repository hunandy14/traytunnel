//! boringtun 封包泵（設計書 §1.3）。
//!
//! `Tunn` 只被本任務碰，因此完全不需要 Mutex——onetun 用 `Mutex<Box<Tunn>>`，
//! 每個封包都要搶一次鎖；這裡改成 device 任務獨佔，零鎖。
//!
//! 計時器同樣不照 onetun 的 `loop { update_timers(); sleep(1ms) }`（每秒約千次
//! 空轉），改成一條 250ms 的 `tokio::time::interval`。

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use boringtun::noise::{Tunn, TunnResult};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// WireGuard 的 `REJECT_AFTER_TIME`：握手超過這個歲數就不能再算 connected
pub const REJECT_AFTER: Duration = Duration::from_secs(180);

/// 首次握手的寬限期：device 起來之後**一次都還沒握上**時的耐心值。
///
/// 這裡刻意不沿用 [`REJECT_AFTER`]。那 180 秒是 WireGuard 對**既有 session**
/// 的規定——握過一次之後，那把金鑰超過 180 秒就不准再用；「從頭到尾沒握上」
/// 是完全不同的一件事：對端沒開機、端點的 IP 漂走了、UDP 被中間某一跳擋掉。
/// 拿 180 秒當耐心值的話，使用者要盯著三分鐘的 connecting 才看得到
/// reconnecting，而底下靠 reconnecting 觸發的自癒（重建引擎、重解析端點）
/// 也一起被押到三分鐘之後。
///
/// 15 秒的來源是 boringtun 的 `REKEY_TIMEOUT`（5 秒）：足夠讓它重送兩次以上
/// 握手，線路只是慢或掉了一兩顆封包不會被誤判成失敗。
pub const FIRST_HANDSHAKE_GRACE: Duration = Duration::from_secs(15);

/// 計時器 tick 間隔（onetun 是 1ms 空轉，這裡固定 250ms）
pub const TIMER_TICK: Duration = Duration::from_millis(250);

/// 收發緩衝大小。`encapsulate` 要求 dst ≥ src+32 且 ≥148，一塊 64 KiB 綽綽有餘，
/// 而且**重複使用**——每包一次配置是 onetun 的另一個熱點
const MAX_PACKET: usize = 65536;

/// 外送／入站 IP 封包通道的深度。滿了就丟包（這是 IP 層，重送是 TCP 的事），
/// 比阻塞整條泵好
const PACKET_CHANNEL_DEPTH: usize = 256;

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
    /// 握手陳舊門檻。**只管已經握上過的那條 session**（`Some(age)` 那一支）。
    ///
    /// 設計書 §1.3 寫死 `REJECT_AFTER`，但 §5 W4.12 要求可注入，否則那條測試
    /// 就是一個 200 秒的測試。預設值仍是 [`REJECT_AFTER`]。
    pub stale_after: Duration,
    /// 一次都還沒握上時的耐心值（`None` 那一支），預設 [`FIRST_HANDSHAKE_GRACE`]。
    ///
    /// 與 `stale_after` 分成兩個欄位而不是共用一個，理由見
    /// [`FIRST_HANDSHAKE_GRACE`]：兩者量的是不同的事情，值也差一個數量級。
    pub first_handshake_grace: Duration,
}

/// 起 device 任務。任務在 `cancel` 被取消或所有 sender 掉光時結束。
pub fn spawn(cfg: DeviceConfig, cancel: CancellationToken) -> std::io::Result<DeviceHandle> {
    // 先用 std 綁再交給 tokio：綁定失敗要當場回 `Err`（埠被佔是常見情況，
    // 不該變成任務內部一個只有日誌看得到的事件），而且 `local_addr` 要在
    // 回傳 handle 之前就知道（`ListenPort = 0` 時測試檯靠它互連）。
    let socket = std::net::UdpSocket::bind(cfg.bind)?;
    let local_addr = socket.local_addr()?;

    let mut tunn =
        Tunn::new(cfg.private_key, cfg.peer_public_key, cfg.preshared_key, cfg.keepalive, 0, None);

    // 啟動當下**就地**送一次握手，不等任務被排到，也不等第一個封包：
    //  * 使用者按下連線後幾百毫秒內就成立 `connected`，而不是等他開網頁；
    //  * 「這一段之間有沒有封包出去」那類斷言（W4.8）才有一個確定的起點——
    //    握手排在任務裡送的話，它會落在測試讀計數器之前或之後全憑排程。
    //
    // **這一送必須走 std 的 socket，而且要在它被切成 non-blocking、交給 tokio
    // 之前**。兩層理由，少哪一層都會退化成同一個症狀：
    //  * `try_send_to` 只在 tokio 已經從 I/O driver 收過一次「可寫」事件之後才會
    //    真的下系統呼叫，剛註冊的 socket 那份快取是空的，於是它一律回
    //    `WouldBlock`；
    //  * 就算不經 tokio，non-blocking 的 `send_to` 在送出佇列滿的時候一樣會回
    //    `WouldBlock`，而下面這一手只會記一行 debug 就算了。
    // 兩種情況下封包都被靜靜丟掉，握手要拖到 boringtun 的 REKEY_TIMEOUT（5 秒）
    // 重試才成立——隧道看起來「就是慢五秒」，而且沒有任何錯誤訊息。
    // 阻塞式的 socket 沒有這個問題，而這一送是行程剛開始、佇列必定是空的那一刻。
    let mut tx_buf = vec![0u8; MAX_PACKET];
    if let TunnResult::WriteToNetwork(packet) = tunn.format_handshake_initiation(&mut tx_buf, false)
    {
        match socket.send_to(packet, cfg.endpoint) {
            Ok(_) => note_udp_tx(),
            Err(e) => log::debug!("wg device: initial handshake not sent: {e}"),
        }
    }

    // 交給 tokio 之前才切 non-blocking——`from_std` 要求它是 non-blocking 的
    socket.set_nonblocking(true)?;
    let udp = tokio::net::UdpSocket::from_std(socket)?;

    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(PACKET_CHANNEL_DEPTH);
    let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>(PACKET_CHANNEL_DEPTH);
    let (ev_tx, ev_rx) = mpsc::channel::<DeviceEvent>(16);

    let join = tokio::spawn(pump(
        PumpState {
            tunn,
            tx_buf,
            endpoint: cfg.endpoint,
            stale_after: cfg.stale_after,
            first_handshake_grace: cfg.first_handshake_grace,
        },
        udp,
        out_rx,
        in_tx,
        ev_tx,
        cancel,
    ));

    Ok(DeviceHandle { outbound: out_tx, inbound: in_rx, events: ev_rx, join, local_addr })
}

pub struct DeviceHandle {
    /// stack → device：待加密的 IP 封包
    pub outbound: mpsc::Sender<Vec<u8>>,
    /// device → stack：已解密的 IP 封包
    pub inbound: mpsc::Receiver<Vec<u8>>,
    pub events: mpsc::Receiver<DeviceEvent>,
    /// 泵任務本身。正式路徑不讀它（收尾一律靠 `CancellationToken`），
    /// 留著是為了讓測試檯等得到「任務真的結束了」（W4.10）
    #[allow(dead_code)]
    pub join: tokio::task::JoinHandle<()>,
    /// 綁定完成後 UDP socket 的實際本地位址（`listen_port = 0` 時測試檯要靠它互連）
    #[allow(dead_code)]
    pub local_addr: SocketAddr,
}

/// 目前對外報出去的握手狀態，用來只在**變化時**推事件
#[derive(PartialEq, Eq, Clone, Copy)]
enum Reported {
    Nothing,
    Ok,
    Stale,
}

/// `spawn` 已經替我們建好、也已經送出第一次握手的那一份狀態
struct PumpState {
    tunn: Tunn,
    tx_buf: Vec<u8>,
    endpoint: SocketAddr,
    stale_after: Duration,
    first_handshake_grace: Duration,
}

async fn pump(
    state: PumpState,
    udp: tokio::net::UdpSocket,
    mut outbound: mpsc::Receiver<Vec<u8>>,
    inbound: mpsc::Sender<Vec<u8>>,
    events: mpsc::Sender<DeviceEvent>,
    cancel: CancellationToken,
) {
    let PumpState { mut tunn, mut tx_buf, endpoint, stale_after, first_handshake_grace } = state;
    let mut rx_buf = vec![0u8; MAX_PACKET];
    // decapsulate 的「空轉續抽」要在前一顆封包還借著 tx_buf 的時候再寫一顆出去，
    // 因此需要第二塊。onetun 是在迴圈裡每一輪配置一塊新的，這裡改成重複使用。
    let mut aux_buf = vec![0u8; MAX_PACKET];

    let started = Instant::now();
    let mut reported = Reported::Nothing;
    let mut timer = tokio::time::interval(TIMER_TICK);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,

            packet = outbound.recv() => {
                let Some(packet) = packet else { break };
                match tunn.encapsulate(&packet, &mut tx_buf) {
                    TunnResult::WriteToNetwork(out) => send_udp(&udp, out, endpoint).await,
                    // 握手還沒完成時 boringtun 會把封包排進自己的 queue，這是常態
                    TunnResult::Done => {}
                    TunnResult::Err(e) => log::warn!("wg device: encapsulate failed: {e:?}"),
                    _ => {}
                }
            }

            received = udp.recv_from(&mut rx_buf) => {
                let (len, src) = match received {
                    Ok(v) => v,
                    // R7：Windows 的 UDP socket 收到 ICMP port-unreachable 之後，
                    // 下一次 recv_from 會回 WSAECONNRESET。把它當致命錯誤會讓
                    // 隧道在對端還沒起來時直接死掉（W4.9）。
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                    // 其餘的錯誤是這個 socket 真的壞了（介面被拔掉、handle 被關）。
                    // 照舊 `continue` 的話迴圈會用 100% CPU 空轉並且再也收不到東西，
                    // 而且畫面會一直停在 connected——推 Fatal 讓 supervise 收掉這一輪，
                    // 5 秒後整組重起才是對的
                    Err(e) => {
                        log::warn!("wg device: udp recv failed: {e}");
                        let _ = events.send(DeviceEvent::Fatal(e.to_string())).await;
                        break;
                    }
                };
                // 借用檢查：decapsulate 同時要 rx_buf 的唯讀切片與 tx_buf 的可變
                // 切片，兩塊是不同的緩衝，因此不需要複製
                match tunn.decapsulate(Some(src.ip()), &rx_buf[..len], &mut tx_buf) {
                    TunnResult::WriteToNetwork(out) => {
                        send_udp(&udp, out, endpoint).await;
                        // boringtun 的契約：回了 WriteToNetwork 之後要一直空轉
                        // decapsulate 直到不再是 WriteToNetwork（noise/mod.rs:270-275）
                        while let TunnResult::WriteToNetwork(more) =
                            tunn.decapsulate(None, &[], &mut aux_buf)
                        {
                            send_udp(&udp, more, endpoint).await;
                        }
                    }
                    TunnResult::WriteToTunnelV4(packet, _)
                    | TunnResult::WriteToTunnelV6(packet, _) => {
                        // 滿了就丟——這是 IP 層，重送交給上層的 TCP
                        let full = inbound.try_send(packet.to_vec()).is_err();
                        if full {
                            log::debug!("wg device: inbound queue full, dropping packet");
                        }
                    }
                    // 垃圾位元組走到這裡（W4.13）：丟掉就好，不 panic、不影響既有連線
                    _ => {}
                }
            }

            _ = timer.tick() => {
                match tunn.update_timers(&mut tx_buf) {
                    TunnResult::WriteToNetwork(out) => send_udp(&udp, out, endpoint).await,
                    TunnResult::Err(boringtun::noise::errors::WireGuardError::ConnectionExpired) => {
                        // onetun 這裡用 #[async_recursion] 遞迴回自己，我們攤平成
                        // 一次直接送出——遞迴那一層在這個狀態機裡不多買到任何東西
                        if let TunnResult::WriteToNetwork(out) =
                            tunn.format_handshake_initiation(&mut tx_buf, false)
                        {
                            send_udp(&udp, out, endpoint).await;
                        }
                    }
                    TunnResult::Err(e) => log::debug!("wg device: timer: {e:?}"),
                    _ => {}
                }

                let next = classify(
                    tunn.time_since_last_handshake(),
                    started,
                    stale_after,
                    first_handshake_grace,
                );
                if next != reported && next != Reported::Nothing {
                    reported = next;
                    let event = match next {
                        Reported::Ok => DeviceEvent::HandshakeOk,
                        _ => DeviceEvent::HandshakeStale,
                    };
                    if events.send(event).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
}

/// 握手歲數 → 要對外報的狀態。
///
/// 從來沒握上（`None`）時，用「device 起來多久了」對照
/// [`FIRST_HANDSHAKE_GRACE`]：對端從頭到尾沒回話也必須在寬限期之後翻成 stale，
/// 不可以永遠停在「還沒有結論」（W4.12／W4.16／W4.17）。**這一支刻意不看
/// `stale_after`**——那 180 秒量的是既有 session 的歲數，不是等待的耐心。
fn classify(
    age: Option<Duration>,
    started: Instant,
    stale_after: Duration,
    first_handshake_grace: Duration,
) -> Reported {
    match age {
        Some(age) if age < stale_after => Reported::Ok,
        Some(_) => Reported::Stale,
        None if started.elapsed() >= first_handshake_grace => Reported::Stale,
        None => Reported::Nothing,
    }
}

async fn send_udp(udp: &tokio::net::UdpSocket, packet: &[u8], endpoint: SocketAddr) {
    match udp.send_to(packet, endpoint).await {
        Ok(_) => note_udp_tx(),
        // 對端還沒起來時 Windows 會用 WSAECONNRESET 回應上一次的送出，
        // 這同樣不是致命錯誤（R7）
        Err(e) => log::debug!("wg device: udp send failed: {e}"),
    }
}

/// `[Peer] Endpoint` 的主機名解析。
///
/// **整個 `wg/` 底下唯一允許呼叫系統解析器的地方**（設計書 §2.2 的三道洩漏防線
/// 之一，由 W1.31 的 grep 型測試釘住）：端點是隧道外的位址，本來就必須用系統
/// 解析器，而且要每次重連前重解一次，動態 DNS 的端點才跟得上。
pub async fn resolve_endpoint(endpoint: &str) -> Result<SocketAddr, String> {
    note_resolve();
    tokio::net::lookup_host(endpoint)
        .await
        .map_err(|e| format!("解析不到端點 {endpoint}：{e}"))?
        .next()
        .ok_or_else(|| format!("解析不到端點 {endpoint}"))
}

#[cfg(test)]
fn note_udp_tx() {
    UDP_TX_COUNT.bump();
}

#[cfg(not(test))]
#[inline]
fn note_udp_tx() {}

/// 測試觀測點：[`resolve_endpoint`] 被呼叫過幾次。
///
/// 「reconnecting 卡太久就重建引擎，重建會重新解析端點」（DDNS 自癒）這條性質
/// 從外面看不出來——解析的結果多半還是同一個 IP。手法與 [`UDP_TX_COUNT`] 相同，
/// 一樣是 thread-local（理由見那邊的說明）。
#[cfg(test)]
fn note_resolve() {
    RESOLVE_LOCAL.with(|c| c.set(c.get() + 1));
}

#[cfg(not(test))]
#[inline]
fn note_resolve() {}

#[cfg(test)]
thread_local! {
    static RESOLVE_LOCAL: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 這條執行緒上 [`resolve_endpoint`] 至今被呼叫的次數（測試用）
#[cfg(test)]
pub(crate) fn resolve_count() -> usize {
    RESOLVE_LOCAL.with(|c| c.get())
}

/// 測試觀測點：送進 UDP socket 的封包數。
///
/// W4.8（AllowedIPs 擋下時不得有任何封包出去）與 W5.6（沒有 DNS 伺服器時不得
/// 有任何 DNS 封包出去）都要斷言「一個位元組都沒送出去」，只靠黑箱看不出來。
///
/// **計數是 thread-local 的**，不是一個共用的 `AtomicUsize`：`cargo test` 預設
/// 平行跑，而 `#[tokio::test]` 用的是 current-thread runtime——同一條測試的所有
/// 任務都在自己那條執行緒上，換成全域計數的話別條測試的隧道流量會把「這一段
/// 之間沒有任何封包出去」這個斷言打成雜訊。對呼叫端而言介面不變（`.load(順序)`）。
#[cfg(test)]
pub(crate) struct UdpTxCount;

#[cfg(test)]
impl UdpTxCount {
    fn bump(&self) {
        UDP_TX_LOCAL.with(|c| c.set(c.get() + 1));
    }

    pub(crate) fn load(&self, _order: std::sync::atomic::Ordering) -> usize {
        UDP_TX_LOCAL.with(|c| c.get())
    }
}

#[cfg(test)]
thread_local! {
    static UDP_TX_LOCAL: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) static UDP_TX_COUNT: UdpTxCount = UdpTxCount;
