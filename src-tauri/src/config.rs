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

impl Default for Config {
    fn default() -> Self {
        Config {
            host: "your-host.example.com".into(),
            user: "your-user".into(),
            proxy_command: "cloudflared access ssh --hostname %h".into(),
            close_to_tray: true,
            forwards: vec![
                Forward { name: "exit-a".into(), local: 1080, remote: "127.0.0.1:1080".into() },
                Forward { name: "exit-b".into(), local: 1083, remote: "127.0.0.1:1083".into() },
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
    /// 解析失敗，壞檔已備份，改用預設值且未覆寫原檔
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
         # 修改後可直接在設定介面按 Save，或重新啟動程式。\n\
         host = \"{host}\"\n\
         user = \"{user}\"\n\
         # 不需要 ProxyCommand 時留空字串即可。\n\
         proxyCommand = \"{proxy}\"\n\
         # 關閉鈕（X）是否只隱藏到系統匣。\n\
         closeToTray = {close}\n\
         \n\
         # 每個 [[forwards]] 是一組本地埠轉發，第一個埠會用來判斷隧道是否連上。\n\
         [[forwards]]\n\
         name = \"exit-a\"\n\
         local = 1080\n\
         remote = \"127.0.0.1:1080\"\n\
         \n\
         [[forwards]]\n\
         name = \"exit-b\"\n\
         local = 1083\n\
         remote = \"127.0.0.1:1083\"\n",
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

    let mut tables = ArrayOfTables::new();
    for f in &cfg.forwards {
        let mut t = Table::new();
        t["name"] = value(f.name.as_str());
        t["local"] = value(f.local as i64);
        t["remote"] = value(f.remote.as_str());
        tables.push(t);
    }
    doc["forwards"] = Item::ArrayOfTables(tables);

    std::fs::write(path, doc.to_string())
}

/// 由設定介面的多行文字解析 forwards，格式為「name local remote」。
pub fn parse_forward_lines(text: &str) -> Result<Vec<Forward>, String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let ok = parts.len() == 3
            && parts[1].chars().all(|c| c.is_ascii_digit())
            && !parts[1].is_empty()
            && valid_remote(parts[2]);
        if !ok {
            return Err(line.to_string());
        }
        let local: u16 = parts[1].parse().map_err(|_| line.to_string())?;
        out.push(Forward { name: parts[0].into(), local, remote: parts[2].into() });
    }
    Ok(out)
}

/// remote 必須是 host:port，host 不含冒號或空白
fn valid_remote(s: &str) -> bool {
    match s.rsplit_once(':') {
        Some((h, p)) => {
            !h.is_empty()
                && !h.contains(':')
                && !h.chars().any(|c| c.is_whitespace())
                && !p.is_empty()
                && p.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
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
        cfg.forwards = vec![Forward { name: "x".into(), local: 1090, remote: "127.0.0.1:9".into() }];
        write_config(&dir, &cfg).unwrap();
        let saved = std::fs::read_to_string(dir.join(TOML_NAME)).unwrap();
        assert!(saved.contains("# 保留我"));
        assert!(saved.contains("c.example.com"));
        let back = parse_config(&saved).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn forward_lines_round_trip() {
        let f = parse_forward_lines("exit-a  1080  127.0.0.1:1080\n\nexit-b 1083 example.com:1083\n").unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[1].name, "exit-b");
        assert_eq!(f[1].local, 1083);
        assert_eq!(f[1].remote, "example.com:1083");
    }

    #[test]
    fn forward_lines_reject_bad_input() {
        assert!(parse_forward_lines("exit-a 1080").is_err());
        assert!(parse_forward_lines("exit-a abc 127.0.0.1:1080").is_err());
        assert!(parse_forward_lines("exit-a 1080 127.0.0.1").is_err());
        assert_eq!(parse_forward_lines("bad line here now").unwrap_err(), "bad line here now");
    }
}
