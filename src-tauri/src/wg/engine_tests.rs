//! 雙引擎 loopback 整合測試——設計書 §5 的 W4 系列（16 條，成本 S／M）。
//!
//! 測試檯：同一個行程內起兩顆引擎，走 localhost UDP 互連。
//!
//! ```text
//!    引擎 A（客戶端側，被測物）                 引擎 B（伺服器側，測試檯）
//!    ┌────────────────────────┐               ┌────────────────────────┐
//!    │ Tunn(a_priv, b_pub)    │               │ Tunn(b_priv, a_pub)    │
//!    │ UDP 127.0.0.1:pA  ─────┼──────────────▶│ UDP 127.0.0.1:pB       │
//!    │ 位址 10.9.0.1/32       │◀──────────────┼─ 位址 10.9.0.2/32      │
//!    │ stack + SOCKS5 監聽    │               │ stack + Listen(:7) echo│
//!    └────────────────────────┘               └────────────────────────┘
//! ```
//!
//! 金鑰是固定的測試常數（`[0x01; 32]`／`[0x02; 32]`），不需要 RNG，
//! **也不需要、不允許碰任何真實的 `.conf`**。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::wg::conf::IpNet;
use crate::wg::device::{self, DeviceConfig, DeviceEvent, DeviceHandle};
use crate::wg::stack::{self, StackCmd, StackConfig, StackHandle};

use boringtun::x25519::{PublicKey, StaticSecret};

const A_PRIV: [u8; 32] = [0x01; 32];
const B_PRIV: [u8; 32] = [0x02; 32];
const A_ADDR: Ipv4Addr = Ipv4Addr::new(10, 9, 0, 1);
const B_ADDR: Ipv4Addr = Ipv4Addr::new(10, 9, 0, 2);
/// echo 服務在 B 側監聽的埠（RFC 862 的 echo）
const ECHO_PORT: u16 = 7;
/// 整合測試的耐心上限：超過就 FAIL，不可以卡住整套
const PATIENCE: Duration = Duration::from_secs(10);

async fn deadline<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(PATIENCE, f).await.expect("等待逾時：隧道沒有動起來")
}

/// 借兩個沒人用的 UDP 埠給測試檯的兩端
fn two_free_udp_ports() -> (u16, u16) {
    let a = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let b = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    (a.local_addr().unwrap().port(), b.local_addr().unwrap().port())
}

fn net(addr: Ipv4Addr, prefix: u8) -> IpNet {
    IpNet { addr: IpAddr::V4(addr), prefix }
}

/// 一端的 device + stack
struct Side {
    device: DeviceHandle,
    stack: StackHandle,
}

struct SideSpec {
    private: [u8; 32],
    peer_public: [u8; 32],
    preshared: Option<[u8; 32]>,
    address: Ipv4Addr,
    bind_port: u16,
    peer_port: u16,
    allowed_ips: Vec<IpNet>,
    stale_after: Duration,
}

impl SideSpec {
    fn a(pa: u16, pb: u16) -> Self {
        SideSpec {
            private: A_PRIV,
            peer_public: *PublicKey::from(&StaticSecret::from(B_PRIV)).as_bytes(),
            preshared: None,
            address: A_ADDR,
            bind_port: pa,
            peer_port: pb,
            allowed_ips: vec![net(Ipv4Addr::UNSPECIFIED, 0)],
            stale_after: device::REJECT_AFTER,
        }
    }

    fn b(pa: u16, pb: u16) -> Self {
        SideSpec {
            private: B_PRIV,
            peer_public: *PublicKey::from(&StaticSecret::from(A_PRIV)).as_bytes(),
            preshared: None,
            address: B_ADDR,
            bind_port: pb,
            peer_port: pa,
            allowed_ips: vec![net(Ipv4Addr::UNSPECIFIED, 0)],
            stale_after: device::REJECT_AFTER,
        }
    }
}

fn spin_up(spec: &SideSpec, cancel: &tokio_util::sync::CancellationToken) -> Side {
    let device = device::spawn(
        DeviceConfig {
            private_key: StaticSecret::from(spec.private),
            peer_public_key: PublicKey::from(spec.peer_public),
            preshared_key: spec.preshared,
            keepalive: Some(1),
            endpoint: SocketAddr::from((Ipv4Addr::LOCALHOST, spec.peer_port)),
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, spec.bind_port)),
            stale_after: spec.stale_after,
        },
        cancel.clone(),
    )
    .expect("UDP 綁得起來");
    let outbound = device.outbound.clone();
    // device 的 inbound 交給 stack，這裡先用一個佔位再換回來
    let (_dummy_tx, dummy_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let mut device = device;
    let inbound = std::mem::replace(&mut device.inbound, dummy_rx);
    let stack = stack::spawn(
        StackConfig {
            addresses: vec![smoltcp::wire::IpCidr::new(
                smoltcp::wire::IpAddress::from(spec.address),
                32,
            )],
            dns_servers: vec![],
            mtu: crate::wg::conf::DEFAULT_MTU,
            allowed_ips: spec.allowed_ips.clone(),
            max_connections: stack::DEFAULT_MAX_CONNECTIONS,
            dns_timeout: crate::wg::dns::DEFAULT_TIMEOUT,
        },
        outbound,
        inbound,
        cancel.clone(),
    );
    Side { device, stack }
}

/// B 側：在隧道內位址上開一個 echo 監聽（這正是把 `StackCmd::Listen` 列進 API 的理由）
fn spawn_echo(b: &Side, cancel: tokio_util::sync::CancellationToken) {
    let (accept_tx, mut accept_rx) = tokio::sync::mpsc::channel(8);
    let cmd = b.stack.cmd.clone();
    tokio::spawn(async move {
        let _ = cmd
            .send(StackCmd::Listen {
                endpoint: smoltcp::wire::IpEndpoint::new(
                    smoltcp::wire::IpAddress::from(B_ADDR),
                    ECHO_PORT,
                ),
                accept: accept_tx,
            })
            .await;
        loop {
            let conn = tokio::select! {
                _ = cancel.cancelled() => break,
                c = accept_rx.recv() => match c { Some(c) => c, None => break },
            };
            tokio::spawn(async move {
                let mut conn = conn;
                while let Some(chunk) = conn.rx.recv().await {
                    if conn.tx.send(chunk).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
}

/// A 側：綁一個本地 SOCKS5 監聽並起 serve_socks5
async fn spawn_socks5(a: &Side, cancel: tokio_util::sync::CancellationToken) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind((crate::wg::socks5::BIND_ADDR, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cmd = a.stack.cmd.clone();
    tokio::spawn(async move { crate::wg::socks5::serve_socks5(listener, cmd, cancel).await });
    addr
}

/// 走 SOCKS5 連到隧道內的 `ip:port`，回傳 (連線, 回覆碼)
async fn socks5_connect(proxy: SocketAddr, ip: Ipv4Addr, port: u16) -> (TcpStream, u8) {
    let mut s = deadline(TcpStream::connect(proxy)).await.unwrap();
    deadline(s.write_all(&[0x05, 0x01, 0x00])).await.unwrap();
    let mut hello = [0u8; 2];
    deadline(s.read_exact(&mut hello)).await.unwrap();
    assert_eq!(hello, [0x05, 0x00]);
    let mut req = vec![0x05, 0x01, 0x00, 0x01];
    req.extend_from_slice(&ip.octets());
    req.extend_from_slice(&port.to_be_bytes());
    deadline(s.write_all(&req)).await.unwrap();
    let mut head = [0u8; 10];
    deadline(s.read_exact(&mut head)).await.unwrap();
    (s, head[1])
}

/// 一整組測試檯：A、B 兩端 + A 的 SOCKS5 埠
struct Bench {
    a: Side,
    #[allow(dead_code)]
    b: Side,
    socks: SocketAddr,
    cancel: tokio_util::sync::CancellationToken,
}

async fn bench() -> Bench {
    let (pa, pb) = two_free_udp_ports();
    let cancel = tokio_util::sync::CancellationToken::new();
    let a = spin_up(&SideSpec::a(pa, pb), &cancel);
    let b = spin_up(&SideSpec::b(pa, pb), &cancel);
    spawn_echo(&b, cancel.clone());
    let socks = spawn_socks5(&a, cancel.clone()).await;
    Bench { a, b, socks, cancel }
}

/// W4.1 兩顆引擎起來後，A 要在幾秒內推出 HandshakeOk
#[tokio::test]
async fn both_engines_complete_a_handshake() {
    let mut bench = bench().await;
    let ev = deadline(bench.a.device.events.recv()).await.expect("要有事件");
    assert_eq!(ev, DeviceEvent::HandshakeOk);
    bench.cancel.cancel();
}

/// W4.2 default route 的回歸測試（D2／R11）。
///
/// 拿掉 `add_default_ipv4_route` 之後，`Medium::Ip` 的 `has_neighbor()` 一律
/// 是 false，TCP 的 SYN 會**靜默地**送不出去——沒有錯誤、沒有日誌，只是不動。
/// 這條測試存在的唯一意義就是釘住那一行。
#[tokio::test]
async fn without_a_default_route_the_syn_never_leaves() {
    stack::SKIP_DEFAULT_ROUTE.store(true, std::sync::atomic::Ordering::SeqCst);
    let outcome = tokio::time::timeout(Duration::from_secs(2), async {
        let bench = bench().await;
        let r = socks5_connect(bench.socks, B_ADDR, ECHO_PORT).await;
        bench.cancel.cancel();
        r
    })
    .await;
    stack::SKIP_DEFAULT_ROUTE.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(outcome.is_err(), "沒有 default route 就該完全沒有回覆——這正是它難以察覺的地方");
}

/// W4.3 SOCKS5 CONNECT 到 echo，送一個位元組
#[tokio::test]
async fn a_single_byte_echoes_back() {
    let bench = bench().await;
    let (mut s, code) = socks5_connect(bench.socks, B_ADDR, ECHO_PORT).await;
    assert_eq!(code, 0x00);
    deadline(s.write_all(b"x")).await.unwrap();
    let mut got = [0u8; 1];
    deadline(s.read_exact(&mut got)).await.unwrap();
    assert_eq!(&got, b"x");
    bench.cancel.cancel();
}

/// W4.4 1 MiB 隨機資料（驗 smoltcp 的分段、視窗、重組）
#[tokio::test]
async fn a_megabyte_survives_the_tunnel_intact() {
    let bench = bench().await;
    let (s, code) = socks5_connect(bench.socks, B_ADDR, ECHO_PORT).await;
    assert_eq!(code, 0x00);
    let payload: Vec<u8> = (0..1024 * 1024).map(|i| ((i * 31 + 7) % 251) as u8).collect();
    let (mut r, mut w) = s.into_split();
    let sent = payload.clone();
    let writer = tokio::spawn(async move {
        w.write_all(&sent).await.unwrap();
        w.shutdown().await.unwrap();
    });
    let mut got = Vec::with_capacity(payload.len());
    deadline(r.read_to_end(&mut got)).await.unwrap();
    writer.await.unwrap();
    assert_eq!(got.len(), payload.len());
    assert!(got == payload, "位元組要完全一致");
    bench.cancel.cancel();
}

/// W4.5 8 條連線同時各送 128 KiB，互不串線（驗虛擬埠隔離）
#[tokio::test]
async fn eight_concurrent_connections_do_not_cross_talk() {
    let bench = bench().await;
    let mut tasks = Vec::new();
    for i in 0u8..8 {
        let proxy = bench.socks;
        tasks.push(tokio::spawn(async move {
            let (s, code) = socks5_connect(proxy, B_ADDR, ECHO_PORT).await;
            assert_eq!(code, 0x00);
            let payload = vec![i; 128 * 1024];
            let (mut r, mut w) = s.into_split();
            let sent = payload.clone();
            let writer = tokio::spawn(async move {
                w.write_all(&sent).await.unwrap();
                w.shutdown().await.unwrap();
            });
            let mut got = Vec::new();
            r.read_to_end(&mut got).await.unwrap();
            writer.await.unwrap();
            assert_eq!(got, payload, "第 {i} 條收到別人的資料了");
        }));
    }
    for t in tasks {
        deadline(t).await.unwrap();
    }
    bench.cancel.cancel();
}

/// W4.6 客戶端半關之後，echo 端把剩餘資料送回再關
#[tokio::test]
async fn a_half_close_flushes_the_remaining_bytes() {
    let bench = bench().await;
    let (s, code) = socks5_connect(bench.socks, B_ADDR, ECHO_PORT).await;
    assert_eq!(code, 0x00);
    let (mut r, mut w) = s.into_split();
    deadline(w.write_all(b"the tail end")).await.unwrap();
    deadline(w.shutdown()).await.unwrap();
    let mut got = Vec::new();
    deadline(r.read_to_end(&mut got)).await.unwrap();
    assert_eq!(got, b"the tail end", "半關不可以吃掉最後一段");
    bench.cancel.cancel();
}

/// W4.7 連到沒人聽的埠：SOCKS5 要回 0x05，不可以掛到逾時
#[tokio::test]
async fn a_closed_port_answers_refused_rather_than_hanging() {
    let bench = bench().await;
    let (_s, code) = socks5_connect(bench.socks, B_ADDR, 9).await;
    assert_eq!(code, 0x05, "B 側的 smoltcp 要對未監聽的埠回 RST");
    bench.cancel.cancel();
}

/// W4.8 AllowedIPs 擋下時回 0x02，而且**一個封包都不可以送進 UDP socket**
#[tokio::test]
async fn a_destination_outside_allowed_ips_never_reaches_the_wire() {
    let (pa, pb) = two_free_udp_ports();
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut spec = SideSpec::a(pa, pb);
    spec.allowed_ips = vec![net(Ipv4Addr::new(10, 9, 0, 0), 24)];
    let a = spin_up(&spec, &cancel);
    let socks = spawn_socks5(&a, cancel.clone()).await;

    let before = device::UDP_TX_COUNT.load(std::sync::atomic::Ordering::SeqCst);
    let (_s, code) = socks5_connect(socks, Ipv4Addr::new(10, 99, 0, 1), 80).await;
    assert_eq!(code, 0x02);
    assert_eq!(
        device::UDP_TX_COUNT.load(std::sync::atomic::Ordering::SeqCst),
        before,
        "被 AllowedIPs 擋下的目的地不可以有任何封包出去"
    );
    cancel.cancel();
}

/// W4.9 Windows 的 WSAECONNRESET（R7）：device 任務要存活並照常收發
#[tokio::test]
async fn a_udp_connection_reset_does_not_kill_the_device() {
    let (pa, pb) = two_free_udp_ports();
    let cancel = tokio_util::sync::CancellationToken::new();
    // 先讓 A 對著一個沒人聽的埠送，Windows 會在下一次 recv_from 回 ConnectionReset
    let a = spin_up(&SideSpec::a(pa, pb), &cancel);
    tokio::time::sleep(Duration::from_millis(300)).await;
    // 對端這時才起來，隧道必須還能握手——把 reset 當致命錯誤就會死在這裡
    let b = spin_up(&SideSpec::b(pa, pb), &cancel);
    spawn_echo(&b, cancel.clone());
    let mut a = a;
    let ev = deadline(a.device.events.recv()).await.expect("device 任務要還活著");
    assert_eq!(ev, DeviceEvent::HandshakeOk);
    cancel.cancel();
}

/// W4.10 cancel：三顆任務都在 500ms 內結束，UDP 與 TCP 埠都釋放
#[tokio::test]
async fn cancelling_tears_the_whole_tree_down_and_frees_the_ports() {
    let (pa, pb) = two_free_udp_ports();
    let cancel = tokio_util::sync::CancellationToken::new();
    let a = spin_up(&SideSpec::a(pa, pb), &cancel);
    let socks = spawn_socks5(&a, cancel.clone()).await;
    let Side { device, stack } = a;
    cancel.cancel();
    for (what, join) in [("device", device.join), ("stack", stack.join)] {
        tokio::time::timeout(Duration::from_millis(500), join)
            .await
            .unwrap_or_else(|_| panic!("{what} 任務 500ms 內沒收工"))
            .unwrap();
    }
    std::net::UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, pa)))
        .expect("UDP 埠要被釋放");
    tokio::net::TcpListener::bind(socks).await.expect("SOCKS5 監聽埠要被釋放");
}

/// W4.11 起→停→起（同一個 socksPort）不可以撞 AddrInUse
#[tokio::test]
async fn a_restart_on_the_same_port_does_not_hit_addr_in_use() {
    let (pa, pb) = two_free_udp_ports();
    for round in 0..2 {
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut a = spin_up(&SideSpec::a(pa, pb), &cancel);
        let b = spin_up(&SideSpec::b(pa, pb), &cancel);
        spawn_echo(&b, cancel.clone());
        let ev = deadline(a.device.events.recv()).await.expect("這一輪要握得起來");
        assert_eq!(ev, DeviceEvent::HandshakeOk, "第 {round} 輪");
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_millis(500), a.device.join).await;
    }
}

/// W4.12 對端靜默超過門檻：A 要推 HandshakeStale（門檻用注入的短值）
#[tokio::test]
async fn a_silent_peer_eventually_goes_stale() {
    let (pa, pb) = two_free_udp_ports();
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut spec = SideSpec::a(pa, pb);
    spec.stale_after = Duration::from_millis(600);
    let mut a = spin_up(&spec, &cancel);
    let mut saw_stale = false;
    while let Some(ev) = deadline(a.device.events.recv()).await {
        if ev == DeviceEvent::HandshakeStale {
            saw_stale = true;
            break;
        }
    }
    assert!(saw_stale, "對端從頭到尾沒回話，不可以一直停在 connecting 以外的假象");
    cancel.cancel();
}

/// W4.13 對 A 的 UDP 埠灌垃圾：boringtun 要全部丟掉，不 panic、不影響既有連線
#[tokio::test]
async fn garbage_on_the_udp_port_is_dropped_silently() {
    let bench = bench().await;
    let (mut s, code) = socks5_connect(bench.socks, B_ADDR, ECHO_PORT).await;
    assert_eq!(code, 0x00);

    let junk = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, bench.a.device.local_addr.port()));
    for i in 0u8..32 {
        junk.send_to(&[i; 200], target).unwrap();
    }

    deadline(s.write_all(b"still here")).await.unwrap();
    let mut got = [0u8; 10];
    deadline(s.read_exact(&mut got)).await.unwrap();
    assert_eq!(&got, b"still here", "垃圾封包不可以影響既有連線");
    bench.cancel.cancel();
}

/// W4.14 靜態轉發：直接連本地埠，完全不經 SOCKS5 協商
#[tokio::test]
async fn a_static_forward_reaches_the_echo_service() {
    let bench = bench().await;
    let listener = tokio::net::TcpListener::bind((crate::wg::socks5::BIND_ADDR, 0)).await.unwrap();
    let local = listener.local_addr().unwrap();
    let cmd = bench.a.stack.cmd.clone();
    let c = bench.cancel.clone();
    tokio::spawn(async move {
        crate::wg::socks5::serve_forward(listener, cmd, format!("{B_ADDR}:{ECHO_PORT}"), c).await
    });

    let mut s = deadline(TcpStream::connect(local)).await.unwrap();
    deadline(s.write_all(b"no negotiation")).await.unwrap();
    let mut got = [0u8; 14];
    deadline(s.read_exact(&mut got)).await.unwrap();
    assert_eq!(&got, b"no negotiation");
    bench.cancel.cancel();
}

/// W4.15 反壓：echo 端不讀時，A 側的寫入要卡住而不是無限吃進來
#[tokio::test]
async fn the_tunnel_applies_backpressure_instead_of_growing_without_bound() {
    let bench = bench().await;
    let (s, code) = socks5_connect(bench.socks, B_ADDR, ECHO_PORT).await;
    assert_eq!(code, 0x00);
    let (_r, mut w) = s.into_split();
    // 刻意完全不讀下行，寫 8 MiB
    let blocked = tokio::time::timeout(Duration::from_secs(3), async move {
        let chunk = vec![0u8; 64 * 1024];
        for _ in 0..128 {
            w.write_all(&chunk).await.unwrap();
        }
    })
    .await;
    assert!(blocked.is_err(), "對端不讀時寫入端要被反壓卡住");
    bench.cancel.cancel();
}

/// W4.16 PresharedKey 一致／不一致
#[tokio::test]
async fn a_mismatched_preshared_key_never_reports_connected() {
    // 一致：握得起來
    let (pa, pb) = two_free_udp_ports();
    let cancel = tokio_util::sync::CancellationToken::new();
    let psk = [0x07u8; 32];
    let mut sa = SideSpec::a(pa, pb);
    sa.preshared = Some(psk);
    let mut sb = SideSpec::b(pa, pb);
    sb.preshared = Some(psk);
    let mut a = spin_up(&sa, &cancel);
    let _b = spin_up(&sb, &cancel);
    assert_eq!(deadline(a.device.events.recv()).await, Some(DeviceEvent::HandshakeOk));
    cancel.cancel();

    // 不一致：永遠握不上，而且最終是 Stale，不可以誤報 connected
    let (pa, pb) = two_free_udp_ports();
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut sa = SideSpec::a(pa, pb);
    sa.preshared = Some([0x07; 32]);
    sa.stale_after = Duration::from_millis(600);
    let mut sb = SideSpec::b(pa, pb);
    sb.preshared = Some([0x08; 32]);
    let mut a = spin_up(&sa, &cancel);
    let _b = spin_up(&sb, &cancel);
    let mut saw_stale = false;
    while let Some(ev) = deadline(a.device.events.recv()).await {
        assert_ne!(ev, DeviceEvent::HandshakeOk, "PSK 不一致不可能握得起來");
        if ev == DeviceEvent::HandshakeStale {
            saw_stale = true;
            break;
        }
    }
    assert!(saw_stale);
    cancel.cancel();
}
