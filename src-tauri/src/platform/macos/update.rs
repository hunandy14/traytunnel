//! 應用內更新（macOS）。
//!
//! 對外的每一支函式都與 Windows 同名同簽章（`platform/mod.rs` 那份清單），
//! 但底下的機制是另一套，因為封裝格式完全不同。三件事先講清楚，後面的取捨都從
//! 這裡長出來：
//!
//! ## 1. 沒有暫存交棒——macOS 的更新是「當場下載、當場替換、重啟生效」
//!
//! Windows 那條路的核心是暫存：背景靜默下載一顆簽好章的 `setup.exe` 放著，
//! **下一次啟動**的最早期才交棒給它，使用者從頭到尾不必按任何東西。那套之所以
//! 成立，是因為 NSIS 安裝程式是一支獨立的執行檔，可以在我們已經退出之後才動手。
//!
//! macOS 這邊沒有對應的東西。官方流程（`tauri-plugin-updater` 的
//! `Update::install`）是把 `.app.tar.gz` 解到暫存目錄，再把整個 `.app` bundle
//! **原地換掉**——這件事是在我們自己還跑著的時候做的，沒有第三方安裝程式可以
//! 接手，也沒有「放著等下次開機」這個中間態。硬要造一個 mac 版暫存區，得到的會是
//! 「bundle 已經被換掉、但程式還跑著舊的映像」這種比沒有暫存更難收拾的狀態。
//!
//! 因此三支暫存相關的函式在 macOS 上是**語意正確的 no-op**，不是佔位：
//!
//! * [`apply_pending_at_startup`]：沒有東西可以交棒，回空；
//! * [`restore_staged`]：沒有暫存區可以認回，不碰狀態；
//! * [`discard_staged`]：沒有檔案要清，只把狀態欄位歸零（本來就是零）。
//!
//! 連帶的，[`Pending`] 永遠不會被建構、`AppState::staged_version()` 永遠是 `None`，
//! 於是系統匣那一段「Restart to update」在 macOS 上不會出現，[`apply_now`]
//! （它唯一的呼叫端）也就不會被按到。真正的更新入口只有一個：設定頁那顆主鈕
//! → [`install`]。
//!
//! ## 2. 兩條車道的判定改看「跑不跑在 .app bundle 裡」
//!
//! Windows 分安裝版／可攜版，讀的是 NSIS 寫的 HKCU 解除安裝機碼。macOS 沒有安裝
//! 機碼這種東西，能不能就地更新完全取決於**這支執行檔在不在一顆 `.app` 裡**
//! （見 [`bundle_of`]）：在，替換的對象就是那顆 bundle；不在（`cargo tauri dev`、
//! 直接跑 `target/debug/traytunnel`），那就沒有 bundle 可以換，只能比版本並把
//! 使用者送去 Releases 頁——與 Windows 可攜車道同一個結局，`UpdateInfo.installed`
//! 也同樣是 `false`，前端那顆鈕自己會從「Update to vX」變成「Get vX」。
//!
//! 這道閘不是可有可無的：外掛的 `extract_path` 在非 bundle 情形下會退成「執行檔
//! 所在的資料夾」，真讓它裝下去等於把 `target/debug/` 整個 rename 掉。
//!
//! ## 3. ad-hoc 簽章的現實
//!
//! 我們目前出的是 ad-hoc 簽章（`codesign -s -`）、未公證的 bundle。標準流程照走，
//! 但有兩個已知的坎要寫在這裡，不是靠實機踩到才知道：
//!
//! * **App Management（macOS 14 Sonoma 起）**：修改別的 app bundle 需要同一組
//!   Team ID 或使用者在「隱私權與安全性 → App 管理」放行。app 更新自己有豁免，
//!   但豁免建立在有效簽章上；ad-hoc 沒有 Team ID，`.app` 若位在
//!   `/Applications`，`std::fs::rename` 很可能拿到 `PermissionDenied`。
//! * **外掛的退路是彈一次管理員密碼**：拿到 `PermissionDenied` 時它會走
//!   AppleScript 的 `with administrator privileges` 把替換做完（見外掛
//!   `updater.rs` 的 macOS `install_inner`）。那是一個當面的密碼提示——這正是
//!   macOS 這條路**絕不在背景自動下載安裝**的另一個理由：背景跳密碼框是不可接受的。
//!
//! 把 `.app` 放在使用者自己寫得動的位置（`~/Applications`）時整段替換是純
//! `rename`，不會碰到上面任何一個坎。查證與出處見 PR 說明。
//!
//! ## 背景車道做什麼
//!
//! 只查、只填狀態、只記一行日誌，**不下載、不安裝**（理由見上面第 3 點）。
//! 所以 Windows 那套「下載失敗就退避重試」的排程在這裡沒有對應物，間隔是固定的
//! ——這條路上根本沒有會失敗的下載可以退避。

use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

use crate::state::UpdateInfo;
use crate::Shared;

/// 啟動後隔這麼久才做第一次檢查：開機當下要先把系統匣、隧道那些真正要緊的事做完，
/// 更新檢查是最不急的一件。與 Windows 同一個值。
const FIRST_DELAY: Duration = Duration::from_secs(8);

/// 常駐期間的檢查間隔。與 Windows 同一個值。
const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 查 latest.json 的逾時。
///
/// updater 外掛 builder 的預設是 `None`，也就是**完全沒有上限**：GitHub 那邊一旦
/// 是半開的連線（封包進得去、回應永遠不來），這個 async 任務就再也不會回來。
/// 背景檢查每 24 小時起一次，卡住的任務會一直累積；手動按下的那顆「Check now」
/// 更糟，前端的 await 沒有逾時，按鈕會永遠停在轉圈。與 Windows 同一個值。
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// 下載更新包的逾時。
///
/// 與 [`CHECK_TIMEOUT`] 分開設是因為兩段的性質完全不同：查版本只拉一份幾百位元組
/// 的 JSON，超過半分鐘一定是卡住了；下載拉的是十幾 MB 的 `.app.tar.gz`，而
/// reqwest 的 `timeout` 管的是**整個請求含讀完 body** 的總時間，設窄了會把慢速但
/// 正常的下載一起砍掉。它要擋的是永遠不會結束的連線，不是慢的連線。
///
/// 這一段的值傳不進 builder——外掛建 `Update` 物件時把 timeout 寫死成 `None`
/// （2.10.1 的 updater.rs），builder 上設的那個只作用在 check 那次請求。
/// 所以只能在拿到 `Update` 物件之後對它的 pub 欄位直接賦值。與 Windows 同一個值。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Releases 列表頁：下拉選單的「Download from Releases」開這裡，
/// 使用者可以自己挑要哪一版（含更早的版本）
const RELEASES_PAGE: &str = "https://github.com/hunandy14/traytunnel/releases";

/// 單一版本的 release 頁前綴。發佈說明與該版的下載資產都在同一頁上，
/// 所以「View release notes」與非 bundle 版的「Get vX.Y.Z」開的是同一個網址。
const RELEASE_TAG_PREFIX: &str = "https://github.com/hunandy14/traytunnel/releases/tag/v";

/// 還不知道是哪一版（沒查過或查不到）時，release 頁退回這裡
const LATEST_RELEASE_PAGE: &str = "https://github.com/hunandy14/traytunnel/releases/latest";

/// 已經下載完、等下一次啟動才安裝的那一版。
///
/// **macOS 上永遠不會有人建構它**，理由見本檔開頭第 1 節。型別留著是因為
/// `AppState` 的 `set_staged`／`staged_version` 吃的是
/// `crate::platform::update::Pending`，兩個平台必須各給一個同名型別；
/// macOS 這一份唯一的作用就是讓 `st.set_staged(None)` 有型別可以推導。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub version: String,
}

// ---------------------------------------------------------------- 車道判定

/// 這支執行檔所屬的 `.app` bundle，不在 bundle 裡就是 `None`。
///
/// 純函式，實機與測試共用。判定條件是路徑長成
/// `<something>.app/Contents/MacOS/<binary>`：倒數第一層要是 `MacOS`、
/// 倒數第二層要是 `Contents`、再上一層的副檔名要是 `app`。
///
/// 比外掛的 `extract_path_from_executable` 嚴一點（它只看路徑字串裡有沒有
/// `Contents/MacOS`），而且**只能往嚴的方向差**：我們說「不是 bundle」時什麼都不做，
/// 我們說「是 bundle」時外掛一定也算得出同一個目錄——`the_bundle_we_detect_is_the_one_the_plugin_would_replace`
/// 就是在釘這件事。反過來鬆的話，被 rename 掉的會是某個不該動的資料夾。
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos_dir.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let bundle = contents.parent()?;
    if bundle.extension()? != "app" {
        return None;
    }
    Some(bundle.to_path_buf())
}

/// 這次跑的這一份能不能就地更新自己（＝跑在一顆 `.app` bundle 裡）。
///
/// 對應 Windows 的 `is_installed`，也是 `UpdateInfo.installed` 的來源：
/// `true` 前端給「Update to vX.Y.Z」（走 [`install`]），
/// `false` 給「Get vX.Y.Z」（走 [`open_release_page`]）。
pub fn is_installed() -> bool {
    std::env::current_exe().ok().as_deref().and_then(bundle_of).is_some()
}

/// 這台機器在 latest.json 的 `platforms` 裡對應的鍵。
///
/// 外掛找目標時的順序是 `darwin-<arch>-app` → `darwin-<arch>`（後綴來自
/// bundle type，macOS 上恆為 `app`），兩個都找不到才報錯；發佈端只放後者就夠，
/// 而這一支回的就是後者。`the_target_key_matches_the_plugins_own` 把它釘死在
/// 外掛自己的 `target()` 上，免得哪天兩邊漂掉而症狀只是「更新永遠查不到」。
fn target_key() -> String {
    // 外掛的 `updater_os()` 在 macOS 上回的是 `darwin`（不是 `macos`），
    // `updater_arch()` 對 aarch64／x86_64 回的就是 Rust 自己那兩個名字
    format!("darwin-{}", std::env::consts::ARCH)
}

/// 版本號的比較用形式：去空白、去前導的 v。
///
/// 全模組只有這一份：`is_newer`、`UpdateInfo` 的版本欄位、release 頁網址都吃它，
/// 各自寫一次 `trim_start_matches` 的話遲早會有一處漏掉而讓 `v0.7.0`
/// 與 `0.7.0` 被當成兩個版本。
fn normalize_version(version: &str) -> &str {
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

/// 這一份執行檔是哪一版。用編譯期常數，與 Windows 同一個來源。
fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// ---------------------------------------------------------------- 檢查

/// 外掛回報的那一版要不要真的當成新版，由自家的 [`is_newer`] 再判一次。
///
/// 外掛預設的比較器確實是嚴格大於（2.10.1 的 updater.rs：
/// `release.version > self.current_version`），所以正常情況下這一關不會擋掉任何
/// 東西。留著它是因為這條路上「說有新版」的權力整個握在外部相依手上：換版本、
/// 有人塞了 version_comparator、或 latest.json 長出沒預期的形狀，都可能讓那個
/// Option 變成 Some 而我們這層毫無反抗餘地。
///
/// 更新提示的失敗方向是不對稱的：漏報只是使用者晚幾天更新，誤報卻是叫他去
/// 重裝一個他已經在用的版本。
fn accept(remote: &str, current: &str, installed: bool) -> Option<UpdateInfo> {
    if !is_newer(remote, current) {
        return None;
    }
    Some(UpdateInfo { version: normalize_version(remote).to_string(), installed })
}

/// 這個錯誤是不是「latest.json 裡沒有這個平台的條目」。
///
/// **這一支是 macOS 這條路最要緊的一段。** 外掛的 `check()` 在解析完 release
/// 之後、判斷「有沒有新版」之**前**就會先去挑目標網址
/// （2.10.1 的 `updater.rs`：`let (download_url, signature) = self.get_urls(..)?;`
/// 排在 `should_update` 的分支上面），所以只要 `platforms` 裡沒有
/// `darwin-<arch>`，回來的不是 `Ok(None)`（沒有新版），而是一個
/// `TargetsNotFound` 的 **Err**——而且遠端版本是新是舊都一樣會發生。
///
/// 現行線上的 latest.json 只有 `windows-x86_64` 一條（macOS 發佈端由 W4-P 車道
/// 補上），照著 Err 走的話手動檢查會回一句看不懂的錯誤、背景檢查會每天在活動
/// 日誌記一行失敗。正確的語意是「這個平台目前沒有更新可拿」，也就是
/// `Ok(None)`，所以這裡把這兩個變體單獨挑出來降級。
///
/// 只降級這兩個：其餘的錯誤（網路、簽章、JSON 壞掉）都是真的失敗，要照實報。
fn is_target_missing(e: &tauri_plugin_updater::Error) -> bool {
    use tauri_plugin_updater::Error;
    matches!(e, Error::TargetNotFound(_) | Error::TargetsNotFound(_))
}

/// 查一次遠端，回「有沒有比現在新的版本」。
///
/// 兩條車道共用這一支：真正的差別只在 `UpdateInfo.installed`（也就是查到之後
/// 那顆鈕按下去會發生什麼），查詢本身是同一件事、同一份 latest.json。
/// 這與 Windows 不同——那邊可攜車道刻意繞開外掛自己拿 latest.json，是因為
/// 外掛的 check 一路連著 download＋install；macOS 這條路上 check 只是 check，
/// 下載與安裝都在 [`install`] 裡另外發動，沒有繞開的必要。
///
/// 不能用 `app.updater()` 那個便利方法：它建出來的 updater 沒有逾時上限，
/// 遇到半開的連線會讓整個檢查任務永遠掛著。改走 builder 自己補一道
/// [`CHECK_TIMEOUT`]。
async fn check_lane(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater =
        app.updater_builder().timeout(CHECK_TIMEOUT).build().map_err(|e| e.to_string())?;
    let found = match updater.check().await {
        Ok(found) => found,
        // latest.json 還沒有這個平台的條目＝這個平台沒有更新可拿，不是失敗
        Err(e) if is_target_missing(&e) => {
            log::info!("update check: {} has no entry in latest.json yet", target_key());
            return Ok(None);
        }
        Err(e) => return Err(e.to_string()),
    };
    let Some(update) = found else {
        return Ok(None);
    };
    Ok(accept(&update.version, current_version(), is_installed()))
}

/// 背景檢查的排程：啟動延遲一次，之後每 24 小時一次，跟著程式活到結束。
///
/// 對照 Windows：那邊下載失敗會把間隔縮成退避序列，因為它的背景車道**會下載**。
/// macOS 的背景車道只查不下載（本檔開頭第 3 節：背景自動安裝可能跳出管理員密碼
/// 提示，那是不能在使用者不在場時發生的事），沒有會失敗的下載可以退避，
/// 所以間隔固定。查詢本身失敗就等下一輪，與 Windows 的
/// `Outcome::Settled` 同一個處置。
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

/// 使用者剛把「Automatic updates」打開時立刻查一次，不必等到明天這個時候。
pub fn check_now(state: &Shared) {
    let st = state.clone();
    tauri::async_runtime::spawn(async move {
        check_once(&st).await;
    });
}

/// 背景查一次。任何失敗都只記一行就算了——更新查不到不影響程式本身能不能用，
/// 沒有理由為它彈通知或改變任何狀態。
async fn check_once(st: &Shared) {
    // 關掉就是完全不連外：這道閘在任何請求送出之前
    if !st.checks_for_updates() {
        return;
    }
    let found = match check_lane(&st.app).await {
        Ok(found) => found,
        Err(e) => {
            st.log(format!("update check failed: {e}"));
            return;
        }
    };
    // 每 24 小時會再查一次，同一版重複記一行只會讓活動日誌看起來像真的又發生了
    // 什麼事，所以「偵測到新版」這一行跟著 set_update 的去重走
    if st.set_update(found.clone()) {
        if let Some(u) = found {
            st.log(format!("update available: v{}", u.version));
        }
    }
}

/// 使用者主動按下的檢查（設定頁的「Check for updates」與下拉的「Check now」）。
///
/// 刻意**不**看 `checks_for_updates` 那道閘：它管的是「要不要自己在背景連外」，
/// 而使用者親手按下這顆鈕，就是對這一次連外的明示同意。拿背景開關去擋一個
/// 當面的請求，得到的只會是一顆按了沒反應的鈕。
///
/// 與背景車道的另一個差別是結果要回傳：按鈕靠它呈現 Up to date／Check failed
/// 那兩個瞬態，而背景車道對這兩種結果都是靜默的。共用狀態照樣更新，兩條車道與
/// 介面看到的始終是同一份事實。
///
/// Windows 版在這裡還會多做一件事——查到新版且自動更新開著時順手把它下載進暫存區。
/// macOS 沒有那一步（本檔開頭第 1 節），查到就是查到，要不要裝由使用者按下一顆鈕決定。
pub async fn check_manually(st: &Shared) -> Result<Option<UpdateInfo>, String> {
    let found = match check_lane(&st.app).await {
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

// ---------------------------------------------------------------- 暫存（macOS 沒有）

/// 啟動最早期的交棒判定。回傳要補進活動日誌的行。
///
/// macOS 沒有暫存交棒（本檔開頭第 1 節），沒有東西可以交棒，所以「什麼都沒做，
/// 也沒有日誌要補」就是這一支唯一正確的答案。
///
/// 呼叫位置（`lib.rs::run` 最開頭、`tauri::Builder` 之前）是 Windows 那條路的規格，
/// 這裡照樣不做任何有副作用的事，維持那個位置的假設不被打破。
pub fn apply_pending_at_startup(_tray: bool) -> Vec<String> {
    Vec::new()
}

/// 把暫存區裡那份就緒的更新認回狀態。
///
/// macOS 沒有暫存區可以認回（本檔開頭第 1 節）。`AppState` 建構時 `pending`
/// 欄位預設就是 `None`，這裡什麼都不做，狀態自然停在「沒有就緒的更新」，
/// 於是系統匣不會長出一顆按下去必定失敗的「Restart to update」。
pub fn restore_staged(_st: &Shared) {}

/// 關掉自動更新時把暫存區清乾淨。
///
/// macOS 沒有檔案要清（本檔開頭第 1 節），但狀態還是要歸零——這一支的呼叫端是
/// `commands::set_automatic_updates(false)`，那裡的承諾是「關掉之後畫面上不會再
/// 掛著任何跟更新有關的東西」。`set_staged(None)` 與 `set_update_stalled(false)`
/// 在 macOS 上都是把一個本來就是零的格子再寫一次零，留著是為了讓這個承諾不依賴
/// 「macOS 剛好沒有人會去設那兩格」這個巧合。
pub fn discard_staged(st: &Shared) {
    st.set_staged(None);
    st.set_update_stalled(false);
}

/// 套用已經下載好的那一份（系統匣的「Restart to update」）。
///
/// macOS 上這一支**到不了**：那個選單項只在 `AppState::staged_version()` 是 Some
/// 時才會被畫出來（見 `traymenu`），而 macOS 從來不會有暫存（本檔開頭第 1 節）。
/// 真的被叫到只可能是共用核心哪天改了畫選單的條件，所以這裡回一句說得清楚的錯誤，
/// 讓它出現在活動日誌裡，而不是假裝做了什麼。
pub fn apply_now(_st: &Shared) -> Result<(), String> {
    Err("macOS installs updates immediately, there is nothing waiting to be applied".into())
}

// ---------------------------------------------------------------- 下載與安裝

/// 設定頁那顆綠色主鈕（「Update to vX.Y.Z」）：下載、原地替換 bundle、重啟。
///
/// **不看「Automatic updates」開關**，理由與 [`check_manually`] 相同：使用者按下
/// 這顆鈕就是對這一次更新的明示同意，拿背景開關去擋只會變成按了沒反應。
///
/// 順序上有兩件事是規格的一部分：
///
/// 1. **`is_installed()` 那道閘排在最前面。** 不在 bundle 裡就沒有 bundle 可以換，
///    而外掛的 `extract_path` 這時會退成「執行檔所在的資料夾」——真讓它裝下去，
///    被 rename 掉的是 `target/debug/`。前端在這種情形下本來就走
///    [`open_release_page`]（`installed` 是 `false`），這道閘是第二層保險。
/// 2. **替換那一步一定要離開 async 執行緒。** 它要解開十幾 MB 的 tar 並搬動整個
///    bundle，是純阻塞的檔案 IO；而且外掛在權限不足時會走 AppleScript 的管理員
///    提權，那條路是「把閉包丟到主執行緒、然後在原地 `rx.recv()` 等結果」——在
///    主執行緒上呼叫它會直接自鎖。`spawn_blocking` 兩件事一起解決。
///
/// 正常路徑上這支函式**不會回來**：`AppHandle::restart()` 的回傳型別是 `!`。
/// 前端那顆鈕因此只需要處理 Err（`sheet.ts::startUpdate` 正是這樣寫的）。
pub async fn install(st: &Shared) -> Result<(), String> {
    if !is_installed() {
        return Err("This build cannot update itself".into());
    }
    let updater =
        st.app.updater_builder().timeout(CHECK_TIMEOUT).build().map_err(|e| e.to_string())?;
    let found = match updater.check().await {
        Ok(found) => found,
        Err(e) if is_target_missing(&e) => None,
        Err(e) => return Err(e.to_string()),
    };
    let mut update = found.ok_or_else(|| "No update available".to_string())?;
    // 「有沒有新版」不外包給外部相依，這一關與檢查那條路走同一支判定
    if accept(&update.version, current_version(), true).is_none() {
        return Err("No update available".into());
    }
    // builder 上那個逾時只管 check 那次請求，Update 物件的 timeout 是外掛寫死的
    // None（＝下載沒有任何上限）。兩段的合理值差了一個數量級，理由見常數本身。
    update.timeout = Some(DOWNLOAD_TIMEOUT);

    let version = normalize_version(&update.version).to_string();
    st.log(format!("downloading update v{version}"));
    // 下載回來的 bytes **已經過 minisign 驗簽**（外掛 updater.rs 的
    // `verify_signature`，驗不過就是 Err），所以交給 install 的一定是簽章對得上
    // 的那一份。macOS 不像 Windows 需要再記一份 SHA-256——那是給「在磁碟上躺到
    // 下一次啟動」的暫存檔用的，這裡的 bytes 從驗簽到解開都沒有離開過記憶體。
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;

    st.log(format!("installing update v{version}"));
    tauri::async_runtime::spawn_blocking(move || update.install(bytes).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())??;

    restart_into(st, &version)
}

/// 換完 bundle 之後把自己重啟到新版上。
///
/// 收尾那一組（`mark_exiting`／`kill_all_jobs`）要在重啟之前自己做：`lib.rs::run`
/// 沒有掛 `RunEvent` 回呼，隧道是靠 `do_exit` 手動收的，重啟這條路一樣得收，
/// 否則新舊兩個行程會同時抓著同一批本地埠。
///
/// 視窗位置**不必**自己存（Windows 那邊要，因為它 `std::process::exit(0)` 繞過了
/// `RunEvent::Exit`，tauri-plugin-window-state 落地存檔的 hook 不會跑）。
/// `AppHandle::restart()` 在非主執行緒上是走「請事件迴圈正常退出、退完再重新
/// 執行自己」，`RunEvent::Exit` 照發，window-state 與 single-instance 兩顆外掛的
/// 收尾（存視窗幾何、拆掉 `/tmp` 的那顆 socket）都會照常跑完才輪到重啟。
///
/// 重啟帶的是**原本那一份 argv**（tauri 的 `process::restart` 拿 `Env::args_os`）。
/// 於是「開機自啟帶著 `--tray` 起來的那一份，更新完仍然縮在系統匣」——那是這件事
/// 最要緊的情形（`sys::plist_contents` 寫的就是 `--tray`）。
/// 與 Windows 的差別只在「手動開起來、之後才收進系統匣」那一種：Windows 會補一個
/// `--tray` 給安裝程式，macOS 這條路沒有地方塞參數，視窗會跟著回來。
fn restart_into(st: &Shared, version: &str) -> Result<(), String> {
    st.log(format!("restarting to finish updating to v{version}"));
    st.mark_exiting();
    st.kill_all_jobs();
    st.app.restart()
}

// ---------------------------------------------------------------- 開瀏覽器

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
/// 非 bundle 版的「Get vX.Y.Z」與下拉的「View release notes」都走這裡。
pub fn open_release_page(st: &Shared, version: Option<&str>) {
    open_page(st, &release_url(version));
}

/// Releases 列表頁：下拉的「Download from Releases」走這裡，
/// 讓使用者自己挑版本換檔案。這條路不下載、不碰自己這顆 bundle。
pub fn open_releases_page(st: &Shared) {
    open_page(st, RELEASES_PAGE);
}

fn open_page(st: &Shared, url: &str) {
    if let Err(e) = super::sys::open_url(url) {
        st.log(format!("could not open {url}: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 更新資訊清單。這裡只有 live 測試會去拉它，實機那條路是 updater 外掛
    /// 自己照 tauri.conf.json 的 endpoints 去拿——兩邊指的是同一份檔案，
    /// `the_live_endpoint_is_the_one_the_app_ships_with` 釘住這件事。
    const LATEST_JSON: &str =
        "https://github.com/hunandy14/traytunnel/releases/latest/download/latest.json";

    /// GitHub 對沒有 User-Agent 的請求會直接回 403，一定要帶
    const USER_AGENT: &str = concat!("traytunnel/", env!("CARGO_PKG_VERSION"));

    fn exe_in(bundle: &str) -> PathBuf {
        PathBuf::from(format!("{bundle}/Contents/MacOS/traytunnel"))
    }

    /// 正常安裝的樣子：`/Applications/traytunnel.app/Contents/MacOS/traytunnel`
    #[test]
    fn an_executable_inside_an_app_bundle_is_recognised() {
        assert_eq!(
            bundle_of(&exe_in("/Applications/traytunnel.app")),
            Some(PathBuf::from("/Applications/traytunnel.app"))
        );
        // 使用者自己寫得動的位置（不會撞到 App Management／提權那兩個坎）
        assert_eq!(
            bundle_of(&exe_in("/Users/me/Applications/traytunnel.app")),
            Some(PathBuf::from("/Users/me/Applications/traytunnel.app"))
        );
        // 路徑裡有空白也照樣認得
        assert_eq!(
            bundle_of(&exe_in("/Users/bob smith/My Apps/tray tunnel.app")),
            Some(PathBuf::from("/Users/bob smith/My Apps/tray tunnel.app"))
        );
    }

    /// `cargo tauri dev`／直接跑 target 底下那顆執行檔：**不是** bundle。
    ///
    /// 這一條錯掉的代價不是「更新查不到」，是 `install` 會讓外掛把
    /// `target/debug/` 整個 rename 掉——開發樹當場消失，而且沒有任何提示。
    #[test]
    fn a_bare_executable_is_not_a_bundle() {
        assert_eq!(bundle_of(Path::new("/repo/src-tauri/target/debug/traytunnel")), None);
        assert_eq!(bundle_of(Path::new("/usr/local/bin/traytunnel")), None);
        assert_eq!(bundle_of(Path::new("traytunnel")), None);
    }

    /// 三層都要對得上，缺一層或名字不對都不算。半套的路徑不可以被當成 bundle：
    /// 認錯的話被替換掉的會是某個不該動的資料夾
    #[test]
    fn a_half_matching_path_is_not_a_bundle() {
        // 少了 .app 副檔名
        assert_eq!(bundle_of(&exe_in("/Applications/traytunnel")), None);
        // 中間那層不是 Contents
        assert_eq!(
            bundle_of(Path::new("/Applications/traytunnel.app/Resources/MacOS/traytunnel")),
            None
        );
        // 最後那層不是 MacOS
        assert_eq!(
            bundle_of(Path::new("/Applications/traytunnel.app/Contents/Helpers/traytunnel")),
            None
        );
        // 副檔名不是 app
        assert_eq!(bundle_of(&exe_in("/Applications/traytunnel.bundle")), None);
    }

    /// 我們認出來的那顆 bundle，必須就是外掛真的會去替換的那個目錄。
    ///
    /// 兩邊的判定不是同一份程式碼（外掛只看路徑字串裡有沒有 `Contents/MacOS`，
    /// 我們是逐層比對），差一格的後果是 `install` 把錯的東西 rename 掉。
    /// 外掛的 `extract_path_from_executable` 是 pub 的，直接拿它來對。
    #[test]
    fn the_bundle_we_detect_is_the_one_the_plugin_would_replace() {
        for bundle in ["/Applications/traytunnel.app", "/Users/me/Applications/tray tunnel.app"] {
            let exe = exe_in(bundle);
            let ours = bundle_of(&exe).expect("這是一顆 bundle");
            let theirs =
                tauri_plugin_updater::extract_path_from_executable(&exe).expect("外掛也算得出來");
            assert_eq!(ours, theirs, "替換目標必須一致：{}", exe.display());
        }
    }

    /// latest.json 的 `platforms` 要用哪個鍵，權威是外掛自己的 `target()`。
    ///
    /// 這個字串是發佈端（W4-P）與執行期唯一的接點，而且完全沒有型別把關：
    /// 寫錯的症狀是 macOS 的更新從此靜默查不到，沒有人會注意到。
    /// 順帶釘住 macOS 上那半段是 `darwin` 不是 `macos`——外掛的 `updater_os()`
    /// 原始碼裡自己都留著一句「TODO shouldn't this be macos instead?」。
    #[test]
    fn the_target_key_matches_the_plugins_own() {
        assert_eq!(Some(target_key()), tauri_plugin_updater::target());
        assert!(target_key().starts_with("darwin-"), "{}", target_key());
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

    /// 最後一道閘：外掛就算回了 Some，版本沒有嚴格大於就不算數。
    /// 「有沒有新版」這個判斷不可以整個外包給外部相依。
    #[test]
    fn a_version_that_is_not_newer_is_refused() {
        assert_eq!(accept("0.5.0", "0.5.0", true), None);
        assert_eq!(accept("0.4.9", "0.5.0", true), None);
        assert_eq!(accept("", "0.5.0", true), None);
        assert_eq!(accept("latest", "0.5.0", true), None);
    }

    /// 真的有新版時照樣要放行，而且版本號存進去是不帶 v 的（`UpdateInfo` 的契約，
    /// 前端會自己補上 v 顯示成 `Update to v0.6.0`）。
    ///
    /// `installed` 決定前端給的是哪一顆鈕：bundle 裡是「Update to vX」（走
    /// [`install`]），bundle 外是「Get vX」（走 [`open_release_page`]）。
    #[test]
    fn a_real_update_passes_through_with_the_lane_flag() {
        let bundled = accept("0.6.0", "0.5.0", true).expect("0.6.0 比 0.5.0 新");
        assert_eq!(bundled, UpdateInfo { version: "0.6.0".into(), installed: true });
        let bare = accept("v0.6.0", "0.5.0", false).expect("帶 v 的一樣認得");
        assert_eq!(bare, UpdateInfo { version: "0.6.0".into(), installed: false });
    }

    /// 「這個平台還沒有條目」要降級成「沒有更新」，其餘錯誤照實往上報。
    ///
    /// 這是 macOS 這條路上最容易錯的一格：外掛挑目標網址排在「有沒有新版」的
    /// 判斷**之前**，所以 latest.json 少了 `darwin-<arch>` 時，check 回的是 Err
    /// 而不是 `Ok(None)`——照著 Err 走，使用者按下檢查會看到一句
    /// 「None of the fallback platforms ... were found」。
    #[test]
    fn a_missing_platform_entry_is_not_a_failure() {
        use tauri_plugin_updater::Error;
        assert!(is_target_missing(&Error::TargetNotFound("darwin-aarch64".into())));
        assert!(is_target_missing(&Error::TargetsNotFound(vec![
            "darwin-aarch64-app".into(),
            "darwin-aarch64".into(),
        ])));
        // 真的失敗要照實報，不可以被一起吞掉
        assert!(!is_target_missing(&Error::ReleaseNotFound));
        assert!(!is_target_missing(&Error::EmptyEndpoints));
        assert!(!is_target_missing(&Error::Network("connection reset".into())));
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

    /// 這條路上的每一個網址都必須是 https：`sys::open_url` 只放行 https，
    /// 組出一個別的 scheme 的話按下去只會在活動日誌留一行「could not open」
    #[test]
    fn every_page_this_module_opens_is_https() {
        for url in [RELEASES_PAGE, LATEST_RELEASE_PAGE, &release_url(Some("0.6.0"))] {
            assert!(url.starts_with("https://"), "{url}");
        }
        assert!(RELEASE_TAG_PREFIX.starts_with("https://"));
    }

    /// live 測試拉的那份 latest.json，必須就是出貨設定裡 updater 外掛會去拿的
    /// 那一份——不然 live 測到的東西跟實機走的是兩條路
    #[test]
    fn the_live_endpoint_is_the_one_the_app_ships_with() {
        let raw = include_str!("../../../tauri.conf.json");
        let conf: serde_json::Value =
            serde_json::from_str(raw).expect("tauri.conf.json 必須是合法 JSON");
        let endpoints = conf
            .pointer("/plugins/updater/endpoints")
            .and_then(|v| v.as_array())
            .expect("plugins.updater.endpoints 不可以消失");
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].as_str(), Some(LATEST_JSON));
    }

    /// 實機測試：對真的 endpoint 拉一次 latest.json，確認 macOS 這一端的降級行為。
    ///
    /// ```text
    /// cargo test --lib -- --ignored --nocapture live_latest_json
    /// ```
    ///
    /// 唯讀、無副作用：只做一次 GET，不下載更新包、不碰任何檔案。**絕不可以**
    /// 在這裡呼叫 `download_and_install`——那會替換掉這台機器上的某個 bundle。
    ///
    /// 驗的是「發佈端還沒補上 macOS 條目」這段過渡期的行為：`platforms` 裡沒有
    /// `darwin-<arch>` 時，外掛挑目標網址那一步吐的是 `TargetNotFound`，而
    /// [`is_target_missing`] 必須把它認出來降級成「沒有更新」。W4-P 把
    /// `darwin-aarch64` 補上之後這條測試會走另一個分支，一樣是綠的。
    #[test]
    #[ignore]
    fn live_latest_json_has_no_darwin_entry_yet_and_that_is_not_a_failure() {
        let config =
            ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).build();
        let agent = ureq::Agent::new_with_config(config);
        let mut resp = agent
            .get(LATEST_JSON)
            .header("User-Agent", USER_AGENT)
            .call()
            .expect("拉得到 latest.json");
        let raw: String = resp.body_mut().read_to_string().expect("讀得到內容");
        println!("latest.json（{} bytes）：{raw}", raw.len());

        let release: tauri_plugin_updater::RemoteRelease =
            serde_json::from_str(&raw).expect("外掛必須解析得出來，否則 check 整個失敗");
        println!("遠端版本：{}，本機版本：{}", release.version, current_version());

        let key = target_key();
        match release.download_url(&key) {
            Ok(url) => println!("latest.json 已經有 {key} 條目：{url}"),
            Err(e) => {
                assert!(
                    is_target_missing(&e),
                    "少了平台條目時外掛吐的必須是我們認得的那個錯誤，實際是：{e}"
                );
                println!("latest.json 還沒有 {key} 條目（{e}）→ 降級成「沒有更新」，不是失敗");
            }
        }
    }
}
