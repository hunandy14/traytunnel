//! `config` 的 wg 與列分類法測試——設計書 §6 的 W3 系列（43 條，全部 F），
//! 檔尾續編 W3.46～W3.49：`WgProxy.mtu` 覆寫欄位（PM 裁決 2026-08-24）。
//!
//! 與 `config_tests.rs` 同一層、同一個掛法（`#[path]`），只是把這一輪新加的
//! 測試隔成獨立檔，既有那份一千四百行的檔案這一輪只被允許補新欄位。
//!
//! 本檔的領域模型基準是 §1「列分類法」：列由 `kind`（機制：`forward`／`socks`）
//! 與 `probeProxy`（語意：後端是不是代理）兩個**正交**維度描述，兩者在 SSH 與
//! WG 上是同一套規則。

use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

fn tmp_dir(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let p = std::env::temp_dir().join(format!(
        "traytunnel-wgtest-{}-{}-{}",
        std::process::id(),
        tag,
        n
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// ①③ 純轉發列：有 remote、不探測
fn fwd(name: &str, local: u16, remote: &str) -> Forward {
    Forward {
        name: name.into(),
        local,
        remote: Some(remote.into()),
        kind: RowKind::Forward,
        probe_proxy: false,
        enabled: true,
    }
}

/// ②④ 轉發列 + probeProxy：後端是別人跑的代理服務
fn probed(name: &str, local: u16, remote: &str) -> Forward {
    Forward { probe_proxy: true, ..fwd(name, local, remote) }
}

/// ⑤ socks 列：引擎自建的 SOCKS5 listener，沒有 remote、沒有 probeProxy
fn socks(name: &str, local: u16) -> Forward {
    Forward {
        name: name.into(),
        local,
        remote: None,
        kind: RowKind::Socks,
        probe_proxy: false,
        enabled: true,
    }
}

fn src(name: &str, forwards: Vec<Forward>) -> Source {
    Source {
        name: name.into(),
        host: format!("{name}.example.com"),
        user: "bob".into(),
        proxy_command: String::new(),
        enabled: true,
        forwards,
    }
}

fn proxy(name: &str, forwards: Vec<Forward>) -> WgProxy {
    WgProxy {
        name: name.into(),
        conf_path: format!("wg/{name}.conf"),
        enabled: true,
        mtu: None,
        forwards,
    }
}

fn cfg_of(sources: Vec<Source>, wg_proxies: Vec<WgProxy>) -> Config {
    Config { close_to_tray: true, check_for_updates: None, sources, wg_proxies }
}

/// 一列 upsert 的輸入
#[allow(clippy::too_many_arguments)]
fn input<'a>(
    connection: &'a str,
    conn_kind: ConnKind,
    orig: Option<u16>,
    name: &'a str,
    local: u16,
    remote: Option<&'a str>,
    kind: RowKind,
    probe_proxy: bool,
) -> RowInput<'a> {
    RowInput { connection, conn_kind, original_local: orig, name, local, remote, kind, probe_proxy }
}

/// 一份 ssh + wg 都有的完整設定檔文字：一條 socks 列 ⑤、一條 ④、一條 ③
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
  probeProxy = true

[[wgProxies]]
name = \"ax4200\"
confPath = \"wg/ax4200.conf\"

  [[wgProxies.forwards]]
  name = \"socks\"
  local = 1085
  kind = \"socks\"

  [[wgProxies.forwards]]
  name = \"corp\"
  local = 1086
  remote = \"10.0.0.9:1080\"
  probeProxy = true

  [[wgProxies.forwards]]
  name = \"nas-ssh\"
  local = 2222
  remote = \"10.0.0.5:22\"
";

// ------------------------------------------------------------ 基本設定模型

/// W3.1 沒有 wgProxies 的舊設定檔：照樣解析得過，也不可以觸發舊制遷移
#[test]
fn a_config_without_wg_proxies_still_parses_and_is_not_legacy() {
    let raw = "closeToTray = true\n\n[[sources]]\nname = \"hk\"\nhost = \"h\"\nuser = \"u\"\n";
    let (cfg, legacy) = parse_document(raw).unwrap();
    assert!(!legacy, "新增一個可選段落不可以讓舊檔被判成舊制");
    assert!(cfg.wg_proxies.is_empty());
}

/// W3.2 完整的 wgProxies 段落逐欄位對上，enabled 省略時 true，kind 與 probeProxy 正確
#[test]
fn wg_rows_are_parsed_field_by_field() {
    let cfg = parse_config(BOTH).unwrap();
    assert_eq!(cfg.wg_proxies.len(), 1);
    let p = &cfg.wg_proxies[0];
    assert_eq!(p.name, "ax4200");
    assert_eq!(p.conf_path, "wg/ax4200.conf");
    assert!(p.enabled, "省略 enabled 視為 true");
    assert_eq!(p.forwards.len(), 3);

    // ⑤ socks 列：沒有 remote、kind = socks
    assert_eq!(p.forwards[0].name, "socks");
    assert_eq!(p.forwards[0].local, 1085);
    assert_eq!(p.forwards[0].kind, RowKind::Socks);
    assert_eq!(p.forwards[0].remote, None, "引擎自建的 listener 沒有目的地");
    assert!(!p.forwards[0].probe_proxy, "socks 列不帶這個旗標");

    // ④ forward + probeProxy
    assert_eq!(p.forwards[1].kind, RowKind::Forward);
    assert_eq!(p.forwards[1].remote.as_deref(), Some("10.0.0.9:1080"));
    assert!(p.forwards[1].probe_proxy);

    // ③ 純轉發
    assert_eq!(p.forwards[2].kind, RowKind::Forward);
    assert_eq!(p.forwards[2].remote.as_deref(), Some("10.0.0.5:22"));
    assert!(!p.forwards[2].probe_proxy);
}

/// W3.3 `locals()` 是**所有連線所有列**的聯集，不分 kind，順序照設定檔
#[test]
fn locals_covers_every_row_of_every_connection() {
    let cfg = parse_config(BOTH).unwrap();
    assert_eq!(cfg.locals(), vec![1080, 1085, 1086, 2222]);
}

/// W3.4 `enabled_locals()`：wg 的列只在「連線 enabled 且列 enabled」時出現
#[test]
fn a_wg_row_is_only_enabled_when_its_connection_is() {
    let mut cfg = cfg_of(
        vec![src("hk", vec![fwd("exit-a", 1080, "127.0.0.1:1080")])],
        vec![proxy("ax4200", vec![socks("socks", 1085), fwd("nas", 2222, "10.0.0.5:22")])],
    );
    assert_eq!(cfg.enabled_locals(), vec![1080, 1085, 2222]);

    cfg.wg_proxies[0].enabled = false;
    assert_eq!(cfg.enabled_locals(), vec![1080], "連線沒開，列不可能起得來");

    cfg.wg_proxies[0].enabled = true;
    cfg.wg_proxies[0].forwards[1].enabled = false;
    assert_eq!(cfg.enabled_locals(), vec![1080, 1085]);
}

/// `enabled_ssh_locals()` **一條 wg 的列都不可以帶進來**。
///
/// 這是啟動自癒看門狗的準入名單。wg 的列沒有自己的監看迴圈（底下所有列都由
/// 引擎那一條代管），所以拿 wg 的埠去問「監看位子在不在」永遠是否——混進來
/// 的話看門狗每次啟動都會對同一批 wg 列誤報，還會拿 ssh 那一套動詞去踢它們。
#[test]
fn the_watchdogs_ssh_roster_never_contains_a_wg_row() {
    let mut cfg = cfg_of(
        vec![src("hk", vec![fwd("exit-a", 1080, "127.0.0.1:1080")])],
        vec![proxy("ax4200", vec![socks("socks", 1085), fwd("nas", 2222, "10.0.0.5:22")])],
    );
    assert_eq!(cfg.enabled_ssh_locals(), vec![1080]);
    // wg 那邊怎麼開關都不影響這份名單
    cfg.wg_proxies[0].enabled = false;
    assert_eq!(cfg.enabled_ssh_locals(), vec![1080]);
    // 停用 ssh 那一條才會讓它消失
    cfg.sources[0].forwards[0].enabled = false;
    assert!(cfg.enabled_ssh_locals().is_empty());
    // 而 enabled_locals() 仍然是「ssh 那一半 + wg 那一半」的聯集
    cfg.sources[0].forwards[0].enabled = true;
    cfg.wg_proxies[0].enabled = true;
    assert_eq!(cfg.enabled_locals(), vec![1080, 1085, 2222]);
}

/// W3.5 wg 的某條列撞到 ssh 出口的 local
#[test]
fn a_wg_row_clashing_with_an_ssh_exit_is_rejected() {
    let cfg = cfg_of(
        vec![src("hk", vec![fwd("exit-a", 1080, "127.0.0.1:1080")])],
        vec![proxy("ax4200", vec![socks("socks", 1080)])],
    );
    let err = validate_config(&cfg).expect_err("跨連線型撞埠要擋");
    assert!(err.contains("1080"), "訊息要點名埠號：{err}");
    assert!(err.contains("exit-a") && err.contains("ax4200"), "訊息要點名兩邊：{err}");
}

/// W3.6 兩條 wg 連線各有一條列撞同一個 local
#[test]
fn two_wg_connections_cannot_share_a_local_port() {
    let cfg = cfg_of(
        vec![],
        vec![proxy("a", vec![socks("s", 1085)]), proxy("b", vec![socks("s", 1085)])],
    );
    assert!(validate_config(&cfg).is_err());
}

/// W3.7 同一條 wg 連線的 socks 列與 forward 列撞同一個 local
#[test]
fn a_socks_row_and_a_forward_row_cannot_share_a_local_port() {
    let cfg = cfg_of(
        vec![],
        vec![proxy("ax4200", vec![socks("s", 1085), fwd("self", 1085, "10.0.0.5:22")])],
    );
    assert!(validate_config(&cfg).is_err());
}

/// W3.8 wg 連線名與某個 source 撞名（日誌前綴 `[名字]` 會撞）
#[test]
fn a_wg_name_cannot_collide_with_a_source_name() {
    let cfg = cfg_of(vec![src("hk", vec![])], vec![proxy("hk", vec![])]);
    let err = validate_config(&cfg).expect_err("與源撞名要擋");
    assert!(err.contains("hk"), "{err}");
}

/// W3.9 wg 連線名含空白／中括號／為空
#[test]
fn a_wg_name_must_not_be_empty_or_contain_spaces_or_brackets() {
    for bad in ["", "  ", "two words", "a]b", "a[b"] {
        let cfg = cfg_of(vec![], vec![proxy(bad, vec![])]);
        assert!(validate_config(&cfg).is_err(), "名字 {bad:?} 要擋");
        assert!(
            validate_wg_proxy(&cfg, None, bad, "wg/x.conf", None).unwrap().starts_with("name: "),
            "訊息要掛回 name 欄位：{bad:?}"
        );
    }
}

/// W3.10 confPath 為空白字串
#[test]
fn an_empty_conf_path_is_rejected() {
    let mut cfg = cfg_of(vec![], vec![proxy("ax4200", vec![])]);
    cfg.wg_proxies[0].conf_path = "   ".into();
    assert!(validate_config(&cfg).is_err());
    assert!(validate_wg_proxy(&cfg, None, "ax4200", "   ", None)
        .unwrap()
        .starts_with("confPath: "));
}

/// W3.11 某條列的 `local = 0`
#[test]
fn a_row_with_local_zero_is_rejected() {
    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![socks("s", 0)])]);
    let err = validate_config(&cfg).expect_err("0 不是可監聽的埠");
    assert!(err.contains("local"), "{err}");
}

/// W3.12 撞埠訊息要分辨得出佔用者是哪一條連線的**哪一種列**
#[test]
fn a_clash_message_says_which_connection_and_which_kind_of_row() {
    let cfg = parse_config(BOTH).unwrap();
    let on_socks = port_owner(&cfg, 1085).expect("1085 是 socks 列");
    assert!(on_socks.contains("ax4200") && on_socks.contains("socks"), "{on_socks}");
    let on_forward = port_owner(&cfg, 2222).expect("2222 是 wg 的 forward 列");
    assert!(on_forward.contains("ax4200") && on_forward.contains("nas-ssh"), "{on_forward}");
    assert_ne!(on_socks, on_forward, "兩種列的說法要分得開");
    // ssh 的列仍然認得
    assert!(port_owner(&cfg, 1080).unwrap().contains("exit-a"));
    assert!(port_owner(&cfg, 9999).is_none());
}

// ------------------------------------------------------------ 寫回

/// W3.13 寫回：`[[wgProxies]]` 上方的手寫註解要留在原位
#[test]
fn a_comment_above_a_wg_connection_survives_a_rewrite() {
    let dir = tmp_dir("wg-comment");
    let raw = format!("# 家裡的路由器\n{BOTH}");
    let path = dir.join(TOML_NAME);
    std::fs::write(&path, &raw).unwrap();
    let mut cfg = parse_config(&raw).unwrap();
    cfg.wg_proxies[0].conf_path = "wg/ax6000.conf".into();
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("# 家裡的路由器"), "註解不見了：{saved}");
    assert!(saved.contains("wg/ax6000.conf"), "{saved}");
}

/// W3.14 寫回：`[[wgProxies.forwards]]` 的註解（認 `local`）
#[test]
fn a_comment_above_a_wg_row_survives_a_rewrite() {
    let dir = tmp_dir("wg-row-comment");
    let raw = BOTH.replace(
        "  [[wgProxies.forwards]]\n  name = \"nas-ssh\"",
        "  # NAS 的 ssh\n  [[wgProxies.forwards]]\n  name = \"nas-ssh\"",
    );
    let path = dir.join(TOML_NAME);
    std::fs::write(&path, &raw).unwrap();
    let mut cfg = parse_config(&raw).unwrap();
    cfg.wg_proxies[0].forwards[2].remote = Some("10.0.0.6:22".into());
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("# NAS 的 ssh"), "註解不見了：{saved}");
    assert!(saved.contains("10.0.0.6:22"), "{saved}");
}

/// W3.15 寫回：改 wg 連線的 name，註解消失但值正確（與 source 改名同一個刻意取捨）
#[test]
fn renaming_a_wg_connection_drops_its_comment_but_keeps_the_values() {
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

/// W3.16 寫回：新增一條 wg 連線，既有 `[[sources]]` 段落與註解一字不動
#[test]
fn adding_a_wg_connection_leaves_the_ssh_section_untouched() {
    let dir = tmp_dir("wg-add");
    let raw = "closeToTray = true\n\n# 香港那台\n[[sources]]\nname = \"hk\"\nhost = \"hk.example.com\"\nuser = \"bob\"\n";
    let path = dir.join(TOML_NAME);
    std::fs::write(path.as_path(), raw).unwrap();
    let mut cfg = parse_config(raw).unwrap();
    cfg.wg_proxies.push(proxy("ax4200", vec![socks("socks", 1085)]));
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

/// W3.18 `source_name_of` 對兩種 wg 列都回所屬連線名（日誌前綴要對得上）
#[test]
fn source_name_of_knows_the_wg_rows_too() {
    let cfg = parse_config(BOTH).unwrap();
    assert_eq!(cfg.source_name_of(1085), Some("ax4200"), "socks 列");
    assert_eq!(cfg.source_name_of(2222), Some("ax4200"), "forward 列");
    assert_eq!(cfg.source_name_of(1080), Some("hk"), "ssh 那邊不可以被改壞");
    // 跨兩型的統一查詢也要指得出連線本身
    let (conn, row) = cfg.row(1086).expect("1086 是 wg 的 ④ 型列");
    assert_eq!(conn.name(), "ax4200");
    assert_eq!(conn.kind(), ConnKind::Wg);
    assert_eq!(row.name, "corp");
    assert_eq!(cfg.row(1080).unwrap().0.kind(), ConnKind::Ssh);
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

// -------------------------------------------------- 列分類法（§1）的驗證規則

/// W3.20 `forward` 列缺 `remote`（ssh 與 wg 各一）：皆錯誤，訊息前綴 `remote:`
#[test]
fn a_forward_row_without_a_remote_is_rejected_on_both_connection_kinds() {
    let cfg = cfg_of(vec![src("hk", vec![])], vec![proxy("ax4200", vec![])]);
    for (conn, conn_kind) in [("hk", ConnKind::Ssh), ("ax4200", ConnKind::Wg)] {
        let err = validate_forward(
            &cfg,
            &input(conn, conn_kind, None, "x", 1090, None, RowKind::Forward, false),
        )
        .unwrap_or_else(|| panic!("{conn}：kind = forward 時 remote 必填"));
        assert!(err.starts_with("remote: "), "訊息要掛回 remote 欄位：{err}");
    }
    // 結構上也要擋：手寫進檔案的同一筆是壞檔
    let bad = cfg_of(
        vec![],
        vec![proxy("ax4200", vec![Forward { remote: None, ..fwd("x", 1090, "10.0.0.5:22") }])],
    );
    assert!(validate_config(&bad).is_err());
}

/// W3.21 `socks` 列**帶** `remote`：引擎自建的 listener 沒有目的地可言
#[test]
fn a_socks_row_must_not_carry_a_remote() {
    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![])]);
    let err = validate_forward(
        &cfg,
        &input(
            "ax4200",
            ConnKind::Wg,
            None,
            "s",
            1085,
            Some("10.0.0.9:1080"),
            RowKind::Socks,
            false,
        ),
    )
    .expect("socks 列帶 remote 要擋");
    assert!(err.starts_with("remote: "), "{err}");

    let bad = cfg_of(
        vec![],
        vec![proxy(
            "ax4200",
            vec![Forward { remote: Some("10.0.0.9:1080".into()), ..socks("s", 1085) }],
        )],
    );
    assert!(validate_config(&bad).is_err());
}

/// W3.22 `socks` 列**帶** `probeProxy`：它恆測，這個欄位不得出現
#[test]
fn a_socks_row_must_not_carry_probe_proxy() {
    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![])]);
    let err = validate_forward(
        &cfg,
        &input("ax4200", ConnKind::Wg, None, "s", 1085, None, RowKind::Socks, true),
    )
    .expect("socks 列帶 probeProxy 要擋");
    assert!(err.starts_with("probeProxy: "), "訊息要掛回那顆 switch：{err}");

    let bad = cfg_of(
        vec![],
        vec![proxy("ax4200", vec![Forward { probe_proxy: true, ..socks("s", 1085) }])],
    );
    assert!(validate_config(&bad).is_err());
}

/// W3.23 **SSH 連線底下有 `socks` 列**：ssh 沒有自建代理這回事（§1.2 的機制表）
#[test]
fn an_ssh_connection_cannot_have_a_socks_row() {
    let cfg = cfg_of(vec![src("hk", vec![socks("s", 1085)])], vec![]);
    let err = validate_config(&cfg).expect_err("ssh 沒有自建代理這回事");
    assert!(err.contains("socks"), "訊息要說得出是 socks 列的問題：{err}");

    let empty = cfg_of(vec![src("hk", vec![])], vec![]);
    let err = validate_forward(
        &empty,
        &input("hk", ConnKind::Ssh, None, "s", 1085, None, RowKind::Socks, false),
    )
    .expect("從 IPC 那一側踩進來也要擋");
    assert!(err.starts_with("kind: "), "{err}");
}

/// W3.24 合法的 `socks` 列（只有 name／local，掛在 wg 底下）
#[test]
fn a_bare_socks_row_under_a_wg_connection_is_valid() {
    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![])]);
    let made = prepare_forward(
        &cfg,
        &input("ax4200", ConnKind::Wg, None, "socks", 1085, None, RowKind::Socks, false),
        true,
    )
    .expect("只有名稱與本地埠的 socks 列是合法的");
    assert_eq!(made.kind, RowKind::Socks);
    assert_eq!(made.remote, None);
    assert!(!made.probe_proxy);
}

/// W3.25 `should_probe`：§1.3 那張表的**唯一實作**，排程與 UI 都問它
#[test]
fn should_probe_is_the_only_implementation_of_the_five_row_table() {
    // ① ssh forward、③ wg forward：不測
    assert!(!should_probe(RowKind::Forward, false));
    // ② ssh forward + probeProxy、④ wg forward + probeProxy：測
    assert!(should_probe(RowKind::Forward, true));
    // ⑤ wg socks：恆測，與旗標無關
    assert!(should_probe(RowKind::Socks, false));
    assert!(should_probe(RowKind::Socks, true));
}

/// W3.26 `needs_detect`：`socks` 列協定已知，免識別
#[test]
fn only_forward_rows_need_protocol_detection() {
    assert!(needs_detect(RowKind::Forward));
    assert!(!needs_detect(RowKind::Socks));
}

// ------------------------------------------------------------ 遷移（§1.7）

/// W3.27 **遷移**：既有 `[[sources.forwards]]` 沒有 `probeProxy` 鍵時一律讀成 true。
///
/// 這是保住現役使用者出口 IP 顯示的那一條，最不能壞——今天每一條出口都會自動
/// 跑 ipinfo 檢測，遷移成 false 的話所有人的出口 IP 會一夜之間全部消失。
#[test]
fn an_old_forward_without_the_key_migrates_to_probe_proxy_true() {
    let raw = "[[sources]]\nname=\"hk\"\nhost=\"h\"\nuser=\"u\"\n\
               [[sources.forwards]]\nname=\"a\"\nlocal=1080\nremote=\"127.0.0.1:1080\"\n";
    let cfg = parse_config(raw).unwrap();
    assert!(cfg.forward(1080).unwrap().probe_proxy, "舊檔缺鍵一律補 true");

    // 明寫 false 的當然照使用者寫的算，遷移不可以蓋掉它
    let explicit =
        raw.replace("remote=\"127.0.0.1:1080\"\n", "remote=\"127.0.0.1:1080\"\nprobeProxy=false\n");
    assert!(!parse_config(&explicit).unwrap().forward(1080).unwrap().probe_proxy);
}

/// W3.28 遷移不碰 `kind`：舊檔缺鍵時就是 serde 預設的 `Forward`
#[test]
fn the_migration_never_touches_kind() {
    let cfg = parse_config(BOTH).unwrap();
    assert_eq!(cfg.forward(1080).unwrap().kind, RowKind::Forward);
    assert_eq!(cfg.wg_proxies[0].forwards[2].kind, RowKind::Forward, "缺 kind 鍵就是轉發");
    // socks 列明寫了 kind，不可以被掃成 Forward
    assert_eq!(cfg.wg_proxies[0].forwards[0].kind, RowKind::Socks);
}

/// W3.29 遷移後存檔：`probeProxy = true` 落檔，該筆上方的手寫註解仍在
#[test]
fn the_migrated_value_lands_in_the_file_and_keeps_the_comment() {
    let dir = tmp_dir("wg-migrate-write");
    let raw = "closeToTray = true\n\n[[sources]]\nname = \"hk\"\nhost = \"h\"\nuser = \"u\"\n\n\
               # 這條註解要活過遷移\n[[sources.forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\n";
    let path = dir.join(TOML_NAME);
    std::fs::write(&path, raw).unwrap();
    let cfg = parse_config(raw).unwrap();
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("probeProxy = true"), "遷移的值要落檔：{saved}");
    assert!(saved.contains("# 這條註解要活過遷移"), "{saved}");
}

/// W3.30 遷移不改變 `LoadOutcome`：結構沒變，只是補了一個欄位
#[test]
fn the_probe_proxy_backfill_is_not_a_structural_migration() {
    let dir = tmp_dir("wg-migrate-outcome");
    let raw = "closeToTray = true\n\n[[sources]]\nname = \"hk\"\nhost = \"h\"\nuser = \"u\"\n\n\
               [[sources.forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\n";
    std::fs::write(dir.join(TOML_NAME), raw).unwrap();
    let out = load_from_dir(&dir);
    assert!(
        matches!(out, LoadOutcome::Loaded(_)),
        "補一個有預設值的欄位不算 Migrated——那是留給 v2→v3 那種結構遷移的"
    );
    assert!(out.config().forward(1080).unwrap().probe_proxy);
}

/// W3.31 新建路徑不指定 `probe_proxy` 時得到 `false`（與遷移相反的那一半）
#[test]
fn a_newly_created_forward_defaults_to_not_probing() {
    let cfg = cfg_of(vec![src("hk", vec![])], vec![]);
    let made = prepare_forward(
        &cfg,
        &input(
            "hk",
            ConnKind::Ssh,
            None,
            "db",
            5432,
            Some("10.1.0.9:5432"),
            RowKind::Forward,
            false,
        ),
        true,
    )
    .expect("一般的轉發應該過");
    assert!(!made.probe_proxy, "新建預設 false：單純的轉發是更基本也更無害的那一種");
}

// -------------------------------------------- kind 不可變／probeProxy 可變

/// W3.32 **kind 不可變**：對一條既有的 `socks` 列走 `upsertForward`
#[test]
fn editing_a_socks_row_as_a_forward_is_refused() {
    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![socks("socks", 1085)])]);
    let before = cfg.clone();
    let err = prepare_forward(
        &cfg,
        &input(
            "ax4200",
            ConnKind::Wg,
            Some(1085),
            "socks",
            1085,
            Some("10.0.0.9:1080"),
            RowKind::Forward,
            false,
        ),
        true,
    )
    .expect_err("列的種類建立後不可變更");
    assert!(err.starts_with("kind: "), "{err}");
    assert_eq!(cfg, before, "被擋下來的 upsert 一個欄位都不可以改到");
}

/// W3.33 反向：對既有的 `forward` 列走 `upsertWgSocks`
#[test]
fn editing_a_forward_row_as_a_socks_row_is_refused() {
    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![fwd("nas", 2222, "10.0.0.5:22")])]);
    let err = prepare_forward(
        &cfg,
        &input("ax4200", ConnKind::Wg, Some(2222), "nas", 2222, None, RowKind::Socks, false),
        true,
    )
    .expect_err("反向一樣不可以");
    assert!(err.starts_with("kind: "), "{err}");
}

/// W3.34 **`probeProxy` 可變**：它不在不可變之列，改它不動任何連線
#[test]
fn probe_proxy_can_be_flipped_on_an_existing_row() {
    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![fwd("corp", 1086, "10.0.0.9:1080")])]);
    let made = prepare_forward(
        &cfg,
        &input(
            "ax4200",
            ConnKind::Wg,
            Some(1086),
            "corp",
            1086,
            Some("10.0.0.9:1080"),
            RowKind::Forward,
            true,
        ),
        true,
    )
    .expect("probeProxy 隨時可改");
    assert!(made.probe_proxy);

    // 反向關掉一樣可以
    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![probed("corp", 1086, "10.0.0.9:1080")])]);
    let made = prepare_forward(
        &cfg,
        &input(
            "ax4200",
            ConnKind::Wg,
            Some(1086),
            "corp",
            1086,
            Some("10.0.0.9:1080"),
            RowKind::Forward,
            false,
        ),
        true,
    )
    .expect("關掉也可以");
    assert!(!made.probe_proxy);
}

/// W3.35 同 kind 的正常編輯（改 name／remote）
#[test]
fn a_same_kind_edit_goes_through() {
    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![fwd("nas", 2222, "10.0.0.5:22")])]);
    let made = prepare_forward(
        &cfg,
        &input(
            "ax4200",
            ConnKind::Wg,
            Some(2222),
            "nas-ssh",
            2222,
            Some("10.0.0.6:22"),
            RowKind::Forward,
            false,
        ),
        true,
    )
    .expect("同 kind 的編輯本來就該過");
    assert_eq!(made.name, "nas-ssh");
    assert_eq!(made.remote.as_deref(), Some("10.0.0.6:22"));
}

/// W3.36 **連線型別不可變**（U1）：`upsertWgProxy` 的 originalName 指向 ssh 源名
#[test]
fn a_wg_upsert_cannot_take_over_an_ssh_source_name() {
    let cfg = cfg_of(vec![src("hk", vec![])], vec![]);
    let err = validate_wg_proxy(&cfg, Some("hk"), "hk", "wg/hk.conf", None)
        .expect("不得把 ssh 源改寫成 wg 連線");
    assert!(err.starts_with("name: "), "{err}");
}

/// W3.37 `upsertForward` 的 `connectionKind` 與 `connection` 指到的實際型別不符
#[test]
fn a_mismatched_connection_kind_is_refused() {
    let cfg = cfg_of(vec![src("hk", vec![])], vec![proxy("ax4200", vec![])]);
    // 說是 wg，實際是 ssh 源
    let err = validate_forward(
        &cfg,
        &input("hk", ConnKind::Wg, None, "x", 1090, Some("10.0.0.5:22"), RowKind::Forward, false),
    )
    .expect("型別不符要擋");
    assert!(!err.is_empty(), "{err}");
    // 反向：說是 ssh，實際是 wg 連線
    assert!(validate_forward(
        &cfg,
        &input(
            "ax4200",
            ConnKind::Ssh,
            None,
            "x",
            1090,
            Some("10.0.0.5:22"),
            RowKind::Forward,
            false
        ),
    )
    .is_some());
}

/// W3.38 `upsertWgSocks` 的 `connection` 指向 ssh 源（W3.23 從 IPC 那一側踩進來）
#[test]
fn a_socks_upsert_against_an_ssh_source_is_refused() {
    let cfg = cfg_of(vec![src("hk", vec![])], vec![]);
    assert!(prepare_forward(
        &cfg,
        &input("hk", ConnKind::Wg, None, "s", 1085, None, RowKind::Socks, false),
        true,
    )
    .is_err());
    assert!(prepare_forward(
        &cfg,
        &input("hk", ConnKind::Ssh, None, "s", 1085, None, RowKind::Socks, false),
        true,
    )
    .is_err());
}

// ------------------------------------------------------------ 查詢與排序

/// W3.39 `socks_rows()` / `probed_rows()` 都跨兩型連線
#[test]
fn socks_rows_and_probed_rows_span_both_connection_kinds() {
    let cfg = parse_config(BOTH).unwrap();
    let socks: Vec<u16> = cfg.socks_rows().iter().map(|f| f.local).collect();
    assert_eq!(socks, vec![1085], "只回 kind == Socks");
    let probed: Vec<u16> = cfg.probed_rows().iter().map(|f| f.local).collect();
    assert_eq!(probed, vec![1080, 1085, 1086], "② ⑤ ④ 都要進，③ 不進");
}

/// W3.40 `ordered_rows`：socks 列全部排在 forward 列之前，同 kind 內維持設定檔順序
#[test]
fn socks_rows_are_ordered_before_forward_rows() {
    let rows = vec![
        fwd("nas", 2222, "10.0.0.5:22"),
        socks("s1", 1085),
        probed("corp", 1086, "10.0.0.9:1080"),
        socks("s2", 1087),
    ];
    let ordered: Vec<u16> = ordered_rows(&rows).iter().map(|f| f.local).collect();
    assert_eq!(ordered, vec![1085, 1087, 2222, 1086], "socks 在前，同 kind 內照原順序");

    // SSH 連線只會有 forward 列，這條排序對它是恆等式
    let ssh_rows = vec![fwd("a", 1080, "127.0.0.1:1080"), fwd("db", 5432, "10.1.0.9:5432")];
    let ordered: Vec<u16> = ordered_rows(&ssh_rows).iter().map(|f| f.local).collect();
    assert_eq!(ordered, vec![1080, 5432]);
}

/// W3.41 零列的 wg 連線：設定合法，`locals()` 不含它
#[test]
fn a_wg_connection_with_no_rows_is_valid_and_owns_no_port() {
    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![])]);
    assert!(validate_config(&cfg).is_ok(), "0 條列是合法的（§1.3）");
    assert!(cfg.locals().is_empty(), "它自己沒有埠");
    assert!(cfg.enabled_locals().is_empty());
}

/// W3.42 同一條 wg 連線有 3 條 `socks` 列（0..N，§1.3）
#[test]
fn a_wg_connection_can_have_many_socks_rows() {
    let cfg = cfg_of(
        vec![],
        vec![proxy("ax4200", vec![socks("s1", 1085), socks("s2", 1087), socks("s3", 1089)])],
    );
    assert!(validate_config(&cfg).is_ok(), "socksPort 不再是連線的頂層欄位");
    assert_eq!(cfg.locals(), vec![1085, 1087, 1089], "三個埠各自獨立");
    assert_eq!(cfg.socks_rows().len(), 3);
}

/// W3.43 存檔時的鍵省略規則：舊檔改一個欄位不會突然長出三個新鍵
#[test]
fn the_writer_omits_the_keys_that_carry_their_default() {
    let dir = tmp_dir("wg-key-omission");
    let path = dir.join(TOML_NAME);
    let cfg = cfg_of(
        vec![],
        vec![proxy(
            "ax4200",
            vec![
                socks("s", 1085),
                Forward { probe_proxy: false, ..fwd("nas", 2222, "10.0.0.5:22") },
            ],
        )],
    );
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();

    // socks 列：有 kind、沒有 remote、沒有 probeProxy
    let socks_table = saved
        .split("[[wgProxies.forwards]]")
        .find(|s| s.contains("name = \"s\""))
        .expect("找不到 socks 列那一段");
    assert!(socks_table.contains("kind = \"socks\""), "{socks_table}");
    assert!(!socks_table.contains("remote"), "remote = None 要移除該鍵而不是寫空字串");
    assert!(!socks_table.contains("probeProxy"), "socks 列不得帶 probeProxy");

    // forward 列：沒有 kind、沒有 probeProxy
    let fwd_table = saved
        .split("[[wgProxies.forwards]]")
        .find(|s| s.contains("name = \"nas\""))
        .expect("找不到 forward 列那一段");
    assert!(!fwd_table.contains("kind"), "kind == Forward 不寫這個鍵：{fwd_table}");
    assert!(!fwd_table.contains("probeProxy"), "probe_proxy == false 不寫：{fwd_table}");
    assert!(fwd_table.contains("remote = \"10.0.0.5:22\""), "{fwd_table}");
}

/// W3.44 **關掉 switch 的來回**（W3.28 的端到端版）：一列 `probeProxy = true`
/// 改成 false → 存檔 → 重新讀檔，讀回來仍然是 false。
///
/// 擋的是「規格上說得通、實作卻在存檔那一步把標記弄丟」：`probeProxy = false`
/// 是省略不寫的，所以檔案上「使用者把 switch 關了」與「這是一份還沒被新版寫過
/// 的舊檔」長得一模一樣。分辨兩者的是 `kind` 鍵（§1.7）——存檔那一側漏寫它，
/// 遷移掃描下一次讀檔就會把旗標又補成 true，使用者怎麼關都關不掉。
#[test]
fn turning_probe_proxy_off_survives_a_save_and_reload() {
    let dir = tmp_dir("wg-probe-off-roundtrip");
    let path = dir.join(TOML_NAME);
    let raw = "closeToTray = true\n\n[[sources]]\nname = \"hk\"\nhost = \"h\"\nuser = \"u\"\n\n\
               [[sources.forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\nprobeProxy = true\n";
    std::fs::write(&path, raw).unwrap();

    let mut cfg = parse_config(raw).unwrap();
    assert!(cfg.forward(1080).unwrap().probe_proxy, "前提：這一列本來是開著的");

    // 使用者把那顆 switch 關掉
    cfg.forward_mut(1080).unwrap().probe_proxy = false;
    write_config_at(&path, &cfg).unwrap();

    let reread = parse_config(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(!reread.forward(1080).unwrap().probe_proxy, "關掉的檢測不可以自己又亮起來");
    assert_eq!(reread, cfg, "整份設定要一字不差地回來");
}

/// W3.45 舊格式檔的 idempotency：讀 → 存 → 再讀 → 再存，第二次的輸出與第一次
/// 逐位元組相同。
///
/// 第一次存檔會補上 `kind`（遷移標記）與遷移出來的 `probeProxy = true`，之後就
/// 穩定了——不會每次開程式都改一次使用者的檔案，也不會讓備份工具每天看到 diff。
#[test]
fn saving_an_old_format_file_settles_after_the_first_pass() {
    let dir = tmp_dir("wg-idempotent");
    let path = dir.join(TOML_NAME);
    let raw = "closeToTray = true\n\n# 香港那台\n[[sources]]\nname = \"hk\"\nhost = \"h\"\nuser = \"u\"\n\n\
               # A 出口\n[[sources.forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\n";
    std::fs::write(&path, raw).unwrap();

    let first = {
        let cfg = parse_config(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(cfg.forward(1080).unwrap().probe_proxy, "舊格式列要被遷移成 true");
        write_config_at(&path, &cfg).unwrap();
        std::fs::read_to_string(&path).unwrap()
    };
    assert!(first.contains("kind = \"forward\""), "第一次存檔要補上遷移標記：{first}");
    assert!(first.contains("# 香港那台") && first.contains("# A 出口"), "註解要留著：{first}");

    let second = {
        let cfg = parse_config(&first).unwrap();
        write_config_at(&path, &cfg).unwrap();
        std::fs::read_to_string(&path).unwrap()
    };
    assert_eq!(second, first, "第二次存檔不可以再動檔案一個位元組");
}

// -------------------------------------- MTU 覆寫欄位（PM 裁決 2026-08-24）

/// W3.46 `mtu` 是選填的：舊設定檔沒有這個鍵，讀進來就是 None，也不觸發遷移
#[test]
fn a_wg_proxy_without_an_mtu_key_parses_as_none() {
    let cfg = parse_config(BOTH).unwrap();
    assert_eq!(cfg.wg_proxy("ax4200").unwrap().mtu, None, "沒寫 mtu 就是「照 .conf」");
    assert!(validate_config(&cfg).is_ok());
}

/// W3.47 有寫就讀得到，而且鍵名是 camelCase 的 `mtu`
#[test]
fn a_written_mtu_key_round_trips() {
    let raw = BOTH.replace(
        "confPath = \"wg/ax4200.conf\"",
        "confPath = \"wg/ax4200.conf\"
mtu = 1400",
    );
    let cfg = parse_config(&raw).unwrap();
    assert_eq!(cfg.wg_proxy("ax4200").unwrap().mtu, Some(1400));
    assert!(validate_config(&cfg).is_ok());
}

/// W3.48 存檔的鍵省略規則（W3.43 的同一條規則，套到 mtu 上）：
/// None 不寫鍵、Some 寫鍵，而且把覆寫拿掉時舊的鍵要真的消失。
#[test]
fn the_writer_omits_mtu_when_there_is_no_override() {
    let dir = tmp_dir("wg-mtu-omission");
    let path = dir.join(TOML_NAME);

    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![socks("s", 1085)])]);
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("mtu"), "沒有覆寫就不該長出 mtu 鍵：{saved}");

    // 填上覆寫 → 落檔
    let mut cfg = parse_config(&saved).unwrap();
    cfg.wg_proxy_mut("ax4200").unwrap().mtu = Some(1400);
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("mtu = 1400"), "{saved}");
    assert_eq!(parse_config(&saved).unwrap().wg_proxy("ax4200").unwrap().mtu, Some(1400));

    // 再把覆寫清掉 → 鍵要跟著消失，不可以留一個看起來像被指定過的數字
    let mut cfg = parse_config(&saved).unwrap();
    cfg.wg_proxy_mut("ax4200").unwrap().mtu = None;
    write_config_at(&path, &cfg).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("mtu"), "清掉覆寫後殘留了 mtu 鍵：{saved}");
    assert_eq!(parse_config(&saved).unwrap().wg_proxy("ax4200").unwrap().mtu, None);
}

/// W3.49 驗證：空（None）合法、範圍內合法、越界兩端各報一次，訊息前綴 `mtu:`。
///
/// 欄位驗證（`validate_wg_proxy`，掛回表單欄位）與整份設定的不變量檢查
/// （`validate_config`，擋壞掉的設定檔）**兩條路徑都要擋**——前者是使用者
/// 打字的入口，後者是有人手改 toml 的入口。
#[test]
fn the_mtu_override_is_range_checked_on_both_paths() {
    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![socks("s", 1085)])]);

    assert_eq!(validate_wg_proxy(&cfg, None, "new", "wg/new.conf", None), None, "空＝不覆寫＝合法");
    assert_eq!(validate_wg_proxy(&cfg, None, "new", "wg/new.conf", Some(1400)), None);
    // 邊界本身是合法的（576..=9000 是閉區間）
    assert_eq!(validate_wg_proxy(&cfg, None, "new", "wg/new.conf", Some(576)), None);
    assert_eq!(validate_wg_proxy(&cfg, None, "new", "wg/new.conf", Some(9000)), None);

    for bad in [575usize, 9001, 0] {
        let err = validate_wg_proxy(&cfg, None, "new", "wg/new.conf", Some(bad))
            .unwrap_or_else(|| panic!("mtu = {bad} 要被擋下來"));
        assert!(err.starts_with("mtu: "), "訊息要掛回 mtu 欄位：{err}");
        assert_eq!(err, mtu_range_error(), "前後端共用的那一句，不可以各寫一份");
    }

    // 手改 toml 那條路徑
    let mut bad_cfg = cfg.clone();
    bad_cfg.wg_proxies[0].mtu = Some(100);
    let err = validate_config(&bad_cfg).unwrap_err();
    assert!(err.contains("mtu"), "{err}");
    bad_cfg.wg_proxies[0].mtu = Some(1400);
    assert!(validate_config(&bad_cfg).is_ok());
}

// ------------------------------------------------- 新連線預設附贈 SOCKS5（W3.50）
//
// `default_socks_row` 管的是兩個條件（設定層淨空、執行期淨空）。第三個條件
// 「編輯路徑永不附」**沒有對應的測試，因為它不是這個函式的行為**：
// `upsert_wg_proxy` 只在新增那一條 match 臂上叫它，編輯那一臂結構上就走不到，
// 函式本身也不再收 `original_name`。要讓那條規則失效得先改掉那個 match，
// 而那是一眼看得出來的結構改動，不是一個布林會悄悄翻掉的判斷。

/// W3.50 新建 ＋ 1080 兩層都淨空 → 附一條列，內容與手建的 socks 列逐欄位相同。
#[test]
fn a_brand_new_wg_connection_gets_a_default_socks_row_on_1080() {
    // 1080 沒有人登記：ssh 那邊掛 2222，另一條 wg 掛 1085
    let cfg = cfg_of(
        vec![src("hk", vec![fwd("db", 2222, "127.0.0.1:5432")])],
        vec![proxy("ax4200", vec![socks("s", 1085)]), proxy("new", vec![])],
    );

    let row = default_socks_row(&cfg, "new", false).expect("兩層都淨空就要附");
    assert_eq!(row, socks(DEFAULT_SOCKS_NAME, DEFAULT_SOCKS_PORT), "與手建的 socks 列無異");
    assert_eq!(row.local, 1080);
    assert_eq!(row.name, "socks");
    assert_eq!(row.remote, None, "socks 列沒有目的地");
    assert_eq!(row.kind, RowKind::Socks);
    assert!(!row.probe_proxy, "socks 列恆測，不得帶這個旗標");
    assert!(row.enabled, "附上去就是要它跑");
}

/// W3.50 設定層被 SSH 的出口佔著 → 不附，而且**不找替代埠**。
#[test]
fn a_default_socks_row_is_skipped_when_an_ssh_exit_holds_1080() {
    let cfg = cfg_of(
        vec![src("hk", vec![probed("exit-a", 1080, "127.0.0.1:1080")])],
        vec![proxy("new", vec![])],
    );
    assert_eq!(default_socks_row(&cfg, "new", false), None);
}

/// W3.50 設定層被**另一條 WG 連線**的列佔著 → 一樣不附。
///
/// 本地埠是列的全域唯一鍵（D5），佔用者在哪一條連線、哪一型連線底下都不影響結論。
#[test]
fn a_default_socks_row_is_skipped_when_another_wg_row_holds_1080() {
    let cfg = cfg_of(vec![], vec![proxy("ax4200", vec![socks("s", 1080)]), proxy("new", vec![])]);
    assert_eq!(default_socks_row(&cfg, "new", false), None);

    // 換成別條連線底下的 forward 列，結論不變
    let cfg = cfg_of(
        vec![],
        vec![proxy("home", vec![fwd("web", 1080, "10.0.0.9:80")]), proxy("new", vec![])],
    );
    assert_eq!(default_socks_row(&cfg, "new", false), None);
}

/// W3.50 設定層淨空、但執行期探測說有人在聽 → 不附。
///
/// 設定裡沒人登記不代表沒人在用：使用者本來就跑著一份別的代理是最常見的情況。
#[test]
fn a_default_socks_row_is_skipped_when_something_is_already_listening_on_1080() {
    let cfg = cfg_of(vec![], vec![proxy("new", vec![])]);
    assert_eq!(default_socks_row(&cfg, "new", true), None);
    // 同一份設定，探測換成「沒人在聽」就附得出來——差別只在執行期那一層
    assert!(default_socks_row(&cfg, "new", false).is_some());
}

/// W3.50 附贈走的是手建列同一條 `prepare_forward`，所以**連線必須先進設定**。
///
/// 這一條釘的是呼叫端的順序契約（先 push `WgProxy` 再問附贈）：查不到所屬連線時
/// `validate_forward` 會擋（W3.37），結論就是不附——反過來寫的話這個功能會安靜
/// 地永遠不生效，而且哪一條測試都不會紅。
#[test]
fn a_default_socks_row_needs_its_connection_to_be_in_the_config_already() {
    let cfg = cfg_of(vec![], vec![]);
    assert_eq!(default_socks_row(&cfg, "not-pushed-yet", false), None);

    // 同一個名字，連線推進去之後就附得出來
    let cfg = cfg_of(vec![], vec![proxy("not-pushed-yet", vec![])]);
    assert!(default_socks_row(&cfg, "not-pushed-yet", false).is_some());
}

/// W3.50 兩端的常數不可以各走各的：dev-mock 裡那一份必須逐字對得上這裡。
///
/// 比照 W1.31 的 `include_str!` 手法。mock 是使用者在瀏覽器裡演練同一條規則的
/// 地方，埠或名字漂掉的話畫面演的就不是產品的行為，而那種偏差沒有任何自動檢查
/// 抓得到——除了這一條。
#[test]
fn the_dev_mock_pins_the_same_default_socks_constants() {
    let mock = include_str!("../../src/dev-mock.ts");
    let port = format!("DEFAULT_SOCKS_PORT = {DEFAULT_SOCKS_PORT}");
    let name = format!("DEFAULT_SOCKS_NAME = \"{DEFAULT_SOCKS_NAME}\"");
    assert!(mock.contains(&port), "dev-mock.ts 缺少或改掉了 `{port}`");
    assert!(mock.contains(&name), "dev-mock.ts 缺少或改掉了 `{name}`");
}
