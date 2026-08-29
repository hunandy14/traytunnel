//! 受監督行程群組的登記簿，以及啟動時的「收屍」。
//!
//! ## 為什麼需要這一份
//!
//! Windows 那邊 [`super::super::windows::spawn::ProcessSupervisor`] 靠的是 Job
//! Object 的 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`：那是**核心**的保證——handle
//! 一關（正常退出、當掉、被工作管理員結束、登出，全都算）核心就把整個 job 收掉，
//! 使用者空間一行程式碼都不必跑。
//!
//! macOS 沒有等價物。[`super::spawn::ProcessSupervisor`] 用的是行程群組
//! （`process_group(0)` ＋ Drop 時 `kill(-pgid, SIGKILL)`），而 **Drop 是使用者
//! 空間的程式碼**：行程被 `SIGKILL`、當掉（panic=abort／EXC_BAD_ACCESS）、被
//! `kill <pid>` 的預設 SIGTERM 帶走時，它一次都不會跑。偏偏 `ssh -N` 是這世上最
//! 不容易自己死掉的東西之一——stdin 是 `Stdio::null()`（不會讀到 EOF）、父行程
//! 死了也只是被 launchd 收養、`SIGPIPE` 被忽略——於是它會繼續握著 `-L` 的本地埠
//! 不放。下一次啟動時每一列都撞上 `PORT_BUSY`，而重連迴圈是無退避無上限的
//! 五秒一輪，使用者看到的是「整個 app 永遠連不上，重開機才好」。
//!
//! 三道防線裡，signal handler（`SIGTERM`／`SIGHUP`／`SIGINT`，見
//! [`super::spawn::install_termination_handler`]）與 tauri 的 `RunEvent::Exit`
//! 掛鉤（Dock 的 Quit、登出的 AppleEvent）能涵蓋所有「還有機會跑程式碼」的退出
//! 路徑。剩下 `SIGKILL` 與真正的當機是**沒有任何辦法**在當下補救的——那正是這
//! 一份登記簿存在的理由：把 pgid 寫在磁碟上，下一次啟動時回頭把上一輪的殘骸清掉。
//!
//! ## 誤殺的防線
//!
//! pid（也就是 pgid）會回收再用。單看「這個 pgid 還活著」就送 `SIGKILL`，等於
//! 拿一個上一輪留下的數字去砍今天不相干的行程群組。所以每一筆登記除了 pgid 還
//! 記下 spawn 當下那一行**完整命令列**，收屍前先用 `ps` 問一次那個群組裡現在
//! 有誰、命令列對不對得上（[`commands_match`]），對不上就整筆跳過。
//!
//! 原則是**寧可漏殺不誤殺**：漏殺的代價是使用者手動 `kill` 一次（或那個埠這次
//! 連不上），誤殺的代價是砍掉別人的程式，兩者不對等。
//!
//! 另外還有一道「上一個主人還活著就整輪不掃」的閘（[`decide_sweep`] 的
//! `owner_alive`）。正常情況下 single-instance 外掛保證同時只有一個實例——第二
//! 個實例在 `Builder::build()` 裡就 `process::exit(0)` 了，根本走不到呼叫這裡的
//! `setup` 閉包——但那顆外掛在少數錯誤分支（socket 回了 `NotFound`／
//! `ConnectionRefused` 以外的錯）會選擇「照常啟動」，這時兩個實例並存，掃下去
//! 就會把還在服役的那一份 ssh 砍掉。記一個 `ownerPid` 就能擋掉這種情況。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// 登記簿放 `~/Library/Application Support/<identifier>/supervised-pgids.json`。
///
/// identifier 直接寫死而不是走 `app.path().app_local_data_dir()`，理由與
/// Windows 那邊的 `update::staging_dir` 一樣：這一層完全沒有 `AppHandle`
/// （`ProcessSupervisor::spawn` 拿到的只有一個 `Command`），而這個字串本來就
/// 是 `tauri.conf.json` 的 `identifier`，兩邊不一致的話 CI 也抓不到——所以
/// [`the_identifier_matches_tauri_conf`] 直接把 tauri.conf.json 讀進來比對。
const IDENTIFIER: &str = "com.traytunnel.desktop";

/// 登記簿檔名。
const FILE_NAME: &str = "supervised-pgids.json";

/// 原子寫入用的暫存檔名（同一個資料夾，`rename` 才保證是原子的）。
const TMP_FILE_NAME: &str = "supervised-pgids.json.tmp";

/// 讀改寫的整段互斥。
///
/// `register`／`unregister` 會被多條 tokio 工作執行緒同時呼叫（每個出口一條
/// 監看迴圈），而每一次都是「讀整份檔案→改一筆→寫回去」。沒有這把鎖的話兩次
/// 並行的 `register` 會各自讀到同一份舊內容，後寫的那一份把前一筆蓋掉——被蓋掉
/// 的那個 pgid 就再也不會被收屍。
///
/// 檔案層級的鎖（`flock`）在這裡沒有意義：同一時間只會有一個實例（single-instance
/// 外掛），要防的是行程**內部**的並行。
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

/// 一筆登記：一個行程群組，加上「它應該長什麼樣」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Entry {
    /// 行程群組 id（等於群組領袖的 pid，見 `process_group(0)` 的定義）
    pub pgid: i32,
    /// spawn 當下那一行完整命令列：program 與每個 argv 用單一空白接起來。
    /// 格式刻意對齊 `ps -o command=` 的輸出，收屍時才比對得起來。
    pub command: String,
}

/// 登記簿的整份內容。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Registry {
    /// 寫這份檔案的 app 行程 pid。收屍前用它確認「上一個主人真的已經不在了」。
    pub owner_pid: i32,
    pub entries: Vec<Entry>,
}

// ---------------------------------------------------------------- 純邏輯（可測）

/// 壞掉／空的登記簿一律當成空的，不要讓一份手滑改壞的 JSON 變成啟動失敗。
/// 這份檔案是純粹的自我修復輔助，讀不懂就當上一輪沒留下東西。
pub(super) fn parse(contents: &str) -> Registry {
    serde_json::from_str(contents).unwrap_or_default()
}

pub(super) fn to_json(reg: &Registry) -> String {
    // 這份檔案偶爾要靠人眼查（「上次到底留了什麼」），排版一下不花什麼成本
    serde_json::to_string_pretty(reg).unwrap_or_else(|_| "{}".into())
}

/// 登記的命令列與 `ps` 現在報回來的命令列算不算同一件事。
///
/// 判定是「其中一邊是另一邊的前綴」而不是嚴格相等：`ps` 在極端長的命令列上
/// 可能截斷，某些程式也會在啟動後改寫自己的 argv 尾巴。前綴關係已經足夠嚴格
/// ——登記的那一行從 `ssh` 一路到 `user@host` 都在裡面，一個不相干的行程要
/// 湊出這段前綴實務上等於不可能。
///
/// 空字串一律不算相符：`ps` 問不到東西（行程已經不在）會回空字串，那時候
/// 什麼都不該殺。
pub(super) fn commands_match(recorded: &str, actual: &str) -> bool {
    let recorded = recorded.trim();
    let actual = actual.trim();
    if recorded.is_empty() || actual.is_empty() {
        return false;
    }
    recorded.starts_with(actual) || actual.starts_with(recorded)
}

/// 收屍的決策：回傳「該送 SIGKILL 的 pgid」。
///
/// 抽成純函式（群組裡有誰由呼叫端查好傳進來）才測得到——真的去 `ps` 一顆
/// 系統上的行程沒辦法在單元測試裡穩定重現。
///
/// * `owner_alive` 為 true 時一律回空：上一份登記簿的主人還在跑，那些 pgid
///   是**現役**的，不是殘骸。
/// * 群組裡只要有**任何一支**行程的命令列對得上就殺整組。用「任何一支」而不是
///   「群組領袖」，是因為領袖（ssh）可能已經自己退掉、只剩 ProxyCommand 生出來
///   的孫程序還在——那一樣是要清掉的殘骸。
pub(super) fn decide_sweep(
    reg: &Registry,
    owner_alive: bool,
    group_commands: &mut dyn FnMut(i32) -> Vec<String>,
) -> Vec<i32> {
    if owner_alive {
        return Vec::new();
    }
    let mut doomed = Vec::new();
    for entry in &reg.entries {
        // 與 `spawn` 的 Drop 同一道下界保護：`kill(-0)` 是「殺自己這一組」，
        // `kill(-1)` 是「殺所有殺得動的東西」。這裡吃的是磁碟上的檔案內容
        // （比子程序回報的 pid 更不可信），更不能只靠「理論上不會是 0 或 1」。
        if entry.pgid <= 1 {
            continue;
        }
        if group_commands(entry.pgid).iter().any(|actual| commands_match(&entry.command, actual)) {
            doomed.push(entry.pgid);
        }
    }
    doomed
}

/// 一行命令列：program 與每個 argv 用單一空白接起來，對齊 `ps -o command=`。
pub(super) fn command_line(cmd: &std::process::Command) -> String {
    let mut line = cmd.get_program().to_string_lossy().into_owned();
    for arg in cmd.get_args() {
        line.push(' ');
        line.push_str(&arg.to_string_lossy());
    }
    line
}

// ---------------------------------------------------------------- 檔案 I/O

/// 登記簿的完整路徑。
///
/// **`cfg(test)` 下一律回 `None`**（＝整個登記簿功能在預設測試輪裡是 no-op）。
/// `platform::process_tests` 會真的建 `ProcessSupervisor` 並 spawn 一支 `sleep`，
/// 若照常寫檔就等於預設 `cargo test` 會去動使用者真的
/// `~/Library/Application Support`——本專案明文禁止（同一條規則也管著
/// `sys.rs` 的 `~/Library/LaunchAgents`）。登記簿本身的行為改用底下
/// `*_at(path)` 系列直接打 tempdir 測，涵蓋率不因此打折。
fn registry_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(super::paths::home_dir()?.join("Library").join("Application Support").join(IDENTIFIER))
        .map(|dir| dir.join(FILE_NAME))
}

fn read_at(path: &Path) -> Registry {
    match std::fs::read_to_string(path) {
        Ok(contents) => parse(&contents),
        Err(_) => Registry::default(),
    }
}

/// 先寫暫存檔再 `rename` 蓋上去：`rename(2)` 在同一個檔案系統上是原子的，
/// 於是「寫到一半被 SIGKILL 打斷」留下的是完整的舊版，不是半截的新版。
/// 半截的 JSON 雖然 [`parse`] 讀不懂會退成空的（不會炸），但那等於整份登記
/// 一次全丟，比舊版差得多。
fn write_at(path: &Path, reg: &Registry) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_file_name(TMP_FILE_NAME);
    std::fs::write(&tmp, to_json(reg))?;
    std::fs::rename(&tmp, path)
}

pub(super) fn register_at(path: &Path, owner_pid: i32, pgid: i32, command: &str) -> io::Result<()> {
    let mut reg = read_at(path);
    reg.owner_pid = owner_pid;
    reg.entries.retain(|e| e.pgid != pgid);
    reg.entries.push(Entry { pgid, command: command.to_string() });
    write_at(path, &reg)
}

/// 拿掉一筆。最後一筆被拿掉時整份檔案刪掉，不留一份空殼在使用者的資料夾裡。
pub(super) fn unregister_at(path: &Path, pgid: i32) -> io::Result<()> {
    let mut reg = read_at(path);
    let before = reg.entries.len();
    reg.entries.retain(|e| e.pgid != pgid);
    if reg.entries.len() == before {
        return Ok(());
    }
    if reg.entries.is_empty() {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
    }
    write_at(path, &reg)
}

// ---------------------------------------------------------------- 系統查詢

/// 對一整個行程群組送 `SIGKILL`。
///
/// 群組裡已經沒有任何行程（`ESRCH`）當正常收尾看待，不是錯誤——「殺不到東西」
/// 在這兩個呼叫端（Drop 與收屍）都正是想要的結果。
pub(super) fn kill_group(pgid: i32) {
    // 下界保護見 `decide_sweep` 的同一段說明。這裡再擋一次是因為 Drop 那條路
    // 不經過 `decide_sweep`。
    if pgid <= 1 {
        return;
    }
    let rc = unsafe { libc::kill(-pgid, libc::SIGKILL) };
    if rc != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            log::warn!("kill process group {pgid} failed: {err}");
        }
    }
}

/// 問 `ps` 這個行程群組裡現在有哪些行程，回它們各自的命令列。
///
/// `-g` 收的是行程群組 id，`-ww` 關掉「照終端機寬度截斷」的預設行為（截斷會
/// 讓命令列比對無謂地失手）。用 `ps` 而不是手刻 `sysctl KERN_PROCARGS2`：後者
/// 要解一份 Apple 沒有公開穩定文件的核心緩衝格式，還得整段 unsafe，而這條路
/// 一輩子只在啟動時跑一次、每個殘留 pgid 一次，完全不在乎那一次 fork/exec。
fn group_commands(pgid: i32) -> Vec<String> {
    let out = std::process::Command::new("ps")
        .args(["-ww", "-o", "command=", "-g"])
        .arg(pgid.to_string())
        .output();
    match out {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        Err(e) => {
            log::warn!("could not ask ps about process group {pgid}: {e}");
            Vec::new()
        }
    }
}

/// 上一份登記簿的主人是不是還在跑。
///
/// 兩道都要成立才算：pid 還活著（`kill(pid, 0)`），而且那個 pid 的命令列開頭
/// 真的是我們自己這支執行檔——只看 pid 活不活著會被 pid 回收騙過去。
fn owner_still_running(owner_pid: i32) -> bool {
    if owner_pid <= 1 || owner_pid == std::process::id() as i32 {
        return false;
    }
    if unsafe { libc::kill(owner_pid, 0) } != 0 {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        // 問不到自己是誰的話，寧可保守：pid 還活著就當主人還在，不要掃
        return true;
    };
    let out = std::process::Command::new("ps")
        .args(["-ww", "-o", "command=", "-p"])
        .arg(owner_pid.to_string())
        .output();
    match out {
        Ok(out) => {
            let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
            line.starts_with(&exe.to_string_lossy().into_owned())
        }
        Err(_) => true,
    }
}

// ---------------------------------------------------------------- 對外

/// 記下一個剛 spawn 出來的行程群組。失敗只 warn：程序已經起來了，
/// 讓它跑總比為了收尾機制炸掉呼叫端好（與 `spawn` 那邊「記不到 pgid」同一種取捨）。
pub(super) fn register(pgid: i32, command: &str) {
    let Some(path) = registry_path() else { return };
    let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(e) = register_at(&path, std::process::id() as i32, pgid, command) {
        log::warn!("could not record supervised process group {pgid}: {e}");
    }
}

/// 正常收尾時把一筆拿掉。
pub(super) fn unregister(pgid: i32) {
    let Some(path) = registry_path() else { return };
    let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(e) = unregister_at(&path, pgid) {
        log::warn!("could not clear supervised process group {pgid}: {e}");
    }
}

/// 啟動時收屍：把上一輪被 `SIGKILL`／當機帶走、卻還活著的 ssh 程序樹清掉。
///
/// **呼叫時機有硬性要求**：必須在 single-instance 外掛已經確定我們就是唯一實例
/// 之後（也就是 `Builder::setup` 閉包裡，見 `lib.rs`），而且要在任何一條隧道
/// spawn 之前——前者避免砍到另一個實例現役的連線，後者才來得及把埠讓出來。
pub fn sweep_leftovers() {
    let Some(path) = registry_path() else { return };
    let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !path.is_file() {
        return;
    }
    let reg = read_at(&path);
    if reg.entries.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let owner_alive = owner_still_running(reg.owner_pid);
    let doomed = decide_sweep(&reg, owner_alive, &mut group_commands);
    if owner_alive {
        log::warn!(
            "supervised-pgids.json still belongs to a live instance (pid {}), leaving it alone",
            reg.owner_pid
        );
        return;
    }
    for pgid in &doomed {
        log::warn!(
            "killing a process group left behind by a previous run (pgid {pgid}); the last run \
             did not get to clean up (SIGKILL or a crash)"
        );
        kill_group(*pgid);
    }
    if doomed.len() < reg.entries.len() {
        log::info!(
            "{} of {} recorded process group(s) were already gone (or no longer match what we \
             spawned) and were left alone",
            reg.entries.len() - doomed.len(),
            reg.entries.len()
        );
    }
    // 不論殺了幾個，這一份都不再有意義：留著只會讓下一次啟動重問一次同樣的
    // 死 pgid。真正還在跑的東西會由這一輪的 `register` 重新寫進來。
    let _ = std::fs::remove_file(&path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("traytunnel-test-pgids-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("要建得起 tempdir");
        dir
    }

    /// 寫出去、讀回來要是同一份；壞掉的內容一律退成空的而不是炸掉。
    #[test]
    fn the_registry_round_trips_and_tolerates_garbage() {
        let reg = Registry {
            owner_pid: 4242,
            entries: vec![
                Entry {
                    pgid: 111,
                    command: "ssh -N -L 1080:127.0.0.1:1080 bob@example.com".into(),
                },
                Entry { pgid: 222, command: "ssh -N bob@other".into() },
            ],
        };
        assert_eq!(parse(&to_json(&reg)), reg);

        assert_eq!(parse("not json at all"), Registry::default());
        assert_eq!(parse(""), Registry::default());
        // 缺欄位不該讓整份讀不回來（`#[serde(default)]`）
        assert_eq!(parse("{}"), Registry::default());
        assert_eq!(parse(r#"{"ownerPid":7}"#), Registry { owner_pid: 7, entries: vec![] });
    }

    /// 登記／退登記走一輪真的檔案（tempdir）：新增、去重、移除、最後一筆移除
    /// 之後整份檔案要消失。
    #[test]
    fn register_and_unregister_round_trip_in_a_tempdir() {
        let dir = tempdir("roundtrip");
        let path = dir.join(FILE_NAME);

        register_at(&path, 900, 111, "ssh -N bob@a").expect("第一筆要寫得進去");
        register_at(&path, 900, 222, "ssh -N bob@b").expect("第二筆要寫得進去");
        let reg = read_at(&path);
        assert_eq!(reg.owner_pid, 900);
        assert_eq!(reg.entries.len(), 2);

        // 同一個 pgid 再登記一次是取代，不是長出第二筆
        register_at(&path, 900, 111, "ssh -N bob@a2").expect("重登記要成功");
        let reg = read_at(&path);
        assert_eq!(reg.entries.len(), 2);
        assert_eq!(
            reg.entries.iter().find(|e| e.pgid == 111).map(|e| e.command.as_str()),
            Some("ssh -N bob@a2")
        );

        // 沒登記過的 pgid 退登記是 no-op，不算錯
        unregister_at(&path, 999).expect("退登記不存在的 pgid 不算錯");
        assert_eq!(read_at(&path).entries.len(), 2);

        unregister_at(&path, 111).expect("退登記要成功");
        assert_eq!(read_at(&path).entries.iter().map(|e| e.pgid).collect::<Vec<_>>(), vec![222]);

        unregister_at(&path, 222).expect("最後一筆退登記要成功");
        assert!(!path.exists(), "最後一筆走了就不該再留一份空殼：{}", path.display());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 讀一個根本不存在的檔案回空的登記簿，不是錯誤。
    #[test]
    fn reading_a_missing_registry_yields_an_empty_one() {
        let dir = tempdir("missing");
        assert_eq!(read_at(&dir.join(FILE_NAME)), Registry::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 命令列比對：前綴關係算相符，空字串一律不算，不相干的命令不算。
    #[test]
    fn command_matching_is_prefix_based_and_never_matches_nothing() {
        let recorded = "ssh -N -o ExitOnForwardFailure=yes -L 1080:127.0.0.1:1080 bob@example.com";
        assert!(commands_match(recorded, recorded));
        // ps 截斷了尾巴
        assert!(commands_match(recorded, "ssh -N -o ExitOnForwardFailure=yes -L 1080"));
        // 兩邊各自帶了一點空白
        assert!(commands_match(recorded, &format!("  {recorded}  ")));

        assert!(!commands_match(recorded, ""));
        assert!(!commands_match("", recorded));
        assert!(!commands_match(recorded, "/usr/sbin/cupsd -l"));
        // 只有程式名相同不算：pid 回收之後撞到另一條 ssh 是最容易誤殺的情境
        assert!(!commands_match(recorded, "ssh alice@somewhere-else"));
    }

    /// 收屍決策：命令列對得上才殺，對不上或群組已空一律留著。
    #[test]
    fn the_sweep_only_kills_groups_that_still_look_like_what_we_spawned() {
        let reg = Registry {
            owner_pid: 900,
            entries: vec![
                Entry { pgid: 111, command: "ssh -N bob@a".into() },
                Entry { pgid: 222, command: "ssh -N bob@b".into() },
                Entry { pgid: 333, command: "ssh -N bob@c".into() },
            ],
        };
        let mut ps = |pgid: i32| match pgid {
            // 領袖還在，命令列一模一樣 → 殺
            111 => vec!["ssh -N bob@a".to_string()],
            // 群組已經空了 → 留著
            222 => vec![],
            // pgid 被回收，現在是別人的群組 → 絕對不能殺
            333 => vec!["/usr/libexec/some-daemon --serve".to_string()],
            _ => unreachable!("測試只問這三個 pgid"),
        };
        assert_eq!(decide_sweep(&reg, false, &mut ps), vec![111]);
    }

    /// 領袖（ssh）已經自己退掉、只剩 ProxyCommand 生的孫程序還在——那一樣是
    /// 要清掉的殘骸，判定是「群組裡任何一支對得上」而不是「領袖對得上」。
    #[test]
    fn the_sweep_still_fires_when_only_a_grandchild_is_left() {
        let reg = Registry {
            owner_pid: 900,
            entries: vec![Entry { pgid: 111, command: "ssh -N bob@a".into() }],
        };
        let mut ps = |_: i32| {
            vec!["cloudflared access ssh --hostname a".to_string(), "ssh -N bob@a".to_string()]
        };
        assert_eq!(decide_sweep(&reg, false, &mut ps), vec![111]);
    }

    /// 上一個主人還活著就整輪不掃：那些 pgid 是現役的連線，不是殘骸。
    #[test]
    fn a_live_owner_stops_the_sweep_entirely() {
        let reg = Registry {
            owner_pid: 900,
            entries: vec![Entry { pgid: 111, command: "ssh -N bob@a".into() }],
        };
        let mut ps = |_: i32| vec!["ssh -N bob@a".to_string()];
        assert!(decide_sweep(&reg, true, &mut ps).is_empty());
    }

    /// `kill(-0)`／`kill(-1)` 是完全不同的廣播範圍（殺自己這一組／殺所有殺得動
    /// 的東西），登記簿是磁碟上的檔案、內容比子程序回報的 pid 更不可信，
    /// 這道下界保護一定要在。
    #[test]
    fn nonsense_pgids_are_never_selected() {
        let reg = Registry {
            owner_pid: 900,
            entries: vec![
                Entry { pgid: 0, command: "ssh -N bob@a".into() },
                Entry { pgid: 1, command: "ssh -N bob@a".into() },
                Entry { pgid: -5, command: "ssh -N bob@a".into() },
            ],
        };
        let mut ps = |_: i32| panic!("下界保護要在問 ps 之前就擋掉");
        assert!(decide_sweep(&reg, false, &mut ps).is_empty());
    }

    /// 登記下來的命令列格式必須就是 `ps -o command=` 的格式（program 與 argv
    /// 用單一空白接起來），不然收屍時永遠比對不到。
    #[test]
    fn the_command_line_is_rendered_the_way_ps_renders_it() {
        let mut cmd = std::process::Command::new("ssh");
        cmd.args(["-N", "-o", "ProxyCommand=cloudflared access ssh", "bob@example.com"]);
        assert_eq!(
            command_line(&cmd),
            "ssh -N -o ProxyCommand=cloudflared access ssh bob@example.com"
        );

        // 沒有參數的情況也要是乾淨的一個 token，不要留下尾隨空白
        assert_eq!(command_line(&std::process::Command::new("ssh")), "ssh");
    }

    /// 登記簿的資料夾名就是 tauri.conf.json 的 identifier。寫死一份字串是為了
    /// 讓這一層不必有 `AppHandle`，但寫死就有漂掉的風險，所以直接讀那份設定比對。
    #[test]
    fn the_identifier_matches_tauri_conf() {
        let conf =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json"))
                .expect("讀得到 tauri.conf.json");
        let json: serde_json::Value =
            serde_json::from_str(&conf).expect("tauri.conf.json 要是合法 JSON");
        assert_eq!(json["identifier"].as_str(), Some(IDENTIFIER));
    }
}
