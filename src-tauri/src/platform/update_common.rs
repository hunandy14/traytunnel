//! 應用內更新：兩個平台逐字相同的那一小段純邏輯。
//!
//! `platform/mod.rs` 開頭說得很清楚——整個 `update` 子模組是「按平台各自提供」的
//! （封裝格式完全不同：Windows 是 NSIS 暫存交棒，macOS 是當場替換 bundle），
//! 所以背景排程、暫存、下載安裝這些都留在 `platform::{macos,windows}::update`
//! 裡，不搬過來。這裡放的只是那條路上**不看平台**的字串與數字運算：
//! 逾時／間隔常數、release 網址組法、版本號正規化與 semver 比較。這幾件事在
//! 兩邊原本一字不差各抄一份，抄兩份唯一的下場是「哪天改一個忘了改另一個」，
//! 沒有任何好處，所以上提到這裡，兩平台的 `update.rs` 改成引用同一份。
//!
//! `open_page`（連同 [`open_release_page`]／[`open_releases_page`]）仍然要
//! 「開系統瀏覽器」這件真的碰系統 API 的事——Windows 是 `ShellExecuteW`，
//! macOS 是 `open` 指令，各自留在 `platform::{macos,windows}::sys`。這裡不直接
//! 伸手進 `platform::macos::sys`／`platform::windows::winsys` 去挑，而是讓呼叫端
//! 把 `open_url` 當函式指標傳進來：`platform/mod.rs` 的門面刻意不收
//! `open_url`（它唯一的呼叫端就是這條路），這個模組要是自己 import 兩邊的
//! `open_url` 再用 `cfg` 挑一個，等於在門面之外又開了一個一樣的洞。函式指標
//! 注入把「該用哪個 `open_url`」這個決定留在各平台的 `update.rs` 手上，這裡
//! 只管「查完之後開哪個網址、失敗了記哪一行日誌」。
//!
//! 本檔這幾支的六支測試，兩邊原本逐字相同（連斷言帶註解都一樣），這裡只留一份。
//!
//! ## 出貨設定的單一來源（[`TAURI_CONF`]／[`conf_str`]／[`IDENTIFIER`]／[`PRODUCT_NAME`]）
//!
//! 這一段嚴格說不是「更新」邏輯——`IDENTIFIER` 同時是 macOS `pgids` 登記簿的
//! 資料夾名，跟更新一點關係都沒有。比較乾淨的做法本來是另開一個平台中立的
//! `shipped_conf` 模組，專門收「編譯期讀出貨的 tauri.conf.json」這件事；
//! 但本車道的紀律是不動 `lib.rs`，而新模組得在那裡加一行 `mod` 宣告才掛得上
//! 這棵樹。`update_common` 已經掛在 `platform/mod.rs` 底下不必再加線，且它本來
//! 就是「平台中立的共用邏輯」這個角色，所以退而求其次收在這裡——比起兩平台
//! 各自維護一份「怎麼讀 tauri.conf.json」，多掛一個不完全貼題的名字划算得多。
//! 兩者原本都各自 `include_str!` 一次同一份檔案：Windows 的
//! `update::conf_str`／`PRODUCT_NAME`／`IDENTIFIER`（只有它需要在
//! `apply_pending_at_startup` 那個沒有 `AppHandle` 的時間點問設定），macOS 的
//! `pgids::IDENTIFIER` 則是寫死一份字面常數、另外用一條測試釘住不能漂掉。
//! 現在兩邊都改成從這裡讀同一份，字面常數與那條釘住測試也就一起沒有存在的
//! 必要——不是漂不漂的問題，是它現在就是同一個值的兩個名字。

use std::io;
use std::sync::LazyLock;
use std::time::Duration;

use semver::Version;

use crate::state::UpdateInfo;
use crate::Shared;

/// 啟動後隔這麼久才做第一次檢查：開機當下要先把系統匣、隧道那些真正要緊的事做完，
/// 更新檢查是最不急的一件。兩平台同一個值。
pub const FIRST_DELAY: Duration = Duration::from_secs(8);

/// 常駐期間的檢查間隔。兩平台同一個值。
pub const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 查 latest.json 的逾時。
///
/// updater 外掛 builder 的預設是 `None`，也就是**完全沒有上限**：GitHub 那邊一旦
/// 是半開的連線（封包進得去、回應永遠不來），這個 async 任務就再也不會回來。
/// 背景檢查每 24 小時起一次，卡住的任務會一直累積；手動按下的那顆「Check now」
/// 更糟，前端的 await 沒有逾時，按鈕會永遠停在轉圈。兩平台同一個值。
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// 下載更新包的逾時。
///
/// 與 [`CHECK_TIMEOUT`] 分開設是因為兩段的性質完全不同：查版本只拉一份幾百位元組
/// 的 JSON，超過半分鐘一定是卡住了；下載拉的是十幾 MB 的安裝檔／`.app.tar.gz`，
/// 而 reqwest 的 `timeout` 管的是**整個請求含讀完 body** 的總時間，設窄了會把慢速
/// 但正常的下載一起砍掉。它要擋的是永遠不會結束的連線，不是慢的連線。
///
/// 這一段的值傳不進 builder——外掛建 `Update` 物件時把 timeout 寫死成 `None`
/// （2.10.1 的 updater.rs），builder 上設的那個只作用在 check 那次請求。
/// 所以只能在拿到 `Update` 物件之後對它的 pub 欄位直接賦值。兩平台同一個值。
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// 更新資訊清單。安裝版由 updater 外掛（tauri.conf.json 的 endpoints）去拿，
/// Windows 可攜／單檔車道的 `fetch_latest_version` 與 macOS 的 live 測試各自
/// 另外拉一次，三邊指的是同一份檔案。兩平台原本各自宣告一份（Windows 是
/// production 常數，macOS 只有測試模組裡那份），字面值完全相同，這裡只留一份。
//
// 這兩個 cfg_attr 要長期留著（同 `config::automatic_updates_enabled` 那份的
// 理由）：macOS 唯一的消費者是 `#[ignore]` 的 live 測試，不是 production 路徑
// ——它沒有像 Windows 可攜車道那樣自己拉 latest.json 比版本，查詢整段外包給
// updater 外掛（見 `macos::update` 模組說明）。拿掉的話 macOS 那一腿的
// `cargo clippy --all-targets`（不含 `--tests` 的 `lib` 目標看不到測試模組）
// 會 dead_code 撞上 `-D warnings` 紅燈。
#[cfg_attr(not(windows), allow(dead_code))]
pub const LATEST_JSON: &str =
    "https://github.com/hunandy14/traytunnel/releases/latest/download/latest.json";

/// GitHub 對沒有 User-Agent 的請求會直接回 403，一定要帶。兩平台同一個值，
/// 同樣只有 Windows 的 production 路徑會用到，理由同 [`LATEST_JSON`]。
#[cfg_attr(not(windows), allow(dead_code))]
pub const USER_AGENT: &str = concat!("traytunnel/", env!("CARGO_PKG_VERSION"));

/// Releases 列表頁：下拉選單的「Download from Releases」開這裡，
/// 使用者可以自己挑要哪一版（含更早的版本）
pub const RELEASES_PAGE: &str = "https://github.com/hunandy14/traytunnel/releases";

/// 單一版本的 release 頁前綴。發佈說明與該版的下載資產都在同一頁上，
/// 所以「View release notes」與非就地更新車道的「Get vX.Y.Z」開的是同一個網址。
pub const RELEASE_TAG_PREFIX: &str = "https://github.com/hunandy14/traytunnel/releases/tag/v";

/// 還不知道是哪一版（沒查過或查不到）時，release 頁退回這裡
pub const LATEST_RELEASE_PAGE: &str = "https://github.com/hunandy14/traytunnel/releases/latest";

/// 版本號的比較用形式：去空白、去前導的 v。
///
/// 兩平台都只有這一份：`is_newer`、`UpdateInfo` 的版本欄位、release 頁網址都吃它，
/// 各自寫一次 `trim_start_matches` 的話遲早會有一處漏掉而讓 `v0.7.0`
/// 與 `0.7.0` 被當成兩個版本。
pub fn normalize_version(version: &str) -> &str {
    version.trim().trim_start_matches(['v', 'V'])
}

/// 遠端版本是不是嚴格大於目前這一版。
///
/// 用 semver 的比較規則（它已經在相依樹裡，是 updater 外掛自己在用的那一顆），
/// 因此 0.10.0 > 0.9.0，而 pre-release 一律小於同號的正式版。兩邊任一個解析
/// 不出來就當成「沒有新版」：更新提示寧可漏報，也不要因為一個怪字串誤報。
pub fn is_newer(remote: &str, current: &str) -> bool {
    let parse = |s: &str| Version::parse(normalize_version(s)).ok();
    match (parse(remote), parse(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

/// 這一份執行檔是哪一版。用編譯期常數，兩平台同一個來源。
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 外掛（或自己拿 latest.json 比對出來）回報的那一版，要不要真的當成新版。
///
/// 外掛預設的比較器確實是嚴格大於（2.10.1 的 updater.rs：
/// `release.version > self.current_version`），所以正常情況下這一關不會擋掉任何
/// 東西。留著它是因為這條路上「說有新版」的權力整個握在外部相依（或遠端那份
/// latest.json）手上：換版本、有人塞了 version_comparator、或內容長出沒預期的
/// 形狀，都可能讓那個 Option 變成 Some 而我們這層毫無反抗餘地。
///
/// 更新提示的失敗方向是不對稱的：漏報只是使用者晚幾天更新，誤報卻是叫他去
/// 重裝一個他已經在用的版本。因此版本比對這件事自己再做一次，任何比不出
/// 「嚴格大於」的情形（含空字串、解析不出來的怪版本號）一律當成沒有新版。
///
/// `installed` 純粹是把呼叫端已經知道的「這一份能不能就地更新」原樣包進
/// [`UpdateInfo`]，這支函式自己不判斷——三處呼叫端（macOS 唯一車道、Windows
/// 安裝版車道、Windows 可攜／單檔車道）給的是不同的常數或探測結果，原本各自
/// 抄一份幾乎一模一樣的「比版本、包結構」，現在收成這一份。
pub fn accept(remote: &str, current: &str, installed: bool) -> Option<UpdateInfo> {
    if !is_newer(remote, current) {
        return None;
    }
    Some(UpdateInfo { version: normalize_version(remote).to_string(), installed })
}

/// 某一版的 release 頁網址。版本給 None（還沒查到新版）就退回 releases/latest，
/// 使用者按下「View release notes」時至少看得到最新那一版的說明。
///
/// 純函式，網址組法有問題要在測試裡就看得出來，不必等到實機按下去開錯頁。
/// 版本號裡混進路徑分隔符或空白就不是版本號了，一律退回 latest——那既是網址
/// 正確性，也是不讓一個從遠端來的字串把網址帶去別的地方。
pub fn release_url(version: Option<&str>) -> String {
    let tag = version.map(normalize_version).filter(|v| !v.is_empty() && !v.contains(['/', ' ']));
    match tag {
        Some(v) => format!("{RELEASE_TAG_PREFIX}{v}"),
        None => LATEST_RELEASE_PAGE.to_string(),
    }
}

/// 單一版本的 release 頁：發佈說明與該版的下載資產都在上面。
/// 非就地更新車道的「Get vX.Y.Z」與下拉的「View release notes」都走這裡。
///
/// `open_url` 是呼叫端注入的那個平台的「用系統預設瀏覽器開網址」——見本檔開頭
/// 的說明，這個模組刻意不自己伸手去挑。
pub fn open_release_page(st: &Shared, version: Option<&str>, open_url: fn(&str) -> io::Result<()>) {
    open_page(st, &release_url(version), open_url);
}

/// Releases 列表頁：下拉的「Download from Releases」走這裡，
/// 讓使用者自己挑版本換檔案。這條路不下載、不碰自己這顆執行檔／bundle。
pub fn open_releases_page(st: &Shared, open_url: fn(&str) -> io::Result<()>) {
    open_page(st, RELEASES_PAGE, open_url);
}

fn open_page(st: &Shared, url: &str, open_url: fn(&str) -> io::Result<()>) {
    if let Err(e) = open_url(url) {
        st.log(format!("could not open {url}: {e}"));
    }
}

// ---------------------------------------------------------------- 查完之後的簿記
//
// 手動按下的檢查（設定頁「Check for updates」、下拉「Check now」）與背景排程
// 的檢查，兩邊「查完之後」那一段原本兩平台各抄一份、逐字相同：跑一次
// `check_lane`（各平台自己的判斷邏輯，回傳型別不同，留在呼叫端）→ 失敗記一行
// 「update check failed」→ 成功就 `set_update`→ 依「有新版／已是最新」分別記
// 一行。這裡收成兩支：手動路要把結果整個回傳給呼叫端（按鈕靠它顯示 Up to
// date／Check failed 這兩個瞬態），背景路對這兩種結果都是靜默的，只在
// `set_update` 回報「真的變了」時記一行「update available」，避免每 24 小時
// 都在活動日誌裡重複同一句話。
//
// 兩支都吃已經正規化成 `Result<Option<UpdateInfo>, String>` 的結果，不是
// `check_lane` 本身——Windows 的 `check_lane` 回的是帶著 `Update` 物件的
// `Found`（下載要用），這裡收窄成 `Option<UpdateInfo>` 的話那個物件就丟了，
// 所以呼叫端自己先用 `Found::info()` 轉一次，原始的 `Found` 留在自己手上
// 接著判斷要不要順手下載。

/// 使用者主動按下的檢查：查一次的結果记進 state、記一行日誌，原封不動回傳給
/// 呼叫端。
///
/// 與背景路不同，這裡**兩種結果都記日誌**（有新版／已是最新），因為按鈕上
/// 那兩個瞬態就是靠這兩行日誌背後的 `set_update` 呈現的。
pub fn record_manual_check(
    st: &Shared,
    found: Result<Option<UpdateInfo>, String>,
) -> Result<Option<UpdateInfo>, String> {
    let found = match found {
        Ok(found) => found,
        Err(e) => {
            st.log(format!("update check failed: {e}"));
            return Err(e);
        }
    };
    st.set_update(found.clone());
    match &found {
        Some(u) => st.log(format!("update check: v{} is available", u.version)),
        None => st.log("update check: already up to date"),
    }
    Ok(found)
}

/// 背景排程的檢查：查失敗只記一行就算了——更新查不到不影響程式本身能不能用，
/// 沒有理由為它彈通知或改變任何狀態。
///
/// 查成功一律 `set_update`，但「偵測到新版」這一行只在真的變化時記一次
/// （去重靠 `set_update` 的回傳值）；「已經是最新版」在背景路完全靜默——
/// 每 24 小時都重複同一句話只會讓活動日誌看起來像又發生了什麼事。
///
/// 回傳這一輪查到的東西，讓呼叫端自己接著判斷（Windows 的安裝版車道要看是不是
/// 手上那個 `Found::Installed` 才順手觸發下載，那個判斷留在呼叫端）。
pub fn record_background_check(
    st: &Shared,
    found: Result<Option<UpdateInfo>, String>,
) -> Option<UpdateInfo> {
    let found = match found {
        Ok(found) => found,
        Err(e) => {
            st.log(format!("update check failed: {e}"));
            return None;
        }
    };
    if st.set_update(found.clone()) {
        if let Some(u) = &found {
            st.log(format!("update available: v{}", u.version));
        }
    }
    found
}

// ---------------------------------------------------------------- 出貨設定

/// 會出貨的那份 tauri.conf.json，編譯期就嵌進來。
///
/// Windows 的 `apply_pending_at_startup` 跑在 `tauri::Builder` 之前，那時還沒有
/// `AppHandle` 可以問設定；macOS 的 `pgids` 登記簿那一層完全沒有 `AppHandle`
/// 可拿（`ProcessSupervisor::spawn` 手上只有一個 `Command`）。兩邊都只能自己讀
/// 這份檔案，直接讀而不是各自抄一份常數：抄的話會漂，漂掉的症狀是 Windows
/// 安裝版被誤判成可攜版（更新整條路靜默失效），或 macOS 的登記簿寫進一個沒人
/// 會讀的資料夾。
pub const TAURI_CONF: &str = include_str!("../../tauri.conf.json");

/// tauri.conf.json 裡的一個頂層字串欄位。解析不出來就 panic——那代表出貨的設定
/// 檔壞了或改了形狀，是編譯期就該被發現的事，絕不能默默退回一個猜的值。
pub fn conf_str(key: &str) -> String {
    let conf: serde_json::Value =
        serde_json::from_str(TAURI_CONF).expect("tauri.conf.json 必須是合法 JSON");
    conf.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("tauri.conf.json 少了頂層的 {key}"))
        .to_string()
}

/// 產品名，也就是 NSIS 拿去當解除安裝機碼名的那個字串（tauri.conf.json 的
/// productName）。只有 Windows 用得到（NSIS 的解除安裝機碼），macOS 沒有對應
/// 概念。
#[cfg_attr(not(windows), allow(dead_code))]
pub static PRODUCT_NAME: LazyLock<String> = LazyLock::new(|| conf_str("productName"));

/// 應用識別碼（tauri.conf.json 的 identifier）。兩平台都用得到：Windows 那邊是
/// `%LOCALAPPDATA%` 底下的資料夾名（Tauri 的 `app_local_data_dir()` 在 Windows
/// 上就是 `%LOCALAPPDATA%\{identifier}`）、single-instance 外掛的具名互斥鎖、
/// 通知的 AUMID；macOS 那邊是 `pgids` 登記簿所在的
/// `~/Library/Application Support/{identifier}`。
pub static IDENTIFIER: LazyLock<String> = LazyLock::new(|| conf_str("identifier"));

#[cfg(test)]
mod tests {
    use super::*;

    /// 只有嚴格大於才算新版：同版與舊版都不可以跳出更新提示
    #[test]
    fn only_a_strictly_greater_version_counts() {
        assert!(is_newer("0.5.0", "0.4.3"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.4.3", "0.4.3"));
        assert!(!is_newer("0.4.2", "0.4.3"));
    }

    /// 字串比大小的經典陷阱：字面上 "0.10.0" < "0.9.0"，semver 規則下才是大的
    #[test]
    fn ten_is_newer_than_nine() {
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.9.0", "0.10.0"));
    }

    /// latest.json 的 version 寫成 v0.5.0 也照樣認得；pre-release 小於同號正式版
    #[test]
    fn leading_v_is_tolerated_and_prerelease_is_older() {
        assert!(is_newer("v0.5.0", "0.4.3"));
        assert!(is_newer("0.5.0", "v0.4.3"));
        assert!(!is_newer("0.5.0-rc.1", "0.5.0"));
        assert!(is_newer("0.5.0", "0.5.0-rc.1"));
    }

    /// 解析不出來一律當成沒有新版：更新提示寧可漏報也不要誤報
    #[test]
    fn garbage_never_reports_an_update() {
        assert!(!is_newer("", "0.4.3"));
        assert!(!is_newer("latest", "0.4.3"));
        assert!(!is_newer("0.5", "0.4.3"));
        assert!(!is_newer("0.5.0", "not-a-version"));
    }

    /// release 頁的網址組法：帶不帶 v 都要組出同一個 tag 頁，
    /// 不知道版本時退回 releases/latest 而不是組出一個 tag 是空的壞網址
    #[test]
    fn a_release_url_points_at_that_version_or_falls_back_to_latest() {
        assert_eq!(
            release_url(Some("0.6.0")),
            "https://github.com/hunandy14/traytunnel/releases/tag/v0.6.0"
        );
        assert_eq!(release_url(Some("v0.6.0")), release_url(Some("0.6.0")));
        assert_eq!(release_url(Some("  0.6.0  ")), release_url(Some("0.6.0")));
        let latest = "https://github.com/hunandy14/traytunnel/releases/latest";
        assert_eq!(release_url(None), latest);
        assert_eq!(release_url(Some("")), latest);
        assert_eq!(release_url(Some("   ")), latest);
        // 版本號裡混進路徑分隔符就不是版本號了，不可以讓它把網址帶去別的地方
        assert_eq!(release_url(Some("0.6.0/../../evil")), latest);
    }

    /// 「Download from Releases」開的是列表頁，不是 releases/latest 那一頁——
    /// 整份列表才挑得到更早的版本
    #[test]
    fn the_downloads_menu_item_opens_the_release_list() {
        assert_eq!(RELEASES_PAGE, "https://github.com/hunandy14/traytunnel/releases");
    }

    /// 出貨的 tauri.conf.json 裡，updater 的 endpoint 必須就是 [`LATEST_JSON`]。
    ///
    /// 兩平台原本各自 `include_str!` 一次 tauri.conf.json 釘住這件事，巧的是
    /// 釘法不完全一樣：macOS 那份直接用 JSON pointer 比對字串是不是
    /// `LATEST_JSON`；Windows 那份多經過 `tauri_plugin_updater::Config` 解析出
    /// 型別，順便驗了簽章公鑰不是空的。兩邊查的其實是同一件事的兩個切面，合成
    /// 這一份：**兩邊原本各自檢查過的東西這裡一件都沒少**——公鑰非空
    /// （Windows）、只有一個 endpoint（兩邊）、那個 endpoint 恰好等於
    /// `LATEST_JSON`（macOS 的精確比對，蘊含了 Windows 原本另外分開驗的
    /// https scheme 與 `/latest.json` 結尾）。
    ///
    /// Windows 自己的 `the_shipped_updater_config_parses` 留著，但只保留這裡
    /// 沒有涵蓋到的部分（安裝畫面模式、`createUpdaterArtifacts` 開關）。
    #[test]
    fn the_shipped_updater_endpoint_matches_latest_json() {
        let conf: serde_json::Value =
            serde_json::from_str(TAURI_CONF).expect("tauri.conf.json 必須是合法 JSON");
        let updater = conf.pointer("/plugins/updater").expect("plugins.updater 不可以消失");
        let parsed: tauri_plugin_updater::Config =
            serde_json::from_value(updater.clone()).expect("updater 設定要解析得出來");

        assert!(!parsed.pubkey.is_empty(), "沒有公鑰就驗不了簽章");
        assert_eq!(parsed.endpoints.len(), 1);
        assert_eq!(parsed.endpoints[0].as_str(), LATEST_JSON);
    }
}
