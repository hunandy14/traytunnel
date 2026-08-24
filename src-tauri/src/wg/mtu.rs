//! 隧道內的路徑 MTU 自動探測（PM 裁決 2026-08-24）。
//!
//! 只有在**使用者沒覆寫、`.conf` 也沒明寫** MTU 的時候才會跑（優先序仍是
//! 介面覆寫 ＞ conf 明寫 ＞ 這裡，見 [`super::plan_mtu`]）。做法是握手完成後
//! 往隧道內的閘道（`.conf` 的第一個 DNS 伺服器）送一顆**填滿到 [`HIGH_MTU`]
//! 位元組**的 ICMP echo：
//!
//! * 有回音 → 這條路徑吃得下 1420，本輪引擎就用 1420；
//! * 真的送出去卻沒有回音 → [`Probe::Failed`]，退回 [`SAFE_MTU`] 並記一行
//!   **警告**告訴使用者可以手動把 MTU 調上去；
//! * 根本沒送出去（沒有可打的目標、或等不到握手） → [`Probe::Skipped`]，
//!   一樣用 [`SAFE_MTU`] 但**只記資訊級**——隧道都還沒通的時候丟一句「請手動
//!   調 MTU」是誤導。
//!
//! **一次到位，不做多級二分**：多級探測要好幾個 RTT，而 1420／1280 這兩個值
//! 已經涵蓋了實務上絕大多數的線路；分不出來的那一段本來就該由使用者手填。
//!
//! 結果只活在這一輪連線的執行期裡（supervise 每連線記一份，見
//! [`super::mtu_for_round`]），**不落設定檔**——換了網路再重連時本來就該重探。
//!
//! 實作上刻意**不動用 smoltcp 的 ICMP socket**：那需要多開一個 feature，而且
//! 探測必須發生在正式的 stack 建起來**之前**（smoltcp 的 MTU 是建構參數，
//! 起好之後改不了）。封包本身則是用 smoltcp 的 `wire` 型別組出來的
//! （`Ipv4Repr`／`Icmpv4Repr`），不手工排版面：版面與兩個校驗和都由它負責，
//! 解析那一側也順帶免費拿到校驗和驗證。

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, IpProtocol, Ipv4Packet, Ipv4Repr};

use super::conf::IpNet;

/// 探得過就用這個內層 MTU：以太網的 1500 減掉 WireGuard 的外層表頭
pub const HIGH_MTU: usize = 1420;

/// 探不過（或不能探）時的保守值，就是應用層預設 [`super::conf::APP_DEFAULT_MTU`]
pub const SAFE_MTU: usize = super::conf::APP_DEFAULT_MTU;

/// 一顆探測封包的耐心。隧道內的閘道就在對面，1 秒綽綽有餘
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// 探測前等握手的耐心。
///
/// **必須大於 boringtun 的 `REKEY_TIMEOUT`（5 秒）**：握手的第一顆封包掉了是
/// 常態，等不到那一次重試就宣告放棄的話，只是把「掉了一顆 UDP」誤釘成
/// 「這條線路只吃得下 1280」。8 秒容得下一次完整的重試還有餘裕。
///
/// 這一段等待**不阻塞任何東西**：列的監聽器在探測開始前就綁好了，device 的
/// 狀態事件也照常即時往上送（見 `engine::run` 的順序）。
pub const HANDSHAKE_WAIT: Duration = Duration::from_secs(REKEY_TIMEOUT.as_secs() + 3);

/// boringtun 的 `REKEY_TIMEOUT`。這裡只拿來釘住 [`HANDSHAKE_WAIT`] 與它的關係
pub const REKEY_TIMEOUT: Duration = Duration::from_secs(5);

/// 等不到握手時的跳過原因（[`Probe::Skipped`] 的一種，資訊級）
pub const NO_HANDSHAKE: &str = "handshake not completed";

/// ICMP echo 的識別碼與序號。這條隧道上同時只會有我們這一顆探測封包，
/// 固定值就足以認得出自己的回音
const ECHO_ID: u16 = 0x7767;
const ECHO_SEQ: u16 = 1;

/// IPv4 表頭（無選項）＋ ICMP echo 表頭的長度，用來換算 payload 要填多少
const IPV4_HEADER_LEN: usize = 20;
const ICMP_ECHO_HEADER_LEN: usize = 8;

/// 這一輪引擎的 MTU 決策，由 [`super::plan_mtu`] 產出。
///
/// 做成一個 enum 而不是「一個值 ＋ 一個 bool」：後者容得下
/// 「要探測、但同時又指定了 1400」這種說不通的組合
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// 已經有人指定了（介面覆寫或 `.conf` 明寫，也可能是本連線上一輪探測的
    /// 結果），照著設就是
    Fixed(usize),
    /// 沒有人指定過：這一輪連上之後探一次
    Probe,
}

/// 探測的三種結局。狀態、日誌與日誌等級都由它決定，呼叫端不必自己拼字串
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// 1420 的封包有回音
    Ok,
    /// **封包真的送出去了**，但沒有回音：路徑吃不下 1420，或對端擋 ICMP
    Failed,
    /// 一顆探測封包都沒送出去（沒有可打的目標，或等不到握手）
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

    /// 只有 [`Probe::Failed`] 記警告級。
    ///
    /// **真的量到了「這條路徑吃不下 1420」才算降級**，那時吞吐量掉一截而畫面上
    /// 什麼都看不出來，值得一行警告。反過來說，連握手都還沒成功的時候丟一句
    /// 「請手動調 MTU」只會把使用者推去改一個根本不相干的設定。
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
///   探測不可以是繞過它的後門。這裡呼叫的就是 stack 出口過濾器用的同一個
///   [`super::conf::allowed`]，不另寫一份。
pub fn target(
    addresses: &[IpNet],
    dns: &[IpAddr],
    allowed_ips: &[IpNet],
) -> Result<(Ipv4Addr, Ipv4Addr), &'static str> {
    let Some(dst) = dns.iter().find_map(only_v4) else {
        return Err("no IPv4 DNS server in the .conf");
    };
    let Some(src) = addresses.iter().find_map(|n| only_v4(&n.addr)) else {
        return Err("the interface has no IPv4 address");
    };
    if !super::conf::allowed(allowed_ips, &IpAddr::V4(dst)) {
        return Err("the DNS server is outside AllowedIPs");
    }
    Ok((src, dst))
}

fn only_v4(ip: &IpAddr) -> Option<Ipv4Addr> {
    match ip {
        IpAddr::V4(v4) => Some(*v4),
        IpAddr::V6(_) => None,
    }
}

/// 一顆總長度剛好 `total_len` 位元組的 IPv4 ICMP echo request（含 IP 表頭）。
///
/// 版面、兩個校驗和與 DF 旗標都由 smoltcp 的 `wire` 負責（`Ipv4Repr::emit`
/// 本來就把 DF 設成 true——路徑 MTU 探測被沿路某一跳偷偷切開就失去意義了）。
pub fn echo_request(src: Ipv4Addr, dst: Ipv4Addr, total_len: usize) -> Vec<u8> {
    let total_len = total_len.max(IPV4_HEADER_LEN + ICMP_ECHO_HEADER_LEN);
    let data = vec![0u8; total_len - IPV4_HEADER_LEN - ICMP_ECHO_HEADER_LEN];
    let icmp = Icmpv4Repr::EchoRequest { ident: ECHO_ID, seq_no: ECHO_SEQ, data: &data };
    let ip = Ipv4Repr {
        src_addr: src,
        dst_addr: dst,
        next_header: IpProtocol::Icmp,
        payload_len: icmp.buffer_len(),
        hop_limit: 64,
    };
    let caps = ChecksumCapabilities::default();
    let mut buffer = vec![0u8; ip.buffer_len() + icmp.buffer_len()];
    ip.emit(&mut Ipv4Packet::new_unchecked(&mut buffer[..]), &caps);
    icmp.emit(&mut Icmpv4Packet::new_unchecked(&mut buffer[ip.buffer_len()..]), &caps);
    buffer
}

/// 這顆封包是不是**我們那一顆探測封包**的回音。
///
/// 比對到 ident／seq 為止：隧道內的其他流量（乃至於別人的 ping）不可以被誤認成
/// 探測成功，那會讓一條其實吃不下 1420 的線路被判定成吃得下，症狀就是網頁
/// 載一半的靜默黑洞——正是這整件事要避免的東西。兩個校驗和由 `wire` 的
/// `parse` 順帶驗掉。
pub fn is_echo_reply(packet: &[u8], src: Ipv4Addr, dst: Ipv4Addr) -> bool {
    let caps = ChecksumCapabilities::default();
    let Ok(ipv4) = Ipv4Packet::new_checked(packet) else {
        return false;
    };
    let Ok(header) = Ipv4Repr::parse(&ipv4, &caps) else {
        return false;
    };
    // 回音的來源是我們打的那個閘道，目的地是我們自己
    if header.next_header != IpProtocol::Icmp || header.src_addr != dst || header.dst_addr != src {
        return false;
    }
    let Ok(icmp) = Icmpv4Packet::new_checked(ipv4.payload()) else {
        return false;
    };
    match Icmpv4Repr::parse(&icmp, &caps) {
        Ok(Icmpv4Repr::EchoReply { ident, seq_no, .. }) => ident == ECHO_ID && seq_no == ECHO_SEQ,
        _ => false,
    }
}

#[cfg(test)]
#[path = "mtu_tests.rs"]
pub(crate) mod tests;
