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

/// 一條列的**機制**（設計書 §1.2）。`Socks` 只允許出現在 WG 連線底下。
///
/// 只分兩種，而且分的是「它在技術上怎麼運作」——「後端是不是代理服務」是另一件
/// 正交的事，由 [`Forward::probe_proxy`] 表示。先前那個「`remote=None` ＋
/// `kind=proxy` ＝ 引擎自建」的編碼已作廢（§1.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowKind {
    /// 本地埠 → 一個固定目的地，位元組原樣搬運。`remote` 必填。
    ///
    /// 舊檔缺 `kind` 鍵時就是它——舊檔裡的每一筆本來就都是轉發，不需要遷移邏輯（§1.7）
    #[default]
    Forward,
    /// 引擎在這個埠上自建一個 SOCKS5 伺服器，目的地由使用它的應用逐次指定。
    /// 沒有 `remote`，也不得帶 `probeProxy`（恆測、協定已知）
    Socks,
}

/// 連線的型別（設計書 §1.1）：只決定流量怎麼被運送。建立後不可變（U1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnKind {
    Ssh,
    Wg,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Forward {
    pub name: String,
    pub local: u16,
    /// `socks` 列沒有目的地（引擎內建 listener），`forward` 列必填（§1.3）。
    ///
    /// `skip_serializing_if` 讓 None 不會在 toml 裡寫出一個空字串。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// 機制（§1.2）。省略即 [`RowKind::Forward`]
    #[serde(default)]
    pub kind: RowKind,
    /// 後端是代理服務 ⇒ 做出口檢測並自動識別協定（§1.2）。
    ///
    /// 只對 `kind = Forward` 有意義；`Socks` 列恆測，這個欄位不得出現。
    /// serde 預設 false，**讀檔後的遷移掃描會把舊檔缺鍵的補成 true**（§1.7）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub probe_proxy: bool,
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
    /// 這個源的連線總開關；設定檔沒有這個欄位時視為 true（W6.12 起與 `WgProxy.enabled`
    /// 同一套語意，見 [`apply_source_enabled`]）
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub forwards: Vec<Forward>,
}

/// 一條使用者態 WireGuard 連線：底下掛 0..N 條列（設計書 §5.1）。
///
/// **沒有 `socksPort` 頂層欄位**——SOCKS5 埠是它底下某一條 `kind = "socks"` 列的
/// `local`，而且可以有 0..N 條（§1.3 與 U4）。連線的身分鍵是 `name`，它自己沒有埠。
///
/// 私鑰留在 `conf_path` 指的那份外部 `.conf` 裡，`traytunnel.toml` 只存路徑，
/// 永遠不複製金鑰進來。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WgProxy {
    pub name: String,
    /// 指向外部的標準 wg `.conf`；相對路徑以設定檔所在資料夾為基準（W3.19）
    pub conf_path: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 隧道 MTU 的覆寫值，None 代表「照 `.conf`」。
    ///
    /// 為什麼需要它：不少家用路由器（實測 ASUS）匯出的 `.conf` 根本不寫 MTU，
    /// 而那條線路的路徑 MTU 又小於常見預設值，結果是大封包靜默黑洞——握手正常、
    /// 小請求正常、網頁卻載一半就卡住。使用者不該為此被要求手改一份別的工具產出
    /// 的檔案，所以覆寫住在這裡；`.conf` **永遠只讀不寫**。
    ///
    /// `skip_serializing_if` 讓 None 不會在 toml 裡留下一個鍵（§5.1 的鍵省略規則）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtu: Option<usize>,
    /// 0..N 條列，與 [`Source::forwards`] 是同一個型別
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
    /// 使用者態 WireGuard 代理。對舊設定檔是相容的加法：沒有這一段就是空陣列。
    #[serde(default)]
    pub wg_proxies: Vec<WgProxy>,
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

    /// 每一條列，ssh 的先、wg 的後，各自照設定檔順序
    fn all_rows(&self) -> impl Iterator<Item = &Forward> {
        self.sources
            .iter()
            .flat_map(|s| s.forwards.iter())
            .chain(self.wg_proxies.iter().flat_map(|p| p.forwards.iter()))
    }

    /// 依本地埠找列，**跨兩型連線**——`local` 是列的全域唯一鍵（D5），
    /// 呼叫端不必先知道它掛在 ssh 源還是 wg 連線底下
    pub fn forward(&self, local: u16) -> Option<&Forward> {
        self.all_rows().find(|f| f.local == local)
    }

    pub fn forward_mut(&mut self, local: u16) -> Option<&mut Forward> {
        let ssh = self.sources.iter_mut().flat_map(|s| s.forwards.iter_mut());
        let wg = self.wg_proxies.iter_mut().flat_map(|p| p.forwards.iter_mut());
        ssh.chain(wg).find(|f| f.local == local)
    }

    /// 出口所屬的源
    pub fn source_of(&self, local: u16) -> Option<&Source> {
        self.sources.iter().find(|s| s.forward(local).is_some())
    }

    /// 這條列所屬**連線**的名字（日誌前綴 `[名字]` 靠它）。
    ///
    /// 兩型連線一視同仁：wg 的列回它所屬 `[[wgProxies]]` 的 name（W3.18），
    /// 日誌行的格式因此與 ssh 源完全一致，`log_exit` 一個字都不用改。
    pub fn source_name_of(&self, local: u16) -> Option<&str> {
        self.row(local).map(|(conn, _)| conn.name())
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

    /// **所有連線所有列**的本地埠，不分 kind、不分連線型，順序照設定檔（W3.3）
    pub fn locals(&self) -> Vec<u16> {
        self.all_rows().map(|f| f.local).collect()
    }

    /// 現在該跑的列：兩型連線都要**連線也 enabled 且列自己也 enabled**（W3.4／W6.12）。
    ///
    /// 連線層與列層是兩個獨立的意圖，`AND` 起來才是「這條列現在該不該跑」（§5.5）。
    /// ssh 與 wg 自 W6.12 起同一套規則：程式啟動只拉起總開關開著的連線。
    pub fn enabled_locals(&self) -> Vec<u16> {
        self.sources
            .iter()
            .filter(|s| s.enabled)
            .flat_map(|s| s.forwards.iter())
            .chain(self.wg_proxies.iter().filter(|p| p.enabled).flat_map(|p| p.forwards.iter()))
            .filter(|f| f.enabled)
            .map(|f| f.local)
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

    // ---- wg 連線 ----

    /// 依連線名找 wg 連線。身分鍵是 `name`，wg 連線自己沒有埠（§5.2）
    pub fn wg_proxy(&self, name: &str) -> Option<&WgProxy> {
        self.wg_proxies.iter().find(|p| p.name == name)
    }

    pub fn wg_proxy_mut(&mut self, name: &str) -> Option<&mut WgProxy> {
        self.wg_proxies.iter_mut().find(|p| p.name == name)
    }

    /// 依本地埠找 wg 的列（只查 wg，跨兩型的統一查詢走 [`Config::row`]）
    pub fn wg_row(&self, local: u16) -> Option<&Forward> {
        self.wg_proxies.iter().find_map(|p| p.forwards.iter().find(|f| f.local == local))
    }

    /// 跨兩型連線的統一查詢：這個本地埠是哪一條連線底下的哪一條列。
    ///
    /// 指令層與監看迴圈都用它——`local` 是全域唯一鍵（D5），問到連線本身才知道
    /// 日誌前綴要寫誰、以及這一條列該由 ssh 還是 wg 那一套動詞去啟停。
    pub fn row(&self, local: u16) -> Option<(ConnRef<'_>, &Forward)> {
        let ssh = self.sources.iter().find_map(|s| s.forward(local).map(|f| (ConnRef::Ssh(s), f)));
        let wg = || {
            self.wg_proxies.iter().find_map(|p| {
                p.forwards.iter().find(|f| f.local == local).map(|f| (ConnRef::Wg(p), f))
            })
        };
        ssh.or_else(wg)
    }

    /// 這個本地埠屬於哪一條 wg 連線
    pub fn wg_proxy_of(&self, local: u16) -> Option<&WgProxy> {
        self.wg_proxies.iter().find(|p| p.forwards.iter().any(|f| f.local == local))
    }

    /// 所有 `kind == Socks` 的列（UI 分組要，§1.4）。跨兩型連線——雖然
    /// SSH 連線帶 socks 列是錯誤（W3.23），這一支仍照定義掃全部，
    /// 讓「合法設定裡只有 wg 有 socks 列」由驗證那一關保證，不是靠這裡少掃一半
    pub fn socks_rows(&self) -> Vec<&Forward> {
        self.all_rows().filter(|f| matches!(f.kind, RowKind::Socks)).collect()
    }

    /// 要被探測的列＝`should_probe` 為真的列（自測排程要，§5.4）
    pub fn probed_rows(&self) -> Vec<&Forward> {
        self.all_rows().filter(|f| should_probe(f.kind, f.probe_proxy)).collect()
    }
}

/// 一條列所屬的連線。兩型連線在「列」這一層是對稱的，只有運送方式不同（§1.1）
#[derive(Debug, Clone, Copy)]
pub enum ConnRef<'a> {
    Ssh(&'a Source),
    Wg(&'a WgProxy),
}

impl<'a> ConnRef<'a> {
    /// 取名字時借的是**被指到的那條連線**（`'a`），不是這個 Copy 出來的參照——
    /// 不然 `cfg.row(local).map(|(c, _)| c.name())` 那種一行寫法會借到暫存值
    pub fn name(self) -> &'a str {
        match self {
            ConnRef::Ssh(s) => &s.name,
            ConnRef::Wg(p) => &p.name,
        }
    }
}

impl ConnRef<'_> {
    pub fn kind(&self) -> ConnKind {
        match self {
            ConnRef::Ssh(_) => ConnKind::Ssh,
            ConnRef::Wg(_) => ConnKind::Wg,
        }
    }
}

/// 這一條列要不要被探測。**§1.3 那張表的唯一實作**（W3.25）。
///
/// ①③（純轉發）為 false——它們指向任意 TCP 服務，拿代理協定去打必定失敗，
/// 只會製造一個永遠亮著的假紅點；②④（`probeProxy`）與 ⑤（`socks`）為 true。
pub fn should_probe(kind: RowKind, probe_proxy: bool) -> bool {
    matches!(kind, RowKind::Socks) || probe_proxy
}

/// 要探測的列裡，哪些還需要先識別協定（W3.26）。
///
/// `socks` 列的 listener 是引擎自己起的，協定已知，免識別（§1.5）。
pub fn needs_detect(kind: RowKind) -> bool {
    !matches!(kind, RowKind::Socks)
}

/// 快照與系統匣看到的列順序：`socks` 列一律排在 `forward` 列之前，
/// 同 kind 內維持設定檔順序（§5.3／W3.40）。
///
/// 由後端保證順序、前端只在交界處插區段標題——不交給前端各自排，
/// 否則系統匣與主視窗會排出兩種順序。SSH 連線只會有 `forward` 列，這條是恆等式。
pub fn ordered_rows(forwards: &[Forward]) -> Vec<&Forward> {
    let socks = forwards.iter().filter(|f| matches!(f.kind, RowKind::Socks));
    let rest = forwards.iter().filter(|f| !matches!(f.kind, RowKind::Socks));
    socks.chain(rest).collect()
}

/// ssh 的連線總開關：**只改連線自己的 `enabled`**，列的 `enabled` 一個都不碰。
///
/// 自 W6.12 起與 [`apply_wg_enabled`] 是同一套語意（PM 裁決：SSH 主卡總開關要與
/// WG 現行行為完全一致）——關閉時底下每一條列的逐列意圖原封不動，重新打開時
/// 只有原本 enabled = true 的那幾條會被拉起來，這正是使用者期待的「記憶效果」。
/// 舊制那種「逐條輾平覆寫每列 `Forward.enabled`」的粗粒度行為已經廢止：那是
/// 主卡選單裡已移除的 Disconnect 項在用的語意，不能沿用到總開關上。
/// 回傳 false 代表沒有這條連線。
pub fn apply_source_enabled(cfg: &mut Config, name: &str, on: bool) -> bool {
    match cfg.source_mut(name) {
        Some(s) => {
            // 只有這一行。底下 forwards 的 enabled **一個都不碰**——那是使用者
            // 的逐列意圖，連線重新打開時要原封不動地還給他（比照 apply_wg_enabled）
            s.enabled = on;
            true
        }
        None => false,
    }
}

/// wg 的連線總開關：**只改連線自己的 `enabled`**，列的 `enabled` 一個都不碰
/// （§5.5／W6.10）。回傳 false 代表沒有這條連線（W6.15）。
pub fn apply_wg_enabled(cfg: &mut Config, name: &str, on: bool) -> bool {
    match cfg.wg_proxy_mut(name) {
        Some(p) => {
            // 只有這一行。底下 forwards 的 enabled **一個都不碰**——那是使用者
            // 的逐列意圖，連線重新打開時要原封不動地還給他（§5.5 那張表）
            p.enabled = on;
            true
        }
        None => false,
    }
}

/// 這條列所屬的源有沒有被總開關關掉：`ssh::tunnel::start` 靠它擋下「源關著、
/// 卻還是有辦法讓某一條列連上」的路（例如系統匣直接勾選單一列、或存檔前的
/// upsert 順手把新列拉起來）。
///
/// wg 那邊的等價守門是 [`crate::wg::should_run_engine`]；差別只在 ssh 沒有引擎，
/// 執行單位是列本身，所以這裡問的是單一列而不是整條連線。列不存在（或不屬於
/// 任何 ssh 源）一律回 false，不必另外檢查。
pub fn row_source_enabled(cfg: &Config, local: u16) -> bool {
    cfg.locate(local).is_some_and(|(s, _)| s.enabled)
}

/// 讀檔後的 `probeProxy` 遷移掃描（§1.7／W3.27）。
///
/// serde 的 `default` 分不出「舊檔缺欄位」與「新建、而使用者就是把 switch 關著」，
/// 所以掃的是**文件**而不是設定物件，而且看的是 **`kind` 鍵在不在**：
///
/// | 檔案裡的樣子 | 判定 | `probeProxy` |
/// |---|---|---|
/// | 沒有 `kind`、也沒有 `probeProxy` | 舊格式列 | **補成 `true`** ← 這一條就是遷移 |
/// | 沒有 `kind`、明寫 `probeProxy` | 使用者寫了就照算 | 照寫的值 |
/// | 有 `kind` | 新格式列（這份檔案被新版寫過了） | serde 的值 |
///
/// `kind` 因此是「這份檔案已經被新版存過」的標記，存檔那一側跟著一律寫出它
/// （見 [`sync_forwards`]），關掉的 switch 才存得住。
///
/// **只掃 `[[sources.forwards]]`**：`[[wgProxies]]` 是新版才有的段落，
/// 世界上不存在「舊格式的 wg 列」，掃它只會把使用者剛關掉的旗標又打開
/// （W3.2／W3.28／W3.43）。舊制（v2）那份頂層 `[[forwards]]` 一併照顧到。
///
/// 這**不改變 `LoadOutcome`**、不算 `Migrated`（W3.30）——檔案結構沒變，
/// 只是補了一個有預設值的欄位。
pub fn backfill_probe_proxy(doc: &DocumentMut, cfg: &mut Config) {
    /// 一張列表格要不要被補：`kind` 與 `probeProxy` 兩個鍵都缺席才算舊格式列
    fn is_legacy_row(t: &Table) -> bool {
        !t.contains_key("kind") && !t.contains_key("probeProxy")
    }

    fn backfill_rows(tables: &ArrayOfTables, forwards: &mut [Forward]) {
        for (i, t) in tables.iter().enumerate() {
            if is_legacy_row(t) {
                if let Some(f) = forwards.get_mut(i) {
                    f.probe_proxy = true;
                }
            }
        }
    }

    match doc.get("sources") {
        Some(Item::ArrayOfTables(sources)) => {
            for (i, st) in sources.iter().enumerate() {
                let (Some(Item::ArrayOfTables(rows)), Some(s)) =
                    (st.get("forwards"), cfg.sources.get_mut(i))
                else {
                    continue;
                };
                backfill_rows(rows, &mut s.forwards);
            }
        }
        // 舊制（v2）：頂層 `[[forwards]]`，`into_config` 已經把它們整批收進 sources[0]
        _ => {
            if let (Some(Item::ArrayOfTables(rows)), Some(s)) =
                (doc.get("forwards"), cfg.sources.first_mut())
            {
                backfill_rows(rows, &mut s.forwards);
            }
        }
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
                enabled: true,
                forwards: vec![
                    Forward {
                        name: "exit-a".into(),
                        local: 1080,
                        remote: Some("127.0.0.1:1080".into()),
                        kind: RowKind::Forward,
                        probe_proxy: true,
                        enabled: true,
                    },
                    Forward {
                        name: "exit-b".into(),
                        local: 1083,
                        remote: Some("127.0.0.1:1083".into()),
                        kind: RowKind::Forward,
                        probe_proxy: true,
                        enabled: true,
                    },
                ],
            }],
            wg_proxies: Vec::new(),
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
         # probeProxy = true 代表這條轉發的目的地是一個代理服務，會定期檢測並顯示出口 IP；\n\
         # 目的地是一般 TCP 服務（資料庫、ssh…）時省略不寫即可。\n\
         [[sources.forwards]]\n\
         name = \"{fa}\"\n\
         local = {la}\n\
         remote = \"{ra}\"\n\
         probeProxy = true\n\
         enabled = true\n\
         \n\
         [[sources.forwards]]\n\
         name = \"{fb}\"\n\
         local = {lb}\n\
         remote = \"{rb}\"\n\
         probeProxy = true\n\
         enabled = true\n",
        close = c.close_to_tray,
        sname = s.name,
        host = s.host,
        user = s.user,
        proxy = s.proxy_command,
        fa = s.forwards[0].name,
        la = s.forwards[0].local,
        ra = s.forwards[0].remote.as_deref().unwrap_or_default(),
        fb = s.forwards[1].name,
        lb = s.forwards[1].local,
        rb = s.forwards[1].remote.as_deref().unwrap_or_default(),
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
                // 舊制沒有這個欄位，遷移後視為開啟——舊檔裡的連線本來就是在跑的
                enabled: true,
                forwards: self.forwards,
            }],
            // 舊制當然沒有 wg 代理
            wg_proxies: Vec::new(),
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
    // 反序列化會吃掉 doc，但遷移掃描要看的是**文件**（哪些鍵在檔案裡真的出現過），
    // 解析完就分不出來了，所以先留一份。設定檔本來就只有幾 KB
    let mut cfg: Config = if legacy {
        let old: LegacyConfig =
            toml_edit::de::from_document(doc.clone()).map_err(|e| e.to_string())?;
        old.into_config()
    } else {
        toml_edit::de::from_document(doc.clone()).map_err(|e| e.to_string())?
    };
    backfill_probe_proxy(&doc, &mut cfg);
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
    for p in cfg.wg_proxies.iter_mut() {
        p.name = p.name.trim().to_string();
        p.conf_path = p.conf_path.trim().to_string();
        for f in p.forwards.iter_mut() {
            f.name = f.name.trim().to_string();
        }
    }
}

/// 讀進來的設定同樣支援純埠號的簡寫：手寫 `remote = "8080"` 一樣算數，在這裡就補成完整形式。
///
/// 補在解析的出口處，程式其他地方（ssh 參數、介面顯示、下次存檔）拿到的就永遠是
/// `host:port`，不必各自再判斷一次；下次存檔時檔案裡那個 `8080` 也會被寫成完整形式。
fn normalize_remotes(cfg: &mut Config) {
    let ssh = cfg.sources.iter_mut().flat_map(|s| s.forwards.iter_mut());
    let wg = cfg.wg_proxies.iter_mut().flat_map(|p| p.forwards.iter_mut());
    // `socks` 列沒有目的地（remote 是 None），跳過而不是補一個空字串
    for f in ssh.chain(wg) {
        if let Some(remote) = f.remote.as_deref() {
            f.remote = Some(normalize_remote(remote));
        }
    }
}

/// 只要設定不管遷移旗標的簡便版
#[cfg(test)]
pub fn parse_config(raw: &str) -> Result<Config, String> {
    parse_document(raw).map(|(cfg, _)| cfg)
}

/// 撞埠訊息裡的佔用者描述：說得出是哪一條連線的**哪一種列**（W3.12）。
///
/// `socks` 列與 `forward` 列在使用者眼裡是兩種東西，訊息分不出來的話，
/// 他只會看到一個查不出原因的撞埠。轉發列不加限定詞——那是預設的那一種，
/// 而且既有的 `local: port X already used by <這一段>` 訊息一字未改。
fn describe_row(conn: &str, f: &Forward) -> String {
    match f.kind {
        RowKind::Socks => format!("socks row {} in {conn}", f.name),
        RowKind::Forward => format!("{} in {conn}", f.name),
    }
}

/// 一條列的欄位規則，兩型連線共用（§1.3 的五種列）。
fn check_row(conn: &str, f: &Forward) -> Result<(), String> {
    match f.kind {
        RowKind::Socks => {
            if f.remote.is_some() {
                return Err(format!(
                    "連線 {conn} 的 socks 列 {} 不可以有 remote（引擎自建的 listener 沒有目的地）",
                    f.name
                ));
            }
            if f.probe_proxy {
                return Err(format!(
                    "連線 {conn} 的 socks 列 {} 不可以有 probeProxy（它恆測）",
                    f.name
                ));
            }
        }
        RowKind::Forward => {
            if f.remote.is_none() {
                return Err(format!(
                    "連線 {conn} 的轉發列 {} 缺 remote（kind = forward 時必填）",
                    f.name
                ));
            }
        }
    }
    // 欄位規則與介面輸入共用同一份判定（見 check_forward_fields），兩條路才不會分岔。
    // 壞值放行的話會一路餵進 ssh -L 或 smoltcp，換來的是每 5 秒重連一次卻永遠接不起來
    if !valid_name(&f.name) {
        return Err(format!("連線 {conn} 底下有列的 name 為空或含空白"));
    }
    if f.local == 0 {
        return Err(format!("出口 {} 的 local 要落在 1-65535", f.name));
    }
    if let Some(remote) = f.remote.as_deref() {
        if !valid_remote(&normalize_remote(remote)) {
            return Err(format!(
                "出口 {} 的 remote 不合法：{remote}（要寫成 host:port，例如 127.0.0.1:1080，或只填埠號）",
                f.name
            ));
        }
    }
    Ok(())
}

/// 認領一個本地埠。它是列的**全域唯一鍵**（D5），跨連線、跨連線型都不可以重複。
fn claim_local(seen: &mut Vec<(u16, String)>, conn: &str, f: &Forward) -> Result<(), String> {
    if let Some((_, owner)) = seen.iter().find(|(l, _)| *l == f.local) {
        return Err(format!(
            "本地埠重複：{}（{owner} 與{}撞在一起，跨連線也不可以重複）",
            f.local,
            describe_row(conn, f)
        ));
    }
    seen.push((f.local, describe_row(conn, f)));
    Ok(())
}

/// 讀進來的設定必須自洽，否則寧可當壞檔也不要帶著矛盾的狀態跑
fn validate_config(cfg: &Config) -> Result<(), String> {
    // 兩型連線共用同一個命名空間：日誌前綴是 `[名字]`，撞名就分不出誰是誰（§5.1）
    let mut seen_names: Vec<&str> = Vec::new();
    // （本地埠，佔用者描述）
    let mut seen_locals: Vec<(u16, String)> = Vec::new();

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
            // SSH 沒有「自建代理」這回事（§1.2 的機制表，W3.23）
            if matches!(f.kind, RowKind::Socks) {
                return Err(format!(
                    "連線 {} 的列 {} 是 socks 列，SSH 連線不支援 socks 列",
                    s.name, f.name
                ));
            }
            check_row(&s.name, f)?;
            claim_local(&mut seen_locals, &s.name, f)?;
        }
    }

    for p in &cfg.wg_proxies {
        if !valid_source_name(&p.name) {
            return Err("[[wgProxies]] 的 name 不可為空，也不可含空白或中括號".into());
        }
        if seen_names.contains(&p.name.as_str()) {
            return Err(format!("連線名稱重複：{}", p.name));
        }
        seen_names.push(&p.name);
        if p.conf_path.trim().is_empty() {
            return Err(format!("連線 {} 的 confPath 不可為空", p.name));
        }
        if let Some(n) = p.mtu {
            if !crate::wg::conf::MTU_RANGE.contains(&n) {
                return Err(format!(
                    "連線 {} 的 mtu {n} 超出合法範圍 {}..={}",
                    p.name,
                    crate::wg::conf::MTU_RANGE.start(),
                    crate::wg::conf::MTU_RANGE.end()
                ));
            }
        }
        for f in &p.forwards {
            check_row(&p.name, f)?;
            claim_local(&mut seen_locals, &p.name, f)?;
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

/// 巢狀的列陣列（`[[sources.forwards]]`／`[[wgProxies.forwards]]`）：認 local。
///
/// `mark_kind` 決定 `kind == Forward` 的列要不要寫出 `kind = "forward"`：
///
/// * `[[sources.forwards]]` **要寫**。`kind` 鍵是「這份檔案已經被新版存過」的
///   遷移標記（§1.7），沒有它的話 `probeProxy = false`（省略不寫）與「舊格式檔」
///   在檔案裡長得一模一樣，讀檔那一側就會把使用者剛關掉的 switch 又補回 true
///   ——關掉的檢測存不住，重開程式又自己亮起來，而且怎麼試都關不掉。
/// * `[[wgProxies.forwards]]` **不用寫**。這整個段落是新版才有的，世界上不存在
///   「舊格式的 wg 列」，遷移掃描也不掃它，多寫一行只是噪音（W3.43）。
fn sync_forwards(tables: &mut ArrayOfTables, forwards: &[Forward], mark_kind: bool) {
    sync_tables(
        tables,
        forwards,
        |t| t.get("local").and_then(Item::as_integer),
        |f: &Forward| f.local as i64,
        |t, f| {
            t["name"] = value(f.name.as_str());
            t["local"] = value(f.local as i64);
            // 鍵省略規則（§5.1／W3.43）：舊檔改一個欄位不會突然長出一堆新鍵。
            // `remote = None` 是移除該鍵而不是寫一個空字串——「沒有目的地」與
            // 「目的地是空字串」在型別上分得開，落檔也要分得開
            match f.remote.as_deref() {
                Some(r) => t["remote"] = value(r),
                None => {
                    t.remove("remote");
                }
            }
            match f.kind {
                RowKind::Forward if mark_kind => t["kind"] = value("forward"),
                RowKind::Forward => {
                    t.remove("kind");
                }
                RowKind::Socks => t["kind"] = value("socks"),
            }
            if f.probe_proxy {
                t["probeProxy"] = value(true);
            } else {
                t.remove("probeProxy");
            }
            t["enabled"] = value(f.enabled);
        },
    );
}

/// 頂層的 `[[wgProxies]]`：認 name，順手把自己底下的列也同步掉（W3.13～W3.16）
fn sync_wg_proxies(tables: &mut ArrayOfTables, proxies: &[WgProxy]) {
    sync_tables(
        tables,
        proxies,
        |t| t.get("name").and_then(Item::as_str).map(str::to_owned),
        |p: &WgProxy| p.name.clone(),
        |t, p| {
            t["name"] = value(p.name.as_str());
            t["confPath"] = value(p.conf_path.as_str());
            t["enabled"] = value(p.enabled);
            // 鍵省略規則：沒有覆寫就不留鍵，別讓「照 .conf」在檔案上長成一個
            // 看起來像被明確指定過的數字
            match p.mtu {
                Some(n) => t["mtu"] = value(n as i64),
                None => {
                    t.remove("mtu");
                }
            }
            if !matches!(t.get("forwards"), Some(Item::ArrayOfTables(_))) {
                t["forwards"] = Item::ArrayOfTables(ArrayOfTables::new());
            }
            if let Some(Item::ArrayOfTables(fts)) = t.get_mut("forwards") {
                sync_forwards(fts, &p.forwards, false);
            }
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
            t["enabled"] = value(s.enabled);
            if !matches!(t.get("forwards"), Some(Item::ArrayOfTables(_))) {
                t["forwards"] = Item::ArrayOfTables(ArrayOfTables::new());
            }
            if let Some(Item::ArrayOfTables(fts)) = t.get_mut("forwards") {
                sync_forwards(fts, &s.forwards, true);
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

    // 沒有 wg 連線、檔案裡也還沒有這一段時就完全不碰：舊使用者的設定檔不會
    // 憑空多出一個空段落。反過來，段落已經在檔案裡（哪怕連線被刪光了）就照樣
    // 同步，那些表格才收得掉
    if !cfg.wg_proxies.is_empty() || doc.get("wgProxies").is_some() {
        if !matches!(doc.get("wgProxies"), Some(Item::ArrayOfTables(_))) {
            doc["wgProxies"] = Item::ArrayOfTables(ArrayOfTables::new());
        }
        if let Some(Item::ArrayOfTables(tables)) = doc.get_mut("wgProxies") {
            sync_wg_proxies(tables, &cfg.wg_proxies);
        }
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
/// 一次列 upsert 的輸入（§5.1／§5.5）。
///
/// §5.1 說 `validate_forward` 的簽名要加 `conn_kind`、`kind`、`probe_proxy`，
/// 連同原本的四個就是七個位置參數——收成一個結構才叫得出是誰對誰，
/// 也才擋得住「兩個 bool 互換」那種編譯得過的錯。
///
/// 兩支 IPC upsert（`upsertForward`／`upsertWgSocks`）都組出這個結構再走
/// [`prepare_forward`]，各自帶入固定的 `kind`——驗證與唯一性檢查只有一份實作。
#[derive(Debug, Clone, Copy)]
pub struct RowInput<'a> {
    /// 這一條列要掛進哪一條連線
    pub connection: &'a str,
    /// 那條連線是 ssh 還是 wg（§5.5 的 `connectionKind`）。與 `connection`
    /// 指到的實際型別不符時要回 `Err`（W3.37）
    pub conn_kind: ConnKind,
    /// 編輯前的本地埠，None 代表新增
    pub original_local: Option<u16>,
    pub name: &'a str,
    pub local: u16,
    /// `forward` 列必填、`socks` 列必須為 None（§1.3 的兩條驗證規則）
    pub remote: Option<&'a str>,
    /// 機制。**建立後不可變**（U1）：與既有列不符一律回 `Err`（W3.32／W3.33）
    pub kind: RowKind,
    /// **不在不可變之列**，隨時可改（W3.34）
    pub probe_proxy: bool,
}

/// 新增／編輯列的欄位驗證，回傳 Some(訊息) 代表不通過。
///
/// 本地埠是列的**全域唯一鍵**（D5），因此連停用中的、別條連線底下的、
/// 別一型連線底下的列也算佔用——撞埠訊息要能指出佔用者是哪一條連線的哪一種列
/// （W3.12，實作走 [`port_owner`]）。
///
/// 訊息一律以欄位名開頭（`name: `／`local: `／`remote: `／`kind: `），
/// 前端才能把錯誤掛回對應的欄位上。
///
/// `remote` 先過一次 [`normalize_remote`]，只填埠號的寫法才會被放行；
/// 呼叫端存檔時也要存正規化後的值，驗證與落檔看的才是同一個字串。
pub fn validate_forward(cfg: &Config, input: &RowInput<'_>) -> Option<String> {
    if let Some(orig) = input.original_local {
        if cfg.forward(orig).is_none() {
            return Some(format!("local: no tunnel with port {orig}, it may have been deleted"));
        }
    }
    // 連線要存在，而且型別要跟呼叫端說的一致（W3.37）。型別不符一律擋：
    // 連線型別建立後不可變（U1），拿 ssh 源名去掛 wg 的列是前端的 bug 或繞過 UI 的呼叫
    let conn = match input.conn_kind {
        ConnKind::Ssh => cfg.source(input.connection).map(ConnRef::Ssh),
        ConnKind::Wg => cfg.wg_proxy(input.connection).map(ConnRef::Wg),
    };
    if conn.is_none() {
        return Some(format!(
            "connection: no {} connection called {}",
            match input.conn_kind {
                ConnKind::Ssh => "SSH",
                ConnKind::Wg => "WireGuard",
            },
            input.connection
        ));
    }
    // SSH 沒有自建代理這回事（§1.2 的機制表，W3.23／W3.38）
    if matches!(input.conn_kind, ConnKind::Ssh) && matches!(input.kind, RowKind::Socks) {
        return Some(
            "kind: an SSH connection cannot host a socks row, add it under a WireGuard connection"
                .into(),
        );
    }
    // 列的種類建立後不可變（U1，W3.32／W3.33）
    if let Some(existing) = input.original_local.and_then(|orig| cfg.forward(orig)) {
        if existing.kind != input.kind {
            return Some("kind: 列的種類建立後不可變更，請刪除後重新新增".into());
        }
    }
    if !valid_name(input.name) {
        return Some("name: required, and must not contain spaces".into());
    }
    if input.local == 0 {
        return Some("local: port must be between 1 and 65535".into());
    }
    match input.kind {
        RowKind::Forward => {
            // `remote` 先過一次 normalize_remote，只填埠號的寫法才會被放行
            if !valid_remote(&normalize_remote(input.remote.unwrap_or_default())) {
                return Some(
                    "remote: must look like host:port, for example 127.0.0.1:1080, or just a port"
                        .into(),
                );
            }
        }
        RowKind::Socks => {
            if input.remote.is_some() {
                return Some(
                    "remote: a socks row has no destination, the engine hosts the listener itself"
                        .into(),
                );
            }
            if input.probe_proxy {
                return Some(
                    "probeProxy: a socks row is always probed, it must not carry this flag".into(),
                );
            }
        }
    }
    // 本地埠是列的全域唯一鍵：停用中的、別條連線底下的、別一型連線底下的都算佔用
    if Some(input.local) != input.original_local {
        if let Some(owner) = port_owner(cfg, input.local) {
            return Some(format!("local: port {} already used by {owner}", input.local));
        }
    }
    None
}

/// 新增／編輯出口的前處理：欄位正規化 + 驗證，通過就直接給出要存進設定的那一筆。
///
/// 這是介面輸入進到設定裡的唯一入口。驗證看的是這裡組出來的 [`Forward`]，
/// 呼叫端也只准把回傳的這一筆原封不動存下去，「驗過的字串」與「落檔的字串」
/// 才不可能各走各的——remote 的埠號糖也只在這裡加一次。
pub fn prepare_forward(
    cfg: &Config,
    input: &RowInput<'_>,
    enabled: bool,
) -> Result<Forward, String> {
    let f = Forward {
        name: input.name.trim().to_string(),
        local: input.local,
        remote: input.remote.map(normalize_remote),
        kind: input.kind,
        probe_proxy: input.probe_proxy,
        enabled,
    };
    let checked = RowInput { name: &f.name, remote: f.remote.as_deref(), ..*input };
    match validate_forward(cfg, &checked) {
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

/// `wgProxies.confPath` 的相對路徑解析基準是**設定檔所在資料夾**，
/// 不是行程的工作目錄（W3.19）。絕對路徑原樣回傳。
pub fn resolve_conf_path(config_dir: &Path, conf_path: &str) -> PathBuf {
    let p = Path::new(conf_path.trim());
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        config_dir.join(p)
    }
}

/// 撞埠時的佔用者描述，跨 ssh 的列與 wg 的列都認得（W3.12）。
///
/// 描述要分辨得出佔用者是哪一條連線的**哪一種列**——`socks` 列與 `forward` 列
/// 在使用者眼裡是兩種東西，訊息說不清楚的話，他只會看到一個查不出原因的撞埠。
pub fn port_owner(cfg: &Config, local: u16) -> Option<String> {
    cfg.row(local).map(|(conn, f)| describe_row(conn.name(), f))
}

/// MTU 覆寫欄位越界時的那一句話。**前端有一份逐字相同的副本**（sheet.ts 與
/// dev-mock.ts）：本地檢查與後端檢查講的必須是同一句，否則同一個輸入在按 Save
/// 前後會看到兩種說法。
pub fn mtu_range_error() -> String {
    format!(
        "mtu: must be a whole number between {} and {}",
        crate::wg::conf::MTU_RANGE.start(),
        crate::wg::conf::MTU_RANGE.end()
    )
}

/// 新增／編輯 wg 連線的欄位驗證，回傳掛回欄位的訊息（前綴 `name:`／`confPath:`／`mtu:`），
/// 沒問題時 None（W3.9／W3.10）。
///
/// `original_name` 是編輯前的連線名，None 代表新增。**連線型別建立後不可變**
/// （U1）：`original_name` 指向一個 ssh 源名時一律回 `Err`，不得把 ssh 源
/// 改寫成 wg 連線（W3.36）。
pub fn validate_wg_proxy(
    cfg: &Config,
    original_name: Option<&str>,
    name: &str,
    conf_path: &str,
    mtu: Option<usize>,
) -> Option<String> {
    if let Some(orig) = original_name {
        if cfg.wg_proxy(orig).is_none() {
            // 指到一個 ssh 源名時要說得出真正的理由：連線型別建立後不可變（U1／W3.36），
            // 不是「找不到」。訊息仍掛在 name 欄位上，前端才有地方顯示
            return Some(if cfg.source(orig).is_some() {
                format!("name: {orig} is an SSH connection, the connection type cannot be changed")
            } else {
                format!("name: no connection called {orig}, it may have been deleted")
            });
        }
    }
    if !valid_source_name(name) {
        return Some("name: required, and must not contain spaces or brackets".into());
    }
    // confPath 排在撞名之前：使用者最常同時踩到的是「選了檔案但路徑是空的」，
    // 先報那一個才對得上他當下在看的欄位
    if conf_path.trim().is_empty() {
        return Some("confPath: required".into());
    }
    // 空欄位＝不覆寫＝合法，所以只有真的填了東西才檢查範圍。訊息與前端
    // （sheet.ts 的 localValidateWg、dev-mock 的 validateWgProxy）逐字相同
    if let Some(n) = mtu {
        if !crate::wg::conf::MTU_RANGE.contains(&n) {
            return Some(mtu_range_error());
        }
    }
    // 兩型連線共用一個命名空間（日誌前綴是 `[名字]`）
    let taken = cfg.sources.iter().any(|s| s.name == name)
        || cfg.wg_proxies.iter().any(|p| p.name == name && Some(p.name.as_str()) != original_name);
    if taken {
        return Some(format!("name: connection {name} already exists"));
    }
    None
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

/// W3 系列（wg 設定模型）的測試。
///
/// §5 原本說「加進既有的 config_tests.rs」，這裡改成獨立檔掛在同一層：既有那份
/// 一千四百行的測試檔在這一輪只被允許做「補上新欄位」的機械性修改，把新測試
/// 隔出來才看得清楚哪些是這一棒加的。模組路徑一樣拿得到 config 的私有項。
#[cfg(test)]
#[path = "config_wg_tests.rs"]
mod wg_tests;
