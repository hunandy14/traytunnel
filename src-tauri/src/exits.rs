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
/// 注意這裡只能寫 `socks5://`，不能寫 `socks5h://`：ureq 2.x 的 Proxy 只認得
/// http／socks4／socks4a／socks／socks5，遇到 socks5h 會直接回 Err，探測會在
/// 送出請求前就失敗。DNS 本來就已經走代理——ureq 對 SOCKS5 且目標不是 IP 字面值
/// 時，會把主機名以 TargetAddr::Domain 交給代理解析，等同 curl --socks5-hostname。
fn proxy_url(port: u16) -> String {
    format!("socks5://127.0.0.1:{port}")
}

/// 阻塞式檢測，呼叫端請丟到 blocking 執行緒上跑。
pub fn probe(port: u16) -> ExitTest {
    let proxy = match ureq::Proxy::new(proxy_url(port)) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("exit test port {port}: bad proxy url: {e}");
            return ExitTest::Fail("no response");
        }
    };
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .proxy(proxy)
        .build();

    let resp = match agent.get(URL).call() {
        Ok(r) => r,
        Err(e) => {
            log::warn!("exit test port {port}: request failed: {e}");
            return ExitTest::Fail("no response");
        }
    };
    let body: serde_json::Value = match resp.into_json() {
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

    /// 這支測試就是為了擋住原本的 bug：URL 的 scheme 必須是 ureq 認得的，
    /// 否則探測會在送出請求前就失敗，UI 上看起來像瞬間 no response。
    #[test]
    fn proxy_url_is_accepted_by_ureq() {
        assert!(ureq::Proxy::new(proxy_url(1080)).is_ok());
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
