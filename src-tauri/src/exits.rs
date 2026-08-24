//! 出口自我檢測：經本地埠打 ipinfo.io，逾時 12 秒；外加一個協定識別器。
//!
//! 協定不由使用者選（設計書 §1.5）：使用者往往不知道伺服器上那個埠跑的是什麼，
//! 選錯的症狀是「檢測永遠失敗」，那與「代理真的壞了」在畫面上長得一模一樣。
//! 因此 [`detect`] 自己按固定順序試 SOCKS5 招呼 → HTTP CONNECT。
//!
//! 識別器與 `probe` 都照舊連 `127.0.0.1:{local}`——那是作業系統的 loopback，
//! 而各型列的監聽器就站在那個埠上當入口匝道，**連線方式一個字都不用改**（§5.4）。

use std::time::Duration;

use crate::config::RowKind;

const TIMEOUT_SECS: u64 = 12;
const URL: &str = "https://ipinfo.io/json";

/// 識別階段的逾時。3 秒與 `probe` 的 12 秒都是在「本地埠背後可能是一條跨國
/// 隧道」的前提下訂的（§5.4），別把它壓短。
pub const DETECT_TIMEOUT: Duration = Duration::from_secs(3);

/// SOCKS5 招呼：VER=05、NMETHODS=01、METHOD=00（NO AUTH）。
/// W8.14 釘住送出去的就是這三個位元組，不多不少。
pub const SOCKS5_GREETING: [u8; 3] = [0x05, 0x01, 0x00];

/// 識別不出來時自測要顯示的字（§5.4 第 5 點）
pub const NOT_A_PROXY_TEXT: &str = "此埠回應不像代理服務";

pub enum ExitTest {
    /// 「ip  city, country」
    Ok(String),
    /// 連不上或回應無法解析
    Fail(&'static str),
}

/// 識別出來的代理方言。**執行期快取，不落設定檔**（§1.5）：設定檔只記使用者
/// 填的東西；協定是觀察到的事實，觀察到的東西不進使用者的檔案——不然伺服器
/// 換了設定，檔案裡那個陳舊的判定會一路誤導下去。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyProtocol {
    Socks5,
    Http,
}

impl ProxyProtocol {
    /// 給 `TestView.protocol` 與 UI 徽章用的字串（IPC 契約上的值）
    pub fn as_str(&self) -> &'static str {
        match self {
            ProxyProtocol::Socks5 => "socks5",
            ProxyProtocol::Http => "http",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detected {
    Ok(ProxyProtocol),
    /// 識別出協定，但它要求認證——第一版不支援
    NeedsAuth(ProxyProtocol),
    /// 識別出是 HTTP，但這一次檢測失敗，帶回狀態碼（W8.10）
    BadStatus(ProxyProtocol, u16),
    /// 兩步都不像
    NotAProxy,
}

/// SOCKS5 招呼的回應判定（純位元組，W8.1～W8.6）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocksSniff {
    Ok(ProxyProtocol),
    NeedsAuth(ProxyProtocol),
    /// 不像 SOCKS5——要往下試 HTTP
    NotSocks,
}

/// HTTP CONNECT 回應的判定（純位元組，W8.7～W8.12）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpSniff {
    Ok(ProxyProtocol),
    NeedsAuth(ProxyProtocol),
    /// 是 HTTP，但回的不是 2xx 也不是 407（W8.10）
    BadStatus(u16),
    NotHttp,
}

/// 這一輪要用哪個協定：`socks` 列已知、快取命中直接用、其餘才 `detect`（W8.24）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// `kind == Socks`：listener 是引擎自己起的，協定已知（§5.4 第 2 點）
    Known(ProxyProtocol),
    /// 執行期快取命中，**不再呼叫 `detect`**
    Cached(ProxyProtocol),
    /// 沒命中，這一輪要跑一次識別
    MustDetect,
}

/// SOCKS5 招呼回應的純位元組判定。抽成純函式才測得到（§5.4）。
pub fn sniff_socks5(_reply: &[u8]) -> SocksSniff {
    todo!("W8.1～W8.6")
}

/// HTTP CONNECT 回應的純位元組判定。
pub fn sniff_http(_reply: &[u8]) -> HttpSniff {
    todo!("W8.7～W8.12")
}

/// 兩步的判定合成最終結論（W8.13）：都不像就是 [`Detected::NotAProxy`]。
pub fn combine(_socks: SocksSniff, _http: HttpSniff) -> Detected {
    todo!("W8.13")
}

/// 識別結果 → 自測要顯示的字。
///
/// `NeedsAuth` 顯示「{p} proxy requires authentication」且 `state = fail`，
/// 徽章仍標示 `p`（識別是成功的，只是不支援）；`NotAProxy` 顯示
/// [`NOT_A_PROXY_TEXT`] 且**不顯示徽章**（§5.4 第 4、5 點）。
pub fn detect_message(_detected: &Detected) -> String {
    todo!("W8.13")
}

/// 一條列這一輪的協定從哪裡來（W8.24）。`cached` 是 `ExitRuntime.detected`。
pub fn resolve_protocol(_kind: RowKind, _cached: Option<ProxyProtocol>) -> Resolution {
    todo!("W8.24")
}

/// 阻塞式協定識別，呼叫端請丟到 blocking 執行緒。逾時 [`DETECT_TIMEOUT`]。
///
/// 第一步失敗之後**必須開一條新連線**再試 HTTP：SOCKS5 的招呼已經污染了
/// 那條連線，接著送 CONNECT 只會得到垃圾（W8.16）。
pub fn detect(_port: u16) -> Detected {
    todo!("W8.15～W8.20")
}

/// 測試觀測點：[`detect`] 實際被呼叫的次數。
///
/// W8.24 要斷言「快取命中時不再呼叫 detect」，只靠黑箱看不出來。
#[cfg(test)]
pub(crate) static DETECT_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// 本地 SOCKS5 代理的 URL。
///
/// 這裡一定要寫 `socks5h://`，不能寫 `socks5://`：ureq 3.2 起把兩者分開了——
/// `socks5://` 會先用本機 DNS 解出 IP 再交給代理，`socks5h://` 才是把主機名整個
/// 丟給代理去解析（等同 curl --socks5-hostname）。ureq 2 的 `socks5://` 是後者的
/// 行為，所以升到 3 之後要改寫成 `socks5h://` 才維持得住「DNS 也走代理」。
fn proxy_url(port: u16) -> String {
    format!("socks5h://127.0.0.1:{port}")
}

/// 依識別出來的協定挑 scheme（§5.4）：
/// `Socks5` → [`proxy_url`]（`socks5h://`，不在本機解析主機名，既有規矩不變），
/// `Http` → `http://127.0.0.1:{port}`。
fn proxy_url_of(port: u16, protocol: ProxyProtocol) -> String {
    match protocol {
        ProxyProtocol::Socks5 => proxy_url(port),
        ProxyProtocol::Http => format!("http://127.0.0.1:{port}"),
    }
}

/// 阻塞式檢測，呼叫端請丟到 blocking 執行緒上跑。
///
/// `protocol` 由 [`detect`] 決定（`socks` 列免識別，直接給 `Socks5`）。
pub fn probe(port: u16, protocol: ProxyProtocol) -> ExitTest {
    let proxy = match ureq::Proxy::new(&proxy_url_of(port, protocol)) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("exit test port {port}: bad proxy url: {e}");
            return ExitTest::Fail("no response");
        }
    };
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .proxy(Some(proxy))
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let mut resp = match agent.get(URL).call() {
        Ok(r) => r,
        Err(e) => {
            log::warn!("exit test port {port}: request failed: {e}");
            return ExitTest::Fail("no response");
        }
    };
    let body: serde_json::Value = match resp.body_mut().read_json() {
        Ok(v) => v,
        Err(e) => {
            log::warn!("exit test port {port}: response not json: {e}");
            return ExitTest::Fail("bad response");
        }
    };
    let f = |k: &str| body.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (ip, city, country) = (f("ip"), f("city"), f("country"));
    if ip.is_empty() {
        log::warn!("exit test port {port}: json has no ip field: {body}");
        return ExitTest::Fail("bad response");
    }
    ExitTest::Ok(format!("{ip}  {city}, {country}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// URL 的 scheme 必須是 ureq 認得的，否則探測會在送出請求前就失敗，
    /// UI 上看起來像瞬間 no response。
    #[test]
    fn proxy_url_is_accepted_by_ureq() {
        assert!(ureq::Proxy::new(&proxy_url(1080)).is_ok());
    }

    /// DNS 一定要走代理：ureq 3 用 resolve_target 區分，socks5h 才是「不在本機解析」。
    /// 這條釘住 scheme，避免被改回 socks5://。
    #[test]
    fn proxy_does_not_resolve_target_locally() {
        let proxy = ureq::Proxy::new(&proxy_url(1080)).unwrap();
        assert_eq!(proxy.protocol(), ureq::ProxyProtocol::Socks5h);
        assert!(!proxy.resolve_target(), "主機名要交給代理解析，不能在本機先解 DNS");
    }

    /// 實機驗證用，預設不跑：
    /// cargo test --lib -- --ignored --nocapture live_probe
    #[test]
    #[ignore]
    fn live_probe() {
        for port in [1080u16, 1083] {
            match probe(port, ProxyProtocol::Socks5) {
                ExitTest::Ok(text) => println!("port {port} : {text}"),
                ExitTest::Fail(msg) => println!("port {port} : {msg}"),
            }
        }
    }
}

#[cfg(test)]
#[path = "exits_detect_tests.rs"]
mod detect_tests;
