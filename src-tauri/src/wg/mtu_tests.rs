//! 自動 MTU 探測的測試——W10 系列（PM 裁決 2026-08-24 的第四件）。
//!
//! 探測本身是一顆純粹靠通道驅動的函式，所以**不需要真的架一條隧道**：
//! 餵一顆回音進 inbound 就是「探得過」，什麼都不餵就是「探不過」。
//! 封包的組裝與辨識則是純函式，逐欄位釘住。

use super::*;

const US: Ipv4Addr = Ipv4Addr::new(10, 9, 0, 2);
const GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 9, 0, 1);

fn net(addr: &str, prefix: u8) -> IpNet {
    IpNet { addr: addr.parse().unwrap(), prefix }
}

fn ip(addr: &str) -> IpAddr {
    addr.parse().unwrap()
}

/// 把一顆 echo request 翻成對端會回的那顆 echo reply（來源／目的地對調、
/// 型別改成 0、兩個校驗和重算），長度與原封包相同——真實的 echo reply
/// 本來就會把整段 payload 原樣送回來
fn reply_to(request: &[u8]) -> Vec<u8> {
    let mut p = request.to_vec();
    let (src, dst) = (p[12..16].to_vec(), p[16..20].to_vec());
    p[12..16].copy_from_slice(&dst);
    p[16..20].copy_from_slice(&src);
    p[10..12].copy_from_slice(&[0, 0]);
    let sum = checksum(&p[..20]);
    p[10..12].copy_from_slice(&sum.to_be_bytes());
    p[20] = 0; // ICMP_ECHO_REPLY
    p[22..24].copy_from_slice(&[0, 0]);
    let sum = checksum(&p[20..]);
    p[22..24].copy_from_slice(&sum.to_be_bytes());
    p
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
/// 最後一條是刻意的：`AllowedIPs` 是出口過濾器（§2.2 防線二），
/// 探測不可以變成繞過它的後門
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
}

/// W10.3 探測封包：總長度剛好 [`HIGH_MTU`]、DF 有設、兩個校驗和都對
#[test]
fn the_probe_packet_is_exactly_one_high_mtu_worth_of_bytes() {
    let p = echo_request(US, GATEWAY, HIGH_MTU);
    assert_eq!(p.len(), HIGH_MTU, "探的就是這個尺寸過不過得去");
    assert_eq!(u16::from_be_bytes([p[2], p[3]]) as usize, HIGH_MTU, "IP 的 Total Length 要一致");
    assert_eq!(p[0], 0x45);
    assert_eq!(p[9], 1, "protocol = ICMP");
    assert_eq!(u16::from_be_bytes([p[6], p[7]]) & 0x4000, 0x4000, "路徑 MTU 探測必須設 DF");
    assert_eq!(p[12..16], US.octets(), "來源是介面位址");
    assert_eq!(p[16..20], GATEWAY.octets(), "目的地是隧道內的閘道");
    assert_eq!(p[20], 8, "ICMP echo request");
    // 校驗和的定義：把含校驗和欄位在內的整段再算一次，結果必須是 0
    assert_eq!(checksum(&p[..20]), 0, "IP 表頭校驗和");
    assert_eq!(checksum(&p[20..]), 0, "ICMP 校驗和");
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
    // id 不符（別的 ping）不算——誤認會讓一條吃不下 1420 的線路被判定成吃得下
    let mut wrong_id = good.clone();
    wrong_id[24..26].copy_from_slice(&0x1234u16.to_be_bytes());
    assert!(!is_echo_reply(&wrong_id, US, GATEWAY));
    // 不是 ICMP、以及短到放不下表頭的碎片，都不算
    let mut not_icmp = good.clone();
    not_icmp[9] = 6;
    assert!(!is_echo_reply(&not_icmp, US, GATEWAY));
    assert!(!is_echo_reply(&good[..20], US, GATEWAY));
    assert!(!is_echo_reply(&[], US, GATEWAY));
}

/// W10.5 有回音 → `Ok`，而且**真的有一顆 1420 的封包被送出去**
#[tokio::test]
async fn a_reply_means_the_path_takes_the_high_mtu() {
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(4);
    let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(4);
    // 對面：收到什麼就回它的回音
    tokio::spawn(async move {
        let request = out_rx.recv().await.expect("探測封包要被送出去");
        assert_eq!(request.len(), HIGH_MTU);
        in_tx.send(reply_to(&request)).await.unwrap();
    });
    let got = probe(&out_tx, &mut in_rx, US, GATEWAY, PROBE_TIMEOUT).await;
    assert_eq!(got, Probe::Ok);
    assert_eq!(got.mtu(), HIGH_MTU);
    assert!(!got.is_warning());
}

/// W10.6 沒有回音（對端擋 ICMP，或路徑吃不下 1420）→ 退回安全值，
/// 而且要記**警告**：吞吐量掉一截而畫面上什麼都看不出來
#[tokio::test]
async fn silence_falls_back_to_the_safe_mtu_with_a_warning() {
    let (out_tx, _out_rx) = mpsc::channel::<Vec<u8>>(4);
    let (_in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(4);
    let got = probe(&out_tx, &mut in_rx, US, GATEWAY, Duration::from_millis(150)).await;
    assert_eq!(got, Probe::Failed);
    assert_eq!(got.mtu(), SAFE_MTU);
    assert_eq!(got.mtu(), 1280);
    assert!(got.is_warning(), "降級一定要記警告級（使用者明令）");
    assert!(got.log().contains("set MTU manually"), "要告訴使用者可以手動調上去");
}

/// W10.7 隧道上的其他封包不可以被誤認成回音：雜訊照收，結論仍是逾時
#[tokio::test]
async fn unrelated_inbound_traffic_never_counts_as_a_reply() {
    let (out_tx, _out_rx) = mpsc::channel::<Vec<u8>>(4);
    let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(8);
    for i in 0u8..4 {
        in_tx.send(vec![i; 120]).await.unwrap();
    }
    let got = probe(&out_tx, &mut in_rx, US, GATEWAY, Duration::from_millis(150)).await;
    assert_eq!(got, Probe::Failed);
}

/// W10.8 三種結局各自的 MTU 與日誌。跳過那一支也要留一行——
/// 「為什麼我的隧道只有 1280」必須在日誌裡答得出來
#[test]
fn every_outcome_says_which_mtu_it_picked_and_why() {
    assert_eq!(Probe::Ok.mtu(), 1420);
    assert!(Probe::Ok.log().contains("1420"));
    assert!(!Probe::Ok.is_warning());

    let skipped = Probe::Skipped("no IPv4 DNS server in the .conf");
    assert_eq!(skipped.mtu(), SAFE_MTU);
    assert!(skipped.log().contains("no IPv4 DNS server"), "原因要寫在日誌裡");
    assert!(skipped.log().contains("1280"));
    assert!(!skipped.is_warning(), "沒得探不是降級，不必用警告去嚇人");

    assert_eq!(Probe::Failed.mtu(), SAFE_MTU);
    assert!(Probe::Failed.log().contains("1280"));
}
