//! 已經下載完、等下一次啟動才安裝的那一版：暫存區、就緒標記與相關決策。
//!
//! 這一層刻意**完全不碰 Tauri、不連外、不執行任何東西**：它只認一個資料夾路徑，
//! 讀寫兩個檔案，其餘全是純函式。真正會動到系統的兩件事（下載、把安裝程式起起來）
//! 留在 `update` 本體，於是這裡的每一條規則都測得到，不必真的裝一次。
//!
//! 為什麼要自己做暫存：tauri-plugin-updater 的 `download()` 與 `install()` 是
//! **同一個行程裡用記憶體傳 bytes**（`install(bytes)`），它沒有、也不打算支援
//! 「這次下載、下次啟動再裝」。而 `Update` 物件帶著 http client 與一堆私有欄位，
//! 外面組不出來，所以跨重啟這條路一定得由我們自己接：
//!
//! 1. `download()` 回來的 bytes **已經過 minisign 驗簽**（外掛 updater.rs 的
//!    `verify_signature`，驗不過就是 Err），這一點照用不必重造；
//! 2. 我們把那份 bytes 落地到 `%LOCALAPPDATA%` 底下自管的暫存區，
//!    並把它的 SHA-256 寫進標記；
//! 3. 下一次啟動重算一次雜湊再交棒——落地之後到執行之前這段時間，
//!    檔案是躺在磁碟上的，簽章驗過的是「當時那份 bytes」，不是「現在這顆檔案」。
//!
//! 暫存區放 `%LOCALAPPDATA%` 而不是 `%TEMP%` 有兩個理由：暫存區必須撐過一次
//! 重開機才有意義（`%TEMP%` 會被清），而且從 `%TEMP%` 執行 exe 是防毒啟發式
//! 重點盯防的行為，自管目錄的觀感好得多。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::is_newer;

/// 就緒標記。開機時只讀這一份就知道要不要交棒，不必去碰十幾 MB 的安裝檔
const MARKER_FILE: &str = "pending-update.json";

/// 下載回來的安裝檔。固定檔名（不帶版本號）是刻意的：暫存區同一時間只會有一份，
/// 換版本時直接覆蓋，不會在使用者的 AppData 底下積出一疊過期的安裝檔
const INSTALLER_FILE: &str = "pending-update-setup.exe";

/// 同一版最多嘗試交棒幾次。
///
/// 安裝程式起得來、卻沒有真的把版本換掉時（被防毒攔下、磁碟滿、使用者在
/// UAC 上按取消……），下一次啟動看到的還是同一份標記，於是又交棒一次——
/// 沒有上限的話這就是一個每次開機都跑一次安裝程式的迴圈。三次之後放棄這一版，
/// 把暫存清乾淨回到「沒有待安裝更新」的狀態，等下一次排程重新下載。
pub const MAX_ATTEMPTS: u32 = 3;

/// 就緒標記的內容
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    /// 這份安裝檔會裝出來的版本，不帶 v
    pub version: String,
    /// 安裝檔的完整路徑
    pub installer: PathBuf,
    /// 安裝檔的 SHA-256（小寫十六進位）。交棒前一定要重算一次比對
    pub sha256: String,
    /// 已經交棒過幾次。每次要起安裝程式之前先加一再寫回去，
    /// 這樣就算安裝程式把我們殺掉，計數也已經落地了
    #[serde(default)]
    pub attempts: u32,
}

/// 版本號的比較用形式：去空白、去前導的 v
fn normalize(version: &str) -> &str {
    version.trim().trim_start_matches(['v', 'V'])
}

/// 一份 bytes 的 SHA-256，小寫十六進位
pub fn digest(bytes: &[u8]) -> String {
    let out = Sha256::digest(bytes);
    out.iter().map(|b| format!("{b:02x}")).collect()
}

/// 把下載好的安裝檔存進暫存區並寫下就緒標記。
///
/// 寫入順序是規格的一部分：**安裝檔先寫、標記最後寫**。中途斷電時最壞的結果是
/// 留下一顆沒人認領的安裝檔（下一次 `stage` 直接覆蓋掉），而不是留下一個
/// 指著半截檔案的標記。半截檔案就算真的被認領，雜湊那一關也還會擋一次。
pub fn stage(dir: &Path, version: &str, bytes: &[u8]) -> std::io::Result<Pending> {
    std::fs::create_dir_all(dir)?;
    let installer = dir.join(INSTALLER_FILE);
    std::fs::write(&installer, bytes)?;
    let pending = Pending {
        version: normalize(version).to_string(),
        installer,
        sha256: digest(bytes),
        attempts: 0,
    };
    write_marker(dir, &pending)?;
    Ok(pending)
}

fn write_marker(dir: &Path, pending: &Pending) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(pending).map_err(std::io::Error::other)?;
    std::fs::write(dir.join(MARKER_FILE), json)
}

/// 記下「又要交棒一次了」。回傳這一次是第幾次。
///
/// 一定要在真的起安裝程式**之前**寫回磁碟：安裝程式會把我們關掉，
/// 之後就沒有機會再寫任何東西了。
pub fn note_attempt(dir: &Path, pending: &Pending) -> std::io::Result<u32> {
    let next = pending.attempts.saturating_add(1);
    write_marker(dir, &Pending { attempts: next, ..pending.clone() })?;
    Ok(next)
}

/// 讀就緒標記。標記不在、解析不出來、安裝檔不見了，一律當成「沒有待安裝的更新」。
///
/// **不在這裡驗雜湊**：算十幾 MB 的 SHA-256 要花時間，而絕大多數啟動都是
/// 「根本沒有標記」那一條路。雜湊留到真的決定要交棒的那一刻才算（見 [`verify`]）。
pub fn read(dir: &Path) -> Option<Pending> {
    let raw = std::fs::read(dir.join(MARKER_FILE)).ok()?;
    let pending: Pending = serde_json::from_slice(&raw).ok()?;
    if pending.version.is_empty() || pending.sha256.is_empty() {
        return None;
    }
    if !pending.installer.is_file() {
        return None;
    }
    Some(pending)
}

/// 磁碟上那顆安裝檔是不是還是下載當時驗過簽章的那一份。
///
/// 簽章驗的是「下載回來的那串 bytes」，而我們要執行的是「現在躺在磁碟上的檔案」
/// ——中間隔著一次重開機。寫到一半的殘檔、被別的程式改過的檔案，都在這一關被擋下，
/// 不需要為它們各自設一種狀態。
pub fn verify(pending: &Pending) -> bool {
    match std::fs::read(&pending.installer) {
        Ok(bytes) => digest(&bytes) == pending.sha256.trim().to_lowercase(),
        Err(_) => false,
    }
}

/// 清掉整個暫存區。
///
/// 標記先刪：只要它不在，殘留的安裝檔就已經不會被任何人拿去執行，
/// 就算刪檔那一步失敗（檔案正被防毒掃著之類）也不會留下一個會被誤用的狀態。
pub fn clear(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(MARKER_FILE));
    let _ = std::fs::remove_file(dir.join(INSTALLER_FILE));
}

/// 開機時看到一份標記，該拿它怎麼辦
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    /// 沒有標記，照常啟動
    Nothing,
    /// 標記那一版比正在跑的新：交棒給安裝程式
    Install,
    /// 標記那一版就是正在跑的這一版：上一次更新成功了，記一行再把暫存清掉
    Done,
    /// 標記那一版不比正在跑的新，而且也不是同一版：清掉暫存，什麼都不裝
    Stale,
    /// 同一版已經試過太多次：放棄它，清掉暫存
    GaveUp,
}

/// 開機那一刻的決策。降版保護在這裡：**只有嚴格大於目前這一版才裝**。
///
/// 版本號解析不出來時 [`is_newer`] 一律回 false，於是落到 `Stale` 被清掉
/// ——一份看不懂版本號的標記絕不會被執行。
pub fn apply_action(pending: Option<&Pending>, current: &str) -> Apply {
    let Some(p) = pending else {
        return Apply::Nothing;
    };
    if normalize(&p.version) == normalize(current) {
        return Apply::Done;
    }
    if !is_newer(&p.version, current) {
        return Apply::Stale;
    }
    if p.attempts >= MAX_ATTEMPTS {
        return Apply::GaveUp;
    }
    Apply::Install
}

/// 這一版要不要下載。
///
/// 暫存區裡躺著的就是同一版時不再下載一次（同版去重）；躺著的是別的版本時
/// 照下不誤，新的那一份會直接覆蓋掉舊的（有更新的版本出現就換掉舊 pending）。
pub fn should_download(version: &str, pending: Option<&Pending>) -> bool {
    !matches!(pending, Some(p) if normalize(&p.version) == normalize(version))
}

/// 下載連續失敗這麼多次之後，下一次要隔多久再試。
///
/// 第一次失敗等 15 分鐘，之後每次加倍，上限就是常規的一天。退避要處理的是
/// 「網路壞掉時不要每隔幾分鐘去敲一次 GitHub」，而不是「壞一次就放棄到明天」
/// ——使用者的網路多半幾分鐘就回來了。
pub fn retry_delay(failures: u32) -> Duration {
    const BASE_SECS: u64 = 15 * 60;
    const CAP_SECS: u64 = 24 * 60 * 60;
    if failures == 0 {
        return Duration::ZERO;
    }
    let factor = 1u64.checked_shl(failures - 1).unwrap_or(u64::MAX);
    Duration::from_secs(BASE_SECS.saturating_mul(factor).min(CAP_SECS))
}

/// 交棒給暫存的 NSIS 安裝程式時要帶的參數。
///
/// 這一串照 tauri-plugin-updater 2.10.1 自己組的那一份抄（updater.rs 的
/// `install_inner`，NSIS 分支）：`installMode` 是 quiet 時它給 `/S /R`，
/// 後面固定接 `/UPDATE`，再用 `/ARGS` 把要交給重啟後那支程式的參數接上去。
///
/// * `/S`      完全靜默（NSIS 內建），連進度條都不出現
/// * `/R`      裝完由安裝程式把程式重新啟動起來
/// * `/UPDATE` 告訴 NSIS 模板這是更新，不是全新安裝（模板的 `.onInit` 讀它）
/// * `/ARGS`   之後的字串原樣交給重新啟動的那支程式
///
/// 後兩者的語意在 tauri-bundler 的 `installer.nsi` 裡看得很清楚：
///
/// ```nsis
/// Function .onInstSuccess
///   ${If} $PassiveMode = 1
///   ${OrIf} ${Silent}
///     ${GetOptions} $CMDLINE "/R" $R0
///     ${IfNot} ${Errors}
///       ${GetOptions} $CMDLINE "/ARGS" $R0
///       nsis_tauri_utils::RunAsUser "$INSTDIR\${MAINBINARYNAME}.exe" "$R0"
///     ${EndIf}
///   ${EndIf}
/// FunctionEnd
/// ```
///
/// 也就是說：`/R` 只在靜默或 passive 安裝下才被看一眼（GUI 安裝有自己的
/// 「安裝完成後啟動」勾選框），而 `/ARGS` 之後那一段會**整段**當成重啟時的
/// 命令列參數。`/ARGS` 因此一定要放在最後。
///
/// `tray` 為真才補 `--tray`，而且沒有東西要傳時連 `/ARGS` 都不放——`GetOptions`
/// 對一個沒有值的 `/ARGS` 會回空字串，那是「重啟時帶一個空參數」而不是「不帶參數」。
///
/// 這個布林必須跟著**這一次啟動的樣子**走：使用者自己雙擊開起來的，更新完就該
/// 看到視窗；開機自啟（`--tray`）進來的，更新完當然也不該彈窗（三件套之 3）。
pub fn installer_args(tray: bool) -> Vec<&'static str> {
    let mut args = vec!["/S", "/R", "/UPDATE"];
    if tray {
        args.extend(["/ARGS", "--tray"]);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(version: &str, attempts: u32) -> Pending {
        Pending {
            version: version.into(),
            installer: PathBuf::from("C:\\stage\\setup.exe"),
            sha256: "abc".into(),
            attempts,
        }
    }

    /// 測試用的暫存資料夾，名字帶 pid 與行號，互不干擾
    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("traytunnel-staged-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// 落地一次再讀回來，內容要一模一樣——這是跨重啟唯一的傳話管道，
    /// 形狀對不上就等於整條路默默失效
    #[test]
    fn a_staged_update_reads_back_exactly() {
        let dir = scratch("roundtrip");
        let bytes = b"MZ fake installer".to_vec();
        let staged = stage(&dir, "0.7.0", &bytes).expect("暫存要寫得進去");
        assert_eq!(staged.version, "0.7.0");
        assert_eq!(staged.attempts, 0);
        assert_eq!(staged.sha256, digest(&bytes));
        assert_eq!(read(&dir), Some(staged.clone()));
        assert!(verify(&staged), "剛寫下去的檔案雜湊當然要對得上");
        clear(&dir);
        assert_eq!(read(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 標記寫的是帶 v 的版本也照樣收下，存進去的一律是不帶 v 的形式
    #[test]
    fn a_leading_v_never_reaches_the_marker() {
        let dir = scratch("normalize");
        let staged = stage(&dir, " v0.7.0 ", b"x").expect("要寫得進去");
        assert_eq!(staged.version, "0.7.0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 安裝檔不見了（被防毒隔離、被使用者刪掉）時標記等於不存在，
    /// 不可以讓開機那條路拿著一個指向空氣的路徑去 spawn
    #[test]
    fn a_marker_without_its_installer_is_not_a_marker() {
        let dir = scratch("orphan");
        let staged = stage(&dir, "0.7.0", b"x").expect("要寫得進去");
        std::fs::remove_file(&staged.installer).expect("刪得掉");
        assert_eq!(read(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 落地之後被改過的安裝檔一律不准執行。
    ///
    /// 簽章驗的是「下載回來那串 bytes」，這一關驗的是「現在磁碟上那顆檔案」，
    /// 兩者之間隔著一次重開機。寫到一半的殘檔也由同一關擋下，不必另設狀態。
    #[test]
    fn a_tampered_installer_fails_verification() {
        let dir = scratch("tamper");
        let staged = stage(&dir, "0.7.0", b"original bytes").expect("要寫得進去");
        assert!(verify(&staged));
        std::fs::write(&staged.installer, b"something else entirely").expect("覆蓋得掉");
        assert!(!verify(&staged), "內容變了雜湊就該對不上");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 交棒次數要真的落地，否則重開機之後計數永遠是 0，
    /// 「三次就放棄」的保險絲等於不存在
    #[test]
    fn an_attempt_is_persisted_before_the_installer_runs() {
        let dir = scratch("attempts");
        let staged = stage(&dir, "0.7.0", b"x").expect("要寫得進去");
        assert_eq!(note_attempt(&dir, &staged).expect("寫得回去"), 1);
        let again = read(&dir).expect("標記還在");
        assert_eq!(again.attempts, 1);
        assert_eq!(again.sha256, staged.sha256, "計數以外的欄位一個字都不該變");
        assert_eq!(note_attempt(&dir, &again).expect("寫得回去"), 2);
        assert_eq!(read(&dir).expect("標記還在").attempts, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 開機決策的主線：比較新才裝
    #[test]
    fn only_a_newer_staged_version_gets_installed() {
        assert_eq!(apply_action(None, "0.6.1"), Apply::Nothing);
        assert_eq!(apply_action(Some(&pending("0.7.0", 0)), "0.6.1"), Apply::Install);
        assert_eq!(apply_action(Some(&pending("v0.7.0", 0)), "0.6.1"), Apply::Install);
    }

    /// 標記那一版就是正在跑的這一版＝上一次更新成功了：記一行、清掉，不再裝一次
    #[test]
    fn the_version_we_are_already_running_means_the_update_landed() {
        assert_eq!(apply_action(Some(&pending("0.7.0", 0)), "0.7.0"), Apply::Done);
        assert_eq!(apply_action(Some(&pending("v0.7.0", 1)), "0.7.0"), Apply::Done);
    }

    /// 降版保護：標記比現行版本舊就一律不裝。
    ///
    /// 使用者自己去 Releases 抓了更新的版本蓋上去、或暫存區是很久以前留下來的，
    /// 都會走到這裡。把他手上比較新的那一份降回去是這條路最壞的失敗模式。
    #[test]
    fn a_staged_downgrade_is_refused_and_swept_away() {
        assert_eq!(apply_action(Some(&pending("0.5.0", 0)), "0.6.1"), Apply::Stale);
        // 版本號看不懂時同樣不裝——`is_newer` 對垃圾一律回 false
        assert_eq!(apply_action(Some(&pending("latest", 0)), "0.6.1"), Apply::Stale);
        assert_eq!(apply_action(Some(&pending("0.7", 0)), "0.6.1"), Apply::Stale);
    }

    /// 三次交棒都沒把版本換掉就放棄這一版，免得變成每次開機都跑一次安裝程式
    #[test]
    fn the_same_version_is_given_up_on_after_three_attempts() {
        assert_eq!(apply_action(Some(&pending("0.7.0", 2)), "0.6.1"), Apply::Install);
        assert_eq!(apply_action(Some(&pending("0.7.0", MAX_ATTEMPTS)), "0.6.1"), Apply::GaveUp);
        assert_eq!(apply_action(Some(&pending("0.7.0", 99)), "0.6.1"), Apply::GaveUp);
        // 放棄的判斷排在版本判斷之後：已經裝好的那一版永遠是 Done，不管試過幾次
        assert_eq!(apply_action(Some(&pending("0.7.0", 99)), "0.7.0"), Apply::Done);
    }

    /// 同一版不重複下載；換了版本就照下，新的直接蓋掉舊的
    #[test]
    fn the_same_version_is_never_downloaded_twice() {
        let staged = pending("0.7.0", 0);
        assert!(!should_download("0.7.0", Some(&staged)));
        assert!(!should_download("v0.7.0", Some(&staged)));
        assert!(should_download("0.7.1", Some(&staged)));
        assert!(should_download("0.7.0", None));
    }

    /// 退避是加倍的，而且封頂在一天——不會退避到比常規排程還久
    #[test]
    fn the_retry_delay_backs_off_and_stops_at_a_day() {
        assert_eq!(retry_delay(0), Duration::ZERO);
        assert_eq!(retry_delay(1), Duration::from_secs(15 * 60));
        assert_eq!(retry_delay(2), Duration::from_secs(30 * 60));
        assert_eq!(retry_delay(3), Duration::from_secs(60 * 60));
        let day = Duration::from_secs(24 * 60 * 60);
        assert_eq!(retry_delay(10), day);
        // 位移不可以溢位成 panic 或繞回一個很小的值
        assert_eq!(retry_delay(u32::MAX), day);
    }

    /// 安裝程式的參數是這條路上唯一沒辦法在開發期真的跑一次的東西，
    /// 至少把字串本身釘死：少了 /S 會彈出安裝畫面，少了 /R 更新完程式就不見了
    #[test]
    fn the_installer_runs_silently_and_restarts_the_app() {
        assert_eq!(installer_args(false), vec!["/S", "/R", "/UPDATE"]);
        assert_eq!(installer_args(true), vec!["/S", "/R", "/UPDATE", "/ARGS", "--tray"]);
    }
}
