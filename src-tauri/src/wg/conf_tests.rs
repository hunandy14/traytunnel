//! `wg::conf` 的測試——設計書 §5 的 W1 系列（31 條，全部 F）。
//!
//! 用 `#[path]` 掛回 conf.rs，慣例與 `config_tests.rs` 相同：生產碼與測試各佔
//! 一個檔案，模組路徑仍是 `conf::tests`，`use super::*;` 拿得到私有項。
//!
//! **金鑰一律是自造的測試常數**（`[0x01; 32]` 之類），這裡不碰任何真實 `.conf`。

use super::*;
use base64::Engine as _;

const PRIV: [u8; 32] = [0x01; 32];
const PUB: [u8; 32] = [0x02; 32];
const PSK: [u8; 32] = [0x03; 32];

fn b64(bytes: [u8; 32]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn net(addr: &str, prefix: u8) -> IpNet {
    IpNet { addr: addr.parse().unwrap(), prefix }
}

/// 最小合法檔：Interface 只有 PrivateKey+Address，Peer 只有 PublicKey+Endpoint
fn minimal() -> String {
    format!(
        "[Interface]\nPrivateKey = {}\nAddress = 10.9.0.1/32\n\n\
         [Peer]\nPublicKey = {}\nEndpoint = vpn.example.com:51820\n",
        b64(PRIV),
        b64(PUB)
    )
}

/// 在最小合法檔的 `[Interface]` 段尾追加一行
fn with_interface_line(line: &str) -> String {
    format!(
        "[Interface]\nPrivateKey = {}\nAddress = 10.9.0.1/32\n{line}\n\n\
         [Peer]\nPublicKey = {}\nEndpoint = vpn.example.com:51820\n",
        b64(PRIV),
        b64(PUB)
    )
}

/// 在最小合法檔的 `[Peer]` 段尾追加一行
fn with_peer_line(line: &str) -> String {
    format!(
        "[Interface]\nPrivateKey = {}\nAddress = 10.9.0.1/32\n\n\
         [Peer]\nPublicKey = {}\nEndpoint = vpn.example.com:51820\n{line}\n",
        b64(PRIV),
        b64(PUB)
    )
}

/// W1.1 最小合法檔的每一個預設值
#[test]
fn minimal_conf_fills_every_default() {
    let c = parse(&minimal()).expect("最小合法檔要解析得過");
    assert_eq!(c.mtu, DEFAULT_MTU);
    assert_eq!(c.listen_port, 0);
    assert_eq!(c.keepalive, None);
    assert_eq!(c.preshared_key, None);
    assert!(c.dns.is_empty(), "沒寫 DNS 就是空的，不可以自己補一個");
    // 省略 AllowedIPs 視為全開
    assert_eq!(c.allowed_ips, vec![net("0.0.0.0", 0), net("::", 0)]);
    assert_eq!(c.addresses, vec![net("10.9.0.1", 32)]);
    assert_eq!(c.endpoint, "vpn.example.com:51820");
    assert_eq!(c.peer_public_key, PUB);
}

/// W1.2 使用者那份檔案的結構（值全部自造），每個欄位都要落在正確的位置
#[test]
fn a_full_wg_quick_conf_lands_in_the_right_fields() {
    let raw = format!(
        "[Interface]\nPrivateKey = {}\nAddress = 10.9.0.2/24\nDNS = 10.9.0.1\n\n\
         [Peer]\nPublicKey = {}\nAllowedIPs = 0.0.0.0/0, ::/0\n\
         Endpoint = gw.example.net:51820\nPersistentKeepalive = 25\n",
        b64(PRIV),
        b64(PUB)
    );
    let c = parse(&raw).unwrap();
    assert_eq!(c.addresses, vec![net("10.9.0.2", 24)]);
    assert_eq!(c.dns, vec!["10.9.0.1".parse::<IpAddr>().unwrap()]);
    assert_eq!(c.allowed_ips, vec![net("0.0.0.0", 0), net("::", 0)]);
    assert_eq!(c.endpoint, "gw.example.net:51820");
    assert_eq!(c.keepalive, Some(25));
    assert_eq!(c.peer_public_key, PUB);
}

/// W1.3 區段名大小寫不敏感
#[test]
fn section_names_are_case_insensitive() {
    for (i, p) in [("[interface]", "[peer]"), ("[INTERFACE]", "[PEER]"), ("[Interface]", "[Peer]")]
    {
        let raw = format!(
            "{i}\nPrivateKey = {}\nAddress = 10.9.0.1/32\n\n{p}\nPublicKey = {}\nEndpoint = h:51820\n",
            b64(PRIV),
            b64(PUB)
        );
        assert!(parse(&raw).is_ok(), "{i}/{p} 要認得");
    }
}

/// W1.4 鍵名大小寫不敏感（值仍然大小寫敏感）
#[test]
fn key_names_are_case_insensitive() {
    for key in ["privatekey", "PRIVATEKEY", "PrivateKey"] {
        let raw = format!(
            "[Interface]\n{key} = {}\nAddress = 10.9.0.1/32\n\n[Peer]\nPublicKey = {}\nEndpoint = h:51820\n",
            b64(PRIV),
            b64(PUB)
        );
        assert!(parse(&raw).is_ok(), "{key} 要認得");
    }
}

/// W1.5 `=` 兩側與行首行尾的空白、tab、CRLF 行尾
#[test]
fn whitespace_tabs_and_crlf_are_trimmed() {
    let raw = format!(
        "[Interface]\r\n\tPrivateKey\t=\t{}  \r\n  Address =  10.9.0.1/32 \r\n\r\n\
         [Peer]\r\nPublicKey\t= {}\r\n  Endpoint = h:51820  \r\n",
        b64(PRIV),
        b64(PUB)
    );
    let c = parse(&raw).unwrap();
    assert_eq!(c.addresses, vec![net("10.9.0.1", 32)]);
    assert_eq!(c.endpoint, "h:51820");
}

/// W1.6 整行 `#`／`;` 註解與空行一律跳過
#[test]
fn whole_line_comments_and_blank_lines_are_skipped() {
    let raw = format!(
        "# 這是註解\n\n; 這也是\n[Interface]\n   # 縮排的註解\nPrivateKey = {}\n\
         Address = 10.9.0.1/32\n\n[Peer]\nPublicKey = {}\nEndpoint = h:51820\n",
        b64(PRIV),
        b64(PUB)
    );
    let c = parse(&raw).unwrap();
    assert!(c.warnings.is_empty(), "註解不該產生警告");
    assert_eq!(c.endpoint, "h:51820");
}

/// W1.7 行內註解是壞值，不是可以靜靜吃掉的東西——wg-quick 也不吃
#[test]
fn inline_comments_are_an_error_not_a_silent_trim() {
    let raw = with_peer_line("").replace("Endpoint = h:51820", "Endpoint = h:51820 # 註解");
    let err = parse(&raw).expect_err("行內註解要判壞值");
    assert!(err.to_ascii_lowercase().contains("endpoint"), "訊息要點名 Endpoint：{err}");
}

/// W1.8 缺 `[Interface]`／缺 `[Peer]`／兩個 `[Peer]`
#[test]
fn interface_and_peer_must_appear_exactly_once() {
    let no_iface =
        format!("[Peer]\nPublicKey = {}\nEndpoint = h:51820\n", b64(PUB));
    assert!(parse(&no_iface).is_err(), "缺 [Interface] 要錯");

    let no_peer = format!("[Interface]\nPrivateKey = {}\nAddress = 10.9.0.1/32\n", b64(PRIV));
    assert!(parse(&no_peer).is_err(), "缺 [Peer] 要錯");

    let two_peers = format!(
        "{}\n[Peer]\nPublicKey = {}\nEndpoint = h2:51820\n",
        minimal(),
        b64([0x04; 32])
    );
    let err = parse(&two_peers).expect_err("兩個 [Peer] 要錯");
    assert!(err.contains("peer"), "訊息要說明 v1 只支援單一 peer：{err}");
}

/// W1.9 缺 PrivateKey／PublicKey／Endpoint 各自要有明確錯誤
#[test]
fn missing_required_keys_are_reported_by_name() {
    for (line, needle) in [
        (format!("PrivateKey = {}\n", b64(PRIV)), "privatekey"),
        (format!("PublicKey = {}\n", b64(PUB)), "publickey"),
        ("Endpoint = vpn.example.com:51820\n".to_string(), "endpoint"),
    ] {
        let raw = minimal().replace(&line, "");
        let err = parse(&raw).expect_err(&format!("缺 {needle} 要錯"));
        assert!(err.to_ascii_lowercase().contains(needle), "訊息要點名 {needle}：{err}");
    }
}

/// W1.10 金鑰格式：非 base64／長度不對要錯，且錯誤訊息不得含輸入的任何片段
#[test]
fn key_errors_never_echo_the_input() {
    let bad = "這不是base64!!!";
    let raw = minimal().replace(&b64(PRIV), bad);
    let err = parse(&raw).expect_err("非 base64 的金鑰要錯");
    assert!(!err.contains(bad), "錯誤訊息不得回放輸入：{err}");

    let short = base64::engine::general_purpose::STANDARD.encode([0x01u8; 16]);
    let raw = minimal().replace(&b64(PRIV), &short);
    let err = parse(&raw).expect_err("解碼後不是 32 位元組要錯");
    assert!(!err.contains(&short), "錯誤訊息不得回放輸入：{err}");

    // 多餘空白 trim 後要成功
    let padded = format!("  {}  ", b64(PRIV));
    let raw = minimal().replace(&b64(PRIV), &padded);
    assert!(parse(&raw).is_ok(), "金鑰兩側的空白要 trim 掉");
}

/// W1.11 `Address = 10.0.0.2`（無前綴）補成 /32
#[test]
fn a_bare_v4_address_gets_a_slash_32() {
    let raw = minimal().replace("Address = 10.9.0.1/32", "Address = 10.0.0.2");
    assert_eq!(parse(&raw).unwrap().addresses, vec![net("10.0.0.2", 32)]);
}

/// W1.12 `Address = fd00::2`（無前綴）補成 /128
#[test]
fn a_bare_v6_address_gets_a_slash_128() {
    let raw = minimal().replace("Address = 10.9.0.1/32", "Address = fd00::2");
    assert_eq!(parse(&raw).unwrap().addresses, vec![net("fd00::2", 128)]);
}

/// W1.13 多筆位址：前綴照收，只記錄不影響介面掛載（D2）
#[test]
fn multiple_addresses_keep_their_prefixes() {
    let raw = minimal().replace("Address = 10.9.0.1/32", "Address = 10.0.0.2/24, fd00::2/64");
    assert_eq!(parse(&raw).unwrap().addresses, vec![net("10.0.0.2", 24), net("fd00::2", 64)]);
}

/// W1.14 `DNS` 逗號分隔的兩筆 IP
#[test]
fn dns_takes_a_comma_separated_list() {
    let c = parse(&with_interface_line("DNS = 10.0.0.1, 1.1.1.1")).unwrap();
    assert_eq!(
        c.dns,
        vec!["10.0.0.1".parse::<IpAddr>().unwrap(), "1.1.1.1".parse::<IpAddr>().unwrap()]
    );
}

/// W1.15 `DNS` 裡的搜尋網域（wg-quick 允許）收 IP、跳過網域並警告
#[test]
fn dns_search_domains_are_skipped_with_a_warning() {
    let c = parse(&with_interface_line("DNS = 10.0.0.1, example.com")).unwrap();
    assert_eq!(c.dns, vec!["10.0.0.1".parse::<IpAddr>().unwrap()]);
    assert!(
        c.warnings.iter().any(|w| w.contains("example.com")),
        "被跳過的搜尋網域要留一行警告：{:?}",
        c.warnings
    );
}

/// W1.16 `AllowedIPs` 全開／單一前綴／缺鍵
#[test]
fn allowed_ips_handles_open_single_and_missing() {
    let open = parse(&with_peer_line("AllowedIPs = 0.0.0.0/0, ::/0")).unwrap();
    assert_eq!(open.allowed_ips, vec![net("0.0.0.0", 0), net("::", 0)]);

    let single = parse(&with_peer_line("AllowedIPs = 10.0.0.0/24")).unwrap();
    assert_eq!(single.allowed_ips, vec![net("10.0.0.0", 24)]);

    // 缺鍵視為全開
    assert_eq!(parse(&minimal()).unwrap().allowed_ips, vec![net("0.0.0.0", 0), net("::", 0)]);
}

/// W1.17 `AllowedIPs` 缺前綴是錯誤（與 wg 一致）
#[test]
fn allowed_ips_require_a_prefix() {
    assert!(parse(&with_peer_line("AllowedIPs = 10.0.0.1")).is_err());
}

/// W1.18 `MTU` 合法／越界／格式錯
#[test]
fn mtu_is_range_checked() {
    assert_eq!(parse(&with_interface_line("MTU = 1280")).unwrap().mtu, 1280);
    assert!(MTU_RANGE.contains(&1280));
    assert!(parse(&with_interface_line("MTU = 100")).is_err(), "低於下限要錯");
    assert!(parse(&with_interface_line("MTU = 99999")).is_err(), "高於上限要錯");
    assert!(parse(&with_interface_line("MTU = abc")).is_err(), "非數字要錯");
}

/// W1.19 `PersistentKeepalive` 的三種寫法
#[test]
fn persistent_keepalive_zero_means_none() {
    assert_eq!(parse(&with_peer_line("PersistentKeepalive = 25")).unwrap().keepalive, Some(25));
    assert_eq!(parse(&with_peer_line("PersistentKeepalive = 0")).unwrap().keepalive, None);
    assert_eq!(parse(&minimal()).unwrap().keepalive, None);
}

/// W1.20 `PresharedKey` 有與無
#[test]
fn preshared_key_is_optional() {
    let with = parse(&with_peer_line(&format!("PresharedKey = {}", b64(PSK)))).unwrap();
    assert_eq!(with.preshared_key, Some(PSK));
    assert_eq!(parse(&minimal()).unwrap().preshared_key, None);
}

/// W1.21 `ListenPort`
#[test]
fn listen_port_is_read_from_the_interface_section() {
    assert_eq!(parse(&with_interface_line("ListenPort = 51820")).unwrap().listen_port, 51820);
}

/// W1.22 wg-quick 的主機路由／腳本鍵：認得、跳過、記一行，**永不執行**
#[test]
fn wg_quick_script_hooks_are_recognised_but_never_carried() {
    const MARKER: &str = "echo-pwned-marker";
    let hooks = format!(
        "Table = off\nFwMark = 0x1234\nSaveConfig = true\n\
         PreUp = {MARKER}\nPostUp = {MARKER}\nPreDown = {MARKER}\nPostDown = {MARKER}"
    );
    let c = parse(&with_interface_line(&hooks)).expect("這些鍵是接受但忽略，不是錯誤");
    for key in ["Table", "FwMark", "SaveConfig", "PreUp", "PostUp", "PreDown", "PostDown"] {
        assert!(
            c.warnings.iter().any(|w| w.contains(key)),
            "{key} 要各留一行警告：{:?}",
            c.warnings
        );
    }
    // WgConf 裡沒有任何欄位承載它們的值——不可能被誰拿去執行
    let dumped = format!("{c:?}");
    assert!(!dumped.contains(MARKER), "腳本內容不可以被任何欄位帶著走：{dumped}");
}

/// W1.23 wireproxy 的 `[Socks5]` 段跳過，警告要說清楚監聽埠以 traytunnel 為準
#[test]
fn wireproxy_socks5_section_is_skipped_with_an_explicit_warning() {
    let raw = format!("{}\n[Socks5]\nBindAddress = 127.0.0.1:25344\n", minimal());
    let c = parse(&raw).expect("wireproxy 擴充段是容忍跳過，不是錯誤");
    let w = c.warnings.join("\n");
    assert!(w.contains("Socks5"), "警告要點名 [Socks5]：{w}");
    assert!(w.contains("traytunnel"), "警告要說明監聽埠以 traytunnel 設定為準：{w}");
}

/// W1.24 其餘 wireproxy 擴充段各自跳過並警告
#[test]
fn every_other_wireproxy_section_is_skipped_with_a_warning() {
    let sections =
        ["TCPClientTunnel", "TCPServerTunnel", "STDIOTunnel", "http", "SNI", "Resolve", "UDPProxyTunnel"];
    let mut raw = minimal();
    for s in sections {
        raw.push_str(&format!("\n[{s}]\nWhatever = 1\n"));
    }
    let c = parse(&raw).unwrap();
    for s in sections {
        assert!(c.warnings.iter().any(|w| w.contains(s)), "[{s}] 要留一行警告：{:?}", c.warnings);
    }
}

/// W1.25 根層（無區段）的 `WGConfig`：跳過並警告，**不遞迴載入外部檔案**
#[test]
fn a_root_level_key_is_skipped_and_never_loads_another_file() {
    let raw = format!("WGConfig = other.conf\n{}", minimal());
    let c = parse(&raw).unwrap();
    assert!(
        c.warnings.iter().any(|w| w.contains("WGConfig")),
        "區段之前的鍵要留一行警告：{:?}",
        c.warnings
    );
    assert_eq!(c.endpoint, "vpn.example.com:51820", "不可以去讀 other.conf");
}

/// W1.26 完全不認得的區段與鍵：向前相容，跳過並警告即可
#[test]
fn unknown_sections_and_keys_are_forward_compatible() {
    let raw = format!("{}\n[Future]\nWhatever = 1\n", with_interface_line("SomethingNew = 1"));
    let c = parse(&raw).expect("不認得的東西不該讓整份檔案報錯");
    assert!(c.warnings.iter().any(|w| w.contains("Future")));
    assert!(c.warnings.iter().any(|w| w.contains("SomethingNew")));
}

/// W1.27 `Endpoint` 只做形狀檢查，主機名留到重連前才解析
#[test]
fn endpoint_is_shape_checked_but_never_resolved() {
    for good in ["h:51820", "1.2.3.4:51820", "[fd00::1]:51820"] {
        let raw = minimal().replace("vpn.example.com:51820", good);
        let c = parse(&raw).unwrap_or_else(|e| panic!("{good} 應該過：{e}"));
        assert_eq!(c.endpoint, good, "主機名不可以在解析階段就被解掉");
    }
    for bad in ["h", "h:0", "h:99999"] {
        let raw = minimal().replace("vpn.example.com:51820", bad);
        assert!(parse(&raw).is_err(), "{bad} 應該錯");
    }
}

/// W1.28 UTF-8 BOM（`config.rs::strip_bom` 已有先例）
#[test]
fn a_utf8_bom_is_stripped() {
    let raw = format!("\u{feff}{}", minimal());
    assert!(parse(&raw).is_ok(), "BOM 要剝掉");
}

/// W1.29 `SecretKey` 的 Debug 不得洩漏任何金鑰位元組
#[test]
fn secret_key_debug_is_a_fixed_redacted_string() {
    let key = SecretKey(boringtun::x25519::StaticSecret::from(PRIV));
    let shown = format!("{key:?}");
    assert_eq!(shown, REDACTED);
    assert!(!shown.contains(&b64(PRIV)), "不得出現 base64 表示");
    assert!(!shown.contains("01010101"), "不得出現十六進位表示");
}

/// W1.30 `ConfSummary` 的 serde 輸出不得有金鑰欄位
#[test]
fn conf_summary_never_serialises_a_key() {
    let c = parse(&minimal()).unwrap();
    let v = serde_json::to_value(c.summary()).unwrap();
    let obj = v.as_object().expect("摘要要是一個物件");
    assert!(!obj.contains_key("privateKey"));
    assert!(!obj.contains_key("presharedKey"));
    // 連值都不可以夾帶
    assert!(!v.to_string().contains(&b64(PRIV)));
}

/// W1.31 洩漏防線的 grep 型測試：`wg/` 底下除 device.rs 的 `resolve_endpoint`
/// 之外，資料路徑上不得出現對系統解析器的呼叫（設計書 §2.2 防線一）
#[test]
fn no_system_resolver_anywhere_but_device_resolve_endpoint() {
    const SOURCES: [(&str, &str); 5] = [
        ("conf.rs", include_str!("conf.rs")),
        ("dns.rs", include_str!("dns.rs")),
        ("engine.rs", include_str!("engine.rs")),
        ("socks5.rs", include_str!("socks5.rs")),
        ("stack.rs", include_str!("stack.rs")),
    ];
    const FORBIDDEN: [&str; 3] = ["to_socket_addrs", "lookup_host", "ToSocketAddrs"];
    for (name, body) in SOURCES {
        for needle in FORBIDDEN {
            assert!(
                !body.contains(needle),
                "{name} 出現了 {needle}：socks5h 的語意要求隧道內的名字一律由隧道內的 DNS 解析，\
                 唯一允許呼叫系統解析器的地方是 device.rs 的 resolve_endpoint"
            );
        }
    }
}
