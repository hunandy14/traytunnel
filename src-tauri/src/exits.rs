//! 出口自我檢測：經本地 SOCKS5 埠打 ipinfo.io，逾時 12 秒。

use std::time::Duration;

const TIMEOUT_SECS: u64 = 12;
const URL: &str = "https://ipinfo.io/json";

pub enum ExitTest {
    /// 「ip  city, country」
    Ok(String),
    /// 連不上或回應無法解析
    Fail(&'static str),
}

/// 本地 SOCKS5 代理的 URL。
///
/// 這裡一定要寫 `socks5h://`，不能寫 `socks5://`：ureq 3.2 起把兩者分開了——
/// `socks5://` 會先用本機 DNS 解出 IP 再交給代理，`socks5h://` 才是把主機名整個
/// 丟給代理去解析（等同 curl --socks5-hostname）。ureq 2 的 `socks5://` 是後者的
/// 行為，所以升到 3 之後要改寫成 `socks5h://` 才維持得住「DNS 也走代理」。
fn proxy_url(port: u16) -> String {
    format!("socks5h://127.0.0.1:{port}")
}

/// 阻塞式檢測，呼叫端請丟到 blocking 執行緒上跑。
pub fn probe(port: u16) -> ExitTest {
    let proxy = match ureq::Proxy::new(&proxy_url(port)) {
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
            match probe(port) {
                ExitTest::Ok(text) => println!("port {port} : {text}"),
                ExitTest::Fail(msg) => println!("port {port} : {msg}"),
            }
        }
    }
}
