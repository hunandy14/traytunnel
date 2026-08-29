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

use std::io;
use std::time::Duration;

use semver::Version;

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
}
