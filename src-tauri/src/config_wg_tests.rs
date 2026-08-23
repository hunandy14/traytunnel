//! `config` 的 wg 代理測試——設計書 §5 的 W3 系列（19 條，全部 F）。
//!
//! 與 `config_tests.rs` 同一層、同一個掛法（`#[path]`），只是把這一輪新加的
//! 測試隔成獨立檔，既有那份一千四百行的檔案這一輪只被允許補新欄位。

use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

fn tmp_dir(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let p = std::env::temp_dir()
        .join(format!("traytunnel-wgtest-{}-{}-{}", std::process::id(), tag, n));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn fwd(name: &str, local: u16) -> Forward {
    Forward { name: name.into(), local, remote: "127.0.0.1:1080".into(), enabled: true }
}

fn src(name: &str, forwards: Vec<Forward>) -> Source {
    Source {
        name: name.into(),
        host: format!("{name}.example.com"),
        user: "bob".into(),
        proxy_command: String::new(),
        forwards,
    }
}

fn proxy(name: &str, socks_port: u16, forwards: Vec<Forward>) -> WgProxy {
    WgProxy {
        name: name.into(),
        conf_path: format!("wg/{name}.conf"),
        socks_port,
        enabled: true,
        forwards,
    }
}

/// 一份 ssh + wg 都有的完整設定檔文字
const BOTH: &str = "\
closeToTray = true

[[sources]]
name = \"hk\"
host = \"hk.example.com\"
user = \"bob\"

  [[sources.forwards]]
  name = \"exit-a\"
  local = 1080
  remote = \"127.0.0.1:1080\"

[[wgProxies]]
name = \"ax4200\"
confPath = \"wg/ax4200.conf\"
socksPort = 1085

  [[wgProxies.forwards]]
  name = \"nas-ssh\"
  local = 2222
  remote = \"10.0.0.5:22\"
";

/// W3.1 沒有 wgProxies 的舊設定檔：照樣解析得過，也不可以觸發舊制遷移
#[test]
fn a_config_without_wg_proxies_still_parses_and_is_not_legacy() {
    let raw = "closeToTray = true\n\n[[sources]]\nname = \"hk\"\nhost = \"h\"\nuser = \"u\"\n";
    let (cfg, legacy) = parse_document(raw).unwrap();
    assert!(!legacy, "新增一個可選段落不可以讓舊檔被判成舊制");
    assert!(cfg.wg_proxies.is_empty());
}

/// W3.2 完整的 wgProxies 段落逐欄位對上，enabled 省略時是 true
#[test]
fn wg_proxies_and_their_forwards_are_parsed_field_by_field() {
    let cfg = parse_config(BOTH).unwrap();
    assert_eq!(cfg.wg_proxies.len(), 1);
    let p = &cfg.wg_proxies[0];
    assert_eq!(p.name, "ax4200");
    assert_eq!(p.conf_path, "wg/ax4200.conf");
    assert_eq!(p.socks_port, 1085);
    assert!(p.enabled, "省略 enabled 視為 true");
    assert_eq!(p.forwards.len(), 1);
    assert_eq!(p.forwards[0].name, "nas-ssh");
    assert_eq!(p.forwards[0].local, 2222);
    assert_eq!(p.forwards[0].remote, "10.0.0.5:22");
    assert!(p.forwards[0].enabled);
}

/// W3.3 `locals()` 是三者的聯集，順序照設定檔
#[test]
fn locals_covers_ssh_exits_socks_ports_and_wg_forwards() {
    let cfg = parse_config(BOTH).unwrap();
    assert_eq!(cfg.locals(), vec![1080, 1085, 2222]);
}

/// W3.4 `enabled_locals()`：wg 轉發只在代理也 enabled 時才算數
#[test]
fn a_wg_forward_is_only_enabled_when_its_proxy_is() {
    let mut cfg = Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![src("hk", vec![fwd("exit-a", 1080)])],
        wg_proxies: vec![proxy("ax4200", 1085, vec![fwd("nas-ssh", 2222)])],
    };
    assert_eq!(cfg.enabled_locals(), vec![1080, 1085, 2222]);

    cfg.wg_proxies[0].enabled = false;
    assert_eq!(cfg.enabled_locals(), vec![1080], "代理沒開，轉發不可能起得來");

    cfg.wg_proxies[0].enabled = true;
    cfg.wg_proxies[0].forwards[0].enabled = false;
    assert_eq!(cfg.enabled_locals(), vec![1080, 1085]);
}

/// W3.5 socksPort 撞到 ssh 出口的 local
#[test]
fn a_socks_port_clashing_with_an_ssh_exit_is_rejected() {
    let cfg = Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![src("hk", vec![fwd("exit-a", 1080)])],
        wg_proxies: vec![proxy("ax4200", 1080, vec![])],
    };
    let err = validate_config(&cfg).expect_err("跨種類撞埠要擋");
    assert!(err.contains("1080"), "訊息要點名埠號：{err}");
    assert!(err.contains("exit-a") && err.contains("ax4200"), "訊息要點名兩邊：{err}");
}

/// W3.6 兩顆代理的 socksPort 相同
#[test]
fn two_proxies_cannot_share_a_socks_port() {
    let cfg = Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![],
        wg_proxies: vec![proxy("a", 1085, vec![]), proxy("b", 1085, vec![])],
    };
    assert!(validate_config(&cfg).is_err());
}

/// W3.7 wg 轉發的 local 撞到自己代理的 socksPort
#[test]
fn a_wg_forward_cannot_reuse_its_own_socks_port() {
    let cfg = Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![],
        wg_proxies: vec![proxy("ax4200", 1085, vec![fwd("self", 1085)])],
    };
    assert!(validate_config(&cfg).is_err());
}

/// W3.8 代理名與某個 source 撞名（日誌前綴 `[名字]` 會撞）
#[test]
fn a_proxy_name_cannot_collide_with_a_source_name() {
    let cfg = Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![src("hk", vec![])],
        wg_proxies: vec![proxy("hk", 1085, vec![])],
    };
    let err = validate_config(&cfg).expect_err("與源撞名要擋");
    assert!(err.contains("hk"), "{err}");
}

/// W3.9 代理名含空白／中括號／為空
#[test]
fn a_proxy_name_must_not_be_empty_or_contain_spaces_or_brackets() {
    for bad in ["", "  ", "two words", "a]b", "a[b"] {
        let cfg = Config {
            close_to_tray: true,
            check_for_updates: None,
            sources: vec![],
            wg_proxies: vec![proxy(bad, 1085, vec![])],
        };
        assert!(validate_config(&cfg).is_err(), "名字 {bad:?} 要擋");
        assert!(
            validate_wg_proxy(&cfg, None, bad, "wg/x.conf", 1085)
                .unwrap()
                .starts_with("name: "),
            "訊息要掛回 name 欄位：{bad:?}"
        );
    }
}

/// W3.10 confPath 為空白字串
#[test]
fn an_empty_conf_path_is_rejected() {
    let mut cfg = Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![],
        wg_proxies: vec![proxy("ax4200", 1085, vec![])],
    };
    cfg.wg_proxies[0].conf_path = "   ".into();
    assert!(validate_config(&cfg).is_err());
    assert!(validate_wg_proxy(&cfg, None, "ax4200", "   ", 1085)
        .unwrap()
        .starts_with("confPath: "));
}

/// W3.11 socksPort = 0
#[test]
fn socks_port_zero_is_rejected() {
    let cfg = Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![],
        wg_proxies: vec![proxy("ax4200", 0, vec![])],
    };
    assert!(validate_config(&cfg).is_err());
    assert!(validate_wg_proxy(&cfg, None, "ax4200", "wg/x.conf", 0)
        .unwrap()
        .starts_with("socksPort: "));
}

/// W3.12 撞埠訊息要分辨得出佔用者是 wg 代理還是 wg 轉發
#[test]
fn a_clash_message_says_whether_the_owner_is_a_proxy_or_a_forward() {
    let cfg = parse_config(BOTH).unwrap();
    let on_proxy = port_owner(&cfg, 1085).expect("1085 是代理的 socksPort");
    assert!(on_proxy.contains("ax4200"), "{on_proxy}");
    let on_forward = port_owner(&cfg, 2222).expect("2222 是 wg 轉發");
    assert!(on_forward.contains("nas-ssh"), "{on_forward}");
    assert_ne!(on_proxy, on_forward, "兩種佔用者的說法要分得開");
    // ssh 出口仍然認得
    assert!(port_owner(&cfg, 1080).unwrap().contains("exit-a"));
    assert!(port_owner(&cfg, 9999).is_none());
}

/// W3.13 寫回：`[[wgProxies]]` 上方的手寫註解要留在原位
#[test]
fn a_comment_above_a_proxy_survives_a_rewrite() {
    let dir = tmp_dir("wg-comment");
    let raw = format!("# 家裡的路由器\n{BOTH}");
    let path = dir.join(TOML_NAME);
    std::fs::write(&path, &raw).unwrap();
    let mut cfg = parse_config(&raw).unwrap();
    cfg.wg_proxies[0].socks_port = 1086;
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("# 家裡的路由器"), "註解不見了：{saved}");
    assert!(saved.contains("socksPort = 1086"), "{saved}");
}

/// W3.14 寫回：`[[wgProxies.forwards]]` 的註解（認 `local`）
#[test]
fn a_comment_above_a_wg_forward_survives_a_rewrite() {
    let dir = tmp_dir("wg-fwd-comment");
    let raw = BOTH.replace("  [[wgProxies.forwards]]", "  # NAS 的 ssh\n  [[wgProxies.forwards]]");
    let path = dir.join(TOML_NAME);
    std::fs::write(&path, &raw).unwrap();
    let mut cfg = parse_config(&raw).unwrap();
    cfg.wg_proxies[0].forwards[0].remote = "10.0.0.6:22".into();
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("# NAS 的 ssh"), "註解不見了：{saved}");
    assert!(saved.contains("10.0.0.6:22"), "{saved}");
}

/// W3.15 寫回：改代理的 name，註解消失但值正確（與 source 改名同一個刻意取捨）
#[test]
fn renaming_a_proxy_drops_its_comment_but_keeps_the_values() {
    let dir = tmp_dir("wg-rename");
    let raw = format!("# 家裡的路由器\n{BOTH}");
    let path = dir.join(TOML_NAME);
    std::fs::write(&path, &raw).unwrap();
    let mut cfg = parse_config(&raw).unwrap();
    cfg.wg_proxies[0].name = "ax6000".into();
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("name = \"ax6000\""), "{saved}");
    assert_eq!(parse_config(&saved).unwrap(), cfg);
}

/// W3.16 寫回：新增一顆代理，既有 `[[sources]]` 段落與註解一字不動
#[test]
fn adding_a_proxy_leaves_the_ssh_section_untouched() {
    let dir = tmp_dir("wg-add");
    let raw = "closeToTray = true\n\n# 香港那台\n[[sources]]\nname = \"hk\"\nhost = \"hk.example.com\"\nuser = \"bob\"\n";
    let path = dir.join(TOML_NAME);
    std::fs::write(path.as_path(), raw).unwrap();
    let mut cfg = parse_config(raw).unwrap();
    cfg.wg_proxies.push(proxy("ax4200", 1085, vec![]));
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("# 香港那台"), "{saved}");
    assert!(saved.contains("[[wgProxies]]"), "{saved}");
    assert_eq!(parse_config(&saved).unwrap(), cfg);
}

/// W3.17 讀寫來回：parse → write → parse 得到相等的 Config
#[test]
fn a_round_trip_through_the_file_is_lossless() {
    let dir = tmp_dir("wg-roundtrip");
    let path = dir.join(TOML_NAME);
    let cfg = parse_config(BOTH).unwrap();
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert_eq!(parse_config(&saved).unwrap(), cfg);
}

/// W3.18 `source_name_of` 對 socksPort 與 wg 轉發的 local 都回代理名
#[test]
fn source_name_of_knows_the_wg_ports_too() {
    let cfg = parse_config(BOTH).unwrap();
    assert_eq!(cfg.source_name_of(1085), Some("ax4200"), "日誌前綴要對得上");
    assert_eq!(cfg.source_name_of(2222), Some("ax4200"));
    assert_eq!(cfg.source_name_of(1080), Some("hk"), "ssh 那邊不可以被改壞");
}

/// W3.19 相對 confPath 以**設定檔所在資料夾**為基準，不是行程工作目錄
#[test]
fn a_relative_conf_path_is_resolved_against_the_config_dir() {
    let dir = tmp_dir("wg-relative");
    assert_eq!(resolve_conf_path(&dir, "wg/ax4200.conf"), dir.join("wg").join("ax4200.conf"));
    // 絕對路徑原樣回傳
    let abs = dir.join("elsewhere.conf");
    assert_eq!(resolve_conf_path(&dir, &abs.to_string_lossy()), abs);
    // 明確釘住「不是」以工作目錄為基準
    assert_ne!(
        resolve_conf_path(&dir, "wg/ax4200.conf"),
        std::env::current_dir().unwrap().join("wg").join("ax4200.conf")
    );
}
