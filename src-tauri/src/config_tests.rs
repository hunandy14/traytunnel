//! `config` 的測試，用 `#[path]` 掛回 config.rs。
//!
//! 生產碼與測試各佔一個檔案，config.rs 才不會被將近千行的測試蓋住；
//! 模組路徑仍是 `config::tests`，`use super::*;` 拿到的一樣是 config 的私有項。

use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

fn tmp_dir(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let p =
        std::env::temp_dir().join(format!("traytunnel-test-{}-{}-{}", std::process::id(), tag, n));
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

// ------------------------------------------------------------ 路徑優先序

/// 一般的執行檔名，測試裡除非要測 p 記號否則都用它
const PLAIN: &str = "traytunnel";

/// 四象限之一：exe 旁有同名檔、家目錄問得出來 → 可攜模式優先
#[test]
fn portable_file_beside_the_exe_wins() {
    let exe = tmp_dir("loc-portable");
    let home = tmp_dir("loc-home");
    std::fs::write(exe.join(TOML_NAME), "closeToTray = true\n").unwrap();
    let loc = resolve_location(&exe, PLAIN, Some(&home));
    assert!(loc.portable);
    assert_eq!(loc.path, exe.join(TOML_NAME));
    assert_eq!(loc.dir(), exe);
    assert_eq!(file_name_of(&loc.path), TOML_NAME);
}

/// 之二：exe 旁沒有同名檔、家目錄問得出來 → 家目錄的點檔
#[test]
fn falls_back_to_the_home_dotfile() {
    let exe = tmp_dir("loc-noportable");
    let home = tmp_dir("loc-home2");
    let loc = resolve_location(&exe, PLAIN, Some(&home));
    assert!(!loc.portable);
    assert_eq!(loc.path, home.join(HOME_TOML_NAME));
    assert_eq!(loc.dir(), home);
    assert_eq!(file_name_of(&loc.path), HOME_TOML_NAME);
}

/// 之三：exe 旁有同名檔、家目錄問不出來 → 一樣是可攜模式
#[test]
fn portable_wins_even_without_a_home_dir() {
    let exe = tmp_dir("loc-portable-nohome");
    std::fs::write(exe.join(TOML_NAME), "closeToTray = true\n").unwrap();
    let loc = resolve_location(&exe, PLAIN, None);
    assert!(loc.portable);
    assert_eq!(loc.path, exe.join(TOML_NAME));
}

/// 之四：兩邊都沒有 → 退回 exe 目錄，但檔名維持點檔，
/// 免得自己生出來的檔案下次啟動被當成可攜模式
#[test]
fn without_a_home_dir_it_uses_the_exe_dir_dotfile() {
    let exe = tmp_dir("loc-nohome");
    let loc = resolve_location(&exe, PLAIN, None);
    assert!(!loc.portable);
    assert_eq!(loc.path, exe.join(HOME_TOML_NAME));
    assert_ne!(loc.path, exe.join(TOML_NAME));
}

// ------------------------------ 檔名含 p 的可攜記號（Rufus 慣例）

/// 產品名 traytunnel 不是 p 結尾，這是整個記號成立的前提
#[test]
fn the_product_name_itself_does_not_end_with_p() {
    assert!(!stem_marks_portable("traytunnel"));
    assert!(!stem_marks_portable("traytunnel-0.2.0"));
    assert!(!stem_marks_portable(""));
}

/// 記號是結尾的 p，大小寫不敏感（Rufus 本尊就是 rufus-4.5p.exe 這種寫法）
#[test]
fn a_trailing_p_marks_portable() {
    assert!(stem_marks_portable("traytunnel-0.2.0p"));
    assert!(stem_marks_portable("traytunnel-p"));
    assert!(stem_marks_portable("traytunnelp"));
    // 大寫一樣算
    assert!(stem_marks_portable("traytunnel-0.2.0P"));
    assert!(stem_marks_portable("TRAYTUNNEL-P"));
}

/// p 在中間或開頭都不算，只看最後一個字元
#[test]
fn a_p_elsewhere_in_the_stem_does_not_mark_portable() {
    assert!(!stem_marks_portable("traytunnel-portable"));
    assert!(!stem_marks_portable("portable-traytunnel"));
    assert!(!stem_marks_portable("tray-p-tunnel"));
    assert!(!stem_marks_portable("traytunnel-preview"));
}

/// Windows 複製檔案自動取的名字含 Copy（裡面有 p），
/// 絕對不可以因此就把使用者的設定改讀到 exe 旁邊去
#[test]
fn a_windows_copy_is_not_portable() {
    assert!(!stem_marks_portable("traytunnel - Copy"));
    assert!(!stem_marks_portable("traytunnel - Copy (2)"));

    let exe = tmp_dir("loc-copy");
    let home = tmp_dir("loc-home-copy");
    let loc = resolve_location(&exe, "traytunnel - Copy", Some(&home));
    assert!(!loc.portable);
    assert_eq!(loc.path, home.join(HOME_TOML_NAME));
}

/// 檔名含 p 時，設定檔就在 exe 旁邊，即使檔案還不存在、家目錄也問得出來
#[test]
fn p_in_the_stem_wins_over_the_home_dotfile() {
    let exe = tmp_dir("loc-pstem");
    let home = tmp_dir("loc-home-pstem");
    let loc = resolve_location(&exe, "traytunnel-0.2.0p", Some(&home));
    assert!(loc.portable);
    assert_eq!(loc.path, exe.join(TOML_NAME));
    // 家目錄那份完全不碰
    assert!(!home.join(HOME_TOML_NAME).exists());
}

/// 檔名含 p 但檔案還不存在時，load 要就地建一份預設檔（Rufus 建 ini 的行為）
#[test]
fn p_in_the_stem_creates_the_file_next_to_the_exe() {
    let exe = tmp_dir("loc-pstem-create");
    let home = tmp_dir("loc-home-pstem-create");
    let loc = resolve_location(&exe, "traytunnel-p", Some(&home));
    assert!(!loc.path.exists(), "解析階段不該自己先建檔");

    let out = load_from_path(&loc.path);
    assert!(matches!(out, LoadOutcome::Created(_)));
    assert!(exe.join(TOML_NAME).is_file());
    assert_eq!(out.config(), &Config::default());
    // 家目錄依舊乾乾淨淨
    assert!(!home.join(HOME_TOML_NAME).exists());
}

/// 空檔是可攜模式的開關（`type nul > traytunnel.toml`），要被補成預設內容，
/// 不可以當成壞檔
#[test]
fn an_empty_file_is_filled_in_with_defaults() {
    let dir = tmp_dir("empty-file");
    let path = dir.join(TOML_NAME);
    std::fs::write(&path, "").unwrap();

    let out = load_from_path(&path);
    assert!(matches!(out, LoadOutcome::Created(_)), "空檔應該走 Created");
    assert_eq!(out.config(), &Config::default());
    // 內容真的被補上去了，而且不該留下壞檔備份
    assert!(std::fs::metadata(&path).unwrap().len() > 0);
    assert!(!broken_path(&path).exists());
    // 補完之後再讀一次就是正常的 Loaded
    assert!(matches!(load_from_path(&path), LoadOutcome::Loaded(_)));
}

/// 空檔擺在 exe 旁邊就是可攜模式的開關：程式補內容，家目錄那份不碰
#[test]
fn an_empty_file_beside_the_exe_turns_on_portable_mode() {
    let exe = tmp_dir("empty-portable");
    let home = tmp_dir("empty-portable-home");
    std::fs::write(exe.join(TOML_NAME), "").unwrap();

    let loc = resolve_location(&exe, PLAIN, Some(&home));
    assert!(loc.portable);
    assert!(matches!(load_from_path(&loc.path), LoadOutcome::Created(_)));
    assert!(std::fs::metadata(exe.join(TOML_NAME)).unwrap().len() > 0);
    assert!(!home.join(HOME_TOML_NAME).exists());
}

/// 兩個觸發條件同時成立（檔名 p 結尾 ＋ exe 旁已有檔案）時是同一個結果，
/// 而且既有檔案要被讀進來，不可以被預設值蓋掉
#[test]
fn both_portable_triggers_agree_on_the_same_file() {
    let exe = tmp_dir("loc-both");
    let home = tmp_dir("loc-home-both");
    let raw = "closeToTray = false\n\n[[sources]]\nname = \"hk\"\nhost = \"h\"\nuser = \"u\"\n";
    std::fs::write(exe.join(TOML_NAME), raw).unwrap();

    let by_stem = resolve_location(&exe, "traytunnel-p", Some(&home));
    let by_file = resolve_location(&exe, PLAIN, Some(&home));
    assert_eq!(by_stem, by_file);
    assert!(by_stem.portable);

    let out = load_from_path(&by_stem.path);
    assert!(matches!(out, LoadOutcome::Loaded(_)));
    assert!(!out.config().close_to_tray);
    assert_eq!(std::fs::read_to_string(exe.join(TOML_NAME)).unwrap(), raw);
}

/// 資料夾不算數，同名的資料夾不該讓程式誤判成可攜模式
#[test]
fn a_directory_named_like_the_config_is_not_portable() {
    let exe = tmp_dir("loc-dir");
    let home = tmp_dir("loc-home3");
    std::fs::create_dir_all(exe.join(TOML_NAME)).unwrap();
    assert!(!resolve_location(&exe, PLAIN, Some(&home)).portable);
}

/// 壞檔備份跟著生效檔名走，兩種模式各自不同名
#[test]
fn broken_path_follows_the_live_file_name() {
    assert_eq!(
        broken_path(Path::new("C:\\app\\traytunnel.toml")),
        PathBuf::from("C:\\app\\traytunnel.toml.broken")
    );
    assert_eq!(
        broken_path(Path::new("C:\\Users\\bob\\.traytunnel.toml")),
        PathBuf::from("C:\\Users\\bob\\.traytunnel.toml.broken")
    );
}

// ------------------------------------------------------------ 完整路徑讀寫

/// 讀寫一律吃完整路徑，家目錄那個點檔也要能建、能讀、能寫回
#[test]
fn loads_and_writes_a_dotfile_by_path() {
    let dir = tmp_dir("dotfile");
    let path = dir.join(HOME_TOML_NAME);

    let out = load_from_path(&path);
    assert!(matches!(out, LoadOutcome::Created(_)));
    assert!(path.exists());
    // 不可以順手在旁邊生出可攜模式的檔案
    assert!(!dir.join(TOML_NAME).exists());

    let mut cfg = out.config().clone();
    cfg.close_to_tray = false;
    write_config_at(&path, &cfg).unwrap();
    let back = load_from_path(&path);
    assert!(matches!(back, LoadOutcome::Loaded(_)));
    assert_eq!(back.config(), &cfg);
}

/// 點檔壞掉時備份是 .traytunnel.toml.broken，而且原檔不被覆寫
#[test]
fn broken_dotfile_is_backed_up_next_to_itself() {
    let dir = tmp_dir("dotfile-broken");
    let path = dir.join(HOME_TOML_NAME);
    let bad = "closeToTray = true\nthis is not toml @@@\n";
    std::fs::write(&path, bad).unwrap();

    let out = load_from_path(&path);
    match &out {
        LoadOutcome::Broken { backup, .. } => {
            let backup = backup.as_ref().expect("應該有備份");
            assert_eq!(backup, &dir.join(".traytunnel.toml.broken"));
            assert_eq!(std::fs::read_to_string(backup).unwrap(), bad);
        }
        _ => panic!("預期 Broken"),
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), bad);
}

// ------------------------------------------------------------ 新制解析

#[test]
fn parses_toml_with_comments() {
    let raw = r#"
# 我的註解
closeToTray = false

[[sources]]
name = "hk"
host = "h.example.com"
user = "bob"
proxyCommand = "cloudflared access ssh --hostname %h"

[[sources.forwards]]
name = "a"
local = 1080
remote = "127.0.0.1:1080"
"#;
    let cfg = parse_config(raw).unwrap();
    assert!(!cfg.close_to_tray);
    assert_eq!(cfg.sources.len(), 1);
    assert_eq!(cfg.sources[0].name, "hk");
    assert_eq!(cfg.sources[0].host, "h.example.com");
    assert_eq!(cfg.sources[0].user, "bob");
    assert_eq!(cfg.sources[0].forwards.len(), 1);
    assert_eq!(cfg.sources[0].forwards[0].local, 1080);
}

#[test]
fn parses_multiple_sources() {
    let raw = r#"
closeToTray = true

[[sources]]
name = "hk"
host = "hk.example.com"
user = "bob"

[[sources.forwards]]
name = "a"
local = 1080
remote = "127.0.0.1:1080"

[[sources]]
name = "tw"
host = "tw.example.com"
user = "alice"

[[sources.forwards]]
name = "b"
local = 1083
remote = "127.0.0.1:1083"
enabled = false
"#;
    let cfg = parse_config(raw).unwrap();
    assert_eq!(cfg.sources.len(), 2);
    assert_eq!(cfg.locals(), vec![1080, 1083]);
    assert_eq!(cfg.enabled_locals(), vec![1080]);
    assert_eq!(cfg.source_name_of(1083), Some("tw"));
    assert_eq!(cfg.locate(1080).unwrap().0.user, "bob");
}

/// 單一源的埠清單：跨源不會混到，問一個不存在的源就是空的
#[test]
fn locals_of_only_covers_that_source() {
    let cfg = Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![
            src("hk", vec![fwd("a", 1080), Forward { enabled: false, ..fwd("b", 1083) }]),
            src("tw", vec![fwd("c", 1090)]),
        ],
    };
    assert_eq!(cfg.locals_of("hk"), vec![1080, 1083]);
    assert_eq!(cfg.enabled_locals_of("hk"), vec![1080]);
    assert_eq!(cfg.locals_of("tw"), vec![1090]);
    assert!(cfg.locals_of("nope").is_empty());
    assert!(cfg.enabled_locals_of("nope").is_empty());
}

/// 沒有 enabled 欄位時一律當成啟用
#[test]
fn forward_enabled_defaults_to_true() {
    let raw = "[[sources]]\nname = \"s\"\nhost = \"h\"\nuser = \"u\"\n\n[[sources.forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\n";
    let cfg = parse_config(raw).unwrap();
    assert!(cfg.sources[0].forwards[0].enabled);
}

#[test]
fn forward_enabled_is_read_back() {
    let raw = "[[sources]]\nname = \"s\"\nhost = \"h\"\nuser = \"u\"\n\n[[sources.forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\nenabled = false\n";
    let cfg = parse_config(raw).unwrap();
    assert!(!cfg.sources[0].forwards[0].enabled);
}

#[test]
fn missing_fields_fall_back_to_defaults() {
    let cfg = parse_config("[[sources]]\nname = \"s\"\nhost = \"h\"\nuser = \"u\"\n").unwrap();
    assert!(cfg.close_to_tray);
    assert_eq!(cfg.sources[0].proxy_command, "");
    assert!(cfg.sources[0].forwards.is_empty());
}

/// checkForUpdates 的預設值跟著模式走：設定檔沒寫時，一般模式開、可攜模式關。
/// 這是可攜版「不主動連外」承諾的落點，寫死成單一預設值就等於毀約。
#[test]
fn check_for_updates_defaults_follow_the_mode() {
    let cfg = parse_config("closeToTray = true\n").unwrap();
    assert_eq!(cfg.check_for_updates, None, "沒寫就該是 None，不可以在解析時就決定");
    assert!(cfg.checks_for_updates(false), "一般模式預設要檢查");
    assert!(!cfg.checks_for_updates(true), "可攜模式預設不檢查");
}

/// 寫了就照使用者寫的算，兩種模式下都一樣——明示永遠蓋過預設
#[test]
fn an_explicit_check_for_updates_wins_in_both_modes() {
    let on = parse_config("checkForUpdates = true\n").unwrap();
    assert_eq!(on.check_for_updates, Some(true));
    assert!(on.checks_for_updates(true) && on.checks_for_updates(false));

    let off = parse_config("checkForUpdates = false\n").unwrap();
    assert_eq!(off.check_for_updates, Some(false));
    assert!(!off.checks_for_updates(true) && !off.checks_for_updates(false));
}

/// 沒設定過就不要把鍵寫進檔案：一寫下去，當下算出來的預設值就被固定住了，
/// 同一份可攜設定之後被一般模式讀到（或反過來）時就跟不動模式了
#[test]
fn write_only_persists_check_for_updates_when_set() {
    let dir = tmp_dir("check-updates");
    let mut cfg = Config::default();
    write_config(&dir, &cfg).unwrap();
    let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
    assert!(!saved.contains("\ncheckForUpdates"), "不該寫出這個鍵：{saved}");
    assert_eq!(parse_config(&saved).unwrap().check_for_updates, None);

    cfg.check_for_updates = Some(false);
    write_config(&dir, &cfg).unwrap();
    let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
    assert!(saved.contains("checkForUpdates = false"), "{saved}");
    assert_eq!(parse_config(&saved).unwrap().check_for_updates, Some(false));

    // 改回 true 是就地改寫，不會多長出第二個鍵
    cfg.check_for_updates = Some(true);
    write_config(&dir, &cfg).unwrap();
    let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
    // 只數沒有被井字號註解掉的那一行（預設檔本來就帶一行說明用的註解）
    assert_eq!(saved.matches("\ncheckForUpdates = ").count(), 1, "{saved}");
    assert_eq!(parse_config(&saved).unwrap().check_for_updates, Some(true));
}

/// 一個源都沒有也是合法設定（使用者可以刪到零源）
#[test]
fn zero_sources_is_valid() {
    let cfg = parse_config("closeToTray = false\n").unwrap();
    assert!(cfg.sources.is_empty());
    assert!(!cfg.close_to_tray);
}

// ------------------------------------------------------------ 檔案自洽性

#[test]
fn rejects_empty_host_or_user() {
    assert!(parse_config("[[sources]]\nname=\"s\"\nhost = \"\"\nuser = \"u\"\n").is_err());
    assert!(parse_config("[[sources]]\nname=\"s\"\nhost = \"h\"\nuser = \"   \"\n").is_err());
    // 缺欄位一樣要擋下來，不能靜靜地變成空字串
    assert!(parse_config("[[sources]]\nname=\"s\"\nuser = \"u\"\n").is_err());
}

#[test]
fn rejects_bad_source_names() {
    assert!(parse_config("[[sources]]\nname=\"\"\nhost=\"h\"\nuser=\"u\"\n").is_err());
    assert!(parse_config("[[sources]]\nname=\"two words\"\nhost=\"h\"\nuser=\"u\"\n").is_err());
}

#[test]
fn rejects_duplicate_source_names() {
    let raw = "[[sources]]\nname=\"s\"\nhost=\"h\"\nuser=\"u\"\n\n[[sources]]\nname=\"s\"\nhost=\"h2\"\nuser=\"u\"\n";
    assert!(parse_config(raw).is_err());
}

/// local 是全域唯一鍵，兩個源撞同一個埠就是壞檔
#[test]
fn rejects_duplicate_local_across_sources() {
    let raw = "[[sources]]\nname=\"a\"\nhost=\"h\"\nuser=\"u\"\n\n[[sources.forwards]]\nname=\"x\"\nlocal=1080\nremote=\"127.0.0.1:1\"\n\n[[sources]]\nname=\"b\"\nhost=\"h2\"\nuser=\"u\"\n\n[[sources.forwards]]\nname=\"y\"\nlocal=1080\nremote=\"127.0.0.1:2\"\n";
    let err = parse_config(raw).unwrap_err();
    assert!(err.contains("1080"));
}

/// 手寫的 remote 走的是與介面輸入同一條規則，壞值不可以一路餵進 ssh -L
#[test]
fn rejects_hand_written_bad_remote() {
    let with = |remote: &str| {
        format!(
            "[[sources]]\nname=\"s\"\nhost=\"h\"\nuser=\"u\"\n\
             [[sources.forwards]]\nname=\"a\"\nlocal=1080\nremote=\"{remote}\"\n"
        )
    };
    for bad in ["127.0.0.1", "nope", "has space:22", "127.0.0.1:abc", "0", "70000", ""] {
        let err = parse_config(&with(bad)).unwrap_err();
        assert!(err.contains("remote"), "{bad} 的訊息要點名 remote：{err}");
    }
    // 合法的兩種寫法照樣過（純埠號在驗證之前已經被補成完整形式）
    assert!(parse_config(&with("127.0.0.1:1080")).is_ok());
    assert!(parse_config(&with("8080")).is_ok());
    assert!(parse_config(&with("example.com:22")).is_ok());
}

#[test]
fn empty_host_makes_the_file_broken_not_overwritten() {
    let dir = tmp_dir("empty-host");
    let raw = "[[sources]]\nname = \"s\"\nhost = \"\"\nuser = \"u\"\n";
    std::fs::write(dir.join(TOML_NAME), raw).unwrap();
    let out = load_from_dir(&dir);
    assert!(matches!(out, LoadOutcome::Broken { .. }));
    assert_eq!(std::fs::read_to_string(dir.join(TOML_NAME)).unwrap(), raw);
}

#[test]
fn broken_file_is_backed_up_and_never_overwritten() {
    let dir = tmp_dir("broken");
    let bad = "closeToTray = true\nthis is not toml @@@\n";
    std::fs::write(dir.join(TOML_NAME), bad).unwrap();
    let out = load_from_dir(&dir);
    match &out {
        LoadOutcome::Broken { backup, .. } => {
            assert!(backup.as_ref().expect("應該有備份").exists());
        }
        _ => panic!("預期 Broken"),
    }
    assert_eq!(out.config(), &Config::default());
    // 原檔完全沒被動過
    assert_eq!(std::fs::read_to_string(dir.join(TOML_NAME)).unwrap(), bad);
    assert_eq!(std::fs::read_to_string(dir.join(BROKEN_NAME)).unwrap(), bad);
}

/// 唯讀只在「壞檔 ＋ 備份不出來」這一格成立：那時原檔是使用者僅存的一份，
/// 任何回寫都會把它輾成預設值。其餘結果一律照常可寫。
#[test]
fn only_a_broken_file_without_a_backup_turns_read_only() {
    let broken_no_backup =
        LoadOutcome::Broken { config: Config::default(), backup: None, error: "boom".into() };
    assert!(broken_no_backup.read_only());

    let broken_with_backup = LoadOutcome::Broken {
        config: Config::default(),
        backup: Some(PathBuf::from("C:\\app\\traytunnel.toml.broken")),
        error: "boom".into(),
    };
    assert!(!broken_with_backup.read_only());
    assert!(!LoadOutcome::Loaded(Config::default()).read_only());
    assert!(!LoadOutcome::Created(Config::default()).read_only());
    assert!(!LoadOutcome::Migrated(Config::default()).read_only());
}

/// 壞檔但備份成功的那條路仍然可寫，這是唯讀規則的下邊界
#[test]
fn a_backed_up_broken_file_stays_writable() {
    let dir = tmp_dir("broken-writable");
    std::fs::write(dir.join(TOML_NAME), "not toml @@@\n").unwrap();
    let out = load_from_dir(&dir);
    assert!(matches!(out, LoadOutcome::Broken { backup: Some(_), .. }));
    assert!(!out.read_only());
}

/// 舊制但內容壞掉（host 空字串）時，一樣是壞檔而不是遷移
#[test]
fn broken_legacy_file_is_not_migrated() {
    let dir = tmp_dir("broken-legacy");
    let raw = "host = \"\"\nuser = \"u\"\n";
    std::fs::write(dir.join(TOML_NAME), raw).unwrap();
    let out = load_from_dir(&dir);
    assert!(matches!(out, LoadOutcome::Broken { .. }));
    assert_eq!(std::fs::read_to_string(dir.join(TOML_NAME)).unwrap(), raw);
}

/// 預設檔寫不出去時不可以回報 Created：記憶體照樣用預設值跑，
/// 但「已經幫你建好檔了」這句話是假的，上層要據此給不同的訊息
#[test]
fn a_failed_creation_is_not_reported_as_created() {
    let dir = tmp_dir("create-fail");
    // 上層資料夾不存在，fs::write 一定失敗
    let path = dir.join("no-such-dir").join(TOML_NAME);
    let out = load_from_path(&path);
    match &out {
        LoadOutcome::CreateFailed { error, .. } => assert!(!error.is_empty()),
        other => panic!("預期 CreateFailed，拿到 {other:?}"),
    }
    assert_eq!(out.config(), &Config::default());
    assert!(!path.exists());
    // 沒有原檔要保護，不必切唯讀
    assert!(!out.read_only());
}

#[test]
fn creates_default_file_when_missing() {
    let dir = tmp_dir("create");
    let out = load_from_dir(&dir);
    assert!(matches!(out, LoadOutcome::Created(_)));
    assert_eq!(out.config(), &Config::default());
    assert!(dir.join(TOML_NAME).exists());
    // 再讀一次應該可以正常解析回同樣的內容
    let again = load_from_dir(&dir);
    assert!(matches!(again, LoadOutcome::Loaded(_)));
    assert_eq!(again.config(), &Config::default());
}

// ------------------------------------------------------------ 舊制遷移

/// 舊制設定檔（頂層 host）自動包成單一 source，源名用 host 的值
#[test]
fn legacy_config_is_wrapped_into_one_source() {
    let raw = "host = \"h.example.com\"\nuser = \"bob\"\nproxyCommand = \"cloudflared access ssh --hostname %h\"\ncloseToTray = false\n\n[[forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\nenabled = false\n";
    let (cfg, migrated) = parse_document(raw).unwrap();
    assert!(migrated);
    assert!(!cfg.close_to_tray);
    assert_eq!(cfg.sources.len(), 1);
    let s = &cfg.sources[0];
    assert_eq!(s.name, "h.example.com");
    assert_eq!(s.host, "h.example.com");
    assert_eq!(s.user, "bob");
    assert_eq!(s.proxy_command, "cloudflared access ssh --hostname %h");
    assert_eq!(s.forwards.len(), 1);
    assert!(!s.forwards[0].enabled);
}

/// host 是 [::1] 這種字面 IPv6 位址時，中括號要被剝掉才生得出合法的源名，
/// 否則遷移出來的設定會當場過不了自己的驗證
#[test]
fn legacy_host_with_brackets_migrates() {
    let raw = "host = \"[::1]\"\nuser = \"bob\"\n";
    let (cfg, migrated) = parse_document(raw).unwrap();
    assert!(migrated);
    assert_eq!(cfg.sources[0].name, "::1");
    // host 本身不動，ssh 還是要拿到原本的字面位址
    assert_eq!(cfg.sources[0].host, "[::1]");
    // 派生出來的源名必須是合法源名
    assert!(valid_source_name(&cfg.sources[0].name));
}

/// 消毒規則：剝掉空白與中括號，全被剝光就退回 default
#[test]
fn derived_source_name_is_sanitised() {
    assert_eq!(source_name_from_host("h.example.com"), "h.example.com");
    assert_eq!(source_name_from_host("  h.example.com  "), "h.example.com");
    assert_eq!(source_name_from_host("[2001:db8::1]"), "2001:db8::1");
    assert_eq!(source_name_from_host("a b"), "ab");
    assert_eq!(source_name_from_host("[ ]"), "default");
    for h in ["h.example.com", "[::1]", "a b", "[ ]", "[]"] {
        assert!(valid_source_name(&source_name_from_host(h)), "{h}");
    }
}

/// 新制檔案不該被誤判成需要遷移
#[test]
fn new_format_is_not_flagged_as_migrated() {
    let raw = "[[sources]]\nname=\"s\"\nhost=\"h\"\nuser=\"u\"\n";
    assert!(!parse_document(raw).unwrap().1);
}

/// 遷移會就地寫回新制，而且檔頭與逐筆 forward 的註解都要留著
#[test]
fn legacy_file_is_migrated_on_disk_keeping_comments() {
    let dir = tmp_dir("migrate");
    std::fs::write(
        dir.join(TOML_NAME),
        "# 我的設定檔\nhost = \"h.example.com\"\nuser = \"bob\"\nproxyCommand = \"\"\ncloseToTray = true\n\n# 這是 A 出口\n[[forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\nenabled = true\n",
    )
    .unwrap();
    let out = load_from_dir(&dir);
    assert!(matches!(out, LoadOutcome::Migrated(_)));

    let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
    assert!(saved.contains("# 我的設定檔"), "檔頭註解要留著：{saved}");
    assert!(saved.contains("# 這是 A 出口"), "逐筆註解要留著：{saved}");
    assert!(saved.contains("[[sources]]"), "{saved}");
    assert!(saved.contains("[[sources.forwards]]"), "{saved}");
    assert!(!saved.contains("[[forwards]]"), "舊的頂層 forwards 要被收走：{saved}");
    // 檔頭註解要留在最上面，頂層不再有 host
    let head: Vec<&str> = saved.lines().take(2).collect();
    assert_eq!(head[0], "# 我的設定檔", "{saved}");
    assert!(head[1].starts_with("closeToTray"), "{saved}");

    // 寫回去的檔案要能原封不動再讀一次，而且不再是舊制
    let (again, migrated) = parse_document(&saved).unwrap();
    assert!(!migrated);
    assert_eq!(&again, out.config());
}

/// PowerShell 5 存檔會帶 UTF-8 BOM，不能因此就把設定當成壞檔
#[test]
fn parses_toml_with_utf8_bom() {
    let raw = "\u{feff}[[sources]]\nname = \"s\"\nhost = \"bom.example.com\"\nuser = \"bob\"\n\n[[sources.forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\n";
    let cfg = parse_config(raw).unwrap();
    assert_eq!(cfg.sources[0].host, "bom.example.com");
    assert_eq!(cfg.sources[0].forwards.len(), 1);
}

#[test]
fn loads_bom_file_from_disk_without_treating_it_as_broken() {
    let dir = tmp_dir("bom");
    std::fs::write(
        dir.join(TOML_NAME),
        "\u{feff}closeToTray = false\n\n[[sources]]\nname = \"s\"\nhost = \"bom.example.com\"\nuser = \"bob\"\nproxyCommand = \"\"\n\n[[sources.forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\n",
    )
    .unwrap();
    let out = load_from_dir(&dir);
    assert!(matches!(out, LoadOutcome::Loaded(_)));
    assert_eq!(out.config().sources[0].host, "bom.example.com");
    assert!(!out.config().close_to_tray);
    // 不該產生壞檔備份
    assert!(!dir.join(BROKEN_NAME).exists());
}

// ------------------------------------------------------------ 寫回

#[test]
fn write_persists_enabled_flag() {
    let dir = tmp_dir("enabled-write");
    let mut cfg = Config::default();
    cfg.sources[0].forwards[1].enabled = false;
    write_config(&dir, &cfg).unwrap();
    let back = parse_config(&std::fs::read_to_string(dir.join(TOML_NAME)).unwrap()).unwrap();
    assert!(back.sources[0].forwards[0].enabled);
    assert!(!back.sources[0].forwards[1].enabled);
    assert_eq!(back, cfg);
}

/// 只改一個 forward 的 enabled 時，寫在該筆上方的註解要留著
#[test]
fn write_keeps_per_forward_comments() {
    let dir = tmp_dir("forward-comments");
    std::fs::write(
        dir.join(TOML_NAME),
        "[[sources]]\nname = \"s\"\nhost = \"h\"\nuser = \"u\"\n\n# 這是 A 出口\n[[sources.forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\n",
    )
    .unwrap();
    let mut cfg = parse_config(&std::fs::read_to_string(dir.join(TOML_NAME)).unwrap()).unwrap();
    cfg.sources[0].forwards[0].enabled = false;
    write_config(&dir, &cfg).unwrap();
    let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
    assert!(saved.contains("# 這是 A 出口"));
    assert!(saved.contains("enabled = false"));
}

/// 寫在單一 source 上方的註解也要留著
#[test]
fn write_keeps_per_source_comments() {
    let dir = tmp_dir("source-comments");
    std::fs::write(
        dir.join(TOML_NAME),
        "# 香港機\n[[sources]]\nname = \"hk\"\nhost = \"h\"\nuser = \"u\"\n\n# 東京機\n[[sources]]\nname = \"tk\"\nhost = \"t\"\nuser = \"u\"\n",
    )
    .unwrap();
    let mut cfg = parse_config(&std::fs::read_to_string(dir.join(TOML_NAME)).unwrap()).unwrap();
    cfg.sources[1].user = "alice".into();
    write_config(&dir, &cfg).unwrap();
    let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
    assert!(saved.contains("# 香港機"));
    assert!(saved.contains("# 東京機"));
    assert!(saved.contains("alice"));
    assert_eq!(parse_config(&saved).unwrap(), cfg);
}

#[test]
fn write_preserves_comments() {
    let dir = tmp_dir("write");
    let raw = "# 保留我\ncloseToTray = true\n\n[[sources]]\nname = \"s\"\nhost = \"a\"\nuser = \"b\"\nproxyCommand = \"\"\n";
    std::fs::write(dir.join(TOML_NAME), raw).unwrap();
    let mut cfg = parse_config(raw).unwrap();
    cfg.sources[0].host = "c.example.com".into();
    cfg.sources[0].forwards = vec![Forward {
        name: "x".into(),
        local: 1090,
        remote: "127.0.0.1:9".into(),
        enabled: true,
    }];
    write_config(&dir, &cfg).unwrap();
    let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
    assert!(saved.contains("# 保留我"));
    assert!(saved.contains("c.example.com"));
    let back = parse_config(&saved).unwrap();
    assert_eq!(back, cfg);
}

/// 帶 BOM 的檔案存檔後也要保留註解，不能退回預設模板
#[test]
fn write_keeps_comments_of_bom_file() {
    let dir = tmp_dir("bom-write");
    std::fs::write(
        dir.join(TOML_NAME),
        "\u{feff}# 保留我\ncloseToTray = true\n\n[[sources]]\nname = \"s\"\nhost = \"a\"\nuser = \"b\"\nproxyCommand = \"\"\n",
    )
    .unwrap();
    let mut cfg = parse_config(&std::fs::read_to_string(dir.join(TOML_NAME)).unwrap()).unwrap();
    cfg.sources[0].host = "c.example.com".into();
    write_config(&dir, &cfg).unwrap();
    let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
    assert!(saved.contains("# 保留我"));
    assert!(saved.contains("c.example.com"));
}

/// 存檔走「暫存檔 + rename」，成功之後資料夾裡不可以留下暫存檔，
/// 生效檔名也必須還是原來那一個
#[test]
fn write_goes_through_a_temp_file_and_leaves_none_behind() {
    let dir = tmp_dir("atomic-write");
    let path = dir.join(TOML_NAME);
    assert_eq!(tmp_path(&path), dir.join("traytunnel.toml.tmp"));
    assert_eq!(
        tmp_path(Path::new("C:\\Users\\bob\\.traytunnel.toml")),
        PathBuf::from("C:\\Users\\bob\\.traytunnel.toml.tmp")
    );

    // 建檔走的是同一條路，一樣不留暫存檔
    assert!(matches!(load_from_path(&path), LoadOutcome::Created(_)));
    assert!(!tmp_path(&path).exists(), "建檔的暫存檔要被 rename 掉");

    let cfg = Config::default();
    write_config(&dir, &cfg).unwrap();
    assert!(!tmp_path(&path).exists(), "暫存檔要被 rename 掉");
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec![TOML_NAME.to_string()]);
    // 換名之後的內容是完整的一份，不是半截檔
    assert_eq!(parse_config(&std::fs::read_to_string(&path).unwrap()).unwrap(), cfg);
}

/// 刪出口後檔案裡不該留下多餘的 [[sources.forwards]]
#[test]
fn write_drops_removed_forwards() {
    let dir = tmp_dir("shrink");
    let mut cfg = Config::default();
    write_config(&dir, &cfg).unwrap();
    cfg.sources[0].forwards.remove(1);
    write_config(&dir, &cfg).unwrap();
    let back = parse_config(&std::fs::read_to_string(dir.join(TOML_NAME)).unwrap()).unwrap();
    assert_eq!(back.sources[0].forwards.len(), 1);
    assert_eq!(back.sources[0].forwards[0].local, 1080);
}

/// 刪源後檔案裡不該留下多餘的 [[sources]]，刪到零源也要能寫得出來
#[test]
fn write_drops_removed_sources() {
    let dir = tmp_dir("shrink-src");
    let mut cfg = Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![src("a", vec![fwd("x", 1080)]), src("b", vec![fwd("y", 1083)])],
    };
    write_config(&dir, &cfg).unwrap();
    assert_eq!(parse_config(&std::fs::read_to_string(dir.join(TOML_NAME)).unwrap()).unwrap(), cfg);

    cfg.sources.remove(0);
    write_config(&dir, &cfg).unwrap();
    let back = parse_config(&std::fs::read_to_string(dir.join(TOML_NAME)).unwrap()).unwrap();
    assert_eq!(back.sources.len(), 1);
    assert_eq!(back.sources[0].name, "b");

    cfg.sources.clear();
    write_config(&dir, &cfg).unwrap();
    let empty = parse_config(&std::fs::read_to_string(dir.join(TOML_NAME)).unwrap()).unwrap();
    assert!(empty.sources.is_empty());
}

/// 新增源時就地補一張新表格，讀回來要一模一樣
#[test]
fn write_appends_new_source() {
    let dir = tmp_dir("grow-src");
    let mut cfg = Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![src("a", vec![fwd("x", 1080)])],
    };
    write_config(&dir, &cfg).unwrap();
    cfg.sources.push(src("b", vec![fwd("y", 1083), fwd("z", 1084)]));
    write_config(&dir, &cfg).unwrap();
    let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
    assert_eq!(parse_config(&saved).unwrap(), cfg);
}

// ------------------------------------------------------------ 驗證

#[test]
fn remote_must_be_host_colon_port() {
    assert!(valid_remote("127.0.0.1:1080"));
    assert!(valid_remote("example.com:22"));
    assert!(!valid_remote("127.0.0.1"));
    assert!(!valid_remote("127.0.0.1:"));
    assert!(!valid_remote(":1080"));
    assert!(!valid_remote("127.0.0.1:abc"));
    assert!(!valid_remote("has space:22"));
    // 冒號只准一個，[::1]:22 這種寫法目前不支援
    assert!(!valid_remote("::1:22"));
}

/// 只填埠號＝伺服器本機的那個埠，補成完整形式再存檔
#[test]
fn bare_port_normalizes_to_loopback() {
    assert_eq!(normalize_remote("1080"), "127.0.0.1:1080");
    assert_eq!(normalize_remote("1"), "127.0.0.1:1");
    assert_eq!(normalize_remote("65535"), "127.0.0.1:65535");
    // 前後空白一併去除
    assert_eq!(normalize_remote("  8080  "), "127.0.0.1:8080");
    // 前導零是有意放行的：0080 就是 80，parse 出來的值才算數
    assert_eq!(normalize_remote("0080"), "127.0.0.1:80");
    assert_eq!(normalize_remote("000001080"), "127.0.0.1:1080");
    // 補完的形式本身就是合法 remote，接得上既有驗證
    let list = vec![src("hk", vec![fwd("a", 1080)])];
    assert!(validate_forward(&list, None, "ok", 1090, "1080").is_none());
}

/// 純數字的守衛是必要的：`parse::<u16>()` 自己會放行 `+80`，
/// 補全時若把它當成 80，一個 ssh 不認得的字串就會靜靜變成合法值
#[test]
fn a_signed_number_is_not_a_bare_port() {
    assert_eq!(normalize_remote("+80"), "+80");
    assert_eq!(normalize_remote("-80"), "-80");
    let list = vec![src("hk", vec![fwd("a", 1080)])];
    assert!(err(&list, None, "ok", 1090, "+80").starts_with("remote: "));
    assert!(err(&list, None, "ok", 1090, "-80").starts_with("remote: "));
}

/// 越界的埠不做簡寫補全，一律擋在 remote 這欄（`0`／`70000` 都不是合法目的地）
#[test]
fn out_of_range_bare_port_is_rejected() {
    assert_eq!(normalize_remote("0"), "0");
    assert_eq!(normalize_remote("65536"), "65536");
    assert_eq!(normalize_remote("70000"), "70000");
    let list = vec![src("hk", vec![fwd("a", 1080)])];
    for bad in ["0", "65536", "70000", "999999999999"] {
        assert!(
            err(&list, None, "ok", 1090, bad).starts_with("remote: "),
            "{bad} 應該被擋在 remote 這欄"
        );
    }
}

/// 既有的 host:port 路徑不受影響：原樣通過，也原樣落檔
#[test]
fn host_port_remotes_pass_through_untouched() {
    assert_eq!(normalize_remote("127.0.0.1:1080"), "127.0.0.1:1080");
    assert_eq!(normalize_remote("example.com:22"), "example.com:22");
    assert_eq!(normalize_remote("10.0.0.5:8080"), "10.0.0.5:8080");
    // 不是純數字也不是 host:port 的照樣被擋，簡寫補全不會讓它變成合法值
    assert_eq!(normalize_remote("nope"), "nope");
    assert_eq!(normalize_remote("127.0.0.1"), "127.0.0.1");
    let list = vec![src("hk", vec![fwd("a", 1080)])];
    assert!(validate_forward(&list, None, "ok", 1090, "example.com:22").is_none());
    assert!(err(&list, None, "ok", 1090, "127.0.0.1").starts_with("remote: "));
}

/// 介面輸入 `8080`，產出的那一筆與落檔的那一行都必須是 `127.0.0.1:8080`。
///
/// upsert_forward 只是把 [`prepare_forward`] 的回傳值原樣存下去，所以這條
/// 從「輸入」一路釘到「檔案內容」的不變量就是那條路徑的規格。
#[test]
fn a_bare_port_from_the_ui_lands_in_the_file_as_the_full_form() {
    let list = vec![src("hk", vec![fwd("a", 1080)])];
    let made = prepare_forward(&list, None, "  web  ", 1090, "8080", true)
        .expect("補完的形式是合法 remote，應該過");
    // 產出：名字經過 trim，remote 是完整形式
    assert_eq!(
        made,
        Forward { name: "web".into(), local: 1090, remote: "127.0.0.1:8080".into(), enabled: true }
    );

    // 落檔：command 端就是把這一筆原樣塞進設定再存，這裡照做一次
    let dir = tmp_dir("normalize-remote");
    let mut cfg = Config { close_to_tray: true, check_for_updates: None, sources: list };
    cfg.sources[0].forwards.push(made.clone());
    write_config(&dir, &cfg).unwrap();
    let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
    assert!(saved.contains("remote = \"127.0.0.1:8080\""), "實際存成：{saved}");
    // 再讀回來還是同一筆，不會因為存檔或讀檔又變形
    assert_eq!(parse_config(&saved).unwrap(), cfg);
    assert_eq!(parse_config(&saved).unwrap().forward(1090).unwrap().remote, "127.0.0.1:8080");
}

/// 驗證不過時不給出半成品：呼叫端沒有東西可以誤存
#[test]
fn prepare_forward_hands_back_the_error_instead_of_a_forward() {
    let list = vec![src("hk", vec![fwd("a", 1080)])];
    // 撞埠、壞名字、壞 remote 都走同一個回傳
    let bad = |name: &str, local: u16, remote: &str| {
        prepare_forward(&list, None, name, local, remote, true).expect_err("這組輸入應該要被擋下來")
    };
    assert!(bad("b", 1080, "8080").starts_with("local: "));
    assert!(bad("", 1090, "8080").starts_with("name: "));
    assert!(bad("b", 1090, "70000").starts_with("remote: "));
    // enabled 原樣帶過去，前處理不擅自改使用者的連線選擇
    assert!(!prepare_forward(&list, None, "b", 1090, "8080", false).unwrap().enabled);
}

/// 檔案裡手寫 `remote = "8080"` 也算數：讀進來就是完整形式
#[test]
fn a_hand_written_bare_port_loads_as_the_full_form() {
    let cfg = parse_config(
        "[[sources]]\nname=\"hk\"\nhost=\"h\"\nuser=\"u\"\n\
         [[sources.forwards]]\nname=\"a\"\nlocal=1080\nremote=\"8080\"\n",
    )
    .expect("手寫純埠號是合法設定");
    assert_eq!(cfg.forward(1080).unwrap().remote, "127.0.0.1:8080");
}

/// 手寫純埠號的檔案：載入正常，重存之後檔案裡就變成完整形式
#[test]
fn rewriting_a_hand_written_bare_port_file_lands_the_full_form() {
    let dir = tmp_dir("bare-port-file");
    std::fs::write(
        dir.join(TOML_NAME),
        "closeToTray = true\n\n[[sources]]\nname = \"hk\"\nhost = \"h\"\nuser = \"u\"\n\n\
         # 這條註解要活過重存\n[[sources.forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"8080\"\n",
    )
    .unwrap();

    let out = load_from_dir(&dir);
    assert!(matches!(out, LoadOutcome::Loaded(_)), "純埠號不該被當成壞檔");
    let cfg = out.config().clone();
    assert_eq!(cfg.forward(1080).unwrap().remote, "127.0.0.1:8080");

    write_config(&dir, &cfg).unwrap();
    let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
    assert!(saved.contains("remote = \"127.0.0.1:8080\""), "重存後應該是完整形式：{saved}");
    assert!(!saved.contains("remote = \"8080\""));
    assert!(saved.contains("# 這條註解要活過重存"));
}

/// 驗證訊息要能被前端逐欄掛回去，格式固定是「欄位: 說明」
fn err(list: &[Source], orig: Option<u16>, name: &str, local: u16, remote: &str) -> String {
    validate_forward(list, orig, name, local, remote).expect("這組輸入應該要被擋下來")
}

#[test]
fn upsert_rejects_duplicate_local_port() {
    let list =
        vec![src("hk", vec![fwd("exit-tw", 1080), Forward { enabled: false, ..fwd("b", 1083) }])];
    // 新增撞到既有的，訊息要點名是誰佔走的
    assert_eq!(
        err(&list, None, "c", 1080, "127.0.0.1:1"),
        "local: port 1080 already used by exit-tw in hk"
    );
    // 連停用中的出口也算佔用
    assert!(err(&list, None, "c", 1083, "127.0.0.1:1").starts_with("local: "));
    // 沒撞到就過
    assert!(validate_forward(&list, None, "c", 1090, "127.0.0.1:1").is_none());
    // 編輯自己時維持原埠不算重複
    assert!(validate_forward(&list, Some(1080), "a2", 1080, "127.0.0.1:1").is_none());
    // 編輯時改成別人的埠要擋
    assert!(err(&list, Some(1080), "a2", 1083, "127.0.0.1:1").starts_with("local: "));
}

/// 撞到別的源的埠一樣要擋，而且訊息要點名那個源
#[test]
fn upsert_rejects_local_port_used_by_another_source() {
    let list = vec![src("hk", vec![fwd("a", 1080)]), src("tw", vec![fwd("b", 1083)])];
    let msg = err(&list, None, "c", 1083, "127.0.0.1:1");
    assert_eq!(msg, "local: port 1083 already used by b in tw");
    // 把 hk 的出口改成 tw 已經佔走的埠也要擋
    assert!(err(&list, Some(1080), "a", 1083, "127.0.0.1:1").starts_with("local: "));
}

#[test]
fn upsert_rejects_bad_name_and_remote() {
    let list = vec![src("hk", vec![fwd("a", 1080)])];
    assert!(err(&list, None, "", 1090, "127.0.0.1:1").starts_with("name: "));
    assert!(err(&list, None, "  ", 1090, "127.0.0.1:1").starts_with("name: "));
    assert!(err(&list, None, "two words", 1090, "127.0.0.1:1").starts_with("name: "));
    assert!(err(&list, None, "ok", 1090, "127.0.0.1").starts_with("remote: "));
    assert!(err(&list, None, "ok", 1090, "nope").starts_with("remote: "));
    assert!(err(&list, None, "ok", 0, "127.0.0.1:1").starts_with("local: "));
}

/// 編輯途中那條隧道被別處刪掉了。訊息整句都釘住：這是使用者在編輯面板上
/// 讀得到的字，用詞要跟介面一致（Tunnel，不是 exit）
#[test]
fn upsert_rejects_unknown_original_port() {
    let list = vec![src("hk", vec![fwd("a", 1080)])];
    assert_eq!(
        err(&list, Some(9999), "a", 1080, "127.0.0.1:1"),
        "local: no tunnel with port 9999, it may have been deleted"
    );
}

#[test]
fn source_validation_requires_name_host_user() {
    let list = vec![src("hk", vec![])];
    assert!(validate_source(&list, None, "tw", "h", "u").is_none());
    assert!(validate_source(&list, None, "", "h", "u").unwrap().starts_with("name: "));
    assert!(validate_source(&list, None, "  ", "h", "u").unwrap().starts_with("name: "));
    assert!(validate_source(&list, None, "two words", "h", "u").unwrap().starts_with("name: "));
    // 日誌行前綴是 [源名]，名字裡不可以再有中括號
    assert!(validate_source(&list, None, "a]b", "h", "u").unwrap().starts_with("name: "));
    assert!(parse_config("[[sources]]\nname=\"a]b\"\nhost=\"h\"\nuser=\"u\"\n").is_err());
    assert!(validate_source(&list, None, "tw", "  ", "u").unwrap().starts_with("host: "));
    assert!(validate_source(&list, None, "tw", "h", "").unwrap().starts_with("user: "));
}

#[test]
fn source_validation_rejects_duplicate_name() {
    let list = vec![src("hk", vec![]), src("tw", vec![])];
    assert_eq!(
        validate_source(&list, None, "hk", "h", "u").unwrap(),
        "name: connection hk already exists"
    );
    // 編輯自己時不算重複
    assert!(validate_source(&list, Some("hk"), "hk", "h", "u").is_none());
    // 改成別人的名字要擋
    assert!(validate_source(&list, Some("hk"), "tw", "h", "u").unwrap().starts_with("name: "));
    // 原本那組連線已經被刪掉，訊息整句釘住，用詞要跟介面一致（Connection，不是 source）
    assert_eq!(
        validate_source(&list, Some("gone"), "x", "h", "u").unwrap(),
        "name: no connection called gone, it may have been deleted"
    );
}
