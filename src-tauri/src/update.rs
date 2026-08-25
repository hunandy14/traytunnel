//! 應用內更新。
//!
//! 兩條車道是**各自獨立**的，共用的只有「發現新版就填進 `AppState` 的 update 欄位」
//! 這一個出口：
//!
//! 1. 安裝版（NSIS 裝進 %LOCALAPPDATA% 的那一份）走 VSCode 式的自動更新：
//!    背景查到新版就靜默下載、驗簽、存進暫存區並寫下就緒標記，**下一次啟動**
//!    的最早期才真的交棒給安裝程式。使用者從頭到尾不必按任何東西。
//! 2. 非安裝版（一般單檔與可攜版）不就地更新，只比對版本並把使用者送去
//!    Releases 頁自己換檔案。
//!
//! 是哪一條車道由 [`is_installed`] 判定：讀 NSIS 寫的 HKCU 解除安裝機碼，
//! 而且 InstallLocation 要真的就是自己這支執行檔的所在資料夾——只看機碼存不存在
//! 是不夠的，使用者大可以裝了一份、又另外抓一顆單檔 exe 放在別的地方跑。
//!
//! 檢查與下載一律在背景做，失敗完全靜默（只在活動日誌留一行），絕不彈窗、不擋操作。
//!
//! 「下載完不立刻裝」是整條路的核心決定：更新永遠不會在使用者正用著的時候把程式
//! 關掉，而重啟後的安裝是靜默的、幾秒就結束的。暫存與標記那一層在
//! [`staged`]，它是純資料，測得到；這裡只留真的會連外、會執行東西的那幾支。

mod staged;

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use semver::Version;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};
use tauri_plugin_window_state::AppHandleExt as _;

pub use staged::Pending;

use crate::state::{UpdateInfo, MAIN_WINDOW};
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

/// Releases 列表頁：下拉選單的「Download from Releases」開這裡，
/// 使用者可以自己挑要哪一版（含更早的版本）
const RELEASES_PAGE: &str = "https://github.com/hunandy14/traytunnel/releases";

/// 單一版本的 release 頁前綴。發佈說明與該版的下載資產都在同一頁上，
/// 所以「View release notes」與可攜版的「Get vX.Y.Z」開的是同一個網址。
const RELEASE_TAG_PREFIX: &str = "https://github.com/hunandy14/traytunnel/releases/tag/v";

/// 還不知道是哪一版（沒查過或查不到）時，release 頁退回這裡
const LATEST_RELEASE_PAGE: &str = "https://github.com/hunandy14/traytunnel/releases/latest";

/// 非安裝版查版本的逾時。查不到就是查不到，沒有理由讓一條卡住的連線一直掛著
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// 安裝版查 latest.json 的逾時。
///
/// updater 外掛的 builder 預設是 `None`，也就是**完全沒有上限**：GitHub 那邊
/// 一旦是半開的連線（封包進得去、回應永遠不來），這個 async 任務就再也不會回來。
/// 背景檢查每 24 小時起一次，卡住的任務會一直累積；手動按下的那顆「Check now」
/// 更糟，前端的 await 沒有逾時，按鈕會永遠停在轉圈。
///
/// 給得比可攜車道的 10 秒寬一些：這條路要拉的是完整的 latest.json 並驗簽章。
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// 下載安裝檔的逾時。
///
/// 與 CHECK_TIMEOUT 分開設是因為兩段的性質完全不同：查版本只拉一份幾百位元組的
/// JSON，超過半分鐘一定是卡住了；下載拉的是十幾 MB 的安裝檔，而 reqwest 的
/// `timeout` 管的是**整個請求含讀完 body** 的總時間，設窄了會把慢速但正常的
/// 下載一起砍掉（使用者看到的是「更新老是失敗」，而不是「網路慢」）。
/// 因此這裡放寬到 10 分鐘：它要擋的是永遠不會結束的連線，不是慢的連線。
///
/// 這一段的值傳不進 builder——外掛建 `Update` 物件時把 timeout 寫死成 `None`
/// （2.10.1 的 updater.rs），builder 上設的那個只作用在 check 那次請求。
/// 所以只能在拿到 Update 物件之後對它的 pub 欄位直接賦值。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// GitHub 對沒有 User-Agent 的請求會直接回 403，一定要帶
const USER_AGENT: &str = concat!("traytunnel/", env!("CARGO_PKG_VERSION"));

/// 會出貨的那份 tauri.conf.json，編譯期就嵌進來。
///
/// 開機那條路（[`apply_pending_at_startup`]）跑在 `tauri::Builder` 之前，
/// 那時還沒有 `AppHandle` 可以問設定，所以產品名與識別碼只能自己拿。
/// 直接讀這份檔案而不是各抄一份常數：抄的話兩邊會漂，而漂掉的症狀是
/// 安裝版被誤判成可攜版（更新整條路靜默失效）或暫存區寫進一個沒人會讀的資料夾。
const TAURI_CONF: &str = include_str!("../tauri.conf.json");

/// tauri.conf.json 裡的一個頂層字串欄位。解析不出來就 panic——那代表出貨的設定
/// 檔壞了或改了形狀，是編譯期就該被發現的事，絕不能默默退回一個猜的值。
fn conf_str(key: &str) -> String {
    let conf: serde_json::Value =
        serde_json::from_str(TAURI_CONF).expect("tauri.conf.json 必須是合法 JSON");
    conf.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("tauri.conf.json 少了頂層的 {key}"))
        .to_string()
}

/// 產品名，也就是 NSIS 拿去當解除安裝機碼名的那個字串（tauri.conf.json 的
/// productName）
static PRODUCT_NAME: LazyLock<String> = LazyLock::new(|| conf_str("productName"));

/// 應用識別碼（tauri.conf.json 的 identifier）。三個地方用到它：
/// `%LOCALAPPDATA%` 底下那個資料夾的名字（Tauri 的 `app_local_data_dir()`
/// 在 Windows 上就是 `%LOCALAPPDATA%\{identifier}`）、single-instance 外掛的
/// 具名互斥鎖、以及通知的 AUMID。
static IDENTIFIER: LazyLock<String> = LazyLock::new(|| conf_str("identifier"));

/// 暫存區在 `%LOCALAPPDATA%\{identifier}` 底下的資料夾名
const STAGING_DIR: &str = "pending-update";

/// NSIS（currentUser 模式）寫解除安裝資訊的機碼位置。
///
/// 機碼名就是 tauri.conf.json 的 productName（Tauri v2 的 NSIS 模板用
/// `${PRODUCTNAME}`），實機驗證過長這樣：
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\traytunnel`
/// 底下有 InstallLocation、UninstallString、DisplayVersion 等值。
fn uninstall_subkey(product: &str) -> String {
    format!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{product}")
}

/// 暫存區：`%LOCALAPPDATA%\{identifier}\pending-update`。
///
/// 自己組而不是走 `app.path().app_local_data_dir()`，理由同 [`PRODUCT_NAME`]
/// ——開機那條路沒有 AppHandle。`the_staging_dir_matches_tauris_own`
/// 釘住這個組法與 Tauri 的算法一致。
///
/// **絕對不可以改成 `%TEMP%`**：暫存區的整個意義就是撐過一次重開機，
/// 而 `%TEMP%` 隨時會被清；何況從 `%TEMP%` 執行 exe 是防毒啟發式重點盯防的行為。
fn staging_dir() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join(&*IDENTIFIER).join(STAGING_DIR))
}

/// 版本號的比較用形式：去空白、去前導的 v。
///
/// 全程式只有這一份：`is_newer`、標記寫入、同版去重、開機決策都吃它，
/// 各自寫一次 `trim_start_matches` 的話遲早會有一處漏掉而讓 `v0.7.0`
/// 與 `0.7.0` 被當成兩個版本。
pub(crate) fn normalize_version(version: &str) -> &str {
    version.trim().trim_start_matches(['v', 'V'])
}

/// 清掉暫存區並把狀態同步歸零。
///
/// 「清檔案」與「清狀態」永遠要一起發生：只清檔案的話介面與系統匣會繼續顯示
/// 一顆按下去必定失敗的「Restart to update」。這一支把兩件事綁死。
fn drop_staged(st: &Shared, dir: &Path) {
    staged::clear(dir);
    st.set_staged(None);
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
pub fn is_installed() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let subkey = uninstall_subkey(&PRODUCT_NAME);
    crate::winsys::read_hkcu_string(&subkey, "InstallLocation")
        .is_some_and(|loc| location_matches(&loc, &exe))
}

// ---------------------------------------------------------------- 背景檢查

/// 這一輪檢查的結果，只夠排程判斷「下一次要等多久」用
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// 沒有需要退避的事：沒查、查失敗、沒新版、下載成功都算
    Settled,
    /// 下載失敗了，下一次要退避著再試
    DownloadFailed,
}

/// 背景檢查的排程：啟動延遲一次，之後每 24 小時一次，跟著程式活到結束。
///
/// 下載失敗會把間隔縮成退避序列（15 分、30 分、1 小時……封頂一天），
/// 這樣網路只是暫時斷掉時不必等到明天才拿得到更新。
pub fn spawn_checker(state: &Shared) {
    let st = state.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_DELAY).await;
        let mut failures: u32 = 0;
        loop {
            failures = match check_once(&st).await {
                Outcome::DownloadFailed => failures.saturating_add(1),
                Outcome::Settled => 0,
            };
            // 退避的上限就是常規間隔，這裡是它唯一的權威來源
            let wait =
                if failures == 0 { INTERVAL } else { staged::retry_delay(failures, INTERVAL) };
            tokio::time::sleep(wait).await;
        }
    });
}

/// 使用者剛把「Automatic updates」打開時立刻查一次，不必等到明天這個時候。
pub fn check_now(state: &Shared) {
    let st = state.clone();
    tauri::async_runtime::spawn(async move {
        check_once(&st).await;
    });
}

/// 使用者主動按下的檢查（設定頁的「Check for updates」與下拉的「Check now」）。
///
/// 刻意**不**看 `checks_for_updates` 那道閘：它管的是「要不要自己在背景連外」，
/// 而使用者親手按下這顆鈕，就是對這一次連外的明示同意。拿背景開關去擋一個
/// 當面的請求，得到的只會是一顆按了沒反應的鈕。
///
/// 與背景車道的另一個差別是結果要回傳：按鈕靠它呈現 Up to date／Check
/// failed 那兩個瞬態，而背景車道對這兩種結果都是靜默的。共用狀態照樣更新，
/// 兩條車道與介面看到的始終是同一份事實。
///
/// 查到新版之後還多做一件 v0.6.1 沒有的事：**自動更新開著時把它交給既有的
/// 暫存鏈**（`stage_if_needed`）。少了這一步，手動查到的那一版不會被下載，
/// 「開著自動更新時，檢查到的更新重啟就會裝上去」這個承諾在手動檢查之後
/// 就不成立了。丟到背景去跑是因為下載是十幾 MB、幾十秒的事，而這支函式的
/// 回傳值是那顆按鈕在等的東西，不可以被它拖住。
pub async fn check_manually(st: &Shared) -> Result<Option<UpdateInfo>, String> {
    let found = match check_lane(&st.app).await {
        Ok(found) => found,
        Err(e) => {
            st.log(format!("update check failed: {e}"));
            return Err(e);
        }
    };
    let info = found.info();
    st.set_update(info.clone());
    match &info {
        Some(u) => st.log(format!("update check: v{} is available", u.version)),
        None => st.log("update check: already up to date"),
    }
    // 開關關著就不下載：`download_and_stage` 那道 A1 閘本來也會擋掉落地，
    // 但沒有理由先白抓十幾 MB 再把它丟掉
    if let Found::Installed { update, .. } = found {
        if st.checks_for_updates() {
            let bg = st.clone();
            tauri::async_runtime::spawn(async move {
                stage_if_needed(&bg, *update).await;
            });
        }
    }
    Ok(info)
}

/// 查一次，查到新版而且這一份是安裝版就順手把它下載進暫存區。
///
/// 任何失敗都只記一行就算了——更新不成功不影響程式本身能不能用，
/// 沒有理由為它彈通知或改變任何狀態。
async fn check_once(st: &Shared) -> Outcome {
    // 關掉就是完全不連外：這道閘在任何請求送出之前
    if !st.checks_for_updates() {
        return Outcome::Settled;
    }
    let found = match check_lane(&st.app).await {
        Ok(found) => found,
        Err(e) => {
            st.log(format!("update check failed: {e}"));
            return Outcome::Settled;
        }
    };
    // 每 24 小時會再查一次，同一版重複記一行只會讓活動日誌看起來像真的又發生了
    // 什麼事，所以「偵測到新版」這一行跟著 set_update 的去重走
    if st.set_update(found.info()) {
        if let Some(u) = found.info() {
            st.log(format!("update available: v{}", u.version));
        }
    }
    match found {
        Found::Installed { update, .. } => stage_if_needed(st, *update).await,
        _ => Outcome::Settled,
    }
}

/// 一輪檢查查到的東西。
///
/// 安裝版那一支**把外掛的 `Update` 物件一起帶出來**：下載只能靠它，而拿到它的
/// 唯一辦法是 `updater.check()`。不帶著走的話下載那一步得再查一次 latest.json，
/// 同一輪就打了 GitHub 兩次，而且兩次之間 release 還可能換掉——那會變成
/// 「宣告下載 A、實際下載 B」。
enum Found {
    None,
    /// 安裝版：`update` 是下載要用的那把鑰匙，`info` 是給介面看的那一份
    Installed {
        update: Box<Update>,
        info: UpdateInfo,
    },
    /// 可攜／單檔版：沒有下載這條路，只有一個版本號
    Unmanaged(UpdateInfo),
}

impl Found {
    fn info(&self) -> Option<UpdateInfo> {
        match self {
            Found::None => None,
            Found::Installed { info, .. } => Some(info.clone()),
            Found::Unmanaged(info) => Some(info.clone()),
        }
    }
}

/// 這次執行該走哪一條車道
async fn check_lane(app: &AppHandle) -> Result<Found, String> {
    if is_installed() {
        check_installed(app).await
    } else {
        check_unmanaged().await
    }
}

/// 這一份執行檔是哪一版。
///
/// 用編譯期常數而不是 `app.package_info().version`（那一份的來源同樣是
/// Cargo.toml 的 version），開機那條路才問得到。
fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 安裝版車道的檢查：走 updater 外掛，簽章驗證與版本比對都由它做。
///
/// 外掛給的 Some 不直接照收，再過一次 [`accept_installed`]——理由見那支函式。
/// 這裡不能用 `app.updater()` 那個便利方法：它建出來的 updater 沒有逾時上限，
/// 遇到半開的連線會讓整個檢查任務永遠掛著。改走 builder 自己補一道 CHECK_TIMEOUT。
/// 拿到的 `Update` 物件**連同結果一起交出去**，下載那一步才不必再查一次
/// latest.json（見 [`Found`]）。
async fn check_installed(app: &AppHandle) -> Result<Found, String> {
    let updater =
        app.updater_builder().timeout(CHECK_TIMEOUT).build().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        return Ok(Found::None);
    };
    match accept_installed(&update.version, current_version()) {
        Some(info) => Ok(Found::Installed { update: Box::new(update), info }),
        None => Ok(Found::None),
    }
}

/// 外掛回報的那一版要不要真的當成新版，由自家的 [`is_newer`] 再判一次。
///
/// 外掛預設的比較器確實是嚴格大於（2.10.1 的 updater.rs：
/// `release.version > self.current_version`），所以正常情況下這一關不會擋掉
/// 任何東西。留著它是因為這條路上「說有新版」的權力整個握在外部相依手上：
/// 換版本、有人塞了 version_comparator、或 latest.json 長出沒預期的形狀，
/// 都可能讓那個 Option 變成 Some 而我們這層毫無反抗餘地。
///
/// 更新提示的失敗方向是不對稱的：漏報只是使用者晚幾天更新，誤報卻是叫他去
/// 重裝一個他已經在用的版本。因此版本比對這件事自己做一次，任何比不出「嚴格
/// 大於」的情形（含空字串、解析不出來的怪版本號）一律當成沒有新版。
fn accept_installed(remote: &str, current: &str) -> Option<UpdateInfo> {
    if !is_newer(remote, current) {
        return None;
    }
    Some(UpdateInfo { version: normalize_version(remote).to_string(), installed: true })
}

// ------------------------------------------------- 可攜／單檔車道（不就地更新）

/// 非安裝版車道的檢查：自己拿同一份 latest.json 比版本。
///
/// 刻意**不**走 updater 外掛：它的 check 一路連著 download＋install，而那條路
/// 對非安裝版是有害的——單檔 exe 沒有安裝程式可以交棒，可攜版更不能被搬到
/// %LOCALAPPDATA% 去（設定檔就在 exe 旁邊，換了位置等於換了一份設定）。
/// 這裡只讀一個版本號，其餘什麼都不做。
async fn check_unmanaged() -> Result<Found, String> {
    // ureq 是阻塞式的，丟到 blocking 執行緒上跑，不擋住 async runtime
    let latest = tauri::async_runtime::spawn_blocking(fetch_latest_version)
        .await
        .map_err(|e| e.to_string())??;
    if !is_newer(&latest, current_version()) {
        return Ok(Found::None);
    }
    Ok(Found::Unmanaged(UpdateInfo {
        version: normalize_version(&latest).to_string(),
        installed: false,
    }))
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
    let parse = |s: &str| Version::parse(normalize_version(s)).ok();
    match (parse(remote), parse(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

/// 某一版的 release 頁網址。版本給 None（還沒查到新版）就退回 releases/latest，
/// 使用者按下「View release notes」時至少看得到最新那一版的說明。
///
/// 純函式，網址組法有問題要在測試裡就看得出來，不必等到實機按下去開錯頁。
pub fn release_url(version: Option<&str>) -> String {
    let tag = version.map(normalize_version).filter(|v| !v.is_empty() && !v.contains(['/', ' ']));
    match tag {
        Some(v) => format!("{RELEASE_TAG_PREFIX}{v}"),
        None => LATEST_RELEASE_PAGE.to_string(),
    }
}

/// 單一版本的 release 頁：發佈說明與該版的下載資產都在上面。
/// 可攜／單檔版的「Get vX.Y.Z」與下拉的「View release notes」都走這裡。
pub fn open_release_page(st: &Shared, version: Option<&str>) {
    open_page(st, &release_url(version));
}

/// Releases 列表頁：下拉的「Download from Releases」走這裡，
/// 讓使用者自己挑版本換檔案。這條路不下載、不碰自己這顆 exe。
pub fn open_releases_page(st: &Shared) {
    open_page(st, RELEASES_PAGE);
}

fn open_page(st: &Shared, url: &str) {
    if let Err(e) = crate::winsys::open_url(url) {
        st.log(format!("could not open {url}: {e}"));
    }
}

// ------------------------------------------------------------ 背景下載與暫存

/// 查到的這一版該不該下載，該的話就下載並存進暫存區。
///
/// 同一版已經躺在暫存區裡就什麼都不做（不重複下載同一版本），只把狀態補上
/// ——重啟之後第一次檢查一定會走到這一條，介面與系統匣靠它認回那份就緒的更新。
async fn stage_if_needed(st: &Shared, update: Update) -> Outcome {
    let Some(dir) = staging_dir() else {
        st.log("update download skipped: LOCALAPPDATA is not set");
        return Outcome::Settled;
    };
    let already = staged::read(&dir);
    if !staged::should_download(&update.version, already.as_ref()) {
        st.set_staged(already);
        st.set_update_stalled(false);
        return Outcome::Settled;
    }
    // 新的一輪嘗試：先把上一次的失敗記號收掉，介面才不會在下載途中還掛著
    // 「Download failed」
    st.set_update_stalled(false);
    match download_and_stage(st, &dir, update).await {
        Ok(pending) => {
            st.log(format!(
                "update v{} downloaded, it will be installed the next time Traytunnel starts",
                pending.version
            ));
            st.set_staged(Some(pending));
            Outcome::Settled
        }
        Err(e) => {
            st.log(format!("update download failed: {e}"));
            // 殘檔清理：寫到一半的安裝檔與指著它的標記一起收掉，
            // 下一次退避到期時是從乾淨狀態重來
            drop_staged(st, &dir);
            // 介面上那顆鈕不可以一直轉圈：轉圈的意思是「正在下載」，
            // 而現在的事實是「下載失敗了，之後會再試」
            st.set_update_stalled(true);
            Outcome::DownloadFailed
        }
    }
}

/// 下載、驗簽、落地。
///
/// `update` 是**這一輪 check 拿到的那一顆**（見 [`Found`]），不再自己重查一次：
/// 同一輪打兩次 GitHub 已經夠糟，更糟的是兩次之間 release 換掉，變成
/// 「宣告下載 A、實際下載 B」。
///
/// `download()` 回來的 bytes **已經過 minisign 驗簽**（外掛 updater.rs 的
/// `verify_signature`，驗不過就是 Err），所以我們拿到的一定是簽章對得上的那份。
/// 之後它要在磁碟上躺到下一次啟動，所以 [`staged::stage`] 會另外記一份 SHA-256。
///
/// 落地**之前**還要再看一次開關（A1）：下載是十幾 MB、幾十秒的事，使用者完全
/// 來得及在這段時間裡把「Automatic updates」關掉，而 `discard_staged` 在那一刻
/// 清的是一個還不存在的暫存區。標記的創生點只有這裡一個，閘就設在這裡。
async fn download_and_stage(
    st: &Shared,
    dir: &Path,
    mut update: Update,
) -> Result<Pending, String> {
    // builder 上那個逾時只管 check 那次請求，Update 物件的 timeout 是外掛寫死的
    // None（＝下載沒有任何上限）。兩段的合理值差了一個數量級，理由見常數本身。
    update.timeout = Some(DOWNLOAD_TIMEOUT);

    st.log(format!("downloading update v{} in the background", update.version));
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;

    accept_staging(st.checks_for_updates())?;
    staged::stage(dir, &update.version, &bytes).map_err(|e| e.to_string())
}

/// 下載途中開關被關掉時的訊息
const GATE_CLOSED_MID_DOWNLOAD: &str =
    "automatic updates were turned off while the download was in flight";

/// 下載完了，這份 bytes 還能不能落地成標記。
///
/// 抽成一支函式是為了讓這道閘看得見也測得起來：它守的是一個沒有它就完全
/// 觀察不到的競態——下載是十幾 MB、幾十秒的事，使用者完全來得及在這段時間裡
/// 把「Automatic updates」關掉，而 `discard_staged` 在那一刻清的是一個**還不存在**
/// 的暫存區。等下載回來才寫下標記的話，那份更新就這樣復活了。
fn accept_staging(still_enabled: bool) -> Result<(), String> {
    if still_enabled {
        Ok(())
    } else {
        Err(GATE_CLOSED_MID_DOWNLOAD.to_string())
    }
}

/// 把暫存區整個丟掉。使用者把「Automatic updates」關掉時走這一條。
///
/// 為什麼非丟不可：開機那條路（[`apply_pending_at_startup`]）雖然也會自己讀一次
/// 設定，但那是最後一道保險；使用者關掉開關的當下就把待安裝的那一份收掉，
/// 介面才不會繼續掛著一顆「Restart to update」，狀態也才與他剛剛的意思一致。
pub fn discard_staged(st: &Shared) {
    let Some(dir) = staging_dir() else {
        return;
    };
    if st.staged_version().is_some() {
        st.log("discarded the update that was waiting to be installed");
    }
    drop_staged(st, &dir);
    st.set_update_stalled(false);
}

/// 啟動時把暫存區裡那份就緒的更新認回來，讓設定頁與系統匣一開始就看得到它。
///
/// **這不是死碼**：[`apply_pending_at_startup`] 有兩條會「留著標記不套用」的路
/// ——已經有另一個實例在跑（A3），或這一次啟動時開關是關的。前者之後就是這一支
/// 把那份更新撈回狀態，使用者才看得到系統匣的「Restart to update」；後者則由
/// 同一道開關擋掉，不會撈。
///
/// 只有安裝版走這條路，而且刻意**不驗雜湊**——那是十幾 MB 的事，留到真的要
/// 交棒的那一刻再算。這裡只是把「有一份 vX.Y.Z 等著」放進狀態。
pub fn restore_staged(st: &Shared) {
    if !is_installed() || !st.checks_for_updates() {
        return;
    }
    let Some(dir) = staging_dir() else {
        return;
    };
    st.set_staged(staged::read(&dir));
}

// ---------------------------------------------------------------- 交棒給安裝程式

/// 把暫存的安裝程式起起來。成功之後呼叫端一定要立刻退出這個行程。
fn launch_installer(installer: &Path, tray: bool) -> std::io::Result<()> {
    std::process::Command::new(installer).args(staged::installer_args(tray)).spawn().map(|_| ())
}

/// single-instance 外掛在 Windows 上用的具名互斥鎖名字。
///
/// 抄自 tauri-plugin-single-instance 2.4.3 的 `platform_impl/windows.rs`：
/// 它用 `CreateMutexW` 建 `{identifier}-sim`。名字裡的版本後綴只有 `semver`
/// 特性開啟時才會加，而我們沒有開，所以跨版本是同一個名字——這正是我們要的：
/// **正在跑的那個實例可能是舊版**。
fn single_instance_mutex_name() -> String {
    format!("{}-sim", *IDENTIFIER)
}

/// 現在是不是已經有另一個 Traytunnel 實例在跑。
///
/// 用 `OpenMutexW`（**只開不建**）去探 single-instance 外掛那把鎖：開得起來就代表
/// 有人握著它。刻意不用 `CreateMutexW`——那會讓我們自己也成為那個名字的擁有者，
/// 幾毫秒後外掛在同一個行程裡再建一次時就會看到 `ERROR_ALREADY_EXISTS`，
/// 把自己誤判成第二個實例。
fn another_instance_is_running() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};

    let name = crate::winsys::wide(&single_instance_mutex_name());
    // SAFETY: name 是 NUL 結尾的寬字串，OpenMutexW 只讀它；回傳的 handle 立刻關掉
    unsafe {
        let handle = OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr());
        if handle.is_null() {
            return false;
        }
        CloseHandle(handle);
        true
    }
}

/// 開機時到底該不該走套用那條路，以及不走的話要拿暫存區怎麼辦。
///
/// 三個布林都是外面探出來的事實，所以這一支是純函式、測得到——那三件事各自
/// 都要碰登錄檔、Win32 或磁碟，混在一起就再也驗不了它們的組合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// 可以往下走，去看暫存區裡有什麼
    Proceed,
    /// 不走，而且**暫存區原樣留著**（之後還會用到）
    SkipAndKeep,
    /// 不走，順手把暫存區清掉（那份東西永遠不會被裝了）
    SkipAndClear,
}

pub fn startup_gate(installed: bool, another_instance: bool, automatic_updates: bool) -> Gate {
    if !installed {
        // 可攜／單檔版根本不該碰暫存區——那是安裝版的東西，
        // 而同一台機器上兩種版本共用同一個 %LOCALAPPDATA% 資料夾
        return Gate::SkipAndKeep;
    }
    if another_instance {
        // 第二實例：正在被使用的第一實例不可以被裝掉。留著，下次冷啟動再說
        return Gate::SkipAndKeep;
    }
    if !automatic_updates {
        // 開關關著就不會有人來裝它，留著只是佔十幾 MB
        return Gate::SkipAndClear;
    }
    Gate::Proceed
}

/// 開機最早期的那一步：有就緒的更新就交棒給安裝程式，這支函式**不會回來**。
///
/// 呼叫位置是規格的一部分：它必須排在 `tauri::Builder` 之前，尤其是
/// **single-instance 外掛註冊之前**。那顆外掛一初始化就把具名互斥鎖拿在手上，
/// 而 NSIS 的靜默安裝會去找還活著的舊行程並把它關掉；我們在還沒拿任何鎖的時候
/// 就 spawn 完安裝程式並 `exit(0)`，等於整個繞開那一段互相等待。這也是為什麼
/// 這支函式拿不到 `AppHandle`，需要的東西（產品名、識別碼、版本、設定）全部自己拿。
/// **不要把它往後搬。**
///
/// 三道閘依序擋在真的交棒之前：
///
/// 1. **不是安裝版**就完全不碰暫存區；
/// 2. **已經有另一個實例在跑**（A3）——這是最兇的一顆暗雷：使用者雙擊了第二次
///    圖示，而暫存區裡剛好有一份更新，於是這個「本來只該去喚醒既有視窗」的第二
///    實例會起安裝程式，把使用者**正在用**的第一實例連同他的隧道一起關掉。
///    這時什麼都不做，讓 single-instance 外掛照常把焦點轉過去就好；標記留著，
///    下一次真正的冷啟動再裝（留著的那一份由 `restore_staged` 撈回狀態）。
/// 3. **自動更新被關掉了**（A2）——套用是自動更新的最後一步，開關關著就不該發生。
///    設定這時還沒被 `AppState` 載入，所以自己走一次輕量路徑讀那一個鍵。
///    關著就順手把暫存清掉，不留一份永遠不會被裝的檔案佔著十幾 MB。
///
/// 回傳的是要補進活動日誌的行——AppState 這時還不存在，所以先收著，
/// 等 setup 裡狀態建好了再一起記。
pub fn apply_pending_at_startup(tray: bool) -> Vec<String> {
    let Some(dir) = staging_dir() else {
        return Vec::new();
    };
    // 這三件事各自要碰登錄檔、Win32 與磁碟，探完之後交給純函式判斷（測得到）
    let gate = startup_gate(
        is_installed(),
        another_instance_is_running(),
        crate::config::automatic_updates_enabled(),
    );
    match gate {
        Gate::Proceed => {}
        Gate::SkipAndKeep => return Vec::new(),
        Gate::SkipAndClear => {
            staged::clear(&dir);
            return Vec::new();
        }
    }
    let current = current_version();
    match staged::apply_action(staged::read(&dir), current) {
        staged::Apply::Nothing => Vec::new(),
        staged::Apply::Done => {
            staged::clear(&dir);
            vec![format!("updated to v{current}")]
        }
        staged::Apply::Stale => {
            staged::clear(&dir);
            Vec::new()
        }
        staged::Apply::GaveUp { version } => {
            staged::clear(&dir);
            vec![format!(
                "gave up installing v{version} after {} attempts, it will be downloaded again",
                staged::MAX_ATTEMPTS
            )]
        }
        staged::Apply::Install(pending) => {
            // 落地之後被動過的檔案一律不執行：簽章驗的是下載當時那串 bytes，
            // 這一關驗的是現在磁碟上這顆檔案
            if !staged::verify(&pending) {
                staged::clear(&dir);
                return vec![format!(
                    "the staged v{} installer did not match its checksum and was discarded",
                    pending.version
                )];
            }
            // 計數只在**這條自動路**上遞增（A5），而且一定要在起安裝程式之前落地：
            // 安裝程式會把我們關掉，之後沒有任何機會再寫東西。手動按下的那條路
            // 不碰這個計數——它的失敗使用者當場看得到，會自己再按一次，
            // 沒有道理讓那幾次把自動重試的額度燒光。
            if let Err(e) = staged::note_attempt(&dir, &pending) {
                staged::clear(&dir);
                return vec![format!("could not record the update attempt: {e}")];
            }
            match launch_installer(&pending.installer, tray) {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    staged::clear(&dir);
                    vec![format!("could not start the staged v{} installer: {e}", pending.version)]
                }
            }
        }
    }
}

/// 系統匣與設定頁的「Restart to update」：不等下一次開機，現在就交棒。
///
/// **順序是這一支唯一要緊的事**（A4）：驗雜湊 → 起安裝程式 → 起得來了才收尾。
///
/// 收尾那一組（`mark_exiting`／`kill_all_jobs`／存視窗狀態／`cleanup_before_exit`）
/// 原本排在 spawn 之前，那是照著 updater 外掛 `on_before_exit` 的位置抄的。
/// 問題是 `cleanup_before_exit` 會把視窗藏起來並清掉資源表，而那**是收不回來的**
/// ——spawn 一旦失敗，使用者就得到一個沒有視窗、也沒有隧道的無頭行程，
/// 而錯誤訊息要顯示在那個已經不存在的視窗上。改成 spawn 成功才收尾之後，
/// 失敗路徑上什麼都還沒動過，錯誤照常顯示、隧道照常跑。
///
/// 代價是安裝程式起來的那一瞬間我們才開始收隧道。這是可以接受的：NSIS 的靜默
/// 安裝要先解壓、還要去找並關掉舊行程，我們手上有的是毫秒級的餘裕；而萬一真的
/// 被搶先關掉，失去的也只是一次視窗位置存檔，不是一個壞掉的狀態。
///
/// 視窗狀態要自己存是因為這條路不會發 `RunEvent::Exit`（行程直接 `exit(0)`），
/// tauri-plugin-window-state 落地存檔的 hook 不會跑——那正是「更新後視窗歸零
/// 置中」的成因。
///
/// 雜湊那一步是十幾 MB 的整檔讀取，呼叫端負責把它放到 blocking 執行緒上
/// （見 `commands::apply_update`），不要在 UI 執行緒上直接叫這一支。
pub fn apply_now(st: &Shared) -> Result<(), String> {
    if !is_installed() {
        return Err("This build cannot update itself".into());
    }
    let dir = staging_dir().ok_or("LOCALAPPDATA is not set")?;
    let pending = staged::read(&dir).ok_or("No update is ready to install")?;
    if !staged::verify(&pending) {
        drop_staged(st, &dir);
        return Err("The staged installer did not match its checksum".into());
    }
    // 視窗已經藏起來（縮在系統匣裡）的話，更新完也不該突然彈一個視窗出來
    let tray = !main_window_visible(&st.app);
    st.log(format!("restarting to install v{}", pending.version));

    // 這裡**不**動 attempts（A5）：那個計數是自動路的保險絲
    launch_installer(&pending.installer, tray).map_err(|e| e.to_string())?;

    // 交棒確定開始了，現在才收尾——上面任何一步失敗時，程式都還是完好的
    st.mark_exiting();
    st.kill_all_jobs();
    if let Err(e) = st.app.save_window_state(crate::winstate::flags()) {
        log::warn!("could not save window state before the update installer takes over: {e}");
    }
    st.app.cleanup_before_exit();
    std::process::exit(0);
}

/// 使用者親手按下的那顆綠色主鈕（設定頁的「Update to vX.Y.Z」）。
///
/// 這是 v0.6.1 的手動更新鏈，尾巴換成 v0.6.2 的暫存＋交棒那一段：
///
/// 1. 暫存區裡已經有一份（自動車道先下載好了）就直接交棒，不必再抓一次十幾 MB；
/// 2. 沒有的話重查一次拿 `Update` 物件，下載（外掛在這一步驗簽），落地成暫存，
///    再走同一支 [`apply_now`]。
///
/// **手動這條路不看「Automatic updates」開關**，理由與 [`check_manually`] 相同：
/// 使用者按下這顆鈕就是對這一次更新的明示同意。所以它也不經過
/// `download_and_stage` ——那一支帶著 A1 那道閘（下載途中開關被關掉就把成果丟掉），
/// 那道閘守的是「背景偷偷下載完才落地」那個競態，套到當面的請求上只會變成
/// 「按了沒反應」。
///
/// 落地之後那份暫存不會變成一個沒人收的爛攤子：交棒成功的話這個行程就沒了，
/// 失敗的話開關關著時下一次啟動的 [`startup_gate`] 會把它清掉（`SkipAndClear`），
/// 開著時則正好被自動車道認回來。
///
/// 重查而不是把先前 check 的 `Update` 物件存起來：那份東西帶著 http client 與
/// 一堆執行期狀態，存進共用狀態只是徒增麻煩，而使用者按下按鈕的當下重查一次，
/// 拿到的也一定是最新的那一版。
pub async fn install(st: &Shared) -> Result<(), String> {
    if !is_installed() {
        return Err("This build cannot update itself".into());
    }
    let dir = staging_dir().ok_or("LOCALAPPDATA is not set")?;
    if st.staged_version().is_some() {
        return hand_over(st).await;
    }
    let updater =
        st.app.updater_builder().timeout(CHECK_TIMEOUT).build().map_err(|e| e.to_string())?;
    let mut update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No update available".to_string())?;
    // builder 上那個逾時只管 check 那次請求，Update 物件的 timeout 是外掛寫死的
    // None（＝下載沒有任何上限）。兩段的合理值差了一個數量級，理由見常數本身。
    update.timeout = Some(DOWNLOAD_TIMEOUT);

    st.log(format!("downloading update v{}", update.version));
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;

    let pending = staged::stage(&dir, &update.version, &bytes).map_err(|e| e.to_string())?;
    st.set_staged(Some(pending));
    // 剛剛才自己抓下來的那一份，退避旗標留著只會讓介面繼續掛著一句「下載失敗」
    st.set_update_stalled(false);
    hand_over(st).await
}

/// 把交棒那一段丟到 blocking 執行緒上（C1）。
///
/// [`apply_now`] 要把十幾 MB 的安裝檔整個讀進來算一次 SHA-256，而它的呼叫端是
/// 一個 async 指令——留在 async 執行緒上會擋住整個 runtime。成功時它自己
/// `exit(0)`，所以這支函式正常路徑上同樣不會回來。
async fn hand_over(st: &Shared) -> Result<(), String> {
    let st = st.clone();
    tauri::async_runtime::spawn_blocking(move || apply_now(&st)).await.map_err(|e| e.to_string())?
}

fn main_window_visible(app: &AppHandle) -> bool {
    app.get_webview_window(MAIN_WINDOW).and_then(|w| w.is_visible().ok()).unwrap_or(false)
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

    /// 安裝版車道的最後一道閘：外掛就算回了 Some，版本沒有嚴格大於就不算數。
    ///
    /// 「有沒有新版」這個判斷不可以整個外包給外部相依，這裡釘住自己一定會再判一次。
    #[test]
    fn the_installed_lane_refuses_a_version_that_is_not_newer() {
        assert_eq!(accept_installed("0.5.0", "0.5.0"), None);
        assert_eq!(accept_installed("0.4.9", "0.5.0"), None);
        // 版本號怪到解析不出來時同樣不報，寧可漏報也不要叫人去重裝已經在用的版本
        assert_eq!(accept_installed("", "0.5.0"), None);
        assert_eq!(accept_installed("latest", "0.5.0"), None);
    }

    /// 真的有新版時照樣要放行，而且版本號存進去是不帶 v 的（UpdateInfo 的契約，
    /// 前端會自己補上 v 顯示成 `Update to v0.6.0`）
    #[test]
    fn the_installed_lane_still_passes_a_real_update_through() {
        let found = accept_installed("0.6.0", "0.5.0").expect("0.6.0 比 0.5.0 新");
        assert_eq!(found, UpdateInfo { version: "0.6.0".into(), installed: true });
        let prefixed = accept_installed("v0.6.0", "0.5.0").expect("帶 v 的一樣認得");
        assert_eq!(prefixed.version, "0.6.0");
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

    /// `hidden` 屬性的 `display: none` 只來自瀏覽器預設樣式表，層疊順序上輸給
    /// 任何一條作者樣式，所以 `.setting-row { display: flex }` 之類的規則會讓
    /// `node.hidden = true` 完全失效：JS 以為藏起來了，畫面上那一塊卻一直在，
    /// 而且顯示的是 index.html 裡寫死的靜態佔位內容。
    ///
    /// 樣式表是真的會出貨的檔案，比照 `the_shipped_updater_config_parses`
    /// 的做法直接讀它，把這條全域規則擋在 CI，避免它被刪掉。
    #[test]
    fn the_stylesheet_makes_the_hidden_attribute_actually_hide() {
        let css = include_str!("../../src/styles.css");
        let normalized: String = css.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            normalized.contains("[hidden]{display:none!important;}"),
            "styles.css 必須有全域的 [hidden] 規則，否則 node.hidden 會被任何一條 display 規則蓋掉"
        );
    }

    /// release workflow 組出來的 latest.json 必須是 updater 外掛吃得下的形狀。
    ///
    /// 這條路上完全沒有型別把關：workflow 是一段 PowerShell，外掛是外部相依，
    /// 中間只靠一份執行期才會下載的 JSON 對接。形狀一旦對不上，症狀是安裝版的
    /// 更新從此靜默失效——檢查失敗只會在活動日誌留一行，沒有人會注意到。
    ///
    /// 最脆弱的是 `pub_date`：workflow 寫的是
    /// `[System.DateTime]::UtcNow.ToString("o")`（7 位小數再接 Z），而外掛拿
    /// RFC3339 去解析它，解不出來時是整份 release 反序列化失敗，不是忽略那個欄位。
    /// 有人把它改成 `ToString()` 或別的格式，這裡就會紅。
    #[test]
    fn the_release_workflow_manifest_still_deserializes() {
        let raw = r#"{
            "version": "0.5.0",
            "pub_date": "2026-08-22T09:41:07.1234567Z",
            "platforms": {
                "windows-x86_64": {
                    "signature": "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZQo=",
                    "url": "https://github.com/hunandy14/traytunnel/releases/download/v0.5.0/traytunnel-0.5.0-setup.exe"
                }
            }
        }"#;
        let release: tauri_plugin_updater::RemoteRelease =
            serde_json::from_str(raw).expect("release.yml 產出的 latest.json 必須解析得出來");

        assert_eq!(release.version.to_string(), "0.5.0");
        assert!(release.pub_date.is_some(), "pub_date 解析成功才代表格式對得上");
        // 目標鍵要跟 tauri 對這個平台用的字串一致，否則會是 TargetNotFound
        assert!(release.download_url("windows-x86_64").is_ok());
        assert!(release.signature("windows-x86_64").is_ok());
    }

    /// 上面那條要能真的擋住格式改動，前提是外掛對 pub_date 嚴格。
    /// 這裡釘住「非 RFC3339 會失敗」，免得有人以為那個欄位隨便寫都行。
    #[test]
    fn a_pub_date_that_is_not_rfc3339_is_rejected() {
        let raw = r#"{
            "version": "0.5.0",
            "pub_date": "2026-08-22 09:41:07",
            "platforms": {
                "windows-x86_64": { "signature": "sig", "url": "https://example.com/a.exe" }
            }
        }"#;
        let parsed = serde_json::from_str::<tauri_plugin_updater::RemoteRelease>(raw);
        assert!(parsed.is_err(), "外掛對 pub_date 是嚴格的，這裡不該通過");
    }

    /// 機碼名跟著 productName 走，不是 identifier（實機上是 `...\Uninstall\traytunnel`）
    #[test]
    fn uninstall_key_is_under_hkcu_uninstall() {
        assert_eq!(
            uninstall_subkey("traytunnel"),
            "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\traytunnel"
        );
    }

    /// 暫存區的組法要與 Tauri 的 `app_local_data_dir()` 一致：
    /// Windows 上那一支就是 `%LOCALAPPDATA%\{identifier}`。組錯的話，
    /// 寫進去的標記與讀出來的標記會是兩個不同的資料夾，更新永遠不會被套用。
    ///
    /// 產品名與識別碼本身不必再釘——它們現在直接讀出貨的 tauri.conf.json
    /// （見 `conf_str`），沒有第二份可以漂掉。
    #[test]
    fn the_staging_dir_matches_tauris_own() {
        let Some(dir) = staging_dir() else {
            // CI 上沒有 LOCALAPPDATA 時這條沒有東西好驗
            return;
        };
        let local = std::env::var("LOCALAPPDATA").expect("staging_dir 回了 Some 就一定讀得到");
        assert_eq!(dir, PathBuf::from(local).join(&*IDENTIFIER).join("pending-update"));
    }

    /// single-instance 外掛在 Windows 上建的是 `{identifier}-sim` 這把具名鎖
    /// （2.4.3 的 platform_impl/windows.rs）。名字錯掉的話探測永遠回 false，
    /// A3 那道保護等於不存在——而症狀是「偶爾雙擊圖示會把正在用的程式裝掉」，
    /// 幾乎不可能從使用者回報裡認出來。
    #[test]
    fn the_instance_probe_uses_the_plugins_own_mutex_name() {
        assert_eq!(single_instance_mutex_name(), format!("{}-sim", *IDENTIFIER));
        assert!(single_instance_mutex_name().ends_with("-sim"));
        // 名字裡不可以有版本號：外掛的版本後綴只有 semver 特性開著才會加，
        // 而我們沒開——加了的話就探不到「正在跑的是舊版」那個實例
        assert!(!single_instance_mutex_name().contains(current_version()));
    }

    /// 開機三道閘的組合。這一支是純函式，因為它要判斷的三件事各自都得碰
    /// 登錄檔、Win32 與磁碟，混在一起就再也驗不了它們的組合。
    #[test]
    fn the_startup_gate_only_proceeds_for_an_installed_solo_auto_updating_run() {
        assert_eq!(startup_gate(true, false, true), Gate::Proceed);
    }

    /// 這份原始碼自己。下面兩條測的是**語句順序**與**某一支不可以碰某個 API**，
    /// 那是型別系統管不到、卻一改就靜默出錯的東西，所以直接讀原文釘住
    /// （與 `the_stylesheet_makes_the_hidden_attribute_actually_hide`、
    /// `the_shipped_updater_config_parses` 同一套做法）。
    const THIS_FILE: &str = include_str!("update.rs");

    /// 取出某一支函式的本體（從簽名那一行到下一個頂層 `}`）。
    ///
    /// **一定要先正規化換行**：這個 repo 的工作區是 LF，而 CI 檢出時 git 會把它
    /// 換成 CRLF，於是寫死的 `"\n}\n"` 在 CI 上永遠找不到——症狀是「本機全綠、
    /// CI 紅」，而且紅的原因跟被測的東西一點關係都沒有。
    fn body_of(name: &str) -> String {
        let src = THIS_FILE.replace("\r\n", "\n");
        let start = src.find(name).unwrap_or_else(|| panic!("找不到 {name}"));
        let rest = &src[start..];
        let end = rest.find("\n}\n").unwrap_or_else(|| panic!("{name} 沒有結尾"));
        rest[..end].to_string()
    }

    /// 下載途中把開關關掉，那份下載**不可以**還是落地成標記。
    ///
    /// 這是一個很容易漏掉的競態：`discard_staged` 在使用者按下開關的當下清暫存，
    /// 但那時下載還在路上，它清的是一個還不存在的東西；幾十秒後 `stage()` 一寫，
    /// 那份被拒絕的更新就復活了，而且下一次開機真的會裝上去。
    ///
    /// 標記的創生點只有 `download_and_stage` 一個，所以閘設在那裡，
    /// 而且必須排在 `download()` 與 `staged::stage()` **之間**。
    #[test]
    fn a_download_that_finishes_after_the_switch_was_turned_off_is_thrown_away() {
        assert!(accept_staging(true).is_ok());
        assert_eq!(accept_staging(false), Err(GATE_CLOSED_MID_DOWNLOAD.to_string()));

        let body = body_of("async fn download_and_stage");
        let download = body.find(".download(").expect("要有下載那一步");
        let gate = body.find("accept_staging(").expect("下載完一定要再看一次開關");
        let stage = body.find("staged::stage(").expect("要有落地那一步");
        assert!(download < gate && gate < stage, "閘必須夾在下載與落地之間：{body}");
    }

    /// 手動按下的「Restart to update」**不可以**消耗自動重試的額度。
    ///
    /// `attempts` 是自動路的保險絲（同一版連三次交棒都沒把版本換掉就放棄它）。
    /// 手動那條路的失敗使用者當場看得到、會自己再按一次，讓那幾次把額度燒光的話，
    /// 一個好端端的更新會因為使用者多按了三下就被永久丟掉。
    #[test]
    fn the_manual_restart_never_burns_the_automatic_retry_budget() {
        let manual = body_of("pub fn apply_now");
        assert!(
            !manual.contains("note_attempt"),
            "apply_now 不可以動 attempts，那是自動路的保險絲：{manual}"
        );
        // 反過來，自動那一條一定要記
        let auto = body_of("pub fn apply_pending_at_startup");
        assert!(auto.contains("note_attempt"), "開機自動安裝必須把嘗試次數落地");
    }

    /// 手動那條路**不可以**被「Automatic updates」開關擋下。
    ///
    /// 這是整個設計的核心不對稱，而且是最容易被下一個讀者「順手修正」掉的一條：
    /// `download_and_stage` 上面明明白白有一道看開關的閘（A1），照著抄到手動路上
    /// 看起來很合理——實際結果是使用者關掉自動更新之後，那顆綠色的
    /// 「Update to vX.Y.Z」按下去毫無反應。
    ///
    /// 兩支手動入口都不准出現 `checks_for_updates` 那道**前置**閘：
    /// `check_manually` 只在查完之後拿它決定「要不要順手下載」，
    /// `install` 則從頭到尾都不該問它。
    #[test]
    fn the_manual_lane_ignores_the_automatic_updates_switch() {
        let install = body_of("pub async fn install");
        assert!(
            !install.contains("checks_for_updates") && !install.contains("accept_staging"),
            "手動更新是使用者當面的請求，不可以被自動更新開關擋下：{install}"
        );

        let check = body_of("pub async fn check_manually");
        let lane = check.find("check_lane(").expect("要有真的去查的那一步");
        let gate = check.find("checks_for_updates()").expect("下載與否還是要看開關");
        assert!(lane < gate, "開關只准決定「查到之後要不要順手下載」，不准擋住查詢本身：{check}");
    }

    /// 手動更新的順序：下載 → 落地 → 交棒，而且交棒要走 blocking 執行緒（C1）。
    ///
    /// 落地那一步不是可有可無的中繼站：[`apply_now`] 唯一的資料來源就是暫存區，
    /// 少了它交棒那一步會直接回「No update is ready to install」——而使用者剛剛
    /// 才等完一次十幾 MB 的下載。
    #[test]
    fn the_manual_update_downloads_then_stages_then_hands_over_off_thread() {
        let body = body_of("pub async fn install");
        let download = body.find(".download(").expect("要有下載那一步");
        let stage = body.find("staged::stage(").expect("下載回來要先落地成暫存");
        let hand = body.rfind("hand_over(").expect("最後要交棒");
        assert!(download < stage && stage < hand, "順序必須是下載→落地→交棒：{body}");

        // 交棒會整檔算一次 SHA-256，留在 async 執行緒上會擋住整個 runtime
        let off_thread = body_of("async fn hand_over");
        assert!(
            off_thread.contains("spawn_blocking"),
            "apply_now 是十幾 MB 的整檔讀取，一定要丟到 blocking 執行緒：{off_thread}"
        );
    }

    /// 手動更新同樣不可以消耗自動重試的額度（A5）。
    ///
    /// `the_manual_restart_never_burns_the_automatic_retry_budget` 釘的是
    /// `apply_now`，這一條釘的是走到它之前的那一段——`install` 自己也不准去記帳。
    #[test]
    fn the_manual_update_never_burns_the_automatic_retry_budget() {
        let body = body_of("pub async fn install");
        assert!(
            !body.contains("note_attempt"),
            "attempts 是自動路的保險絲，手動的失敗使用者當場看得到：{body}"
        );
    }

    /// 設定頁那顆主鈕**永遠在**。
    ///
    /// v0.6.2 把它改成「沒事做就整顆藏起來」（`hidden` 屬性寫在 HTML 上、
    /// 由 JS 開關），於是自動更新關掉之後那一列就完全沒有入口了——使用者
    /// 連查一次都做不到。這一版把它請回來：idle 時是「Check for updates」。
    ///
    /// 直接讀出貨的 index.html，比照
    /// `the_stylesheet_makes_the_hidden_attribute_actually_hide` 的做法。
    #[test]
    fn the_update_button_is_always_in_the_settings_page() {
        let html = include_str!("../../index.html").replace("\r\n", "\n");
        let start = html.find(r#"id="btn-update""#).expect("版本列一定要有主鈕");
        let tag = &html[start..start + html[start..].find('>').expect("標籤沒有結尾")];
        assert!(!tag.contains("hidden"), "主鈕不可以預設藏起來：{tag}");

        // 下拉裡的「Check now」是同一件事的第二個入口，一起釘住
        assert!(html.contains(r#"id="mi-check-now""#), "下拉要保留 Check now");
    }

    /// 兩支手動指令都要真的註冊進 invoke_handler。
    ///
    /// 漏掉的症狀只有執行期才看得到（前端 invoke 直接 reject，按鈕按下去
    /// 跳一句看不懂的錯誤），型別系統完全管不到——指令函式本身照樣編得過。
    #[test]
    fn the_manual_update_commands_are_registered() {
        let lib = include_str!("lib.rs");
        for cmd in ["check_for_updates_now", "install_update"] {
            assert!(
                lib.contains(&format!("commands::{cmd}")),
                "{cmd} 沒有註冊進 invoke_handler，前端會叫不到"
            );
        }
    }

    /// 第二實例絕不套用，而且**標記要留著**。
    ///
    /// 這是這條路上最兇的一顆暗雷：使用者雙擊了第二次圖示，暫存區裡剛好有一份
    /// 更新，於是這個本來只該去喚醒既有視窗的第二實例會起安裝程式，把他
    /// **正在用**的第一實例連同所有隧道一起關掉。清掉標記同樣不行——那會讓
    /// 已經下載好的更新平白消失，下一次還得再抓一次。
    #[test]
    fn a_second_instance_never_installs_and_never_throws_the_update_away() {
        assert_eq!(startup_gate(true, true, true), Gate::SkipAndKeep);
        // 開關關著時「留著」與「清掉」都說得通，但第二實例這一關排在前面：
        // 這時候連判斷都不該再往下走
        assert_eq!(startup_gate(true, true, false), Gate::SkipAndKeep);
    }

    /// 關掉自動更新之後就不會再被自動裝上去。
    ///
    /// 套用那條路跑在設定檔載入之前，`AppState` 還不存在，所以這道閘讀的是
    /// 設定檔原文（`config::automatic_updates_enabled`）。沒有它的話，
    /// 一個剛剛才說「不要自動更新我」的使用者下一次開機還是會被靜默更新。
    #[test]
    fn turning_automatic_updates_off_stops_the_startup_install() {
        assert_eq!(startup_gate(true, false, false), Gate::SkipAndClear);
    }

    /// 可攜／單檔版完全不碰暫存區——那是安裝版的東西，而同一台機器上兩種版本
    /// 共用同一個 `%LOCALAPPDATA%` 資料夾。可攜版跑起來就把它清掉的話，
    /// 使用者裝好的那一份會平白失去已經下載好的更新。
    #[test]
    fn an_unmanaged_build_leaves_the_installed_builds_staging_alone() {
        assert_eq!(startup_gate(false, false, true), Gate::SkipAndKeep);
        assert_eq!(startup_gate(false, false, false), Gate::SkipAndKeep);
        assert_eq!(startup_gate(false, true, true), Gate::SkipAndKeep);
    }

    /// tauri.conf.json 的 plugins.updater 一旦寫壞（少了 pubkey、endpoint 不是
    /// 合法網址），外掛會在 setup 階段回 Err，`run()` 當場 panic——使用者連視窗
    /// 都看不到。那是執行期才會發生的失敗，這裡拿真的會發佈的那份設定解析一次，
    /// 把它擋在 CI。
    #[test]
    fn the_shipped_updater_config_parses() {
        let raw = include_str!("../tauri.conf.json");
        let conf: serde_json::Value =
            serde_json::from_str(raw).expect("tauri.conf.json 必須是合法 JSON");

        let updater = conf.pointer("/plugins/updater").expect("plugins.updater 不可以消失");
        let parsed: tauri_plugin_updater::Config =
            serde_json::from_value(updater.clone()).expect("updater 設定要解析得出來");

        assert!(!parsed.pubkey.is_empty(), "沒有公鑰就驗不了簽章");
        assert_eq!(parsed.endpoints.len(), 1);
        let endpoint = &parsed.endpoints[0];
        assert_eq!(endpoint.scheme(), "https", "非 https 的 endpoint 在 release 建置會被拒絕");
        assert!(endpoint.as_str().ends_with("/latest.json"), "{endpoint}");

        // 安裝走 quiet（NSIS 的 /S /R）：全靜默，連進度條都不出現，裝完自動重啟。
        // 自動更新是在使用者不在場的時候發生的（下次啟動的最早期），
        // 任何一個要人按下去的畫面都會讓整條路卡在那裡
        let windows = parsed.windows.expect("windows 區塊要在");
        assert_eq!(windows.install_mode.to_string(), "quiet");

        // 沒有這一項就簽不出 .sig，release workflow 組 latest.json 那步會直接失敗
        assert_eq!(
            conf.pointer("/bundle/createUpdaterArtifacts"),
            Some(&serde_json::Value::Bool(true))
        );
    }

    /// NSIS 的安裝範圍必須明寫成 currentUser。
    ///
    /// 這與上面那個 `installMode`（quiet／passive／basicUi，講的是**安裝畫面**）
    /// 是兩件不同的事：這一個講的是**裝到哪裡、要不要提權**。留空時 Tauri 的
    /// 預設確實就是 currentUser，但預設值會跟著上游版本走，而這條路禁不起它變動
    /// ——一旦變成 `both`，安裝程式即使使用者選的是 current-user 也會先跳一次
    /// UAC（tauri#9904），而我們的靜默流程沒有任何人在旁邊按「是」，
    /// 更新會就這樣永遠裝不上去。明寫下來，並在這裡釘住。
    ///
    /// 這也正是 `is_installed` 讀 HKCU（而不是 HKLM）解除安裝機碼的前提。
    #[test]
    fn the_installer_stays_in_the_current_user_scope() {
        let raw = include_str!("../tauri.conf.json");
        let conf: serde_json::Value =
            serde_json::from_str(raw).expect("tauri.conf.json 必須是合法 JSON");
        assert_eq!(
            conf.pointer("/bundle/windows/nsis/installMode").and_then(|v| v.as_str()),
            Some("currentUser"),
            "NSIS 必須明鎖 currentUser，否則靜默更新會被 UAC 擋下"
        );
    }
}
