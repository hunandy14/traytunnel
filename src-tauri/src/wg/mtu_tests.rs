//! 自動 MTU 探測的封包與決策測試——W10 系列（PM 裁決 2026-08-24）。
//!
//! 這一份**只測純函式**：探測目標的挑選、封包長什麼樣子、什麼算自己的回音、
//! 三種結局各自的 MTU 與日誌等級。真正把封包送出去、等回音、同時還要轉發
//! device 事件的那一段是引擎的組裝順序問題，測在 `engine_tests.rs`
//! （W10.9～W10.12，覆審打回 2026-08-24 後搬過去的）。

use super::*;

const US: Ipv4Addr = Ipv4Addr::new(10, 9, 0, 2);
const GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 9, 0, 1);

fn net(addr: &str, prefix: u8) -> IpNet {
    IpNet { addr: addr.parse().unwrap(), prefix }
}

fn ip(addr: &str) -> IpAddr {
    addr.parse().unwrap()
}

/// 把一顆 echo request 翻成對端會回的那顆 echo reply。
///
/// 用的是同一組 `wire` 型別（覆審打回 2026-08-24：連測試檯也不再手工排版面
/// 與算校驗和），長度與原封包相同——真實的 echo reply 本來就會把整段 payload
/// 原樣送回來。
pub(crate) fn reply_to(request: &[u8]) -> Vec<u8> {
    let caps = ChecksumCapabilities::default();
    let packet = Ipv4Packet::new_checked(request).expect("request 要是合法的 IPv4");
    let header = Ipv4Repr::parse(&packet, &caps).expect("request 的表頭要解得開");
    let icmp = Icmpv4Packet::new_checked(packet.payload()).unwrap();
    let (ident, seq_no, data) = match Icmpv4Repr::parse(&icmp, &caps).unwrap() {
        Icmpv4Repr::EchoRequest { ident, seq_no, data } => (ident, seq_no, data.to_vec()),
        other => panic!("不是 echo request：{other:?}"),
    };
    let echo = Icmpv4Repr::EchoReply { ident, seq_no, data: &data };
    let ip = Ipv4Repr {
        src_addr: header.dst_addr,
        dst_addr: header.src_addr,
        next_header: IpProtocol::Icmp,
        payload_len: echo.buffer_len(),
        hop_limit: 64,
    };
    let mut buffer = vec![0u8; ip.buffer_len() + echo.buffer_len()];
    ip.emit(&mut Ipv4Packet::new_unchecked(&mut buffer[..]), &caps);
    echo.emit(&mut Icmpv4Packet::new_unchecked(&mut buffer[ip.buffer_len()..]), &caps);
    buffer
}

/// W10.1 探測目標就是 `.conf` 的第一個 IPv4 DNS 伺服器
#[test]
fn the_probe_aims_at_the_first_ipv4_dns_server() {
    let addresses = [net("10.9.0.2", 32)];
    let allowed = [net("0.0.0.0", 0)];
    // IPv6 的 DNS 跳過，取後面那個 v4 的
    let dns = [ip("fd00::1"), ip("10.9.0.1"), ip("10.9.0.53")];
    assert_eq!(target(&addresses, &dns, &allowed), Ok((US, GATEWAY)));
}

/// W10.2 三種「沒得探」：conf 沒 DNS、介面沒 IPv4、DNS 不在 AllowedIPs 內。
///
/// 最後一條是刻意的：`AllowedIPs` 是出口過濾器（§2.2 防線二），探測不可以
/// 變成繞過它的後門。而且擋它的就是 stack 用的同一個 `conf::allowed`
#[test]
fn the_probe_is_skipped_when_there_is_nothing_safe_to_aim_at() {
    let addresses = [net("10.9.0.2", 32)];
    let allowed = [net("10.9.0.0", 24)];

    assert!(target(&addresses, &[], &allowed).is_err(), "conf 沒有 DNS 就不探");
    assert!(target(&addresses, &[ip("fd00::1")], &allowed).is_err(), "只有 v6 的 DNS 也不探");
    assert!(target(&[], &[ip("10.9.0.1")], &allowed).is_err(), "介面沒有 v4 位址就不探");
    assert!(
        target(&addresses, &[ip("8.8.8.8")], &allowed).is_err(),
        "DNS 落在 AllowedIPs 之外時不可以硬送，那是繞過出口過濾器"
    );
    // 與 stack 出口過濾器同一份實作，不是各寫一份
    assert!(!crate::wg::conf::allowed(&allowed, &ip("8.8.8.8")));
    assert!(crate::wg::conf::allowed(&allowed, &ip("10.9.0.1")));
}

/// W10.3 探測封包：總長度剛好 [`HIGH_MTU`]、DF 有設、兩個表頭都解得回來。
///
/// 覆審打回 2026-08-24：原本這條逐位元組比對手工排的版面與自算的校驗和；
/// 改用 `wire` 之後版面歸 smoltcp 管，這裡改成釘住**規格**——尺寸、DF、
/// 兩端位址、以及「解回來就是一顆 echo request」（`parse` 會順帶驗校驗和，
/// 算錯的話這裡就會 Err）
#[test]
fn the_probe_packet_is_exactly_one_high_mtu_worth_of_bytes() {
    let raw = echo_request(US, GATEWAY, HIGH_MTU);
    assert_eq!(raw.len(), HIGH_MTU, "探的就是這個尺寸過不過得去");

    let caps = ChecksumCapabilities::default();
    let packet = Ipv4Packet::new_checked(&raw[..]).expect("要是一顆合法的 IPv4 封包");
    assert!(packet.dont_frag(), "路徑 MTU 探測必須設 DF");
    let header = Ipv4Repr::parse(&packet, &caps).expect("表頭（含校驗和）要解得開");
    assert_eq!(header.src_addr, US);
    assert_eq!(header.dst_addr, GATEWAY);
    assert_eq!(header.next_header, IpProtocol::Icmp);
    assert_eq!(header.buffer_len() + header.payload_len, HIGH_MTU);

    let icmp = Icmpv4Packet::new_checked(packet.payload()).unwrap();
    let repr = Icmpv4Repr::parse(&icmp, &caps).expect("ICMP（含校驗和）要解得開");
    assert!(matches!(repr, Icmpv4Repr::EchoRequest { .. }), "要是 echo request");
}

/// W10.4 只有**自己那一顆**探測封包的回音算數
#[test]
fn only_our_own_echo_reply_counts() {
    let request = echo_request(US, GATEWAY, HIGH_MTU);
    let good = reply_to(&request);
    assert!(is_echo_reply(&good, US, GATEWAY));

    // 別人的回音（來源不是我們打的那個閘道）不算
    assert!(!is_echo_reply(&good, US, Ipv4Addr::new(10, 9, 0, 9)));
    // 打給別人的不算
    assert!(!is_echo_reply(&good, Ipv4Addr::new(10, 9, 0, 8), GATEWAY));
    // request 本身不是 reply
    assert!(!is_echo_reply(&request, US, GATEWAY));
    // 別人的 ping 回音（ident 不同）不算——誤認會讓一條吃不下 1420 的線路
    // 被判定成吃得下，症狀就是網頁載一半的靜默黑洞
    let others = {
        let caps = ChecksumCapabilities::default();
        let data = [0u8; 16];
        let echo = Icmpv4Repr::EchoReply { ident: 0x1234, seq_no: 1, data: &data };
        let ip = Ipv4Repr {
            src_addr: GATEWAY,
            dst_addr: US,
            next_header: IpProtocol::Icmp,
            payload_len: echo.buffer_len(),
            hop_limit: 64,
        };
        let mut buffer = vec![0u8; ip.buffer_len() + echo.buffer_len()];
        ip.emit(&mut Ipv4Packet::new_unchecked(&mut buffer[..]), &caps);
        echo.emit(&mut Icmpv4Packet::new_unchecked(&mut buffer[ip.buffer_len()..]), &caps);
        buffer
    };
    assert!(!is_echo_reply(&others, US, GATEWAY));
    // 校驗和被改壞的不算（`wire` 的 parse 順帶驗掉）
    let mut corrupt = good.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0xff;
    assert!(!is_echo_reply(&corrupt, US, GATEWAY));
    // 短到放不下表頭的碎片不算
    assert!(!is_echo_reply(&good[..20], US, GATEWAY));
    assert!(!is_echo_reply(&[], US, GATEWAY));
}

/// W10.5 三種結局各自的 MTU、日誌與**日誌等級**。
///
/// 覆審打回 2026-08-24 的重點在這裡：只有「封包真的送出去了卻沒回音」
/// 才是警告，那時我們真的量到了東西；沒送出去的一律是資訊級——隧道都還沒通
/// 的時候丟一句「請手動調 MTU」只會把使用者推去改一個不相干的設定
#[test]
fn only_a_real_measurement_gets_a_warning() {
    assert_eq!(Probe::Ok.mtu(), 1420);
    assert!(Probe::Ok.log().contains("1420"));
    assert!(!Probe::Ok.is_warning());

    assert_eq!(Probe::Failed.mtu(), SAFE_MTU);
    assert!(Probe::Failed.log().contains("1280"));
    assert!(Probe::Failed.is_warning(), "真的量到路徑吃不下 1420 才是降級");
    assert!(Probe::Failed.log().contains("set MTU manually"), "要告訴使用者可以手動調上去");

    for why in [NO_HANDSHAKE, "no IPv4 DNS server in the .conf"] {
        let skipped = Probe::Skipped(why);
        assert_eq!(skipped.mtu(), SAFE_MTU);
        assert!(skipped.log().contains(why), "原因要寫在日誌裡：{why}");
        assert!(skipped.log().contains("1280"));
        assert!(!skipped.is_warning(), "一顆封包都沒送出去，沒有資格說線路不行");
        assert!(!skipped.log().contains("set MTU manually"));
    }
}

/// W10.6 等握手的耐心**必須大於** boringtun 的 `REKEY_TIMEOUT`。
///
/// 覆審打回 2026-08-24：原本是 3 秒，比重試間隔還短——握手的第一顆 UDP
/// 掉了就會把一條好好的線路誤釘成 1280。新形狀下這段等待不阻塞任何 UX
/// （列早就綁好、事件照流），所以放長是免費的
#[test]
fn the_handshake_patience_outlasts_one_rekey_timeout() {
    assert!(
        HANDSHAKE_WAIT > REKEY_TIMEOUT,
        "等不到一次重試就放棄，等於把掉一顆 UDP 誤判成線路吃不下 1420"
    );
    assert_eq!(HANDSHAKE_WAIT, Duration::from_secs(8));
    assert_eq!(PROBE_TIMEOUT, Duration::from_secs(1));
}
