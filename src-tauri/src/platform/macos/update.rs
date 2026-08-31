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
//! ## 3. ad-hoc 簽章的現實（查證過，別憑印象改）
//!
//! 我們出的是 ad-hoc 簽章（`codesign -s -`）、未公證的 bundle。查證結論是
//! **機制上走得通**，但踩得到的坑跟簽章沒什麼關係，寫在這裡免得下一個人靠實機
//! 一個個撞出來（出處見 PR 說明）：
//!
//! * **Tauri 官方文件從沒說 macOS 更新需要 Apple 簽章**——updater 那一頁提到
//!   macOS 只有 `.tar.gz` 產物路徑，簽章那一頁一次都沒提到更新。它要的簽章是
//!   minisign 的更新金鑰，與 codesign 是兩回事。
//! * **App Management（macOS 14 起）擋的是「就地改別人 bundle 裡的檔案」，
//!   不是「整包換掉」。** 外掛做的是 `rename` 舊 bundle 去備份、`rename` 新的
//!   進來，屬於後者，預期不會撞上這道 TCC——`tauri-apps` 整個組織搜不到一則
//!   提到 App Management 的 issue，這條路要是常態失敗不可能四年零回報。
//!   （順帶：ad-hoc 沒有 Team ID，「同一組 Team ID 可互改」那條豁免對我們本來
//!   就不成立，但如上，這條路根本不需要它。）
//! * **真正會失敗的是 POSIX 權限**：外掛第一步 `rename` 拿到 `PermissionDenied`
//!   時，會用 AppleScript 的 `with administrator privileges` 把替換做完（見外掛
//!   `updater.rs` 的 macOS `install_inner`）。條件是「`.app` 這個目錄自己寫不動」
//!   ——標準（非 admin）使用者，或 bundle 是 root 所有（用 `.pkg` 裝的、
//!   或曾經 `sudo cp` 過）。一般 admin 帳號 + 自己拖進 `/Applications` 的 bundle
//!   是純 `rename`，不會跳密碼。這正是外掛那條提權路的來源（`tauri#8104`）。
//! * **App Translocation**：bundle 還帶著 `com.apple.quarantine`（從瀏覽器抓下來
//!   的 dmg／zip）而且直接從 `~/Downloads` 或掛載的 dmg 裡啟動時，macOS 會把它
//!   搬到一個唯讀的隨機路徑執行，更新當場死在 `EROFS`
//!   （`plugins-workspace#2148`）。使用者用 Finder 把 app 搬進 `/Applications`
//!   之後就不再 translocate——這是「請先拖進應用程式資料夾」那句安裝指示真正的
//!   技術理由，不只是慣例。
//! * **解壓那一步會掉東西**：外掛的 tar 解壓不還原 xattr，symlink 與執行位元
//!   出過事（`tauri#7480` 的 framework symlink 變成 0 byte 檔案）。但**順序本身
//!   是安全的**——釘住的 2.10.1 是先把整包解到暫存目錄、每一筆都解得出來了，
//!   才去搬舊 bundle（`plugins-workspace#1129` 修掉的正是相反的舊順序）。
//!   所以解壓失敗只是這一次更新沒成功，既有的 app 一根寒毛都沒動到。
//!
//! 兩件與直覺相反、但查證過的事：自己用 HTTP 下載回來的 bytes **不會**被打上
//! `com.apple.quarantine`（那是 LaunchServices 才會做的事，`LSFileQuarantineEnabled`
//! 預設 false 而 Tauri 從沒設過它），所以替換完首次啟動不會跳 Gatekeeper；
//! 而 tar 來回一趟**不會**弄壞 ad-hoc 簽章（簽章存在 Mach-O 的
//! `LC_CODE_SIGNATURE` 與 `_CodeSignature/CodeResources` 這些一般檔案裡，不在 xattr）。
//!
//! ## 背景車道做什麼
//!
//! 只查、只填狀態、只記一行日誌，**不下載、不安裝**。這是這條路上最重要的一個
//! 決定，理由不是上面那些坑，而是第 1 節那件事：macOS 沒有暫存交棒，「安裝」與
//! 「把使用者正在用的程式關掉」是同一個動作。背景自動安裝只有兩種收場——
//! 換完 bundle 卻不重啟（使用者一直跑著已經不存在的舊映像），或替使用者決定
//! 現在就重啟（隧道連同他手上的工作一起斷掉）。Windows 那條路之所以敢在背景
//! 做完整套，正是因為它有暫存：安裝發生在**下一次啟動**，使用者不在場。
//! 這裡沒有那個中間態，所以最後一步只能由使用者按下去。
//!
//! 附帶的好處：上面那顆管理員密碼框，以及整條路上唯一沒有回頭路的那一段
//! ——提權路徑是一句 `rm -rf` 舊 bundle 再 `mv` 新的進去，中間沒有備份可以還原，
//! 斷在中途就沒有 app 可以開了——都只會發生在使用者按了鈕、正看著畫面的時候，
//! 不會在他不在場時發生。
//!
//! 連帶的，Windows 那套「下載失敗就退避重試」的排程在這裡沒有對應物，間隔是
//! 固定的——這條路上根本沒有會失敗的下載可以退避。

use std::path::{Path, PathBuf};

use tauri::AppHandle;
use tauri_plugin_updater::Update;

use crate::platform::update_common::{
    self, accept, current_version, normalize_version, FIRST_DELAY, INTERVAL,
};
use crate::state::UpdateInfo;
use crate::Shared;

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

/// 這一份執行檔能不能就地更新自己。純函式，實機與測試共用。
///
/// 兩道都要成立：
///
/// 1. **跑在一顆 `.app` bundle 裡**（[`bundle_of`]）——沒有 bundle 就沒有東西可換；
/// 2. **不是 App Translocation 的唯讀影本**（`sys::is_app_translocated`）。第二道
///    是這一版補上的：從 dmg 視窗或 `~/Downloads` 直接雙擊時，macOS 跑的是一份掛在
///    唯讀掛載點上的隨機路徑影本，它**看起來完全是一顆合格的 `.app`**（第一道照樣
///    成立），但外掛第一步 `std::fs::rename(extract_path, backup)` 會拿到 **EROFS**
///    ——不是 `PermissionDenied`，所以連外掛那條 AppleScript 提權退路都不會走，
///    使用者看到的是「update failed: Read-only file system (os error 30)」，而且每
///    24 小時的背景檢查會持續重新推薦同一版（`plugins-workspace#2148`，本檔開頭
///    「App Translocation」那一段早就寫著這是已知失敗，卻一直沒有閘）。
///
/// 少了第二道的症狀不只是「更新失敗」：使用者是先等完一次十幾 MB 的下載，才收到
/// 一句看不懂的 os error。加上之後，前端那顆鈕從一開始就是「Get vX.Y.Z」
/// （開 release 頁），與「這一份不能更新自己」的事實一致。
fn can_update_in_place(exe: &Path) -> bool {
    bundle_of(exe).is_some() && !super::sys::is_app_translocated(exe)
}

/// 這次跑的這一份能不能就地更新自己。
///
/// 對應 Windows 的 `is_installed`，也是 `UpdateInfo.installed` 的來源：
/// `true` 前端給「Update to vX.Y.Z」（走 [`install`]），
/// `false` 給「Get vX.Y.Z」（走 [`open_release_page`]）。判定本身見
/// [`can_update_in_place`]。
pub fn is_installed() -> bool {
    std::env::current_exe().ok().is_some_and(|exe| can_update_in_place(&exe))
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

// `normalize_version`／`is_newer`／`current_version` 兩平台逐字相同，已上提到
// [`update_common`]，這裡改成從那邊 `use` 進來（見本檔開頭 import）。

// ---------------------------------------------------------------- 檢查

// `accept`（外掛回報的版本要不要真的算新版，再過一次 `is_newer`）兩平台原本
// 各抄一份（Windows 那邊叫 `accept_installed`，可攜車道還內聯了第三份），
// 已上提到 [`update_common`]，這裡改成從那邊 `use` 進來（見本檔開頭 import）。

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

/// 查一次遠端，拿回外掛的 `Update` 物件（還沒經過 [`accept`] 判斷是不是真的算
/// 新版）。
///
/// [`check_lane`] 與 [`install`] 都要走這三步：建 updater、發 `check()`、把
/// 「這個平台在 latest.json 裡還沒有條目」降級成「沒有東西可拿」——原本兩處各自
/// 抄一份，抄出了一個分歧：`check_lane` 那份會記一行 info，`install` 那份悄悄
/// 吞掉，同一件事在活動日誌裡看起來厚此薄彼。統一在這裡記那一行日誌，兩個呼叫端
/// 都看得到同一句話，也不會有人漏記。
///
/// 建 updater 那一步（連同它那道非有不可的檢查逾時）走
/// [`update_common::build_updater`]——兩個平台三個呼叫點共用同一份，理由見那裡。
async fn fetch_update(app: &AppHandle) -> Result<Option<Update>, String> {
    let updater = update_common::build_updater(app)?;
    match updater.check().await {
        Ok(found) => Ok(found),
        // latest.json 還沒有這個平台的條目＝這個平台沒有更新可拿，不是失敗
        Err(e) if is_target_missing(&e) => {
            log::info!("update check: {} has no entry in latest.json yet", target_key());
            Ok(None)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// 查一次遠端，回「有沒有比現在新的版本」。
///
/// 兩條車道共用這一支：真正的差別只在 `UpdateInfo.installed`（也就是查到之後
/// 那顆鈕按下去會發生什麼），查詢本身是同一件事、同一份 latest.json。
/// 這與 Windows 不同——那邊可攜車道刻意繞開外掛自己拿 latest.json，是因為
/// 外掛的 check 一路連著 download＋install；macOS 這條路上 check 只是 check，
/// 下載與安裝都在 [`install`] 裡另外發動，沒有繞開的必要。
async fn check_lane(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let Some(update) = fetch_update(app).await? else {
        return Ok(None);
    };
    Ok(accept(&update.version, current_version(), is_installed()))
}

/// 背景檢查的排程：啟動延遲一次，之後每 24 小時一次，跟著程式活到結束。
///
/// 對照 Windows：那邊下載失敗會把間隔縮成退避序列，因為它的背景車道**會下載**。
/// macOS 的背景車道只查不下載（理由見本檔開頭「背景車道做什麼」那一節：
/// 沒有暫存交棒，安裝與「把使用者正在用的程式關掉」是同一個動作），
/// 沒有會失敗的下載可以退避，所以間隔固定。查詢本身失敗就等下一輪，
/// 與 Windows 的 `Outcome::Settled` 同一個處置。
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
///
/// 「查完之後怎麼記帳」兩平台逐字相同，已上提到 [`update_common::record_background_check`]
/// （見本檔開頭 import）；macOS 的 `check_lane` 本來就回 `Option<UpdateInfo>`，
/// 不必像 Windows 那樣先轉一次。
async fn check_once(st: &Shared) {
    // 關掉就是完全不連外：這道閘在任何請求送出之前
    if !st.checks_for_updates() {
        return;
    }
    update_common::record_background_check(st, check_lane(&st.app).await);
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
///
/// 「查完之後怎麼記帳」兩平台逐字相同，已上提到 [`update_common::record_manual_check`]
/// （見本檔開頭 import）。
pub async fn check_manually(st: &Shared) -> Result<Option<UpdateInfo>, String> {
    update_common::record_manual_check(st, check_lane(&st.app).await)
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
/// 1. **能不能就地更新那道閘排在最前面**（[`can_update_in_place`]）。不在 bundle 裡
///    就沒有 bundle 可以換，而外掛的 `extract_path` 這時會退成「執行檔所在的資料夾」
///    ——真讓它裝下去，被 rename 掉的是 `target/debug/`；translocated 的唯讀影本則是
///    會死在 `EROFS`。前端在這兩種情形下本來就走 [`open_release_page`]
///    （`installed` 是 `false`），這道閘是第二層保險。
///    translocation 單獨挑出來先擋，是因為它有話可以對使用者說（把 app 搬進
///    應用程式資料夾），而「這一份不能更新自己」那句話對他毫無幫助。
/// 2. **替換那一步一定要離開 async 執行緒。** 它要解開十幾 MB 的 tar 並搬動整個
///    bundle，是純阻塞的檔案 IO；而且外掛在權限不足時會走 AppleScript 的管理員
///    提權，那條路是「把閉包丟到主執行緒、然後在原地 `rx.recv()` 等結果」——在
///    主執行緒上呼叫它會直接自鎖。`spawn_blocking` 兩件事一起解決。
///
/// 正常路徑上這支函式**不會回來**：`AppHandle::restart()` 的回傳型別是 `!`。
/// 前端那顆鈕因此只需要處理 Err（`sheet.ts::startUpdate` 正是這樣寫的）。
pub async fn install(st: &Shared) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if super::sys::is_app_translocated(&exe) {
        log::warn!(
            "refusing to install an update: this run is an App Translocation copy ({}), \
             the bundle is on a read-only mount",
            exe.display()
        );
        return Err(super::sys::translocation_refusal_text(
            "it cannot replace its own bundle (the copy is on a read-only mount)",
            "check for updates again",
        ));
    }
    if !can_update_in_place(&exe) {
        return Err("This build cannot update itself".into());
    }
    let mut update =
        fetch_update(&st.app).await?.ok_or_else(|| "No update available".to_string())?;
    // 「有沒有新版」不外包給外部相依，這一關與檢查那條路走同一支判定
    if accept(&update.version, current_version(), true).is_none() {
        return Err("No update available".into());
    }
    let version = normalize_version(&update.version).to_string();
    st.log(format!("downloading update v{version}"));
    // 下載那一步（連同外掛寫死成 None、只能事後補上的 DOWNLOAD_TIMEOUT）走
    // [`update_common::download`]，三個呼叫點共用同一份逾時。
    //
    // 下載回來的 bytes **已經過 minisign 驗簽**（外掛 updater.rs 的
    // `verify_signature`，驗不過就是 Err），所以交給 install 的一定是簽章對得上
    // 的那一份。macOS 不像 Windows 需要再記一份 SHA-256——那是給「在磁碟上躺到
    // 下一次啟動」的暫存檔用的，這裡的 bytes 從驗簽到解開都沒有離開過記憶體。
    let bytes = update_common::download(&mut update).await?;

    st.log(format!("installing update v{version}"));
    tauri::async_runtime::spawn_blocking(move || update.install(bytes).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())??;

    restart_into(st, &version)
}

/// 換完 bundle 之後把自己重啟到新版上。
///
/// 收尾那一組（`mark_exiting`／`kill_all_jobs`）要在重啟之前自己做，否則新舊
/// 兩個行程會同時抓著同一批本地埠。
///
/// `lib.rs::run` 確實掛了 `RunEvent::Exit` 回呼（`kill_jobs_on_final_exit`，
/// 那是給 Dock Quit 與登出準備的），但這條路**不靠它**：那個掛鉤是最後一道
/// 保險，跑的時機是事件迴圈已經要收攤的當下，而這裡需要的是「在
/// `AppHandle::restart()` 之前就確定埠已經放掉」。先在這裡 `mark_exiting()`
/// 也順便讓那個掛鉤讓路（它看到 `is_exiting()` 就直接返回），不會重複殺一次。
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
    st.shutdown();
    st.app.restart()
}

// ---------------------------------------------------------------- 開瀏覽器
//
// `release_url` 的組法、逾時常數與這兩支 `open_*` 的邏輯兩平台逐字相同，已上提到
// [`update_common`]；這裡的兩支只負責把 macOS 自己的 `sys::open_url` 注入進去
// ——見 `update_common` 開頭「為什麼不直接伸手進 `platform::macos::sys`」那段。

/// 單一版本的 release 頁：發佈說明與該版的下載資產都在上面。
/// 非 bundle 版的「Get vX.Y.Z」與下拉的「View release notes」都走這裡。
pub fn open_release_page(st: &Shared, version: Option<&str>) {
    update_common::open_release_page(st, version, super::sys::open_url);
}

/// Releases 列表頁：下拉的「Download from Releases」走這裡，
/// 讓使用者自己挑版本換檔案。這條路不下載、不碰自己這顆 bundle。
pub fn open_releases_page(st: &Shared) {
    update_common::open_releases_page(st, super::sys::open_url);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::platform::update_common::{
        release_url, LATEST_JSON, LATEST_RELEASE_PAGE, RELEASES_PAGE, RELEASE_TAG_PREFIX,
        USER_AGENT,
    };

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

    /// **App Translocation 的影本不算「能就地更新」**，即使它每一層路徑都長得
    /// 像一顆合格的 `.app`。
    ///
    /// 這是使用者從 dmg 視窗或 `~/Downloads` 直接雙擊時實際會走到的路徑：外掛的
    /// 第一步 `rename` 在那個唯讀掛載點上拿到的是 `EROFS`（不是 `PermissionDenied`，
    /// 所以連提權退路都不會走），而在這道閘補上之前，前端顯示的是「Update to vX」
    /// ——使用者要等完一次十幾 MB 的下載才收到一句看不懂的 os error。
    #[test]
    fn a_translocated_copy_cannot_update_itself() {
        let translocated = PathBuf::from(
            "/private/var/folders/9x/abc/T/AppTranslocation/8B1F-4/d/Traytunnel.app/Contents/MacOS/traytunnel",
        );
        assert!(bundle_of(&translocated).is_some(), "前提：它每一層都長得像一顆 bundle");
        assert!(
            !can_update_in_place(&translocated),
            "translocated 影本在唯讀掛載點上，替換 bundle 必定死在 EROFS"
        );

        // 正常安裝的那一份不受影響
        assert!(can_update_in_place(&exe_in("/Applications/traytunnel.app")));
        // 非 bundle 照舊不算
        assert!(!can_update_in_place(Path::new("/repo/src-tauri/target/debug/traytunnel")));
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

    // `is_newer` 本身的測試（嚴格大於、"0.10.0" vs "0.9.0"、v 前綴、pre-release、
    // 解析失敗的退讓方向……）兩平台逐字相同，已搬到 [`update_common`] 的測試裡，
    // 不在這裡重複一份。

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

    // `release_url` 的網址組法、`RELEASES_PAGE` 的值同樣兩平台逐字相同，測試也
    // 已搬到 [`update_common`]，這裡不重複。

    /// 這條路上的每一個網址都必須是 https：`sys::open_url` 只放行 https，
    /// 組出一個別的 scheme 的話按下去只會在活動日誌留一行「could not open」
    #[test]
    fn every_page_this_module_opens_is_https() {
        for url in [RELEASES_PAGE, LATEST_RELEASE_PAGE, &release_url(Some("0.6.0"))] {
            assert!(url.starts_with("https://"), "{url}");
        }
        assert!(RELEASE_TAG_PREFIX.starts_with("https://"));
    }

    // live 測試拉的那份 latest.json，必須就是出貨設定裡 updater 外掛會去拿的
    // 那一份——這件事與 Windows 逐字相同，已上提到
    // `update_common::tests::the_shipped_updater_endpoint_matches_latest_json`，
    // 不在這裡重複一份。

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
