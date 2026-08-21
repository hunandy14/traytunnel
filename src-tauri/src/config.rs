//! 設定檔：TOML，放在執行檔同目錄的 traytunnel.toml。
//! 讀寫走 toml_edit，保留使用者手寫的註解與排版。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};

pub const TOML_NAME: &str = "traytunnel.toml";
pub const JSON_NAME: &str = "traytunnel.json";
pub const JSON_BAK_NAME: &str = "traytunnel.json.bak";
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
    /// 由舊的 traytunnel.json 轉換而來
    Migrated(Config),
    /// 解析失敗，壞檔已備份，改用預設值且未覆寫原檔
    Broken { config: Config, backup: PathBuf, error: String },
}

impl LoadOutcome {
    pub fn config(&self) -> &Config {
        match self {
            LoadOutcome::Loaded(c)
            | LoadOutcome::Created(c)
            | LoadOutcome::Migrated(c)
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

/// 從指定資料夾讀設定，必要時做 json→toml 遷移。
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

    // 舊版 json 設定：轉換一次，並把舊檔改名保存
    let json_path = dir.join(JSON_NAME);
    if json_path.exists() {
        if let Ok(raw) = std::fs::read_to_string(&json_path) {
            if let Some(cfg) = parse_legacy_json(&raw) {
                let _ = write_config(dir, &cfg);
                let _ = std::fs::rename(&json_path, dir.join(JSON_BAK_NAME));
                return LoadOutcome::Migrated(cfg);
            }
        }
    }

    let cfg = Config::default();
    let _ = std::fs::write(&toml_path, default_document());
    LoadOutcome::Created(cfg)
}

fn broken(dir: &Path, toml_path: &Path, error: String) -> LoadOutcome {
    // 絕不覆寫壞掉的設定檔，只複製一份備份出來
    let backup = dir.join(BROKEN_NAME);
    let _ = std::fs::copy(toml_path, &backup);
    LoadOutcome::Broken { config: Config::default(), backup, error }
}

pub fn parse_config(raw: &str) -> Result<Config, String> {
    let doc: DocumentMut = raw.parse::<DocumentMut>().map_err(|e| e.to_string())?;
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
        .and_then(|s| s.parse::<DocumentMut>().ok())
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

/// 舊版 json 設定的寬鬆解析，local 允許數字或字串。
fn parse_legacy_json(raw: &str) -> Option<Config> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let host = s("host");
    let user = s("user");
    if host.is_empty() || user.is_empty() {
        return None;
    }
    let mut forwards = Vec::new();
    if let Some(arr) = v.get("forwards").and_then(|x| x.as_array()) {
        for f in arr {
            let name = f.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let remote = f.get("remote").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let local = match f.get("local") {
                Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0) as u16,
                Some(serde_json::Value::String(t)) => t.parse::<u16>().unwrap_or(0),
                _ => 0,
            };
            if name.is_empty() || remote.is_empty() || local == 0 {
                continue;
            }
            forwards.push(Forward { name, local, remote });
        }
    }
    Some(Config {
        host,
        user,
        proxy_command: s("proxyCommand"),
        close_to_tray: v.get("closeToTray").and_then(|x| x.as_bool()).unwrap_or(true),
        forwards,
    })
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

    #[test]
    fn migrates_legacy_json() {
        let dir = tmp_dir("migrate");
        std::fs::write(
            dir.join(JSON_NAME),
            r#"{"host":"old.example.com","user":"alice","proxyCommand":"cloudflared access ssh --hostname %h","closeToTray":false,
                "forwards":[{"name":"exit-a","local":1080,"remote":"127.0.0.1:1080"}]}"#,
        )
        .unwrap();
        let out = load_from_dir(&dir);
        assert!(matches!(out, LoadOutcome::Migrated(_)));
        let cfg = out.config();
        assert_eq!(cfg.host, "old.example.com");
        assert_eq!(cfg.user, "alice");
        assert!(!cfg.close_to_tray);
        assert_eq!(cfg.forwards.len(), 1);
        assert!(dir.join(TOML_NAME).exists());
        assert!(!dir.join(JSON_NAME).exists());
        assert!(dir.join(JSON_BAK_NAME).exists());
        // 轉出來的 toml 要能被自己讀回來
        let again = load_from_dir(&dir);
        assert_eq!(again.config(), cfg);
    }

    #[test]
    fn broken_file_is_backed_up_and_never_overwritten() {
        let dir = tmp_dir("broken");
        let bad = "host = \"h\"\nthis is not toml @@@\n";
        std::fs::write(dir.join(TOML_NAME), bad).unwrap();
        let out = load_from_dir(&dir);
        match &out {
            LoadOutcome::Broken { backup, .. } => assert!(backup.exists()),
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
