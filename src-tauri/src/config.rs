//! 設定檔：TOML，放在執行檔同目錄的 traytunnel.toml。
//! 讀寫走 toml_edit，保留使用者手寫的註解與排版。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};

pub const TOML_NAME: &str = "traytunnel.toml";
pub const BROKEN_NAME: &str = "traytunnel.toml.broken";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Forward {
    pub name: String,
    pub local: u16,
    pub remote: String,
    /// 使用者是否要這個出口保持連線；舊設定檔沒有這個欄位時視為 true
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub host: String,
    pub user: String,
    #[serde(default)]
    pub proxy_command: String,
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    #[serde(default)]
    pub forwards: Vec<Forward>,
}

fn default_true() -> bool {
    true
}

impl Config {
    /// 依本地埠找出口，本地埠就是出口的唯一鍵
    pub fn forward(&self, local: u16) -> Option<&Forward> {
        self.forwards.iter().find(|f| f.local == local)
    }

    pub fn forward_mut(&mut self, local: u16) -> Option<&mut Forward> {
        self.forwards.iter_mut().find(|f| f.local == local)
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            host: "your-host.example.com".into(),
            user: "your-user".into(),
            proxy_command: "cloudflared access ssh --hostname %h".into(),
            close_to_tray: true,
            forwards: vec![
                Forward {
                    name: "exit-a".into(),
                    local: 1080,
                    remote: "127.0.0.1:1080".into(),
                    enabled: true,
                },
                Forward {
                    name: "exit-b".into(),
                    local: 1083,
                    remote: "127.0.0.1:1083".into(),
                    enabled: true,
                },
            ],
        }
    }
}

/// 讀取結果，供上層決定要不要提示使用者。
#[derive(Debug)]
pub enum LoadOutcome {
    /// 正常讀到既有設定
    Loaded(Config),
    /// 檔案不存在，已寫入預設值
    Created(Config),
    /// 解析或讀取失敗，改用預設值且未覆寫原檔；backup 只在確實備份成功時有值
    Broken { config: Config, backup: Option<PathBuf>, error: String },
}

impl LoadOutcome {
    pub fn config(&self) -> &Config {
        match self {
            LoadOutcome::Loaded(c)
            | LoadOutcome::Created(c)
            | LoadOutcome::Broken { config: c, .. } => c,
        }
    }
}

/// 預設設定檔內容，附中文註解。
pub fn default_document() -> String {
    let c = Config::default();
    format!(
        "# traytunnel 設定檔，與執行檔放在同一個資料夾。\n\
         # 修改後可直接在設定介面存檔，或重新啟動程式。\n\
         host = \"{host}\"\n\
         user = \"{user}\"\n\
         # 不需要 ProxyCommand 時留空字串即可。\n\
         proxyCommand = \"{proxy}\"\n\
         # 關閉鈕（X）是否只隱藏到系統匣。\n\
         closeToTray = {close}\n\
         \n\
         # 每個 [[forwards]] 是一組本地埠轉發，各自跑一條獨立的 ssh 連線。\n\
         # enabled 記錄使用者最後一次的連線／中斷選擇，省略時視為 true。\n\
         [[forwards]]\n\
         name = \"exit-a\"\n\
         local = 1080\n\
         remote = \"127.0.0.1:1080\"\n\
         enabled = true\n\
         \n\
         [[forwards]]\n\
         name = \"exit-b\"\n\
         local = 1083\n\
         remote = \"127.0.0.1:1083\"\n\
         enabled = true\n",
        host = c.host,
        user = c.user,
        proxy = c.proxy_command,
        close = c.close_to_tray,
    )
}

/// 從指定資料夾讀設定，檔案不存在就寫一份預設值。
pub fn load_from_dir(dir: &Path) -> LoadOutcome {
    let toml_path = dir.join(TOML_NAME);

    if toml_path.exists() {
        let raw = match std::fs::read_to_string(&toml_path) {
            Ok(s) => s,
            Err(e) => return broken(dir, &toml_path, format!("讀取失敗：{e}")),
        };
        return match parse_config(&raw) {
            Ok(cfg) => LoadOutcome::Loaded(cfg),
            Err(e) => broken(dir, &toml_path, e),
        };
    }

    let cfg = Config::default();
    let _ = std::fs::write(&toml_path, default_document());
    LoadOutcome::Created(cfg)
}

/// 用 PowerShell 之類的工具存檔可能會帶 UTF-8 BOM，解析前先剝掉。
fn strip_bom(raw: &str) -> &str {
    raw.strip_prefix('\u{feff}').unwrap_or(raw)
}

fn broken(dir: &Path, toml_path: &Path, error: String) -> LoadOutcome {
    // 絕不覆寫壞掉的設定檔，只複製一份備份出來；連檔案都讀不到時就沒有備份可言
    let target = dir.join(BROKEN_NAME);
    let backup = std::fs::copy(toml_path, &target).ok().map(|_| target);
    LoadOutcome::Broken { config: Config::default(), backup, error }
}

pub fn parse_config(raw: &str) -> Result<Config, String> {
    let doc: DocumentMut = strip_bom(raw).parse::<DocumentMut>().map_err(|e| e.to_string())?;
    let cfg: Config = toml_edit::de::from_document(doc).map_err(|e| e.to_string())?;
    if cfg.host.trim().is_empty() || cfg.user.trim().is_empty() {
        return Err("host 與 user 不可為空".into());
    }
    Ok(cfg)
}

/// 寫回設定，沿用既有檔案的註解與排版。
///
/// `[[forwards]]` 逐張桌就地改寫（多的砍掉、少的補上），使用者寫在單一
/// forward 上方的註解才不會因為存一次檔就整批消失。
pub fn write_config(dir: &Path, cfg: &Config) -> std::io::Result<()> {
    let path = dir.join(TOML_NAME);
    let mut doc = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| strip_bom(&s).parse::<DocumentMut>().ok())
        .unwrap_or_else(|| default_document().parse::<DocumentMut>().unwrap());

    doc["host"] = value(cfg.host.as_str());
    doc["user"] = value(cfg.user.as_str());
    doc["proxyCommand"] = value(cfg.proxy_command.as_str());
    doc["closeToTray"] = value(cfg.close_to_tray);

    if !matches!(doc.get("forwards"), Some(Item::ArrayOfTables(_))) {
        doc["forwards"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    if let Some(Item::ArrayOfTables(tables)) = doc.get_mut("forwards") {
        while tables.len() > cfg.forwards.len() {
            tables.remove(tables.len() - 1);
        }
        for (i, f) in cfg.forwards.iter().enumerate() {
            if i >= tables.len() {
                tables.push(Table::new());
            }
            let t = tables.get_mut(i).expect("剛補齊過，一定拿得到");
            t["name"] = value(f.name.as_str());
            t["local"] = value(f.local as i64);
            t["remote"] = value(f.remote.as_str());
            t["enabled"] = value(f.enabled);
        }
    }

    std::fs::write(path, doc.to_string())
}

/// remote 必須符合 `^[^:\s]+:\d+$`：主機不含冒號與空白，埠是純數字。
pub fn valid_remote(s: &str) -> bool {
    match s.split_once(':') {
        Some((h, p)) => {
            !h.is_empty()
                && !h.chars().any(|c| c.is_whitespace())
                && !p.is_empty()
                && p.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// 出口名稱必須非空且不含空白
pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c.is_whitespace())
}

/// 新增／編輯出口的欄位驗證，回傳 Some(訊息) 代表不通過。
///
/// `original_local` 是編輯前的本地埠，None 代表新增。本地埠是出口的唯一鍵，
/// 因此連停用中的出口也算佔用，不可重複。
///
/// 訊息一律以欄位名開頭（`name: `／`local: `／`remote: `），前端才能把錯誤
/// 掛回對應的欄位上。
pub fn validate_forward(
    forwards: &[Forward],
    original_local: Option<u16>,
    name: &str,
    local: u16,
    remote: &str,
) -> Option<String> {
    if let Some(orig) = original_local {
        if !forwards.iter().any(|f| f.local == orig) {
            return Some(format!("local: no exit with port {orig}, it may have been deleted"));
        }
    }
    if !valid_name(name) {
        return Some("name: required, and must not contain spaces".into());
    }
    if local == 0 {
        return Some("local: port must be between 1 and 65535".into());
    }
    if !valid_remote(remote) {
        return Some("remote: must look like host:port, for example 127.0.0.1:1080".into());
    }
    let clash = forwards
        .iter()
        .find(|f| f.local == local && Some(f.local) != original_local);
    if let Some(other) = clash {
        return Some(format!("local: port {local} already used by {}", other.name));
    }
    None
}

/// 全域連線欄位的驗證，回傳 Some(訊息) 代表不通過。
pub fn validate_global(host: &str, user: &str) -> Option<String> {
    if host.trim().is_empty() {
        return Some("Host is required.".into());
    }
    if user.trim().is_empty() {
        return Some("User is required.".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn tmp_dir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("traytunnel-test-{}-{}-{}", std::process::id(), tag, n));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn fwd(name: &str, local: u16) -> Forward {
        Forward { name: name.into(), local, remote: "127.0.0.1:1080".into(), enabled: true }
    }

    #[test]
    fn parses_toml_with_comments() {
        let raw = r#"
# 我的註解
host = "h.example.com"
user = "bob"
proxyCommand = "cloudflared access ssh --hostname %h"
closeToTray = false

[[forwards]]
name = "a"
local = 1080
remote = "127.0.0.1:1080"
"#;
        let cfg = parse_config(raw).unwrap();
        assert_eq!(cfg.host, "h.example.com");
        assert_eq!(cfg.user, "bob");
        assert!(!cfg.close_to_tray);
        assert_eq!(cfg.forwards.len(), 1);
        assert_eq!(cfg.forwards[0].local, 1080);
    }

    /// 舊設定檔沒有 enabled 欄位，一律當成啟用，升級後不會有人的出口莫名不連
    #[test]
    fn forward_enabled_defaults_to_true() {
        let raw = "host = \"h\"\nuser = \"u\"\n\n[[forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\n";
        let cfg = parse_config(raw).unwrap();
        assert!(cfg.forwards[0].enabled);
    }

    #[test]
    fn forward_enabled_is_read_back() {
        let raw = "host = \"h\"\nuser = \"u\"\n\n[[forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\nenabled = false\n";
        let cfg = parse_config(raw).unwrap();
        assert!(!cfg.forwards[0].enabled);
    }

    #[test]
    fn write_persists_enabled_flag() {
        let dir = tmp_dir("enabled-write");
        let mut cfg = Config::default();
        cfg.forwards[1].enabled = false;
        write_config(&dir, &cfg).unwrap();
        let back = parse_config(&std::fs::read_to_string(dir.join(TOML_NAME)).unwrap()).unwrap();
        assert!(back.forwards[0].enabled);
        assert!(!back.forwards[1].enabled);
        assert_eq!(back, cfg);
    }

    /// 只改一個 forward 的 enabled 時，寫在該筆上方的註解要留著
    #[test]
    fn write_keeps_per_forward_comments() {
        let dir = tmp_dir("forward-comments");
        std::fs::write(
            dir.join(TOML_NAME),
            "host = \"h\"\nuser = \"u\"\n\n# 這是 A 出口\n[[forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\n",
        )
        .unwrap();
        let mut cfg = parse_config(&std::fs::read_to_string(dir.join(TOML_NAME)).unwrap()).unwrap();
        cfg.forwards[0].enabled = false;
        write_config(&dir, &cfg).unwrap();
        let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
        assert!(saved.contains("# 這是 A 出口"));
        assert!(saved.contains("enabled = false"));
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let cfg = parse_config("host = \"h\"\nuser = \"u\"\n").unwrap();
        assert!(cfg.close_to_tray);
        assert_eq!(cfg.proxy_command, "");
        assert!(cfg.forwards.is_empty());
    }

    #[test]
    fn rejects_empty_host_or_user() {
        assert!(parse_config("host = \"\"\nuser = \"u\"\n").is_err());
        assert!(parse_config("host = \"h\"\nuser = \"   \"\n").is_err());
        // 缺欄位一樣要擋下來，不能靜靜地變成空字串
        assert!(parse_config("user = \"u\"\n").is_err());
    }

    #[test]
    fn empty_host_makes_the_file_broken_not_overwritten() {
        let dir = tmp_dir("empty-host");
        let raw = "host = \"\"\nuser = \"u\"\n";
        std::fs::write(dir.join(TOML_NAME), raw).unwrap();
        let out = load_from_dir(&dir);
        assert!(matches!(out, LoadOutcome::Broken { .. }));
        assert_eq!(std::fs::read_to_string(dir.join(TOML_NAME)).unwrap(), raw);
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

    /// PowerShell 5 存檔會帶 UTF-8 BOM，不能因此就把設定當成壞檔
    #[test]
    fn parses_toml_with_utf8_bom() {
        let raw = "\u{feff}host = \"bom.example.com\"\nuser = \"bob\"\n\n[[forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\n";
        let cfg = parse_config(raw).unwrap();
        assert_eq!(cfg.host, "bom.example.com");
        assert_eq!(cfg.forwards.len(), 1);
    }

    #[test]
    fn loads_bom_file_from_disk_without_treating_it_as_broken() {
        let dir = tmp_dir("bom");
        std::fs::write(
            dir.join(TOML_NAME),
            "\u{feff}host = \"bom.example.com\"\nuser = \"bob\"\nproxyCommand = \"\"\ncloseToTray = false\n\n[[forwards]]\nname = \"a\"\nlocal = 1080\nremote = \"127.0.0.1:1080\"\n",
        )
        .unwrap();
        let out = load_from_dir(&dir);
        assert!(matches!(out, LoadOutcome::Loaded(_)));
        assert_eq!(out.config().host, "bom.example.com");
        assert!(!out.config().close_to_tray);
        // 不該產生壞檔備份
        assert!(!dir.join(BROKEN_NAME).exists());
    }

    /// 帶 BOM 的檔案存檔後也要保留註解，不能退回預設模板
    #[test]
    fn write_keeps_comments_of_bom_file() {
        let dir = tmp_dir("bom-write");
        std::fs::write(
            dir.join(TOML_NAME),
            "\u{feff}# 保留我\nhost = \"a\"\nuser = \"b\"\nproxyCommand = \"\"\ncloseToTray = true\n",
        )
        .unwrap();
        let mut cfg = parse_config(&std::fs::read_to_string(dir.join(TOML_NAME)).unwrap()).unwrap();
        cfg.host = "c.example.com".into();
        write_config(&dir, &cfg).unwrap();
        let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
        assert!(saved.contains("# 保留我"));
        assert!(saved.contains("c.example.com"));
    }

    #[test]
    fn broken_file_is_backed_up_and_never_overwritten() {
        let dir = tmp_dir("broken");
        let bad = "host = \"h\"\nthis is not toml @@@\n";
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

    #[test]
    fn write_preserves_comments() {
        let dir = tmp_dir("write");
        let raw = "# 保留我\nhost = \"a\"\nuser = \"b\"\nproxyCommand = \"\"\ncloseToTray = true\n";
        std::fs::write(dir.join(TOML_NAME), raw).unwrap();
        let mut cfg = parse_config(raw).unwrap();
        cfg.host = "c.example.com".into();
        cfg.forwards = vec![Forward {
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

    /// 刪出口後檔案裡不該留下多餘的 [[forwards]]
    #[test]
    fn write_drops_removed_forwards() {
        let dir = tmp_dir("shrink");
        let mut cfg = Config::default();
        write_config(&dir, &cfg).unwrap();
        cfg.forwards.remove(1);
        write_config(&dir, &cfg).unwrap();
        let back = parse_config(&std::fs::read_to_string(dir.join(TOML_NAME)).unwrap()).unwrap();
        assert_eq!(back.forwards.len(), 1);
        assert_eq!(back.forwards[0].local, 1080);
    }

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

    /// 驗證訊息要能被前端逐欄掛回去，格式固定是「欄位: 說明」
    fn err(list: &[Forward], orig: Option<u16>, name: &str, local: u16, remote: &str) -> String {
        validate_forward(list, orig, name, local, remote).expect("這組輸入應該要被擋下來")
    }

    #[test]
    fn upsert_rejects_duplicate_local_port() {
        let list = vec![fwd("exit-tw", 1080), Forward { enabled: false, ..fwd("b", 1083) }];
        // 新增撞到既有的，訊息要點名是誰佔走的
        assert_eq!(
            err(&list, None, "c", 1080, "127.0.0.1:1"),
            "local: port 1080 already used by exit-tw"
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

    #[test]
    fn upsert_rejects_bad_name_and_remote() {
        let list = vec![fwd("a", 1080)];
        assert!(err(&list, None, "", 1090, "127.0.0.1:1").starts_with("name: "));
        assert!(err(&list, None, "  ", 1090, "127.0.0.1:1").starts_with("name: "));
        assert!(err(&list, None, "two words", 1090, "127.0.0.1:1").starts_with("name: "));
        assert!(err(&list, None, "ok", 1090, "127.0.0.1").starts_with("remote: "));
        assert!(err(&list, None, "ok", 1090, "nope").starts_with("remote: "));
        assert!(err(&list, None, "ok", 0, "127.0.0.1:1").starts_with("local: "));
    }

    #[test]
    fn upsert_rejects_unknown_original_port() {
        let list = vec![fwd("a", 1080)];
        assert!(err(&list, Some(9999), "a", 1080, "127.0.0.1:1").starts_with("local: "));
    }

    #[test]
    fn global_validation_requires_host_and_user() {
        assert!(validate_global("h", "u").is_none());
        assert!(validate_global("  ", "u").is_some());
        assert!(validate_global("h", "").is_some());
    }
}
