//! 隧道內的路徑 MTU 自動探測（PM 裁決 2026-08-24）。
//!
//! 只有在**使用者沒覆寫、`.conf` 也沒明寫** MTU 的時候才會跑（優先序仍是
//! 介面覆寫 ＞ conf 明寫 ＞ 這裡，見 `wg::effective_mtu` 與
//! [`super::should_probe_mtu`]）。做法是握手完成後往隧道內的閘道
//! （`.conf` 的第一個 DNS 伺服器）送一顆**填滿到 [`HIGH_MTU`] 位元組**的
//! ICMP echo：
//!
//! * 有回音 → 這條路徑吃得下 1420，本輪引擎就用 1420；
//! * 逾時（含對端擋 ICMP） → 退回 [`SAFE_MTU`]，並記一行**警告**告訴使用者
//!   可以手動把 MTU 調上去。
//!
//! **一次到位，不做多級二分**：多級探測要好幾個 RTT，而 1420／1280 這兩個值
//! 已經涵蓋了實務上絕大多數的線路；分不出來的那一段本來就該由使用者手填。
//!
//! 結果只活在這一輪引擎的生命週期裡，**不落設定檔**——換了網路（換 Wi-Fi、
//! 插上手機熱點）再重連時本來就該重探一次。
//!
//! 實作上刻意**不動用 smoltcp 的 ICMP socket**：那需要多開一個 feature，而且
//! 探測必須發生在正式的 stack 建起來**之前**（smoltcp 的 MTU 是建構參數，
//! 起好之後改不了）。手工組一顆 IPv4 + ICMP 封包直接餵給 device 的 outbound
//! 通道，是這裡侵入性最小的做法：走的是與一般流量完全相同的那條加密路徑。

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use tokio::sync::mpsc;

use super::conf::IpNet;

/// 探得過就用這個內層 MTU：以太網的 1500 減掉 WireGuard 的外層表頭
pub const HIGH_MTU: usize = 1420;

/// 探不過（或不能探）時的保守值，就是應用層預設 [`super::conf::APP_DEFAULT_MTU`]
pub const SAFE_MTU: usize = super::conf::APP_DEFAULT_MTU;

/// 一顆探測封包的耐心。隧道內的閘道就在對面，1 秒綽綽有餘；
/// 這也是「自動探測最多讓連線慢多久」的上限
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// 探測前等握手的耐心。握手沒完成時封包只會被 boringtun 排進佇列，
/// 探了也是逾時。3 秒容得下一次 `REKEY_TIMEOUT`（5 秒）以內的正常握手
pub const HANDSHAKE_WAIT: Duration = Duration::from_secs(3);

/// ICMP echo 的識別碼與序號。這條隧道上同時只會有我們這一顆探測封包，
/// 固定值就足以認得出自己的回音
const ECHO_ID: u16 = 0x7767;
const ECHO_SEQ: u16 = 1;

const IPV4_HEADER_LEN: usize = 20;
const ICMP_HEADER_LEN: usize = 8;
const PROTO_ICMP: u8 = 1;
const ICMP_ECHO_REQUEST: u8 = 8;
const ICMP_ECHO_REPLY: u8 = 0;

/// 探測的三種結局。狀態與日誌都由它決定，呼叫端不必自己拼字串
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// 1420 的封包有回音
    Ok,
    /// 逾時：路徑吃不下 1420，或對端擋 ICMP
    Failed,
    /// 這一輪沒得探（附上原因，會出現在日誌裡）
    Skipped(&'static str),
}

impl Probe {
    /// 這一輪引擎要用的 MTU
    pub fn mtu(&self) -> usize {
        match self {
            Probe::Ok => HIGH_MTU,
            Probe::Failed | Probe::Skipped(_) => SAFE_MTU,
        }
    }

    /// 降級要記**警告級**（使用者明令）：吞吐量掉一截而畫面上什麼都看不出來，
    /// 是最需要一行字解釋的那一類情況
    pub fn is_warning(&self) -> bool {
        matches!(self, Probe::Failed)
    }

    /// 進活動日誌的那一行
    pub fn log(&self) -> String {
        match self {
            Probe::Ok => format!("path MTU probe ok, using MTU {HIGH_MTU}"),
            Probe::Failed => format!(
                "path MTU probe failed, using safe MTU {SAFE_MTU} \
                 — set MTU manually for higher throughput"
            ),
            Probe::Skipped(why) => {
                format!("path MTU probe skipped ({why}), using safe MTU {SAFE_MTU}")
            }
        }
    }
}

/// 探測要從哪個位址打到哪個位址：來源是介面的第一個 IPv4 位址，目的地是
/// `.conf` 的第一個 IPv4 DNS 伺服器（隧道內的閘道）。
///
/// 三種情況直接放棄（回 `Err(原因)`），不亂送封包：
///
/// * `.conf` 沒寫 DNS——沒有一個「一定在隧道另一頭活著」的目標可打；
/// * 介面沒有 IPv4 位址——這顆探測封包是 IPv4 的；
/// * DNS 伺服器不在 `AllowedIPs` 內——那條位址本來就不該進隧道（§2.2 防線二），
///   探測不可以是繞過它的後門。
pub fn target(
    addresses: &[IpNet],
    dns: &[IpAddr],
    allowed: &[IpNet],
) -> Result<(Ipv4Addr, Ipv4Addr), &'static str> {
    let Some(dst) = dns.iter().find_map(|ip| match ip {
        IpAddr::V4(v4) => Some(*v4),
        IpAddr::V6(_) => None,
    }) else {
        return Err("no IPv4 DNS server in the .conf");
    };
    let Some(src) = addresses.iter().find_map(|n| match n.addr {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(_) => None,
    }) else {
        return Err("the interface has no IPv4 address");
    };
    if !allowed.iter().any(|n| n.contains(&IpAddr::V4(dst))) {
        return Err("the DNS server is outside AllowedIPs");
    }
    Ok((src, dst))
}

/// 送一顆填到 `total_len` 位元組的 ICMP echo，等它的回音。
///
/// 收到的其他封包一律丟掉：這時候正式的 stack 還沒起來，隧道上不會有別的
/// 連線，而 TCP 的重送本來就不歸這一層管。
pub async fn probe(
    outbound: &mpsc::Sender<Vec<u8>>,
    inbound: &mut mpsc::Receiver<Vec<u8>>,
    src: Ipv4Addr,
    dst: Ipv4Addr,
    timeout: Duration,
) -> Probe {
    if outbound.send(echo_request(src, dst, HIGH_MTU)).await.is_err() {
        return Probe::Failed;
    }
    let heard = tokio::time::timeout(timeout, async {
        loop {
            match inbound.recv().await {
                Some(packet) if is_echo_reply(&packet, src, dst) => return true,
                Some(_) => continue,
                None => return false,
            }
        }
    })
    .await;
    if heard == Ok(true) {
        Probe::Ok
    } else {
        Probe::Failed
    }
}

/// 一顆總長度剛好 `total_len` 位元組的 IPv4 ICMP echo request（含 IP 表頭）。
///
/// DF 有設：這是路徑 MTU 探測，被沿路某一跳偷偷切開就失去意義了。
pub fn echo_request(src: Ipv4Addr, dst: Ipv4Addr, total_len: usize) -> Vec<u8> {
    let total_len = total_len.max(IPV4_HEADER_LEN + ICMP_HEADER_LEN);
    let mut packet = vec![0u8; total_len];

    // ---- IPv4 表頭（20 位元組，沒有選項）
    packet[0] = 0x45; // 版本 4、IHL 5
    packet[1] = 0; // DSCP/ECN
    packet[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
    packet[4..6].copy_from_slice(&ECHO_ID.to_be_bytes()); // Identification
    packet[6..8].copy_from_slice(&0x4000u16.to_be_bytes()); // Don't Fragment
    packet[8] = 64; // TTL
    packet[9] = PROTO_ICMP;
    // 10..12 是表頭校驗和，先留 0
    packet[12..16].copy_from_slice(&src.octets());
    packet[16..20].copy_from_slice(&dst.octets());
    let ip_sum = checksum(&packet[..IPV4_HEADER_LEN]);
    packet[10..12].copy_from_slice(&ip_sum.to_be_bytes());

    // ---- ICMP echo request，其餘位元組留 0 當 padding
    let icmp = &mut packet[IPV4_HEADER_LEN..];
    icmp[0] = ICMP_ECHO_REQUEST;
    icmp[1] = 0; // code
                 // 2..4 是 ICMP 校驗和，先留 0
    icmp[4..6].copy_from_slice(&ECHO_ID.to_be_bytes());
    icmp[6..8].copy_from_slice(&ECHO_SEQ.to_be_bytes());
    let icmp_sum = checksum(icmp);
    packet[IPV4_HEADER_LEN + 2..IPV4_HEADER_LEN + 4].copy_from_slice(&icmp_sum.to_be_bytes());

    packet
}

/// 這顆封包是不是**我們那一顆探測封包**的回音。
///
/// 比對到 id／seq 為止：隧道內的其他流量（乃至於別人的 ping）不可以被誤認成
/// 探測成功，那會讓一條其實吃不下 1420 的線路被判定成吃得下，症狀就是網頁
/// 載一半的靜默黑洞——正是這整件事要避免的東西。
pub fn is_echo_reply(packet: &[u8], src: Ipv4Addr, dst: Ipv4Addr) -> bool {
    if packet.len() < IPV4_HEADER_LEN + ICMP_HEADER_LEN {
        return false;
    }
    if packet[0] >> 4 != 4 || packet[9] != PROTO_ICMP {
        return false;
    }
    let ihl = (packet[0] & 0x0f) as usize * 4;
    if ihl < IPV4_HEADER_LEN || packet.len() < ihl + ICMP_HEADER_LEN {
        return false;
    }
    // 回音的來源是我們打的目標，目的地是我們自己
    if packet[12..16] != dst.octets() || packet[16..20] != src.octets() {
        return false;
    }
    let icmp = &packet[ihl..];
    icmp[0] == ICMP_ECHO_REPLY
        && icmp[4..6] == ECHO_ID.to_be_bytes()
        && icmp[6..8] == ECHO_SEQ.to_be_bytes()
}

/// RFC 1071 的網際網路校驗和
fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    let (pairs, rest) = bytes.as_chunks::<2>();
    for c in pairs {
        sum += u16::from_be_bytes(*c) as u32;
    }
    if let [last] = rest {
        sum += u16::from_be_bytes([*last, 0]) as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
#[path = "mtu_tests.rs"]
mod tests;
