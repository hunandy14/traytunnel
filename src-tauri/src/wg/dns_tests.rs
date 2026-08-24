//! 隧道內 DNS 的測試——設計書 §5 的 W5 系列（10 條）。
//!
//! 假 DNS 伺服器接在 **B 側 device 的原始 IP 封包**上：`StackCmd` 沒有開 UDP
//! 監聽這個動詞（§1.4 只有 TCP 的 `Listen`），所以 §5 說的「綁在 B 引擎的
//! 10.9.0.2:53」在現有介面下唯一做得到的形式，就是在 B 的 device 兩端直接
//! 收發 IP 封包。這樣反而更嚴格：查詢封包真的得從隧道出去才收得到。

use super::*;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::wg::device::{self, DeviceConfig, DeviceHandle};
use crate::wg::stack::{self, StackCmd, StackConfig, StackHandle};

use boringtun::x25519::{PublicKey, StaticSecret};

const A_PRIV: [u8; 32] = [0x01; 32];
const B_PRIV: [u8; 32] = [0x02; 32];
const A_ADDR: Ipv4Addr = Ipv4Addr::new(10, 9, 0, 1);
const B_ADDR: Ipv4Addr = Ipv4Addr::new(10, 9, 0, 2);
/// 每一次等待的硬上限。
///
/// 這是**失敗才付得到**的數字：查詢正常回來時一秒都不會花在這裡，只有真的卡住
/// 才會等滿。訂得寬是刻意的——CI runner 比開發機慢好幾倍，而且整個 runner 可能
/// 因為別的工作而停頓數秒，訂緊了換來的不是「早點發現問題」，是隨機紅燈。
const PATIENCE: Duration = Duration::from_secs(60);

/// 「要等隧道內那顆假伺服器回話」的那幾條測試給查詢的預算。
///
/// 同樣是失敗才付得到：答案一到就回，寬鬆不會讓測試變慢。刻意不沿用 production 的
/// `DEFAULT_TIMEOUT`（5 秒）——那個值是給真實使用者的取捨，拿來當測試預算在
/// 負載高的機器上太貼邊（本輪就實測到一次整輪停頓把它撞破）。
const QUERY_BUDGET: Duration = Duration::from_secs(30);

async fn deadline<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(PATIENCE, f).await.expect("等待逾時：查詢沒有回來")
}

// ---------------------------------------------------------------- 假 DNS

/// 假伺服器的行為
#[derive(Clone)]
enum Answer {
    /// 回這些位址
    Records(Vec<IpAddr>),
    /// 回 NXDOMAIN
    NxDomain,
    /// 完全不回應
    Silent,
}

#[derive(Default)]
struct DnsSeen {
    /// 收到幾個查詢封包
    queries: AtomicUsize,
}

/// 從 A 的 device 借一組 (outbound, inbound) 出來當「線路」
struct Wire {
    to_peer: tokio::sync::mpsc::Sender<Vec<u8>>,
    from_peer: tokio::sync::mpsc::Receiver<Vec<u8>>,
}

/// 極簡的 IPv4/UDP 拆封：回 (src_ip, dst_ip, src_port, dst_port, payload)
fn parse_udp(pkt: &[u8]) -> Option<(Ipv4Addr, Ipv4Addr, u16, u16, Vec<u8>)> {
    if pkt.len() < 20 || pkt[0] >> 4 != 4 {
        return None;
    }
    let ihl = (pkt[0] & 0x0f) as usize * 4;
    if pkt[9] != 17 || pkt.len() < ihl + 8 {
        return None;
    }
    let src = Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]);
    let dst = Ipv4Addr::new(pkt[16], pkt[17], pkt[18], pkt[19]);
    let sp = u16::from_be_bytes([pkt[ihl], pkt[ihl + 1]]);
    let dp = u16::from_be_bytes([pkt[ihl + 2], pkt[ihl + 3]]);
    Some((src, dst, sp, dp, pkt[ihl + 8..].to_vec()))
}

/// 極簡的 IPv4/UDP 打包（checksum 欄位留 0，測試檯的對端不驗）
fn build_udp(src: Ipv4Addr, dst: Ipv4Addr, sp: u16, dp: u16, payload: &[u8]) -> Vec<u8> {
    let total = 20 + 8 + payload.len();
    let mut p = vec![0u8; total];
    p[0] = 0x45;
    p[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    p[8] = 64; // TTL
    p[9] = 17; // UDP
    p[12..16].copy_from_slice(&src.octets());
    p[16..20].copy_from_slice(&dst.octets());
    p[20..22].copy_from_slice(&sp.to_be_bytes());
    p[22..24].copy_from_slice(&dp.to_be_bytes());
    p[24..26].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    p[28..].copy_from_slice(payload);
    p
}

/// 從 DNS 查詢裡取出 (id, qname, qtype)
fn parse_query(msg: &[u8]) -> Option<(u16, String, u16)> {
    if msg.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([msg[0], msg[1]]);
    let mut i = 12usize;
    let mut labels = Vec::new();
    while i < msg.len() && msg[i] != 0 {
        let len = msg[i] as usize;
        i += 1;
        if i + len > msg.len() {
            return None;
        }
        labels.push(String::from_utf8_lossy(&msg[i..i + len]).to_string());
        i += len;
    }
    i += 1;
    if i + 4 > msg.len() {
        return None;
    }
    let qtype = u16::from_be_bytes([msg[i], msg[i + 1]]);
    Some((id, labels.join("."), qtype))
}

/// 組一份 DNS 回應。`rcode = 3` 就是 NXDOMAIN。
fn build_response(
    query: &[u8],
    id: u16,
    qname: &str,
    qtype: u16,
    addrs: &[IpAddr],
    rcode: u8,
) -> Vec<u8> {
    let answers: Vec<&IpAddr> = addrs
        .iter()
        .filter(|a| (qtype == 1 && a.is_ipv4()) || (qtype == 28 && a.is_ipv6()))
        .collect();
    let mut m = Vec::new();
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&[0x81, 0x80 | rcode]); // QR + RD + RA + rcode
    m.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    m.extend_from_slice(&(answers.len() as u16).to_be_bytes()); // ANCOUNT
    m.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    m.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
                                              // 問題段原樣抄回
    m.extend_from_slice(&query[12..]);
    for a in answers {
        m.extend_from_slice(&[0xC0, 0x0C]); // 指回問題段的名字
        m.extend_from_slice(&qtype.to_be_bytes());
        m.extend_from_slice(&1u16.to_be_bytes()); // IN
        m.extend_from_slice(&60u32.to_be_bytes()); // TTL
        match a {
            IpAddr::V4(v4) => {
                m.extend_from_slice(&4u16.to_be_bytes());
                m.extend_from_slice(&v4.octets());
            }
            IpAddr::V6(v6) => {
                m.extend_from_slice(&16u16.to_be_bytes());
                m.extend_from_slice(&v6.octets());
            }
        }
    }
    let _ = qname;
    m
}

/// 起一顆假 DNS：從 `wire` 收 IP 封包，對 dport=53 的查詢照 `script` 回應
fn spawn_mock_dns(
    mut wire: Wire,
    script: HashMap<String, Answer>,
    cancel: tokio_util::sync::CancellationToken,
) -> Arc<DnsSeen> {
    let seen = Arc::new(DnsSeen::default());
    let out = seen.clone();
    tokio::spawn(async move {
        loop {
            let pkt = tokio::select! {
                _ = cancel.cancelled() => break,
                p = wire.from_peer.recv() => match p { Some(p) => p, None => break },
            };
            let Some((src, dst, sp, dp, payload)) = parse_udp(&pkt) else { continue };
            if dp != 53 {
                continue;
            }
            out.queries.fetch_add(1, Ordering::SeqCst);
            let Some((id, name, qtype)) = parse_query(&payload) else { continue };
            let answer = script.get(&name).cloned().unwrap_or(Answer::NxDomain);
            let msg = match answer {
                Answer::Silent => continue,
                Answer::NxDomain => build_response(&payload, id, &name, qtype, &[], 3),
                Answer::Records(list) => build_response(&payload, id, &name, qtype, &list, 0),
            };
            let reply = build_udp(dst, src, 53, sp, &msg);
            if wire.to_peer.send(reply).await.is_err() {
                break;
            }
        }
    });
    seen
}

/// A 側：device + stack，DNS 伺服器清單由測試指定
struct Client {
    #[allow(dead_code)]
    device: DeviceHandle,
    stack: StackHandle,
}

fn spin_up_client(
    bind_port: u16,
    peer_port: u16,
    dns_servers: Vec<Ipv4Addr>,
    dns_timeout: Duration,
    cancel: &tokio_util::sync::CancellationToken,
) -> Client {
    let mut device = device::spawn(
        DeviceConfig {
            private_key: StaticSecret::from(A_PRIV),
            peer_public_key: PublicKey::from(&StaticSecret::from(B_PRIV)),
            preshared_key: None,
            keepalive: Some(1),
            endpoint: SocketAddr::from((Ipv4Addr::LOCALHOST, peer_port)),
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, bind_port)),
            stale_after: device::REJECT_AFTER,
        },
        cancel.clone(),
    )
    .expect("UDP 綁得起來");
    let outbound = device.outbound.clone();
    let (_dummy_tx, dummy_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let inbound = std::mem::replace(&mut device.inbound, dummy_rx);
    let stack = stack::spawn(
        StackConfig {
            addresses: vec![smoltcp::wire::IpCidr::new(smoltcp::wire::IpAddress::from(A_ADDR), 32)],
            dns_servers: dns_servers.into_iter().map(smoltcp::wire::IpAddress::from).collect(),
            mtu: crate::wg::conf::DEFAULT_MTU,
            allowed_ips: vec![crate::wg::conf::IpNet {
                addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                prefix: 0,
            }],
            max_connections: stack::DEFAULT_MAX_CONNECTIONS,
            dns_timeout,
        },
        outbound,
        inbound,
        cancel.clone(),
    );
    Client { device, stack }
}

/// B 側：只要 device，把它的兩端當成一條線交給假 DNS
fn spin_up_wire(
    bind_port: u16,
    peer_port: u16,
    cancel: &tokio_util::sync::CancellationToken,
) -> (DeviceHandle, Wire) {
    let mut device = device::spawn(
        DeviceConfig {
            private_key: StaticSecret::from(B_PRIV),
            peer_public_key: PublicKey::from(&StaticSecret::from(A_PRIV)),
            preshared_key: None,
            keepalive: Some(1),
            endpoint: SocketAddr::from((Ipv4Addr::LOCALHOST, peer_port)),
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, bind_port)),
            stale_after: device::REJECT_AFTER,
        },
        cancel.clone(),
    )
    .expect("UDP 綁得起來");
    let (_dummy_tx, dummy_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let from_peer = std::mem::replace(&mut device.inbound, dummy_rx);
    let wire = Wire { to_peer: device.outbound.clone(), from_peer };
    (device, wire)
}

fn two_free_udp_ports() -> (u16, u16) {
    let a = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let b = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    (a.local_addr().unwrap().port(), b.local_addr().unwrap().port())
}

async fn resolve(client: &Client, name: &str) -> Result<Vec<IpAddr>, ResolveError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    client
        .stack
        .cmd
        .send(StackCmd::Resolve { name: name.into(), reply: tx })
        .await
        .expect("stack 任務要還在");
    deadline(rx).await.expect("stack 不可以把 oneshot 丟掉")
}

/// 一組「A + 假 DNS」的測試檯
struct Bench {
    client: Client,
    #[allow(dead_code)]
    peer: DeviceHandle,
    seen: Arc<DnsSeen>,
    cancel: tokio_util::sync::CancellationToken,
}

fn bench_with(script: HashMap<String, Answer>, servers: Vec<Ipv4Addr>, timeout: Duration) -> Bench {
    let (pa, pb) = two_free_udp_ports();
    let cancel = tokio_util::sync::CancellationToken::new();
    let client = spin_up_client(pa, pb, servers, timeout, &cancel);
    let (peer, wire) = spin_up_wire(pb, pa, &cancel);
    let seen = spawn_mock_dns(wire, script, cancel.clone());
    Bench { client, peer, seen, cancel }
}

fn script(pairs: &[(&str, Answer)]) -> HashMap<String, Answer> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}

// ---------------------------------------------------------------- 測試

/// W5.1 假 DNS 回 A 記錄，`StackCmd::Resolve` 要拿到那個 IP
#[tokio::test]
async fn a_record_from_the_tunnel_dns_comes_back() {
    let want = IpAddr::V4(Ipv4Addr::new(10, 9, 0, 42));
    let bench = bench_with(
        script(&[("host.test.invalid", Answer::Records(vec![want]))]),
        vec![B_ADDR],
        QUERY_BUDGET,
    );
    let got = resolve(&bench.client, "host.test.invalid").await.unwrap();
    assert_eq!(got, vec![want]);
    bench.cancel.cancel();
}

/// W5.2 只有 AAAA 有結果、且隧道有 v6 位址時，要回 v6
#[tokio::test]
async fn an_aaaa_only_name_resolves_to_v6() {
    let want = IpAddr::V6("fd00::42".parse().unwrap());
    let bench = bench_with(
        script(&[("v6.test.invalid", Answer::Records(vec![want]))]),
        vec![B_ADDR],
        QUERY_BUDGET,
    );
    let got = resolve(&bench.client, "v6.test.invalid").await.unwrap();
    assert_eq!(got, vec![want]);
    bench.cancel.cancel();
}

/// W5.3 洩漏防線：`*.invalid` 系統解析器一定 NXDOMAIN，隧道內卻回得出位址。
/// 哪天有人加了本機回退，這一條會立刻紅。
#[tokio::test]
async fn an_invalid_tld_still_resolves_through_the_tunnel() {
    let want = IpAddr::V4(Ipv4Addr::new(10, 9, 0, 77));
    let bench = bench_with(
        script(&[("nonexistent.invalid", Answer::Records(vec![want]))]),
        vec![B_ADDR],
        QUERY_BUDGET,
    );
    let got = resolve(&bench.client, "nonexistent.invalid").await.unwrap();
    assert_eq!(got, vec![want], "答案只可能來自隧道內的那顆假伺服器");
    bench.cancel.cancel();
}

/// W5.4 NXDOMAIN → `ResolveError::NotFound`（SOCKS5 對應 0x04）
#[tokio::test]
async fn nxdomain_maps_to_not_found() {
    let bench =
        bench_with(script(&[("gone.test.invalid", Answer::NxDomain)]), vec![B_ADDR], QUERY_BUDGET);
    assert_eq!(resolve(&bench.client, "gone.test.invalid").await, Err(ResolveError::NotFound));
    assert_eq!(
        crate::wg::socks5::reply_for_resolve(&ResolveError::NotFound),
        crate::wg::socks5::Reply::HostUnreachable
    );
    bench.cancel.cancel();
}

/// W5.5 伺服器完全不回應 → 逾時（門檻注入成短值，不然這條要跑 5 秒）
#[tokio::test]
async fn a_silent_server_times_out() {
    let bench = bench_with(
        script(&[("quiet.test.invalid", Answer::Silent)]),
        vec![B_ADDR],
        Duration::from_millis(500),
    );
    assert_eq!(resolve(&bench.client, "quiet.test.invalid").await, Err(ResolveError::Timeout));
    bench.cancel.cancel();
}

/// W5.6 `[Interface] DNS` 為空：回 NoServers，且**一個 DNS 封包都不可以送出去**
#[tokio::test]
async fn without_any_server_nothing_is_sent_and_the_error_is_no_servers() {
    let bench = bench_with(HashMap::new(), vec![], crate::wg::dns::DEFAULT_TIMEOUT);
    let before = device::UDP_TX_COUNT.load(Ordering::SeqCst);
    assert_eq!(resolve(&bench.client, "any.test.invalid").await, Err(ResolveError::NoServers));
    assert_eq!(bench.seen.queries.load(Ordering::SeqCst), 0, "不可以有查詢封包出去");
    assert_eq!(
        device::UDP_TX_COUNT.load(Ordering::SeqCst),
        before,
        "沒有伺服器就不該有任何封包被送進 UDP socket"
    );
    bench.cancel.cancel();
}

/// W5.7 16 個查詢槽用罄時，第 17 個立刻回 Timeout 而不是卡住
#[tokio::test]
async fn the_seventeenth_query_fails_fast_instead_of_blocking() {
    let mut s = HashMap::new();
    for i in 0..crate::wg::dns::QUERY_SLOTS + 1 {
        s.insert(format!("slot{i}.test.invalid"), Answer::Silent);
    }
    let bench = bench_with(s, vec![B_ADDR], QUERY_BUDGET);
    let mut pending = Vec::new();
    for i in 0..crate::wg::dns::QUERY_SLOTS {
        let (tx, rx) = tokio::sync::oneshot::channel();
        bench
            .client
            .stack
            .cmd
            .send(StackCmd::Resolve { name: format!("slot{i}.test.invalid"), reply: tx })
            .await
            .unwrap();
        pending.push(rx);
    }
    // 「立刻回」的上限。與查詢預算（30 秒）差了 6 倍，慢機器上仍分得出
    // 「立刻回」與「卡住等逾時」，但不會因為 runner 慢就誤判
    let extra =
        tokio::time::timeout(Duration::from_secs(5), resolve(&bench.client, "slot16.test.invalid"))
            .await
            .expect("用罄時要立刻回，不可以卡住");
    assert_eq!(extra, Err(ResolveError::Timeout));
    bench.cancel.cancel();
}

/// W5.8 兩個伺服器，第一個不回應：smoltcp 的重送要切到第二個
#[tokio::test]
async fn a_second_server_takes_over_when_the_first_is_silent() {
    let want = IpAddr::V4(Ipv4Addr::new(10, 9, 0, 88));
    // 假伺服器只有一顆（B_ADDR），第一順位指向隧道內沒人應答的位址
    let dead = Ipv4Addr::new(10, 9, 0, 254);
    let bench = bench_with(
        script(&[("failover.test.invalid", Answer::Records(vec![want]))]),
        vec![dead, B_ADDR],
        QUERY_BUDGET,
    );
    let got = resolve(&bench.client, "failover.test.invalid").await.unwrap();
    assert_eq!(got, vec![want]);
    bench.cancel.cancel();
}

/// W5.9 `pick()` 的挑選規則（純函式）
#[test]
fn pick_prefers_v4_unless_the_tunnel_has_none() {
    let v4 = Ipv4Addr::new(10, 9, 0, 2);
    let v6: std::net::Ipv6Addr = "fd00::2".parse().unwrap();
    assert_eq!(pick(vec![v4], vec![v6], true), Some(IpAddr::V4(v4)));
    assert_eq!(pick(vec![v4], vec![v6], false), Some(IpAddr::V6(v6)));
    assert_eq!(pick(vec![], vec![v6], true), Some(IpAddr::V6(v6)));
    assert_eq!(pick(vec![v4], vec![], false), Some(IpAddr::V4(v4)));
    assert_eq!(pick(vec![], vec![], true), None);
}

/// W5.10 尾端點與不帶點等價（純函式）
#[test]
fn a_trailing_dot_is_equivalent() {
    assert_eq!(normalize_name("example.com.").unwrap(), normalize_name("example.com").unwrap());
    assert!(normalize_name("").is_err());
}
