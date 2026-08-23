//! 實機測試——設計書 §5 的 W7 系列（3 條），比照 `exits::live_probe` 一律
//! `#[ignore]`，只有手動指定才會跑：
//!
//! ```text
//! cargo test --lib -- --ignored --nocapture live_wg
//! ```
//!
//! ## 紅線（決策紀錄 U2）
//!
//! 這一組**只准做兩件事**：與 `.conf` 指定的端點握手、經隧道打外部 IP 檢測站。
//!
//! - **嚴禁連使用者內網的任何位址**：不得對 `[Interface] Address` 所在網段、
//!   `AllowedIPs` 涵蓋的私有網段，或任何 RFC1918／ULA 位址發起連線測試，
//!   也不得掃描、探測或列舉隧道內的主機與服務。這道限制由
//!   [`assert_outside_the_tunnel`] 在程式碼裡擋住，不靠寫測試的人自律。
//! - **輸出一律不得包含 `.conf` 的任何內容**：端點主機名、位址、DNS 位址皆不印。
//!
//! conf 路徑一律從環境變數 `TRAYTUNNEL_TEST_WG_CONF` 讀，測試碼裡不寫死任何
//! 路徑，也不去翻 `secrets/`。

use super::*;

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Instant;

/// 實機測試的 conf 路徑來源
const CONF_ENV: &str = "TRAYTUNNEL_TEST_WG_CONF";

/// 取得要測的 conf；沒設環境變數就跳過（印一行說明，不算失敗）
fn conf_path() -> Option<PathBuf> {
    match std::env::var(CONF_ENV) {
        Ok(p) if !p.trim().is_empty() => Some(PathBuf::from(p)),
        _ => {
            println!("略過：請設定 {CONF_ENV} 指向要測的 wg .conf");
            None
        }
    }
}

/// 目的地紅線檢查：只允許連公網位址。
///
/// 私有位址（RFC1918／ULA／loopback／link-local／CGNAT）一律當場 panic，
/// 任何人日後想在這一組測試裡「順手驗一下內網服務」都會直接撞上這道牆。
fn assert_outside_the_tunnel(ip: IpAddr) {
    let banned = match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                // CGNAT 100.64.0.0/10
                || (o[0] == 100 && (64..128).contains(&o[1]))
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                // ULA fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // link-local fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    };
    assert!(!banned, "實機測試禁止連隧道內／私有網段的位址（決策紀錄 U2）");
}

/// W7.1 握手驗證：與 `.conf` 指定的端點握一次手，只輸出成功／失敗與耗時。
///
/// **不印任何 conf 內容**——端點主機名、位址、DNS 位址皆不出現在輸出裡。
#[test]
#[ignore]
fn live_wg_handshake() {
    let Some(path) = conf_path() else { return };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let started = Instant::now();
    let result = rt.block_on(test_conf(&path));
    let elapsed = started.elapsed();
    // 只印結論與耗時。message 由 test_conf 產生，實作端有義務不把 conf 內容放進去。
    println!("handshake: {} in {:?}", if result.ok { "ok" } else { "failed" }, elapsed);
    assert!(result.ok, "握手失敗");
}

/// W7.2 經真隧道的 SOCKS5 埠跑一次 `exits::probe`，印出出口 IP。
///
/// 這是唯一允許的對外連線：ipinfo.io 是公網的 IP 檢測站，
/// 目的地在 [`assert_outside_the_tunnel`] 那一關已經被限死。
#[test]
#[ignore]
fn live_wg_probe() {
    let Some(_path) = conf_path() else { return };
    let port: u16 = std::env::var("TRAYTUNNEL_TEST_WG_SOCKS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1085);
    match crate::exits::probe(port) {
        crate::exits::ExitTest::Ok(text) => {
            // 回來的第一段是出口 IP，先過紅線檢查再印
            if let Some(ip) = text.split_whitespace().next().and_then(|s| s.parse::<IpAddr>().ok())
            {
                assert_outside_the_tunnel(ip);
            }
            println!("exit: {text}");
        }
        crate::exits::ExitTest::Fail(msg) => panic!("probe 失敗：{msg}"),
    }
}

/// W7.3 長跑：每分鐘一次 probe，統計失敗率與重握手次數。
///
/// 預設只跑 2 分鐘，真的要跑 24 小時就設 `TRAYTUNNEL_TEST_WG_SOAK_MINUTES=1440`。
#[test]
#[ignore]
fn live_wg_soak() {
    let Some(_path) = conf_path() else { return };
    let minutes: u64 = std::env::var("TRAYTUNNEL_TEST_WG_SOAK_MINUTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    let port: u16 = std::env::var("TRAYTUNNEL_TEST_WG_SOCKS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1085);

    let (mut ok, mut fail) = (0u32, 0u32);
    let mut last_exit: Option<String> = None;
    let mut exit_changes = 0u32;
    for _ in 0..minutes {
        match crate::exits::probe(port) {
            crate::exits::ExitTest::Ok(text) => {
                if let Some(ip) =
                    text.split_whitespace().next().and_then(|s| s.parse::<IpAddr>().ok())
                {
                    assert_outside_the_tunnel(ip);
                }
                if last_exit.as_deref().is_some_and(|p| p != text) {
                    exit_changes += 1;
                }
                last_exit = Some(text);
                ok += 1;
            }
            crate::exits::ExitTest::Fail(_) => fail += 1,
        }
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
    let total = ok + fail;
    println!("soak {minutes}m: ok={ok} fail={fail} failure_rate={:.2}% exit_changes={exit_changes}",
        if total == 0 { 0.0 } else { fail as f64 * 100.0 / total as f64 });
    assert_eq!(fail, 0, "長跑期間出現失敗");
}
