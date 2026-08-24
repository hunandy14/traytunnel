//! 協定識別器的測試——設計書 §6 的 W8 系列（W8.1～W8.27，共 27 條；
//! 經隧道的 W8.28～W8.30 掛在 W4 的雙引擎測試檯上，見 `wg/engine_tests.rs`）。
//!
//! 用 `#[path]` 掛回 exits.rs，慣例與 `config_tests.rs` 相同。既有那兩條
//! 釘樁測試（`proxy_url_is_accepted_by_ureq`／`proxy_does_not_resolve_target_locally`）
//! 留在 exits.rs 自己的 `mod tests` 裡，這一輪一個字都沒動。

use super::*;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::time::Instant;

// ------------------------------------------------------- 純位元組判定（F）

/// W8.1 `05 00`：SOCKS5 無認證
#[test]
fn a_socks5_no_auth_reply_is_recognised() {
    assert_eq!(sniff_socks5(&[0x05, 0x00]), SocksSniff::Ok(ProxyProtocol::Socks5));
}

/// W8.2 `05 FF`：識別成功，但它不接受 NO AUTH
#[test]
fn a_socks5_no_acceptable_method_reply_means_auth_is_required() {
    assert_eq!(sniff_socks5(&[0x05, 0xFF]), SocksSniff::NeedsAuth(ProxyProtocol::Socks5));
}

/// W8.3 `05 02`：它選了 USER/PASS，一樣是「要認證，不支援」
#[test]
fn a_socks5_user_pass_reply_means_auth_is_required() {
    assert_eq!(sniff_socks5(&[0x05, 0x02]), SocksSniff::NeedsAuth(ProxyProtocol::Socks5));
}

/// W8.4 版本不對：不是 SOCKS5，要往下試 HTTP
#[test]
fn a_wrong_version_byte_is_not_socks() {
    assert_eq!(sniff_socks5(&[0x04, 0x00]), SocksSniff::NotSocks);
}

/// W8.5 只有一個位元組：不足以判定，**不可以樂觀認定**
#[test]
fn a_single_byte_is_not_enough_to_claim_socks5() {
    assert_eq!(sniff_socks5(&[0x05]), SocksSniff::NotSocks);
    assert_eq!(sniff_socks5(&[]), SocksSniff::NotSocks);
}

/// W8.6 對方回的是 HTTP 狀態行
#[test]
fn an_http_status_line_is_not_socks() {
    assert_eq!(sniff_socks5(b"HTTP/1.1 400 Bad Request\r\n"), SocksSniff::NotSocks);
}

/// W8.7 `HTTP/1.1 200 Connection established`
#[test]
fn an_http_connect_success_is_recognised() {
    assert_eq!(
        sniff_http(b"HTTP/1.1 200 Connection established\r\n\r\n"),
        HttpSniff::Ok(ProxyProtocol::Http)
    );
}

/// W8.8 HTTP/1.0 也算
#[test]
fn http_one_zero_counts_too() {
    assert_eq!(sniff_http(b"HTTP/1.0 200 OK\r\n\r\n"), HttpSniff::Ok(ProxyProtocol::Http));
}

/// W8.9 `407 Proxy Authentication Required`
#[test]
fn a_407_means_the_http_proxy_wants_credentials() {
    assert_eq!(
        sniff_http(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n"),
        HttpSniff::NeedsAuth(ProxyProtocol::Http)
    );
}

/// W8.10 其他狀態碼：判定為 HTTP，但這一次失敗，訊息要帶回狀態碼
#[test]
fn another_status_code_is_still_http_but_this_probe_failed() {
    assert_eq!(sniff_http(b"HTTP/1.1 403 Forbidden\r\n\r\n"), HttpSniff::BadStatus(403));
    let msg = detect_message(&Detected::BadStatus(ProxyProtocol::Http, 403));
    assert!(msg.contains("403"), "訊息要帶回狀態碼：{msg}");
}

/// W8.11 非 HTTP 的起始行
#[test]
fn an_ssh_banner_is_not_http() {
    assert_eq!(sniff_http(b"SSH-2.0-OpenSSH_9.6"), HttpSniff::NotHttp);
}

/// W8.12 對方直接關連線，一個位元組都沒有
#[test]
fn an_empty_reply_is_not_http() {
    assert_eq!(sniff_http(&[]), HttpSniff::NotHttp);
}

/// W8.13 兩支都不像 → `NotAProxy`，訊息是那一句固定的中文
#[test]
fn neither_dialect_means_not_a_proxy() {
    assert_eq!(combine(SocksSniff::NotSocks, HttpSniff::NotHttp), Detected::NotAProxy);
    assert_eq!(detect_message(&Detected::NotAProxy), NOT_A_PROXY_TEXT);
    // 任一支認得就不是 NotAProxy
    assert_eq!(
        combine(SocksSniff::Ok(ProxyProtocol::Socks5), HttpSniff::NotHttp),
        Detected::Ok(ProxyProtocol::Socks5)
    );
    assert_eq!(
        combine(SocksSniff::NotSocks, HttpSniff::Ok(ProxyProtocol::Http)),
        Detected::Ok(ProxyProtocol::Http)
    );
    // 要認證的也算識別成功，只是不支援；徽章仍要標得出協定
    assert_eq!(
        combine(SocksSniff::NeedsAuth(ProxyProtocol::Socks5), HttpSniff::NotHttp),
        Detected::NeedsAuth(ProxyProtocol::Socks5)
    );
    let msg = detect_message(&Detected::NeedsAuth(ProxyProtocol::Socks5));
    assert!(msg.to_ascii_lowercase().contains("auth"), "要認證的訊息要說得出是認證問題：{msg}");
}

/// W8.14 招呼位元組本身：`05 01 00`，不多不少
#[test]
fn the_greeting_is_exactly_three_bytes() {
    assert_eq!(SOCKS5_GREETING, [0x05u8, 0x01, 0x00]);
    assert_eq!(SOCKS5_GREETING.len(), 3);
}

// --------------------------------------------------------- 走真 socket（M）

/// 綁一個 loopback 埠，每一條進來的連線交給 `handle`。回傳埠號。
///
/// `handle` 會被呼叫多次——識別器第一步失敗之後**必須開新連線**再試第二步
/// （W8.16），假伺服器要接得住第二條。
fn fake_server(handle: impl Fn(TcpStream) + Send + Sync + 'static) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(s) => handle(s),
                Err(_) => break,
            }
        }
    });
    port
}

/// 讀掉對方送來的東西（不看內容），再回一段固定位元組
fn replying_with(bytes: &'static [u8]) -> impl Fn(TcpStream) + Send + Sync + 'static {
    move |mut s: TcpStream| {
        let mut buf = [0u8; 1024];
        let _ = s.read(&mut buf);
        let _ = s.write_all(bytes);
        let _ = s.flush();
    }
}

/// 一個保證沒人在聽的埠：綁完立刻放掉
fn dead_port() -> u16 {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// W8.15 假伺服器回 `05 00`
#[test]
fn detect_finds_socks5_on_the_wire() {
    let port = fake_server(replying_with(&[0x05, 0x00]));
    assert_eq!(detect(port), Detected::Ok(ProxyProtocol::Socks5));
}

/// W8.16 假伺服器回 HTTP：第一步失敗後**真的有重連再試第二步**。
///
/// SOCKS5 的招呼已經污染了那條連線，接著在同一條上送 CONNECT 只會拿到垃圾，
/// 所以第二步一定要開新連線——這條測試靠「連線計數 ≥ 2」把它釘住。
#[test]
fn detect_reconnects_before_trying_http() {
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = seen.clone();
    let port = fake_server(move |mut s: TcpStream| {
        counter.fetch_add(1, Ordering::SeqCst);
        let mut buf = [0u8; 1024];
        let _ = s.read(&mut buf);
        let _ = s.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
        let _ = s.flush();
    });
    assert_eq!(detect(port), Detected::Ok(ProxyProtocol::Http));
    assert!(seen.load(Ordering::SeqCst) >= 2, "第二步必須用一條乾淨的新連線");
}

/// W8.17 收下招呼後不回應：3 秒後往下試 HTTP，再逾時就 NotAProxy，總耗時 < 7 秒
#[test]
fn a_silent_server_falls_through_both_steps_within_seven_seconds() {
    let port = fake_server(|s: TcpStream| {
        // 收下就不回話，也不關連線——把 socket 留著才是「靜默」
        std::thread::sleep(Duration::from_secs(8));
        drop(s);
    });
    let started = Instant::now();
    assert_eq!(detect(port), Detected::NotAProxy);
    assert!(started.elapsed() < Duration::from_secs(7), "兩步的逾時加起來要收在 7 秒內");
}

/// W8.18 假伺服器立刻關閉連線
#[test]
fn a_server_that_hangs_up_immediately_is_not_a_proxy() {
    let port = fake_server(|s: TcpStream| drop(s));
    assert_eq!(detect(port), Detected::NotAProxy);
}

/// W8.19 埠上沒有任何東西在聽：連不上也算不像代理，而且不重試到天荒地老
#[test]
fn a_dead_port_is_not_a_proxy_and_returns_quickly() {
    let started = Instant::now();
    assert_eq!(detect(dead_port()), Detected::NotAProxy);
    assert!(started.elapsed() < DETECT_TIMEOUT * 3, "連不上就該立刻收手");
}

/// W8.20 回一堆垃圾二進位
#[test]
fn binary_garbage_is_not_a_proxy() {
    let port = fake_server(replying_with(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03]));
    assert_eq!(detect(port), Detected::NotAProxy);
}

/// W8.21 識別成 SOCKS5 之後，`probe` 用的仍是 `socks5h://`。
///
/// 沿用 exits.rs 那兩條釘樁測試的骨架：DNS 一定要走代理，不可以退回 `socks5://`。
#[test]
fn a_socks5_probe_still_hands_the_hostname_to_the_proxy() {
    let port = fake_server(replying_with(&[0x05, 0x00]));
    assert_eq!(detect(port), Detected::Ok(ProxyProtocol::Socks5));
    let url = proxy_url_of(port, ProxyProtocol::Socks5);
    assert_eq!(url, format!("socks5h://127.0.0.1:{port}"));
    let proxy = ureq::Proxy::new(&url).unwrap();
    assert_eq!(proxy.protocol(), ureq::ProxyProtocol::Socks5h);
    assert!(!proxy.resolve_target(), "主機名要交給代理解析，不能在本機先解 DNS");
}

/// W8.22 `probe(port, Http)` 的 scheme 是 `http://127.0.0.1:{port}`
#[test]
fn an_http_probe_uses_the_http_scheme() {
    assert_eq!(proxy_url_of(1086, ProxyProtocol::Http), "http://127.0.0.1:1086");
    assert!(ureq::Proxy::new(&proxy_url_of(1086, ProxyProtocol::Http)).is_ok());
}

// ------------------------------------------------------- 排程與快取（F）

/// W8.23 `should_probe` × `needs_detect` 對 §1.3 的五態
#[test]
fn the_five_row_types_get_the_right_schedule() {
    use crate::config::{needs_detect, should_probe, RowKind};
    // ① ssh forward、③ wg forward：連排程都不進
    assert!(!should_probe(RowKind::Forward, false));
    // ② ssh forward + probeProxy、④ wg forward + probeProxy：進排程，而且要識別
    assert!(should_probe(RowKind::Forward, true));
    assert!(needs_detect(RowKind::Forward));
    // ⑤ wg socks：進排程，但**不識別**（協定已知）
    assert!(should_probe(RowKind::Socks, false));
    assert!(!needs_detect(RowKind::Socks));
}

/// W8.24 快取命中：第二次不再呼叫 `detect`
#[test]
fn a_cache_hit_skips_the_detector() {
    use crate::config::RowKind;
    // socks 列協定已知，連快取都不必查
    assert_eq!(resolve_protocol(RowKind::Socks, None), Resolution::Known(ProxyProtocol::Socks5));
    // forward + 尚未識別：這一輪要跑
    assert_eq!(resolve_protocol(RowKind::Forward, None), Resolution::MustDetect);

    DETECT_CALLS.store(0, Ordering::SeqCst);
    let cached = Some(ProxyProtocol::Http);
    assert_eq!(resolve_protocol(RowKind::Forward, cached), Resolution::Cached(ProxyProtocol::Http));
    assert_eq!(resolve_protocol(RowKind::Forward, cached), Resolution::Cached(ProxyProtocol::Http));
    assert_eq!(DETECT_CALLS.load(Ordering::SeqCst), 0, "命中快取就不可以再打一次識別");
}

/// W8.25 `clear_exit_test` 之後快取被清掉，下一次重新識別
#[test]
fn clearing_the_test_also_clears_the_protocol_cache() {
    use crate::config::RowKind;
    use crate::state::{cleared_test_state, TestView};
    let (test, detected) = cleared_test_state(
        Some(TestView::plain("ok", "1.2.3.4  Taipei, TW")),
        Some(ProxyProtocol::Socks5),
    );
    assert!(test.is_none(), "自測顯示要清掉");
    assert!(detected.is_none(), "協定快取的作廢時機與自測憑證完全一致");
    // 清掉之後下一輪就得重新識別
    assert_eq!(resolve_protocol(RowKind::Forward, detected), Resolution::MustDetect);
}

/// W8.26 快取**不落設定檔**：跑完識別之後存出來的檔案裡沒有任何協定痕跡
#[test]
fn the_protocol_cache_never_reaches_the_config_file() {
    use crate::config::{write_config_at, Config, Forward, RowKind, Source, TOML_NAME};
    let dir =
        std::env::temp_dir().join(format!("traytunnel-w826-{}-{}", std::process::id(), line!()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![Source {
            name: "hk".into(),
            host: "h.example.com".into(),
            user: "bob".into(),
            proxy_command: String::new(),
            enabled: true,
            forwards: vec![Forward {
                name: "exit-a".into(),
                local: 1080,
                remote: Some("127.0.0.1:1080".into()),
                kind: RowKind::Forward,
                probe_proxy: true,
                enabled: true,
            }],
        }],
        wg_proxies: Vec::new(),
    };
    let path = dir.join(TOML_NAME);
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    for key in ["protocol", "detected", "socks5h"] {
        assert!(!saved.contains(key), "設定檔不可以留下觀察到的協定（{key}）：{saved}");
    }
    // 使用者填的那一個布林當然還在
    assert!(saved.contains("probeProxy = true"), "{saved}");
}

/// W8.27 `TestView` 的 serde 輸出：有結果時帶 `protocol`，識別失敗時該鍵不存在
#[test]
fn test_view_only_carries_a_protocol_when_there_is_one() {
    use crate::state::TestView;
    let with = TestView {
        state: "ok".into(),
        text: "1.2.3.4  Taipei, TW".into(),
        protocol: Some(ProxyProtocol::Http.as_str().into()),
    };
    let v = serde_json::to_value(&with).unwrap();
    assert_eq!(v["protocol"], "http");

    let without = TestView::plain("fail", NOT_A_PROXY_TEXT);
    let v = serde_json::to_value(&without).unwrap();
    assert!(v.get("protocol").is_none(), "識別不出來就不可以送一個空字串假裝有徽章：{v}");
}
