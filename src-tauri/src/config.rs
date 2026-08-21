//! 設定檔：TOML，放在執行檔同目錄的 traytunnel.toml。
//! 讀寫走 toml_edit，保留使用者手寫的註解與排版。
//!
//! 契約 v3 起改成多連線源：頂層只剩 closeToTray，其餘全部收進 `[[sources]]`，
//! 每個源自己帶 host／user／proxyCommand 與巢狀的 `[[sources.forwards]]`。
//! 本地埠（local）是出口的全域唯一鍵，跨源也不得重複。

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
    /// 使用者是否要這個出口保持連線；設定檔沒有這個欄位時視為 true
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// 一個連線源：一組 ssh 連線參數，底下掛著自己的轉發出口
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub name: String,
    pub host: String,
    pub user: String,
    #[serde(default)]
    pub proxy_command: String,
    #[serde(default)]
    pub forwards: Vec<Forward>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    #[serde(default)]
    pub sources: Vec<Source>,
}

/// 舊制（契約 v2）的設定檔長相，只在自動遷移時用得到
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyConfig {
    host: String,
    user: String,
    #[serde(default)]
    proxy_command: String,
    #[serde(default = "default_true")]
    close_to_tray: bool,
    #[serde(default)]
    forwards: Vec<Forward>,
}

fn default_true() -> bool {
    true
}

impl Source {
    pub fn forward(&self, local: u16) -> Option<&Forward> {
        self.forwards.iter().find(|f| f.local == local)
    }
}

impl Config {
    /// 依本地埠找出口，本地埠是出口的全域唯一鍵
    pub fn forward(&self, local: u16) -> Option<&Forward> {
        self.sources.iter().find_map(|s| s.forward(local))
    }

    pub fn forward_mut(&mut self, local: u16) -> Option<&mut Forward> {
        self.sources
            .iter_mut()
            .find_map(|s| s.forwards.iter_mut().find(|f| f.local == local))
    }

    /// 出口所屬的源
    pub fn source_of(&self, local: u16) -> Option<&Source> {
        self.sources.iter().find(|s| s.forward(local).is_some())
    }

    /// 出口所屬源的名字，找不到時給個好認的替代字串（只用在日誌上）
    pub fn source_name_of(&self, local: u16) -> Option<&str> {
        self.source_of(local).map(|s| s.name.as_str())
    }

    /// 同時取出口與它所屬的源
    pub fn locate(&self, local: u16) -> Option<(&Source, &Forward)> {
        self.sources.iter().find_map(|s| s.forward(local).map(|f| (s, f)))
    }

    pub fn source(&self, name: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.name == name)
    }

    pub fn source_mut(&mut self, name: &str) -> Option<&mut Source> {
        self.sources.iter_mut().find(|s| s.name == name)
    }

    /// 所有出口的本地埠，順序照設定檔
    pub fn locals(&self) -> Vec<u16> {
        self.sources.iter().flat_map(|s| s.forwards.iter().map(|f| f.local)).collect()
    }

    /// 所有 enabled 出口的本地埠
    pub fn enabled_locals(&self) -> Vec<u16> {
        self.sources
            .iter()
            .flat_map(|s| s.forwards.iter().filter(|f| f.enabled).map(|f| f.local))
            .collect()
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            close_to_tray: true,
            sources: vec![Source {
                name: "your-host".into(),
                host: "your-host.example.com".into(),
                user: "your-user".into(),
                proxy_command: "cloudflared access ssh --hostname %h".into(),
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
            }],
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
    /// 讀到舊制設定，已就地遷移成新制並寫回
    Migrated(Config),
    /// 解析或讀取失敗，改用預設值且未覆寫原檔；backup 只在確實備份成功時有值
    Broken { config: Config, backup: Option<PathBuf>, error: String },
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
    let s = &c.sources[0];
    format!(
        "# traytunnel 設定檔，與執行檔放在同一個資料夾。\n\
         # 修改後可直接在設定介面存檔，或重新啟動程式。\n\
         \n\
         # 關閉鈕（X）是否只隱藏到系統匣。\n\
         closeToTray = {close}\n\
         \n\
         # 每個 [[sources]] 是一組 ssh 連線參數，底下可以掛多個轉發出口。\n\
         [[sources]]\n\
         name = \"{sname}\"\n\
         host = \"{host}\"\n\
         user = \"{user}\"\n\
         # 不需要 ProxyCommand 時留空字串即可。\n\
         proxyCommand = \"{proxy}\"\n\
         \n\
         # 每個 [[sources.forwards]] 是一組本地埠轉發，各自跑一條獨立的 ssh 連線。\n\
         # local 是出口的唯一鍵，跨源也不可以重複。\n\
         # enabled 記錄使用者最後一次的連線／中斷選擇，省略時視為 true。\n\
         [[sources.forwards]]\n\
         name = \"{fa}\"\n\
         local = {la}\n\
         remote = \"{ra}\"\n\
         enabled = true\n\
         \n\
         [[sources.forwards]]\n\
         name = \"{fb}\"\n\
         local = {lb}\n\
         remote = \"{rb}\"\n\
         enabled = true\n",
        close = c.close_to_tray,
        sname = s.name,
        host = s.host,
        user = s.user,
        proxy = s.proxy_command,
        fa = s.forwards[0].name,
        la = s.forwards[0].local,
        ra = s.forwards[0].remote,
        fb = s.forwards[1].name,
        lb = s.forwards[1].local,
        rb = s.forwards[1].remote,
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
        return match parse_document(&raw) {
            Ok((cfg, migrated)) => {
                if migrated {
                    // 遷移只改結構，寫回時走同一套就地改寫，註解照樣留著
                    let _ = write_config(dir, &cfg);
                    LoadOutcome::Migrated(cfg)
                } else {
                    LoadOutcome::Loaded(cfg)
                }
            }
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

/// 頂層還有 host 欄位就是舊制設定檔
fn is_legacy(doc: &DocumentMut) -> bool {
    doc.get("host").is_some()
}

impl LegacyConfig {
    /// 舊制包成單一 source，源名預設用 host 的值
    fn into_config(self) -> Config {
        Config {
            close_to_tray: self.close_to_tray,
            sources: vec![Source {
                name: self.host.trim().to_string(),
                host: self.host.trim().to_string(),
                user: self.user.trim().to_string(),
                proxy_command: self.proxy_command,
                forwards: self.forwards,
            }],
        }
    }
}

/// 解析設定檔，回傳 (設定, 是否來自舊制)。
pub fn parse_document(raw: &str) -> Result<(Config, bool), String> {
    let doc: DocumentMut = strip_bom(raw).parse::<DocumentMut>().map_err(|e| e.to_string())?;
    let legacy = is_legacy(&doc);
    let cfg = if legacy {
        let old: LegacyConfig =
            toml_edit::de::from_document(doc).map_err(|e| e.to_string())?;
        old.into_config()
    } else {
        toml_edit::de::from_document(doc).map_err(|e| e.to_string())?
    };
    validate_config(&cfg)?;
    Ok((cfg, legacy))
}

/// 只要設定不管遷移旗標的簡便版
#[cfg(test)]
pub fn parse_config(raw: &str) -> Result<Config, String> {
    parse_document(raw).map(|(cfg, _)| cfg)
}

/// 讀進來的設定必須自洽，否則寧可當壞檔也不要帶著矛盾的狀態跑
fn validate_config(cfg: &Config) -> Result<(), String> {
    let mut seen_names: Vec<&str> = Vec::new();
    let mut seen_locals: Vec<u16> = Vec::new();
    for s in &cfg.sources {
        if !valid_source_name(&s.name) {
            return Err("source 的 name 不可為空，也不可含空白或中括號".into());
        }
        if seen_names.contains(&s.name.as_str()) {
            return Err(format!("source 名稱重複：{}", s.name));
        }
        seen_names.push(&s.name);
        if s.host.trim().is_empty() || s.user.trim().is_empty() {
            return Err(format!("source {} 的 host 與 user 不可為空", s.name));
        }
        for f in &s.forwards {
            if seen_locals.contains(&f.local) {
                return Err(format!("本地埠重複：{}（跨源也不可以重複）", f.local));
            }
            seen_locals.push(f.local);
        }
    }
    Ok(())
}

/// 把舊制的文件結構就地改成新制：頂層連線欄位收成一個 `[[sources]]`，
/// 原本的 `[[forwards]]` 整批搬進去變成 `[[sources.forwards]]`。
///
/// 值不在這裡填，後面的 sync 會照設定物件覆寫一次；這裡只負責搬結構，
/// 所以寫在單筆 forward 上方的註解、檔頭的註解都跟著搬過去。
fn migrate_document(doc: &mut DocumentMut) {
    let lead = doc
        .as_table()
        .key("host")
        .and_then(|k| k.leaf_decor().prefix().and_then(|s| s.as_str()))
        .unwrap_or("")
        .to_string();

    let forwards = doc.remove("forwards");
    doc.remove("host");
    doc.remove("user");
    doc.remove("proxyCommand");

    // 檔頭註解原本掛在 host 上，改掛到 closeToTray，才會繼續留在檔案最上面
    if !lead.trim().is_empty() {
        if doc.get("closeToTray").is_none() {
            doc["closeToTray"] = value(true);
        }
        if let Some(mut k) = doc.as_table_mut().key_mut("closeToTray") {
            let own = k.leaf_decor().prefix().and_then(|s| s.as_str()).unwrap_or("").to_string();
            k.leaf_decor_mut().set_prefix(format!("{lead}{own}"));
        }
    }

    let mut t = Table::new();
    if let Some(Item::ArrayOfTables(a)) = forwards {
        t.insert("forwards", Item::ArrayOfTables(a));
    }
    let mut arr = ArrayOfTables::new();
    arr.push(t);
    doc.insert("sources", Item::ArrayOfTables(arr));
}

/// 逐張桌就地改寫（多的砍掉、少的補上），使用者寫在單筆上方的註解才不會消失
fn sync_forwards(tables: &mut ArrayOfTables, forwards: &[Forward]) {
    while tables.len() > forwards.len() {
        tables.remove(tables.len() - 1);
    }
    for (i, f) in forwards.iter().enumerate() {
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

/// 寫回設定，沿用既有檔案的註解與排版。
///
/// `[[sources]]` 與巢狀的 `[[sources.forwards]]` 都逐張桌就地改寫；讀到的是
/// 舊制檔案時先把結構遷移成新制再寫。
pub fn write_config(dir: &Path, cfg: &Config) -> std::io::Result<()> {
    let path = dir.join(TOML_NAME);
    let mut doc = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| strip_bom(&s).parse::<DocumentMut>().ok())
        .unwrap_or_else(|| default_document().parse::<DocumentMut>().unwrap());

    if is_legacy(&doc) {
        migrate_document(&mut doc);
    }

    doc["closeToTray"] = value(cfg.close_to_tray);

    if !matches!(doc.get("sources"), Some(Item::ArrayOfTables(_))) {
        doc["sources"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    if let Some(Item::ArrayOfTables(tables)) = doc.get_mut("sources") {
        while tables.len() > cfg.sources.len() {
            tables.remove(tables.len() - 1);
        }
        for (i, s) in cfg.sources.iter().enumerate() {
            if i >= tables.len() {
                tables.push(Table::new());
            }
            let t = tables.get_mut(i).expect("剛補齊過，一定拿得到");
            t["name"] = value(s.name.as_str());
            t["host"] = value(s.host.as_str());
            t["user"] = value(s.user.as_str());
            t["proxyCommand"] = value(s.proxy_command.as_str());
            if !matches!(t.get("forwards"), Some(Item::ArrayOfTables(_))) {
                t["forwards"] = Item::ArrayOfTables(ArrayOfTables::new());
            }
            if let Some(Item::ArrayOfTables(fts)) = t.get_mut("forwards") {
                sync_forwards(fts, &s.forwards);
            }
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

/// 名稱必須非空且不含空白，源名與出口名共用這條規則
pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c.is_whitespace())
}

/// 源名還多一條限制：不可含中括號。日誌行前綴是 `[源名]`，
/// 名字裡再冒出一個 `]` 會讓前端切不出正確的源名。
pub fn valid_source_name(s: &str) -> bool {
    valid_name(s) && !s.contains('[') && !s.contains(']')
}

/// 新增／編輯出口的欄位驗證，回傳 Some(訊息) 代表不通過。
///
/// `original_local` 是編輯前的本地埠，None 代表新增。本地埠是出口的全域唯一鍵，
/// 因此連停用中的、別的源底下的出口也算佔用。
///
/// 訊息一律以欄位名開頭（`name: `／`local: `／`remote: `），前端才能把錯誤
/// 掛回對應的欄位上；撞埠時會點名佔用者與它所屬的源。
pub fn validate_forward(
    sources: &[Source],
    original_local: Option<u16>,
    name: &str,
    local: u16,
    remote: &str,
) -> Option<String> {
    if let Some(orig) = original_local {
        if !sources.iter().any(|s| s.forward(orig).is_some()) {
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
    let clash = sources
        .iter()
        .flat_map(|s| s.forwards.iter().map(move |f| (s, f)))
        .find(|(_, f)| f.local == local && Some(f.local) != original_local);
    if let Some((s, f)) = clash {
        return Some(format!("local: port {local} already used by {} in {}", f.name, s.name));
    }
    None
}

/// 新增／編輯連線源的欄位驗證，回傳 Some(訊息) 代表不通過。
///
/// `original_name` 是編輯前的源名，None 代表新增。訊息前綴為 `name: `／
/// `host: `／`user: `。
pub fn validate_source(
    sources: &[Source],
    original_name: Option<&str>,
    name: &str,
    host: &str,
    user: &str,
) -> Option<String> {
    if let Some(orig) = original_name {
        if !sources.iter().any(|s| s.name == orig) {
            return Some(format!("name: no source called {orig}, it may have been deleted"));
        }
    }
    if !valid_source_name(name) {
        return Some("name: required, and must not contain spaces or brackets".into());
    }
    if host.trim().is_empty() {
        return Some("host: required".into());
    }
    if user.trim().is_empty() {
        return Some("user: required".into());
    }
    if sources.iter().any(|s| s.name == name && Some(s.name.as_str()) != original_name) {
        return Some(format!("name: source {name} already exists"));
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

    fn src(name: &str, forwards: Vec<Forward>) -> Source {
        Source {
            name: name.into(),
            host: format!("{name}.example.com"),
            user: "bob".into(),
            proxy_command: String::new(),
            forwards,
        }
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

    /// 新增源時就地補一張新桌，讀回來要一模一樣
    #[test]
    fn write_appends_new_source() {
        let dir = tmp_dir("grow-src");
        let mut cfg = Config { close_to_tray: true, sources: vec![src("a", vec![fwd("x", 1080)])] };
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

    /// 驗證訊息要能被前端逐欄掛回去，格式固定是「欄位: 說明」
    fn err(list: &[Source], orig: Option<u16>, name: &str, local: u16, remote: &str) -> String {
        validate_forward(list, orig, name, local, remote).expect("這組輸入應該要被擋下來")
    }

    #[test]
    fn upsert_rejects_duplicate_local_port() {
        let list = vec![src(
            "hk",
            vec![fwd("exit-tw", 1080), Forward { enabled: false, ..fwd("b", 1083) }],
        )];
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

    #[test]
    fn upsert_rejects_unknown_original_port() {
        let list = vec![src("hk", vec![fwd("a", 1080)])];
        assert!(err(&list, Some(9999), "a", 1080, "127.0.0.1:1").starts_with("local: "));
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
            "name: source hk already exists"
        );
        // 編輯自己時不算重複
        assert!(validate_source(&list, Some("hk"), "hk", "h", "u").is_none());
        // 改成別人的名字要擋
        assert!(validate_source(&list, Some("hk"), "tw", "h", "u").unwrap().starts_with("name: "));
        // 原本的源已經被刪掉
        assert!(validate_source(&list, Some("gone"), "x", "h", "u").unwrap().starts_with("name: "));
    }
}
