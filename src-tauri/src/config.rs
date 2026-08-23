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

/// 可攜模式的檔名：放在執行檔旁邊就生效（KeePass／Rufus 的同名檔慣例）
pub const TOML_NAME: &str = "traytunnel.toml";

/// 家目錄模式的檔名，點開頭，不去污染使用者家目錄的檔案清單
pub const HOME_TOML_NAME: &str = ".traytunnel.toml";

/// 壞檔備份一律是「生效檔名 + 這個後綴」，所以兩種模式的備份也各自不同名
const BROKEN_SUFFIX: &str = ".broken";

/// 存檔用的暫存檔後綴，寫完就 rename 蓋回生效檔名
const TMP_SUFFIX: &str = ".tmp";

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

/// 執行檔主檔名裡的可攜記號：沿用 Rufus 的 `rufus-4.5p.exe` 慣例，記號是**結尾**的 p。
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
    std::env::var_os("USERPROFILE").filter(|s| !s.is_empty()).map(PathBuf::from)
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
    /// 要不要在背景檢查新版。
    ///
    /// 刻意是 `Option`：這一項的預設值**跟著執行模式走**（一般模式開、可攜模式
    /// 關），設定檔裡沒寫的時候不能在這裡就決定成某個布林，否則可攜模式讀進來
    /// 就會拿到一般模式的預設值。實際生效的值一律問 [`Config::checks_for_updates`]。
    #[serde(default)]
    pub check_for_updates: Option<bool>,
    #[serde(default)]
    pub sources: Vec<Source>,
}

/// 舊制（契約 v2）的設定檔長相，只在自動遷移時用得到。
///
/// `deny_unknown_fields` 是判定舊制的第二道防線：v2 的頂層鍵就只有這五個
/// （host／user／proxyCommand／closeToTray／forwards，checkForUpdates 是 v3
/// 之後才有的），冒出別的鍵就代表這份檔案不是它自稱的那種格式，寧可當壞檔備份
/// 起來也不要照舊制解讀完再把不認得的內容寫掉。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
    /// 這次執行到底要不要檢查更新。
    ///
    /// 設定檔沒寫（`None`）時看模式：一般模式視為開啟，可攜模式視為關閉——
    /// 可攜版常見的用法就是丟在隨身碟或隔離環境裡跑，預設不主動連外比較合理，
    /// 而且它本來也只能提示、不能就地更新。寫了就照使用者寫的算。
    pub fn checks_for_updates(&self, portable: bool) -> bool {
        self.check_for_updates.unwrap_or(!portable)
    }

    /// 依本地埠找出口，本地埠是出口的全域唯一鍵
    pub fn forward(&self, local: u16) -> Option<&Forward> {
        self.sources.iter().find_map(|s| s.forward(local))
    }

    pub fn forward_mut(&mut self, local: u16) -> Option<&mut Forward> {
        self.sources.iter_mut().find_map(|s| s.forwards.iter_mut().find(|f| f.local == local))
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

    /// 單一源底下所有出口的本地埠；沒有這個源就是空的
    pub fn locals_of(&self, source: &str) -> Vec<u16> {
        self.source(source)
            .map(|s| s.forwards.iter().map(|f| f.local).collect())
            .unwrap_or_default()
    }

    /// 單一源底下 enabled 出口的本地埠
    pub fn enabled_locals_of(&self, source: &str) -> Vec<u16> {
        self.source(source)
            .map(|s| s.forwards.iter().filter(|f| f.enabled).map(|f| f.local).collect())
            .unwrap_or_default()
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            close_to_tray: true,
            // 預設值跟著模式走，所以這裡刻意留空（見 checks_for_updates）
            check_for_updates: None,
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
    /// 檔案不存在，連預設值都寫不進去（資料夾唯讀、磁碟滿之類）。
    /// 記憶體照樣用預設值跑，但不可以對外宣稱「已建檔」
    CreateFailed { config: Config, error: String },
    /// 讀到舊制設定，已就地遷移成新制並寫回
    Migrated(Config),
    /// 解析或讀取失敗，改用預設值且未覆寫原檔；backup 只在確實備份成功時有值
    Broken { config: Config, backup: Option<PathBuf>, error: String },
}

impl LoadOutcome {
    /// 這次執行要不要把設定切成唯讀。
    ///
    /// 壞檔本身不可怕（備份留著，之後存檔是拿預設值蓋掉一份已經備份過的檔案），
    /// 真正致命的是「壞檔而且連備份都寫不出來」：使用者那份手寫設定只剩磁碟上
    /// 這一份，任何一次回寫都會把它靜靜輾成預設值。這種情況一律拒絕寫入。
    pub fn read_only(&self) -> bool {
        matches!(self, LoadOutcome::Broken { backup: None, .. })
    }

    pub fn config(&self) -> &Config {
        match self {
            LoadOutcome::Loaded(c)
            | LoadOutcome::Created(c)
            | LoadOutcome::Migrated(c)
            | LoadOutcome::CreateFailed { config: c, .. }
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
         # 是否在背景檢查新版（啟動後一次，之後每天一次）。\n\
         # 省略時：一般模式視為 true，可攜模式視為 false。關閉時完全不連外。\n\
         #checkForUpdates = true\n\
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
                    // 遷移只改結構，寫回時走同一套就地改寫，註解照樣留著。
                    // 寫不回去也還能用記憶體裡這份跑，但一定要留下痕跡，
                    // 否則使用者只會看到「已遷移」而檔案其實原封不動
                    if let Err(e) = write_config_at(toml_path, &cfg) {
                        log::warn!(
                            "could not write the migrated config back to {}: {e}",
                            toml_path.display()
                        );
                    }
                    LoadOutcome::Migrated(cfg)
                } else {
                    LoadOutcome::Loaded(cfg)
                }
            }
            Err(e) => broken(toml_path, e),
        };
    }

    let cfg = Config::default();
    // 建檔也走與存檔同一條路：暫存檔寫完再 rename。使用者第一次啟動就遇到
    // 磁碟滿或程式被砍時，資料夾裡不會多出一個半截的設定檔
    match write_atomic(toml_path, &default_document()) {
        Ok(()) => LoadOutcome::Created(cfg),
        Err(e) => {
            log::warn!("could not create the config file at {}: {e}", toml_path.display());
            LoadOutcome::CreateFailed { config: cfg, error: e.to_string() }
        }
    }
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

/// 舊制設定檔＝頂層有 host **而且**沒有 `[[sources]]`。
///
/// 兩個條件缺一不可：只看 host 的話，一份已經有 `[[sources]]`、頂層卻還留著
/// host 的檔案（半途手改、兩份檔案拼在一起）會被當成舊制去解讀——LegacyConfig
/// 讀不到那些 sources，遷移完就只剩頂層那一組連線，其餘的源會在下一次存檔時
/// 被靜靜寫掉。那種檔案的意圖已經無從判斷，交給 [`ambiguous_document`] 去擋。
fn is_legacy(doc: &DocumentMut) -> bool {
    doc.get("host").is_some() && doc.get("sources").is_none()
}

/// 新舊兩制的鍵並存：頂層 host 與 `[[sources]]` 同時出現。
///
/// 這種檔案沒有安全的解讀方式（照舊制讀會丟掉 sources，照新制讀會丟掉頂層那組
/// 連線），一律當壞檔處理——壞檔那條路會先備份原檔再退回預設值，使用者手上那份
/// 資料完整留著，比靜靜挑一半來用好得多。
fn ambiguous_document(doc: &DocumentMut) -> bool {
    doc.get("host").is_some() && doc.get("sources").is_some()
}

/// 從 host 派生源名：源名不可含空白與中括號，但 host 兩者都可能有
/// （`[::1]` 這種字面 IPv6 位址就同時踩到），因此剝掉再用；
/// 剝完是空的就退回一個固定名字，總之要生得出合法的源名。
fn source_name_from_host(host: &str) -> String {
    let cleaned: String =
        host.trim().chars().filter(|c| !c.is_whitespace() && *c != '[' && *c != ']').collect();
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
            // 舊制沒有這個欄位，遷移後照樣讓它跟著模式走
            check_for_updates: None,
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
    if ambiguous_document(&doc) {
        return Err("設定檔同時有舊制的頂層 host 與新制的 [[sources]]，無法判斷該用哪一份".into());
    }
    let legacy = is_legacy(&doc);
    let mut cfg: Config = if legacy {
        let old: LegacyConfig = toml_edit::de::from_document(doc).map_err(|e| e.to_string())?;
        old.into_config()
    } else {
        toml_edit::de::from_document(doc).map_err(|e| e.to_string())?
    };
    trim_config(&mut cfg);
    normalize_remotes(&mut cfg);
    validate_config(&cfg)?;
    Ok((cfg, legacy))
}

/// 手寫的 `host = " myhost "` 兩邊的空白在這裡就剃掉。
///
/// 驗證看的是 `host.trim()`、實際存的卻是原字串，兩份不一樣就會有一整排怪事：
/// 判空說有值、ssh 拿到的是帶空白的主機名、介面顯示也跟著歪。剃在解析的出口處，
/// 之後全程式（驗證、ssh 參數、介面、下次存檔）看到的就是同一份值。
/// 舊制那條路的 host／user 早就在 [`LegacyConfig::into_config`] 剃過，再剃一次是原值。
///
/// 出口的 name 也要剃：驗證會擋掉含空白的名字，介面那條路是先 trim 再驗
/// （[`prepare_forward`]），讀檔這條路要是不剃，同樣一個 `" a "` 在介面上存得進去、
/// 手寫進檔案卻會讓整份設定變壞檔退回預設值——同一個值兩條路兩種下場。
fn trim_config(cfg: &mut Config) {
    for s in cfg.sources.iter_mut() {
        s.name = s.name.trim().to_string();
        s.host = s.host.trim().to_string();
        s.user = s.user.trim().to_string();
        for f in s.forwards.iter_mut() {
            f.name = f.name.trim().to_string();
        }
    }
}

/// 讀進來的設定同樣支援純埠號的簡寫：手寫 `remote = "8080"` 一樣算數，在這裡就補成完整形式。
///
/// 補在解析的出口處，程式其他地方（ssh 參數、介面顯示、下次存檔）拿到的就永遠是
/// `host:port`，不必各自再判斷一次；下次存檔時檔案裡那個 `8080` 也會被寫成完整形式。
fn normalize_remotes(cfg: &mut Config) {
    for f in cfg.sources.iter_mut().flat_map(|s| s.forwards.iter_mut()) {
        f.remote = normalize_remote(&f.remote);
    }
}

/// 只要設定不管遷移旗標的簡便版
#[cfg(test)]
pub fn parse_config(raw: &str) -> Result<Config, String> {
    parse_document(raw).map(|(cfg, _)| cfg)
}

/// 一筆出口的欄位哪裡不合規。
///
/// 規則只有這一份，訊息各自去寫：讀檔（[`validate_config`]）要講的是「檔案第幾段
/// 不對」，介面輸入（[`validate_forward`]）要的是掛得回欄位的 `name: ` 前綴，
/// 兩邊講法不同，但認定合不合規的那條線必須是同一條。
enum ForwardIssue {
    Name,
    Local,
    Remote,
}

/// 出口欄位的共同規則：名字非空且不含空白、本地埠是 1-65535、remote 是 host:port。
///
/// `remote` 在這裡自己過一次 [`normalize_remote`]，只填埠號的寫法兩條路都算數
/// （讀檔那條已經正規化過，再跑一次是原值）。
fn check_forward_fields(name: &str, local: u16, remote: &str) -> Option<ForwardIssue> {
    if !valid_name(name) {
        return Some(ForwardIssue::Name);
    }
    if local == 0 {
        return Some(ForwardIssue::Local);
    }
    if !valid_remote(&normalize_remote(remote)) {
        return Some(ForwardIssue::Remote);
    }
    None
}

/// 讀進來的設定必須自洽，否則寧可當壞檔也不要帶著矛盾的狀態跑
fn validate_config(cfg: &Config) -> Result<(), String> {
    let mut seen_names: Vec<&str> = Vec::new();
    let mut seen_locals: Vec<u16> = Vec::new();
    for s in &cfg.sources {
        if !valid_source_name(&s.name) {
            return Err("[[sources]] 的 name 不可為空，也不可含空白或中括號".into());
        }
        if seen_names.contains(&s.name.as_str()) {
            return Err(format!("連線名稱重複：{}", s.name));
        }
        seen_names.push(&s.name);
        if s.host.trim().is_empty() || s.user.trim().is_empty() {
            return Err(format!("連線 {} 的 host 與 user 不可為空", s.name));
        }
        for f in &s.forwards {
            // 欄位規則與介面輸入共用同一個 check_forward_fields，兩條路才不會分岔。
            // 壞值放行的話會一路餵進 ssh -L，換來的是每 5 秒重連一次卻永遠接不起來
            match check_forward_fields(&f.name, f.local, &f.remote) {
                Some(ForwardIssue::Name) => {
                    return Err(format!(
                        "[[sources.forwards]] 的 name 不可為空，也不可含空白（連線 {}）",
                        s.name
                    ))
                }
                Some(ForwardIssue::Local) => {
                    return Err(format!("出口 {} 的 local 要落在 1-65535", f.name))
                }
                Some(ForwardIssue::Remote) => {
                    return Err(format!(
                        "出口 {} 的 remote 不合法：{}（要寫成 host:port，例如 127.0.0.1:1080，或只填埠號）",
                        f.name, f.remote
                    ))
                }
                None => {}
            }
            if seen_locals.contains(&f.local) {
                return Err(format!("本地埠重複：{}（跨連線也不可以重複）", f.local));
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
///
/// 唯一的例外是新表格的 name：後面的 sync 認 name 找表格（見 [`sync_tables`]），
/// 這裡不先派生出同一個名字的話，那張剛搬好的表格會對不上而被整個丟掉，
/// 連帶把搬進去的 `[[sources.forwards]]` 與註解一起賠掉。
fn migrate_document(doc: &mut DocumentMut) {
    // 與 LegacyConfig::into_config 派生源名的規則同一條，兩邊才對得上
    let name = source_name_from_host(doc.get("host").and_then(Item::as_str).unwrap_or_default());

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
    t["name"] = value(name);
    if let Some(Item::ArrayOfTables(a)) = forwards {
        t.insert("forwards", Item::ArrayOfTables(a));
    }
    let mut arr = ArrayOfTables::new();
    arr.push(t);
    doc.insert("sources", Item::ArrayOfTables(arr));
}

/// 依穩定鍵把設定物件對回既有表格，逐張就地改寫，使用者寫在單筆上方的註解
/// 才會一直跟著它註解的那一筆走。
///
/// 不可以改用位置比對：新增與編輯走的是 retain + push（編輯過的那一筆會跑到陣列
/// 尾端），刪掉中間一筆也會讓後面全部往前挪，位置比對在這兩種情況下會把註解錯掛
/// 到別人頭上，或連同表格一起被截掉。所以這裡認鍵：source 認 name、forward 認
/// local，新陣列逐筆去既有表格裡找同鍵的那一張，找到就地更新欄位（那張表格的
/// decor 原封不動留著），找不到才開一張新的；舊表格的鍵不在新集合裡就被丟掉。
///
/// 限制：改名（source 的 name）與改埠（forward 的 local）等於換了一把鍵，舊表格
/// 對不上，語意就是刪一筆再增一筆——註解跟著舊鍵一起消失。鍵本身就是使用者辨識
/// 那一筆的依據，改鍵時註解未必還適用，所以這個取捨是刻意的。
///
/// 改 source 的 name 時範圍還要再大一圈：被丟掉的是整張 `[[sources]]` 表格，
/// 巢狀在它底下的 `[[sources.forwards]]` 是連同表格一起被換掉的，所以那些出口
/// 各自上方的註解也會一起沒了（出口本身的值不受影響，會照設定物件重新寫出來）。
fn sync_tables<T, K: PartialEq>(
    tables: &mut ArrayOfTables,
    items: &[T],
    table_key: impl Fn(&Table) -> Option<K>,
    item_key: impl Fn(&T) -> K,
    apply: impl Fn(&mut Table, &T),
) {
    // Option 是「這張舊表格還沒被認領」的記號：同一張不可以被兩筆同時挑走
    let mut old: Vec<Option<Table>> = tables.iter().cloned().map(Some).collect();
    let mut out = ArrayOfTables::new();
    for item in items {
        let key = item_key(item);
        let hit = old
            .iter()
            .position(|t| t.as_ref().is_some_and(|t| table_key(t).as_ref() == Some(&key)));
        let mut t = match hit {
            Some(i) => old[i].take().expect("position 挑中的那張一定還在"),
            None => Table::new(),
        };
        apply(&mut t, item);
        // position 記的是這張表格在原檔裡的行序，輸出時會照它排序；重排過就一定
        // 要清掉，否則檔案裡的順序會跟設定物件的順序對不起來
        t.set_position(None);
        out.push(t);
    }
    *tables = out;
}

/// 巢狀的 `[[sources.forwards]]`：認 local
fn sync_forwards(tables: &mut ArrayOfTables, forwards: &[Forward]) {
    sync_tables(
        tables,
        forwards,
        |t| t.get("local").and_then(Item::as_integer),
        |f: &Forward| f.local as i64,
        |t, f| {
            t["name"] = value(f.name.as_str());
            t["local"] = value(f.local as i64);
            t["remote"] = value(f.remote.as_str());
            t["enabled"] = value(f.enabled);
        },
    );
}

/// 頂層的 `[[sources]]`：認 name，順手把自己底下的 forwards 也同步掉
fn sync_sources(tables: &mut ArrayOfTables, sources: &[Source]) {
    sync_tables(
        tables,
        sources,
        |t| t.get("name").and_then(Item::as_str).map(str::to_owned),
        |s: &Source| s.name.clone(),
        |t, s| {
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
        },
    );
}

/// 寫回設定，沿用既有檔案的註解與排版。
///
/// `[[sources]]` 與巢狀的 `[[sources.forwards]]` 都逐張表格就地改寫，改寫時依
/// 穩定鍵認表格（見 [`sync_tables`]）；讀到的是舊制檔案時先把結構遷移成新制再寫。
pub fn write_config_at(path: &Path, cfg: &Config) -> std::io::Result<()> {
    let mut doc = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| strip_bom(&s).parse::<DocumentMut>().ok())
        .unwrap_or_else(|| default_document().parse::<DocumentMut>().unwrap());

    if is_legacy(&doc) {
        migrate_document(&mut doc);
    } else if ambiguous_document(&doc) {
        // 新舊兩制並存的壞檔（讀檔時已經備份過一份）。存檔時要順手把舊制那幾個
        // 頂層鍵清掉，否則寫完還是同一種壞檔：下次啟動又判壞檔，備份會被這份
        // 內容再蓋一次，使用者原本那份資料就真的沒了
        doc.remove("host");
        doc.remove("user");
        doc.remove("proxyCommand");
        doc.remove("forwards");
    }

    doc["closeToTray"] = value(cfg.close_to_tray);

    // 沒有明確設定就不要把鍵寫進去：一寫下去，那個當下算出來的預設值就被固定了，
    // 同一份可攜設定被一般模式讀到（或反過來）時，預設值就跟著模式走不動了。
    // 使用者在設定頁動過開關才會變成 Some，那時才落檔。
    if let Some(on) = cfg.check_for_updates {
        doc["checkForUpdates"] = value(on);
    }

    if !matches!(doc.get("sources"), Some(Item::ArrayOfTables(_))) {
        doc["sources"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    if let Some(Item::ArrayOfTables(tables)) = doc.get_mut("sources") {
        sync_sources(tables, &cfg.sources);
    }

    write_atomic(path, &doc.to_string())
}

/// 落檔一律先寫暫存檔再 rename 蓋過去。
///
/// `fs::write` 是「先截斷再寫」，中途斷電、磁碟滿或行程被砍都會留下一個半截的
/// 設定檔，下次啟動就是壞檔。rename 在同一個資料夾內是原子的（Windows 的
/// `MoveFileEx` 帶 REPLACE_EXISTING），使用者手上永遠只會看到完整的舊版或新版。
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = tmp_path(path);
    // 寫到一半失敗（最典型的就是磁碟寫滿）時，暫存檔已經開出來而且是半截的，
    // 一樣要清掉——「不留半成品」的承諾得涵蓋兩種失敗，不是只有換名那一種
    if let Err(e) = std::fs::write(&tmp, contents) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// 暫存檔路徑：生效檔名直接接上 `.tmp`，與備份檔同樣跟著實際檔名走，
/// 而且一定落在同一個資料夾，rename 才會是同磁碟區的原子換名
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_else(|| TOML_NAME.as_ref()).to_os_string();
    name.push(TMP_SUFFIX);
    path.with_file_name(name)
}

/// 資料夾版的薄包裝，只給測試用
#[cfg(test)]
pub fn write_config(dir: &Path, cfg: &Config) -> std::io::Result<()> {
    write_config_at(&dir.join(TOML_NAME), cfg)
}

/// remote 必須是 `host:port`：主機不含冒號與空白，埠是純數字而且落在 1-65535。
///
/// 埠先確認是純數字再 `parse::<u16>()`：光看是不是數字會放行 `:99999`（ssh 收下
/// 之後就是一句 Bad forwarding specification，每 5 秒重連一次卻永遠接不起來），
/// 光靠 parse 又會放行 `+80` 這種 ssh 不認得的寫法。0 不是可連的目的地，一併擋掉，
/// 與只填埠號那條路（[`normalize_remote`]）的下界對得起來。
pub fn valid_remote(s: &str) -> bool {
    match s.split_once(':') {
        Some((h, p)) => {
            !h.is_empty()
                && !h.chars().any(|c| c.is_whitespace())
                && p.chars().all(|c| c.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|port| port > 0)
        }
        None => false,
    }
}

/// REMOTE 欄位的簡寫形式：只填埠號（純數字且落在 1-65535）時視為「伺服器本機的那個埠」，
/// 補成 `127.0.0.1:<port>`。其他寫法原樣送回去給 `valid_remote` 判，
/// 所以越界的埠（`0`、`70000`）不會被補成合法值，而是照舊被擋在 remote 這欄。
///
/// 補完的完整形式才是存進 toml 的值：設定檔看起來永遠是 `host:port`。
/// 介面輸入（[`prepare_forward`]）與讀檔（[`normalize_remotes`]）兩條路都經過這裡，
/// 手寫檔案的人一樣可以只寫埠號。
pub fn normalize_remote(remote: &str) -> String {
    let s = remote.trim();
    // parse 會放行 `+80` 這種寫法，所以先自己確認是純數字
    if s.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(port) = s.parse::<u16>() {
            if port > 0 {
                return format!("127.0.0.1:{port}");
            }
        }
    }
    s.to_string()
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
///
/// `remote` 先過一次 [`normalize_remote`]，只填埠號的寫法才會被放行；
/// 呼叫端存檔時也要存正規化後的值，驗證與落檔看的才是同一個字串。
pub fn validate_forward(
    sources: &[Source],
    original_local: Option<u16>,
    name: &str,
    local: u16,
    remote: &str,
) -> Option<String> {
    if let Some(orig) = original_local {
        if !sources.iter().any(|s| s.forward(orig).is_some()) {
            return Some(format!("local: no tunnel with port {orig}, it may have been deleted"));
        }
    }
    // 欄位規則與讀檔共用（見 check_forward_fields），這裡只負責翻成前端要的訊息
    match check_forward_fields(name, local, remote) {
        Some(ForwardIssue::Name) => {
            return Some("name: required, and must not contain spaces".into())
        }
        Some(ForwardIssue::Local) => return Some("local: port must be between 1 and 65535".into()),
        Some(ForwardIssue::Remote) => {
            return Some(
                "remote: must look like host:port, for example 127.0.0.1:1080, or just a port"
                    .into(),
            )
        }
        None => {}
    }
    let clash = sources
        .iter()
        .flat_map(|s| s.forwards.iter().map(move |f| (s, f)))
        .find(|(_, f)| f.local == local && Some(local) != original_local);
    if let Some((s, f)) = clash {
        return Some(format!("local: port {local} already used by {} in {}", f.name, s.name));
    }
    None
}

/// 新增／編輯出口的前處理：欄位正規化 + 驗證，通過就直接給出要存進設定的那一筆。
///
/// 這是介面輸入進到設定裡的唯一入口。驗證看的是這裡組出來的 [`Forward`]，
/// 呼叫端也只准把回傳的這一筆原封不動存下去，「驗過的字串」與「落檔的字串」
/// 才不可能各走各的——remote 的埠號糖也只在這裡加一次。
pub fn prepare_forward(
    sources: &[Source],
    original_local: Option<u16>,
    name: &str,
    local: u16,
    remote: &str,
    enabled: bool,
) -> Result<Forward, String> {
    let f =
        Forward { name: name.trim().to_string(), local, remote: normalize_remote(remote), enabled };
    match validate_forward(sources, original_local, &f.name, f.local, &f.remote) {
        Some(err) => Err(err),
        None => Ok(f),
    }
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
            return Some(format!("name: no connection called {orig}, it may have been deleted"));
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
        return Some(format!("name: connection {name} already exists"));
    }
    None
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
