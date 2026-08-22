//! 應用內更新。
//!
//! 兩條車道是**各自獨立**的，共用的只有「發現新版就填進 `AppState` 的 update 欄位」
//! 這一個出口：
//!
//! 1. 安裝版（NSIS 裝進 %LOCALAPPDATA% 的那一份）走官方 tauri-plugin-updater，
//!    下載簽好章的 setup.exe 就地更新，安裝程式起來時這支程式會自己退出。
//! 2. 非安裝版（一般單檔與可攜版）不就地更新，只比對版本並把使用者送去
//!    Releases 頁自己換檔案（見 `portable` 模組）。
//!
//! 是哪一條車道由 [`is_installed`] 判定：讀 NSIS 寫的 HKCU 解除安裝機碼，
//! 而且 InstallLocation 要真的就是自己這支執行檔的所在資料夾——只看機碼存不存在
//! 是不夠的，使用者大可以裝了一份、又另外抓一顆單檔 exe 放在別的地方跑。
//!
//! 檢查一律在背景做，失敗完全靜默（只在活動日誌留一行），絕不彈窗、不擋操作。

use std::path::Path;
use std::time::Duration;

use semver::Version;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

use crate::state::UpdateInfo;
use crate::Shared;

/// 啟動後隔這麼久才做第一次檢查：開機當下要先把系統匣、隧道那些真正要緊的事做完，
/// 更新檢查是最不急的一件。
const FIRST_DELAY: Duration = Duration::from_secs(8);

/// 常駐期間的檢查間隔
const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 更新資訊清單。安裝版由 updater 外掛（tauri.conf.json 的 endpoints）去拿，
/// 非安裝版由下面的 `fetch_latest_version` 自己拿，兩邊指向同一份檔案。
const LATEST_JSON: &str =
    "https://github.com/hunandy14/traytunnel/releases/latest/download/latest.json";

/// 非安裝版按下 Download 時開的頁面
const RELEASES_PAGE: &str = "https://github.com/hunandy14/traytunnel/releases/latest";

/// 非安裝版查版本的逾時。查不到就是查不到，沒有理由讓一條卡住的連線一直掛著
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// GitHub 對沒有 User-Agent 的請求會直接回 403，一定要帶
const USER_AGENT: &str = concat!("traytunnel/", env!("CARGO_PKG_VERSION"));

/// NSIS（currentUser 模式）寫解除安裝資訊的機碼位置。
///
/// 機碼名就是 tauri.conf.json 的 productName（Tauri v2 的 NSIS 模板用
/// `${PRODUCTNAME}`），實機驗證過長這樣：
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\traytunnel`
/// 底下有 InstallLocation、UninstallString、DisplayVersion 等值。
fn uninstall_subkey(product: &str) -> String {
    format!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{product}")
}

/// 產品名，與 NSIS 用來當機碼名的那個字串同一個來源
fn product_name(app: &AppHandle) -> String {
    app.config().product_name.clone().unwrap_or_else(|| app.package_info().name.clone())
}

/// 資料夾路徑的比較用形式：去掉前後空白、NSIS 寫進去的那對雙引號、尾端的分隔符，
/// 再壓成小寫（Windows 路徑不分大小寫）。
fn normalize_dir(s: &str) -> String {
    s.trim().trim_matches('"').trim().trim_end_matches(['\\', '/']).to_lowercase()
}

/// 登錄檔裡的 InstallLocation 是不是就是這支執行檔所在的資料夾。
///
/// 純函式，實機與測試共用。**不要拿死路徑（%LOCALAPPDATA%\traytunnel）去比**：
/// NSIS 允許使用者改安裝位置，寫在登錄檔裡的那一份才是事實。
///
/// 兩個實機細節必須照顧到，否則安裝版會被誤判成可攜版：
/// NSIS 寫進去的值**連雙引號一起寫**（`"C:\Users\me\AppData\Local\traytunnel"`），
/// 而且不同版本可能帶或不帶尾端的反斜線。
pub fn location_matches(install_location: &str, exe: &Path) -> bool {
    let Some(dir) = exe.parent() else {
        return false;
    };
    let want = normalize_dir(&dir.to_string_lossy());
    let got = normalize_dir(install_location);
    !got.is_empty() && got == want
}

/// 這次跑的是不是 NSIS 裝出來的那一份。
///
/// 機碼不在（沒裝過）、值讀不到、或裝的位置根本不是自己所在的資料夾（使用者另外
/// 抓了單檔 exe 在別處跑），一律不算安裝版。
pub fn is_installed(app: &AppHandle) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let subkey = uninstall_subkey(&product_name(app));
    crate::winsys::read_hkcu_string(&subkey, "InstallLocation")
        .is_some_and(|loc| location_matches(&loc, &exe))
}

// ---------------------------------------------------------------- 背景檢查

/// 背景檢查的排程：啟動延遲一次，之後每 24 小時一次，跟著程式活到結束。
pub fn spawn_checker(state: &Shared) {
    let st = state.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_DELAY).await;
        loop {
            check_once(&st).await;
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

/// 使用者剛把「檢查更新」打開時立刻查一次，不必等到明天這個時候。
pub fn check_now(state: &Shared) {
    let st = state.clone();
    tauri::async_runtime::spawn(async move { check_once(&st).await });
}

/// 查一次。任何失敗都只記一行就算了——更新檢查不成功不影響程式本身能不能用，
/// 沒有理由為它彈通知或改變任何狀態。
async fn check_once(st: &Shared) {
    // 關掉就是完全不連外：這道閘在任何請求送出之前
    if !st.checks_for_updates() {
        return;
    }
    let found = if is_installed(&st.app) {
        check_installed(&st.app).await
    } else {
        check_unmanaged(&st.app).await
    };
    match found {
        Ok(found) => st.set_update(found),
        Err(e) => st.log(format!("update check failed: {e}")),
    }
}

/// 安裝版車道的檢查：走 updater 外掛，簽章驗證與版本比對都由它做。
async fn check_installed(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let found = updater.check().await.map_err(|e| e.to_string())?;
    Ok(found.map(|u| UpdateInfo { version: u.version, installed: true }))
}

// ------------------------------------------------- 可攜／單檔車道（不就地更新）

/// 非安裝版車道的檢查：自己拿同一份 latest.json 比版本。
///
/// 刻意**不**走 updater 外掛：它的 check 一路連著 download＋install，而那條路
/// 對非安裝版是有害的——單檔 exe 沒有安裝程式可以交棒，可攜版更不能被搬到
/// %LOCALAPPDATA% 去（設定檔就在 exe 旁邊，換了位置等於換了一份設定）。
/// 這裡只讀一個版本號，其餘什麼都不做。
async fn check_unmanaged(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let current = app.package_info().version.to_string();
    // ureq 是阻塞式的，丟到 blocking 執行緒上跑，不擋住 async runtime
    let latest = tauri::async_runtime::spawn_blocking(fetch_latest_version)
        .await
        .map_err(|e| e.to_string())??;
    if !is_newer(&latest, &current) {
        return Ok(None);
    }
    Ok(Some(UpdateInfo { version: latest, installed: false }))
}

/// 拿 latest.json 的 version 欄位。阻塞式，呼叫端負責丟到 blocking 執行緒。
fn fetch_latest_version() -> Result<String, String> {
    let config = ureq::Agent::config_builder().timeout_global(Some(HTTP_TIMEOUT)).build();
    let agent = ureq::Agent::new_with_config(config);
    let mut resp = agent
        .get(LATEST_JSON)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| e.to_string())?;
    let body: serde_json::Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    body.get("version")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "latest.json has no version field".to_string())
}

/// 遠端版本是不是嚴格大於目前這一版。
///
/// 用 semver 的比較規則（它已經在相依樹裡，是 updater 外掛自己在用的那一顆），
/// 因此 0.10.0 > 0.9.0，而 pre-release 一律小於同號的正式版。兩邊任一個解析
/// 不出來就當成「沒有新版」：更新提示寧可漏報，也不要因為一個怪字串誤報。
pub fn is_newer(remote: &str, current: &str) -> bool {
    let parse = |s: &str| Version::parse(s.trim().trim_start_matches(['v', 'V'])).ok();
    match (parse(remote), parse(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

/// 非安裝版的「Download」：開系統瀏覽器到 Releases 頁，讓使用者自己換檔案。
/// 這條路不下載、不碰自己這顆 exe。
pub fn open_releases_page(st: &Shared) {
    if let Err(e) = crate::winsys::open_url(RELEASES_PAGE) {
        st.log(format!("could not open the releases page: {e}"));
    }
}

// ---------------------------------------------------------------- 就地更新

/// 安裝版的「Restart to update」：重查一次拿到 Update 物件，下載、驗簽、執行安裝程式。
///
/// 刻意重查而不是把先前 check 的 Update 物件存起來：那份東西帶著 http client 與
/// 一堆執行期狀態，存進共用狀態只是徒增麻煩，而使用者按下按鈕的當下重查一次，
/// 拿到的也一定是最新的那一版。
///
/// **下載完成之後才收隧道**：下載可能要幾十秒，這段時間沒有理由先把使用者的連線
/// 斷掉；等真的要交棒給安裝程式了才 kill，安裝程式接手時不會有殘留的 ssh 子程序。
/// Windows 上 `install` 不會回來（它自己 `std::process::exit(0)`），
/// 所以這個函式正常路徑上只會回 Err。
pub async fn install(st: &Shared) -> Result<(), String> {
    if !is_installed(&st.app) {
        return Err("This build cannot update itself".into());
    }
    let updater = st.app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No update available".to_string())?;

    st.log(format!("downloading update v{}", update.version));
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;

    st.log("update downloaded, handing over to the installer");
    st.mark_exiting();
    st.kill_all_jobs();
    update.install(bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn exe_in(dir: &str) -> PathBuf {
        PathBuf::from(format!("{dir}\\traytunnel.exe"))
    }

    /// 實機上 NSIS 寫進 InstallLocation 的值連雙引號一起寫，
    /// 忘了剝就會每次都判成「不是安裝版」，安裝版永遠拿不到就地更新
    #[test]
    fn quoted_install_location_still_matches() {
        let exe = exe_in("C:\\Users\\me\\AppData\\Local\\traytunnel");
        assert!(location_matches("\"C:\\Users\\me\\AppData\\Local\\traytunnel\"", &exe));
        assert!(location_matches("C:\\Users\\me\\AppData\\Local\\traytunnel", &exe));
    }

    /// 尾端的分隔符與大小寫都不該影響判定（Windows 路徑不分大小寫）
    #[test]
    fn trailing_separator_and_case_do_not_matter() {
        let exe = exe_in("C:\\Users\\me\\AppData\\Local\\traytunnel");
        assert!(location_matches("\"C:\\Users\\me\\AppData\\Local\\traytunnel\\\"", &exe));
        assert!(location_matches("c:\\users\\me\\appdata\\local\\TRAYTUNNEL", &exe));
        assert!(location_matches("  C:\\Users\\me\\AppData\\Local\\traytunnel  ", &exe));
    }

    /// 裝過一份、又另外抓了一顆單檔 exe 放在別處跑：機碼是在的，但那份不是我，
    /// 這時要走可攜車道（開瀏覽器），不可以讓 NSIS 安裝程式去蓋掉另一個位置
    #[test]
    fn a_different_folder_is_not_this_install() {
        let exe = exe_in("D:\\tools");
        assert!(!location_matches("\"C:\\Users\\me\\AppData\\Local\\traytunnel\"", &exe));
        // 前綴相同但不是同一個資料夾，一樣不算
        assert!(!location_matches(
            "\"C:\\Users\\me\\AppData\\Local\\traytunnel\"",
            &exe_in("C:\\Users\\me\\AppData\\Local\\traytunnel2")
        ));
    }

    /// 值讀出來是空的（或只有一對引號）時不可以誤判成相符
    #[test]
    fn an_empty_location_never_matches() {
        assert!(!location_matches("", &exe_in("C:\\app")));
        assert!(!location_matches("\"\"", &exe_in("C:\\app")));
        assert!(!location_matches("   ", &exe_in("C:\\app")));
    }

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

    /// 機碼名跟著 productName 走，不是 identifier（實機上是 `...\Uninstall\traytunnel`）
    #[test]
    fn uninstall_key_is_under_hkcu_uninstall() {
        assert_eq!(
            uninstall_subkey("traytunnel"),
            "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\traytunnel"
        );
    }
}
