//! 設定檔：TOML，預設放在使用者家目錄的 `.traytunnel.toml`。
//! 讀寫走 toml_edit，保留使用者手寫的註解與排版。
//!
//! 位置由 [`config_location`] 一次解析完（資料夾與檔名綁在一起），全程式的讀、
//! 寫、壞檔備份都跟著同一個結果走，不再各自拼路徑。
//!
//! 契約 v3 起改成多連線源：頂層只剩 closeToTray，其餘全部收進 `[[sources]]`，
//! 每個源自己帶 host／user／proxyCommand 與巢狀的 `[[sources.forwards]]`。
//! 本地埠（local）是出口的全域唯一鍵，跨源也不得重複。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};

/// 可攜模式的檔名：放在執行檔旁邊就生效（KeePass／Rufus 那套同名檔慣例）
pub const TOML_NAME: &str = "traytunnel.toml";

/// 家目錄模式的檔名，點開頭，不去污染使用者家目錄的檔案清單
pub const HOME_TOML_NAME: &str = ".traytunnel.toml";

/// 壞檔備份一律是「生效檔名 + 這個後綴」，所以兩種模式的備份也各自不同名
const BROKEN_SUFFIX: &str = ".broken";

#[cfg(test)]
pub const BROKEN_NAME: &str = "traytunnel.toml.broken";

/// 設定檔的落腳處。資料夾與檔名是一起決定的（可攜模式與家目錄模式連檔名都不同），
/// 因此解析結果整包傳遞，任何地方都不要再自己拼一次路徑。
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigLocation {
    /// 生效的完整路徑
    pub path: PathBuf,
    /// 是不是可攜模式（設定檔就在執行檔旁邊）
    pub portable: bool,
}

impl ConfigLocation {
    /// 設定檔所在資料夾，建資料夾與開檔案總管時用
    pub fn dir(&self) -> &Path {
        self.path.parent().unwrap_or_else(|| Path::new("."))
    }
}

/// 日誌與通知要顯示的檔名。兩種模式檔名不同（`traytunnel.toml` 與
/// `.traytunnel.toml`，備份再各自加 `.broken`），訊息裡一律不可寫死。
/// 拿不到檔名時退回可攜模式的檔名。
pub fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| TOML_NAME.to_string())
}

/// 執行檔主檔名裡的可攜記號：Rufus 的 `rufus-4.5p.exe` 那套，記號是**結尾**的 p。
///
/// 只認結尾而不是任意位置，否則 Windows 複製檔案自動取的
/// 「traytunnel - Copy.exe」（Copy 裡有 p）會莫名其妙變成可攜模式。
/// `traytunnel` 本身不是 p 結尾，所以結尾的 p 一定是使用者刻意加的，
/// 例如 `traytunnel-p.exe`、`traytunnel-0.2.0p.exe`。大小寫不敏感。
pub fn stem_marks_portable(exe_stem: &str) -> bool {
    matches!(exe_stem.chars().next_back(), Some(c) if c.eq_ignore_ascii_case(&'p'))
}

/// 路徑優先序的純邏輯，實機與測試共用。可攜模式兩個觸發條件**任一成立**即可，
/// 兩者都指向執行檔旁邊的 `traytunnel.toml`：
///
/// 1. 執行檔主檔名以 p 結尾（[`stem_marks_portable`]）→ 可攜模式；檔案還不存在也算，
///    後面 `load_from_path` 會就地建一份預設檔（Rufus 建 ini 的行為）；
/// 2. 執行檔旁邊已經有 `traytunnel.toml` → 可攜模式，直接用它；
/// 3. 都不成立就用家目錄的 `.traytunnel.toml`；
/// 4. 連家目錄都問不出來時退回執行檔目錄，檔名仍維持點檔，
///    才不會反過來把自己變成可攜模式。
pub fn resolve_location(exe_dir: &Path, exe_stem: &str, home: Option<&Path>) -> ConfigLocation {
    let portable = exe_dir.join(TOML_NAME);
    if stem_marks_portable(exe_stem) || portable.is_file() {
        return ConfigLocation { path: portable, portable: true };
    }
    let base = home.unwrap_or(exe_dir);
    ConfigLocation { path: base.join(HOME_TOML_NAME), portable: false }
}

/// 執行檔的所在資料夾與主檔名，取自同一次 `current_exe()`；
/// 問不出來時分別退回工作目錄與空字串（空字串不含 p，不會誤觸可攜模式）
fn exe_parts() -> (PathBuf, String) {
    let exe = std::env::current_exe().ok();
    let dir = exe
        .as_ref()
        .and_then(|p| p.parent())
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = exe
        .as_ref()
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    (dir, stem)
}

/// 使用者家目錄；空字串視同沒有
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// 這次執行實際生效的設定檔位置，順手把資料夾補出來。
/// 全程式只有這一個入口，讀寫與備份都從回傳值派生。
pub fn config_location() -> ConfigLocation {
    let (dir, stem) = exe_parts();
    let loc = resolve_location(&dir, &stem, home_dir().as_deref());
    let _ = std::fs::create_dir_all(loc.dir());
    loc
}

/// 壞檔備份的路徑：生效檔名直接接上 `.broken`
pub fn broken_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_else(|| TOML_NAME.as_ref()).to_os_string();
    name.push(BROKEN_SUFFIX);
    path.with_file_name(name)
}

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
        "# traytunnel 設定檔。設定頁的 Config file 一列會顯示它實際的位置。\n\
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

/// 檔案存在但長度是 0。可攜模式的常見用法就是先 `type nul > traytunnel.toml`
/// 生一個空檔當開關，那種檔案要當成「還沒有設定」而不是壞檔。
/// 問不到 metadata 時保守地當成非空，後面照原路走讀取／壞檔流程。
fn is_empty_file(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.len() == 0).unwrap_or(false)
}

/// 從指定路徑讀設定，檔案不存在（或存在但是空檔）就寫一份預設值。
pub fn load_from_path(toml_path: &Path) -> LoadOutcome {
    if toml_path.exists() && !is_empty_file(toml_path) {
        let raw = match std::fs::read_to_string(toml_path) {
            Ok(s) => s,
            Err(e) => return broken(toml_path, format!("讀取失敗：{e}")),
        };
        return match parse_document(&raw) {
            Ok((cfg, migrated)) => {
                if migrated {
                    // 遷移只改結構，寫回時走同一套就地改寫，註解照樣留著
                    let _ = write_config_at(toml_path, &cfg);
                    LoadOutcome::Migrated(cfg)
                } else {
                    LoadOutcome::Loaded(cfg)
                }
            }
            Err(e) => broken(toml_path, e),
        };
    }

    let cfg = Config::default();
    let _ = std::fs::write(toml_path, default_document());
    LoadOutcome::Created(cfg)
}

/// 資料夾版的薄包裝，只給測試用（實機一律走 [`config_location`] 解析出來的完整路徑）
#[cfg(test)]
pub fn load_from_dir(dir: &Path) -> LoadOutcome {
    load_from_path(&dir.join(TOML_NAME))
}

/// 用 PowerShell 之類的工具存檔可能會帶 UTF-8 BOM，解析前先剝掉。
fn strip_bom(raw: &str) -> &str {
    raw.strip_prefix('\u{feff}').unwrap_or(raw)
}

fn broken(toml_path: &Path, error: String) -> LoadOutcome {
    // 絕不覆寫壞掉的設定檔，只複製一份備份出來；連檔案都讀不到時就沒有備份可言
    let target = broken_path(toml_path);
    let backup = std::fs::copy(toml_path, &target).ok().map(|_| target);
    LoadOutcome::Broken { config: Config::default(), backup, error }
}

/// 頂層還有 host 欄位就是舊制設定檔
fn is_legacy(doc: &DocumentMut) -> bool {
    doc.get("host").is_some()
}

/// 從 host 派生源名：源名不可含空白與中括號，但 host 兩者都可能有
/// （`[::1]` 這種字面 IPv6 位址就同時踩到），因此剝掉再用；
/// 剝完是空的就退回一個固定名字，總之要生得出合法的源名。
fn source_name_from_host(host: &str) -> String {
    let cleaned: String = host
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '[' && *c != ']')
        .collect();
    if cleaned.is_empty() {
        "default".to_string()
    } else {
        cleaned
    }
}

impl LegacyConfig {
    /// 舊制包成單一 source，源名預設用 host 的值（消毒過）
    fn into_config(self) -> Config {
        Config {
            close_to_tray: self.close_to_tray,
            sources: vec![Source {
                name: source_name_from_host(&self.host),
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
pub fn write_config_at(path: &Path, cfg: &Config) -> std::io::Result<()> {
    let mut doc = std::fs::read_to_string(path)
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

/// 資料夾版的薄包裝，只給測試用
#[cfg(test)]
pub fn write_config(dir: &Path, cfg: &Config) -> std::io::Result<()> {
    write_config_at(&dir.join(TOML_NAME), cfg)
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

    // ------------------------------ 檔名含 p 的可攜記號（Rufus 那套）

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
