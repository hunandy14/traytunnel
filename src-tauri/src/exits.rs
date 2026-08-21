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

/// 阻塞式檢測，呼叫端請丟到 blocking 執行緒上跑。
pub fn probe(port: u16) -> ExitTest {
    // socks5h 讓 DNS 也走遠端解析，等同 curl --socks5-hostname
    let proxy = match ureq::Proxy::new(format!("socks5h://127.0.0.1:{port}")) {
        Ok(p) => p,
        Err(_) => return ExitTest::Fail("no response"),
    };
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .proxy(proxy)
        .build();

    let resp = match agent.get(URL).call() {
        Ok(r) => r,
        Err(_) => return ExitTest::Fail("no response"),
    };
    let body: serde_json::Value = match resp.into_json() {
        Ok(v) => v,
        Err(_) => return ExitTest::Fail("bad response"),
    };
    let f = |k: &str| body.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (ip, city, country) = (f("ip"), f("city"), f("country"));
    if ip.is_empty() {
        return ExitTest::Fail("bad response");
    }
    ExitTest::Ok(format!("{ip}  {city}, {country}"))
}
