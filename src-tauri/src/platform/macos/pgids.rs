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
//! 連不上），誤殺的代價是砍掉別人的程式，兩者不對等。已知的漏殺各自記在
//! [`plan_sweep`] 與 [`REGISTRY_LOCK`] 的說明裡。
//!
//! 另外還有一道**逐筆**的「這一筆的主人還活著就跳過」（[`Entry::owner_pid`]）。
//! 正常情況下 single-instance 外掛保證同時只有一個實例——第二個實例在
//! `Builder::build()` 裡就 `process::exit(0)` 了，根本走不到呼叫這裡的 `setup`
//! 閉包——但那顆外掛在少數錯誤分支（socket 回了 `NotFound`／`ConnectionRefused`
//! 以外的錯）會選擇「照常啟動」，這時兩個實例並存。那一段誤殺鏈的完整推演，
//! 以及「為什麼 owner 必須逐筆記、不能整份記一個」，寫在 [`Entry::owner_pid`]。

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

/// 讀改寫的整段互斥。
///
/// `register`／`unregister` 會被多條 tokio 工作執行緒同時呼叫（每個出口一條
/// 監看迴圈），而每一次都是「讀整份檔案→改一筆→寫回去」。沒有這把鎖的話兩次
/// 並行的 `register` 會各自讀到同一份舊內容，後寫的那一份把前一筆蓋掉——被蓋掉
/// 的那個 pgid 就再也不會被收屍。
///
/// 這把鎖只管**行程內部**的並行。跨行程（兩個實例同時在跑，見 [`Entry::owner_pid`]
/// 那一段）的並行沒有用檔案鎖（`flock`）擋：那時兩邊會共用同一個暫存檔
/// （[`crate::config::write_atomic`] 的 `<檔名>.tmp`），最壞情況是其中一邊
/// `rename` 過去的是另一邊寫到一半的內容，
/// [`parse`] 讀不懂就退成空的登記簿。後果是**漏殺**（下一次啟動少收幾具屍體），
/// 不是誤殺，方向可接受；為了一個「single-instance 失手才會發生」的情境去背一套
/// 跨行程鎖的複雜度（還得處理鎖檔本身的殘留與死鎖）划不來。
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

/// 一筆登記：一個行程群組、它應該長什麼樣、以及它是誰 spawn 的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Entry {
    /// 行程群組 id（等於群組領袖的 pid，見 `process_group(0)` 的定義）
    pub pgid: i32,
    /// spawn 當下那一行完整命令列：program 與每個 argv 用單一空白接起來。
    /// 格式刻意對齊 `ps -o command=` 的輸出，收屍時才比對得起來。
    pub command: String,
    /// spawn 出這個群組的 app 行程 pid。
    ///
    /// **逐筆記而不是整份記一個**，這是防誤殺的關鍵一環。正常情況下同時只會有
    /// 一個實例（single-instance 外掛），但那顆外掛在「socket 回了 `NotFound`／
    /// `ConnectionRefused` 以外的錯」時會選擇照常啟動，於是 A、B 兩個實例並存。
    /// 整份只記一個 owner 的話會走成這條誤殺鏈：
    ///
    /// 1. B 啟動、掃描時看到 owner 是 A 而 A 還活著，整輪不掃（正確）；
    /// 2. B 接著 `register`，把整份的 owner 覆寫成 B，於是 A 的條目也「改姓」B；
    /// 3. B 正常退出，只退登記自己那幾筆，A 的條目留著、掛在已死的 B 名下；
    /// 4. C 啟動，看到 owner B 已死就開掃，`ps` 一比對——A **現役**的 ssh 命令列
    ///    當然對得上——於是把還在服役的 A 的隧道全部 SIGKILL。
    ///
    /// 逐筆記 owner 之後，第 2 步不會動到別人的條目，第 4 步也會逐筆看
    /// 「這一筆的主人還在不在」，A 的條目因此永遠不會被 C 掃到。
    pub owner_pid: i32,
}

/// 登記簿的整份內容。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Registry {
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
/// 判定只有一個方向：**`ps` 回報的那一行要以登記的那一行開頭**（相等是它的
/// 特例）。登記的那一行從 `ssh` 一路到 `user@host` 全都得出現在 `ps` 的輸出裡，
/// 一個不相干的行程要湊出這段前綴實務上等於不可能。允許尾巴多出東西，是因為
/// 有些程式會在啟動後改寫自己的 argv 尾端。
///
/// **反方向（`recorded.starts_with(actual)`）已經移除**，那是一個真的會誤殺的
/// 缺口：它讓「`ps` 只印得出 `ssh` 這幾個字」的任何行程匹配上我們每一筆登記。
/// 這不是假想——本機實證，一支 argv[0] 被改成 `ssh` 的 `cat`：
///
/// ```text
/// $ ps -ww -o command= -p <pid>
/// ssh
/// recorded.starts_with("ssh") -> true   ← 這個群組會被整組 SIGKILL
/// ```
///
/// pid（＝pgid）會回收，登記簿裡一個舊 pgid 剛好對上今天別人的群組時，這一條
/// 就是「拿一個舊數字去砍不相干的程式」。當初放行反方向的理由是「`ps` 在極端長
/// 的命令列上可能截斷」——但這裡問 `ps` 一律帶 `-ww`（見 [`commands_by_pgid`]），
/// 截斷這件事根本不會發生，等於用一個不存在的問題換來一個真的誤殺面。
///
/// 空字串一律不算相符：`ps` 問不到東西（行程已經不在）會回空字串，那時候
/// 什麼都不該殺。
pub(super) fn commands_match(recorded: &str, actual: &str) -> bool {
    let recorded = recorded.trim();
    let actual = actual.trim();
    if recorded.is_empty() || actual.is_empty() {
        return false;
    }
    actual.starts_with(recorded)
}

/// 一次收屍要做的兩件事。
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct SweepPlan {
    /// 要送 `SIGKILL` 的 pgid。
    pub doomed: Vec<i32>,
    /// 收屍之後這份檔案還該留下哪些條目——**只留主人還活著的那些**。
    ///
    /// 主人還活著代表那是另一個實例**現役**的隧道，它自己退出時會來退登記，
    /// 這裡不可以連檔案帶條目一起清掉（清掉等於讓那些 pgid 從此沒有人收屍）。
    /// 主人已死的條目則不論殺沒殺到都不必留：殺到了自然沒了，沒殺到代表那個
    /// pgid 已經不是我們的東西（行程沒了、或 pid 被回收給別人），下一次啟動
    /// 再問一次也只會得到同一個答案。
    pub keep: Vec<Entry>,
}

/// 收屍的決策。
///
/// 抽成純函式（「這個 owner 還活著嗎」「這個群組裡有誰」都由呼叫端查好傳進來）
/// 才測得到——真的去 `ps` 一顆系統上的行程沒辦法在單元測試裡穩定重現。
///
/// 兩道閘，逐筆各過一次：
///
/// * **主人還活著就跳過**：那是另一個實例現役的連線，不是殘骸（誤殺鏈的細節
///   見 [`Entry::owner_pid`]）。`owner_alive` 的結果會在這支函式裡快取，
///   同一個 owner 只問一次。
/// * **命令列對得上才殺**：pid 會回收，光看「這個 pgid 還活著」等於拿一個舊
///   數字去砍今天不相干的行程群組。
///
/// 比對的是「群組裡**任何一支**行程」而不是只看群組領袖：領袖已經被回收、
/// 但 `ps` 全表裡仍有同 pgid 的其他行程，這種情形照樣要收。
///
/// **已知漏殺**：ssh 自己先退掉、只剩 ProxyCommand 生出來的孫程序（例如
/// `cloudflared`）還在的話，那一支的命令列與登記的 ssh 互不為前綴，比對不到，
/// 這一組就不會被收。刻意不為了涵蓋它去放寬比對——放寬的代價是誤殺，而誤殺
/// 砍的是別人的程式；漏殺的代價只是那個埠這一次沒讓出來。何況真正卡住埠的是
/// ssh 的 `-L`，ssh 一旦不在，埠本來就已經放掉了。
pub(super) fn plan_sweep(
    reg: &Registry,
    owner_alive: &mut dyn FnMut(i32) -> bool,
    group_commands: &mut dyn FnMut(i32) -> Vec<String>,
) -> SweepPlan {
    let mut plan = SweepPlan::default();
    let mut asked: Vec<(i32, bool)> = Vec::new();
    for entry in &reg.entries {
        let alive = match asked.iter().find(|(pid, _)| *pid == entry.owner_pid) {
            Some((_, alive)) => *alive,
            None => {
                let alive = owner_alive(entry.owner_pid);
                asked.push((entry.owner_pid, alive));
                alive
            }
        };
        if alive {
            plan.keep.push(entry.clone());
            continue;
        }
        // 與 `spawn` 的 Drop 同一道下界保護：`kill(-0)` 是「殺自己這一組」，
        // `kill(-1)` 是「殺所有殺得動的東西」。這裡吃的是磁碟上的檔案內容
        // （比子程序回報的 pid 更不可信），更不能只靠「理論上不會是 0 或 1」。
        if entry.pgid <= 1 {
            continue;
        }
        if group_commands(entry.pgid).iter().any(|actual| commands_match(&entry.command, actual)) {
            plan.doomed.push(entry.pgid);
        }
    }
    plan
}

/// `ps` 報回來的那一行有沒有提到我們這支執行檔的檔名。
///
/// **兩邊都轉小寫再比**，不是原樣 `contains`。macOS 的預設檔案系統（APFS 與
/// 舊的 HFS+）是**不分大小寫**的，同一支執行檔在不同的路徑寫法下都跑得起來，
/// 而「打包出來的那一份」與「`cargo build` 出來的那一份」檔名大小寫本來就不必
///一致（bundle 的執行檔名跟著 `productName` 走，開發產物跟著 crate 名走）。
/// 原樣比對的話，正式版與 dev 版並存時兩邊會互相判定「對方那個 pid 不是我們的
/// 程式、主人已死」，接著把對方**現役**的隧道全部 SIGKILL——正是這個機制最不該
/// 做的事（同一段推演見 [`owner_still_running`] 裡「為什麼比檔名不比完整路徑」）。
///
/// 不分大小寫是往「主人還活著」偏的方向（多認、不少認），與這個模組
/// 「寧可漏殺不誤殺」的總原則一致。同一份紀律在 `lib.rs::heal_autostart` 比對
/// 自啟命令列時也是這樣寫的（兩邊 `to_lowercase` 再 `contains`）。
fn ps_output_mentions(ps_output: &str, exe_name: &str) -> bool {
    ps_output.to_lowercase().contains(&exe_name.to_lowercase())
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
///
/// 原子寫入本身**不再自己做一份**：這裡要的每一個性質（同資料夾的 `.tmp`、
/// 兩種失敗都把暫存檔清掉、`rename` 蓋上去）都跟 [`crate::config::write_atomic`]
/// 一字不差，那邊也是設定檔的落檔路徑。原本兩邊各維護一份逐字相同的實作，
/// 正是 `platform/mod.rs` 開頭第四條規則不准的事。這一層只剩下自己真正的責任：
/// 序列化，以及「資料夾可能還不存在」（`~/Library/Application Support/<id>/`
/// 第一次寫入時就是這樣；設定檔那條路由 `config_location` 事先建好資料夾，
/// `write_atomic` 因此不管這件事）。
fn write_at(path: &Path, reg: &Registry) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::config::write_atomic(path, &to_json(reg))
}

/// 檔案沒有東西可留時就刪掉，不留一份空殼在使用者的資料夾裡。
fn write_or_clear_at(path: &Path, reg: &Registry) -> io::Result<()> {
    if reg.entries.is_empty() {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
    }
    write_at(path, reg)
}

/// 加一筆。**只動自己那一筆**——別的 owner 的條目原封不動留著，這是
/// [`Entry::owner_pid`] 那條誤殺鏈的第 2 步不成立的原因。
pub(super) fn register_at(path: &Path, owner_pid: i32, pgid: i32, command: &str) -> io::Result<()> {
    let mut reg = read_at(path);
    reg.entries.retain(|e| e.pgid != pgid);
    reg.entries.push(Entry { pgid, command: command.to_string(), owner_pid });
    write_at(path, &reg)
}

/// 拿掉一筆。`owner_pid` 也要對得上：只准退登記自己 spawn 的東西。
pub(super) fn unregister_at(path: &Path, owner_pid: i32, pgid: i32) -> io::Result<()> {
    let mut reg = read_at(path);
    let before = reg.entries.len();
    reg.entries.retain(|e| !(e.pgid == pgid && e.owner_pid == owner_pid));
    if reg.entries.len() == before {
        return Ok(());
    }
    write_or_clear_at(path, &reg)
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

/// 把 `ps -axww -o pid=,pgid=,command=` 的輸出解成「pgid → 這一組裡每一支的
/// 命令列」。純函式，`ps` 的實際執行在 [`commands_by_pgid`]。
///
/// 每一行的形狀固定是「pid、pgid、命令列」，前兩欄是**靠右對齊、寬度不定**的
/// 數字（欄與欄之間可能有好幾個空白），第三欄開始整個都是命令，而命令本身含
/// 空白。所以是「切掉前兩個欄位、剩下的整段就是命令」，不可以
/// `split_whitespace` 全切開再用單一空白拼回去——那樣會把命令列裡連續的空白
/// 壓掉，`ps -o command=` 印的是什麼就不再是什麼，而 [`commands_match`] 是逐字
/// 前綴比對，壓掉就對不上了。解不開的行（`ps` 哪天多印了什麼）整行跳過，
/// 不要瞎猜。
fn parse_ps_pgid_table(stdout: &str) -> std::collections::HashMap<i32, Vec<String>> {
    /// 切掉開頭的空白與第一個欄位，回 (那個欄位, 剩下的整段)。
    fn split_first_field(s: &str) -> Option<(&str, &str)> {
        let s = s.trim_start();
        let end = s.find(char::is_whitespace)?;
        Some((&s[..end], &s[end..]))
    }

    let mut table: std::collections::HashMap<i32, Vec<String>> = std::collections::HashMap::new();
    for line in stdout.lines() {
        // 第一欄是 pid，這裡用不到（比對只看命令列），但一定要切掉才輪得到 pgid
        let Some((_pid, rest)) = split_first_field(line) else {
            continue;
        };
        let Some((pgid, command)) = split_first_field(rest) else {
            continue;
        };
        let Ok(pgid) = pgid.parse::<i32>() else {
            continue;
        };
        // 只去頭尾：命令列**內部**的空白原樣留著
        let command = command.trim();
        if command.is_empty() {
            continue;
        }
        table.entry(pgid).or_default().push(command.to_string());
    }
    table
}

/// 問 `ps` 一次，把系統上每一個行程按 pgid 分好組。
///
/// **一次就好**。這條路的呼叫端只有 [`sweep_leftovers`]，而它要問的是登記簿裡
/// 每一筆 pgid「這一組現在有誰」——原本是逐筆 `ps -g <pgid>`，登記簿有 N 筆就
/// fork／exec N 次。一次 `-ax`（所有使用者、含沒有控制終端的）把全表拿回來建
/// 成 `HashMap` 完全等價，而且是啟動路徑上唯一一次外部程序。
///
/// `-ww` 一定要在：它關掉「照終端機寬度截斷」的預設行為，截斷會讓命令列比對
/// 無謂地失手——[`commands_match`] 只認一個方向（`ps` 至少要印出我們登記的
/// 那一整行）正是建立在「不會截斷」這個前提上。
///
/// 用 `ps` 而不是手刻 `sysctl KERN_PROCARGS2`：後者要解一份 Apple 沒有公開穩定
/// 文件的核心緩衝格式，還得整段 unsafe，而這條路一輩子只在啟動時跑這一次。
fn commands_by_pgid() -> std::collections::HashMap<i32, Vec<String>> {
    match std::process::Command::new("ps").args(["-axww", "-o", "pid=,pgid=,command="]).output() {
        Ok(out) => parse_ps_pgid_table(&String::from_utf8_lossy(&out.stdout)),
        Err(e) => {
            log::warn!("could not ask ps about the running process groups: {e}");
            // 問不到就是一張空表，於是每一筆都「群組裡沒有任何行程」＝不殺。
            // 方向是漏殺不是誤殺，與整個模組的原則一致
            std::collections::HashMap::new()
        }
    }
}

/// 某一筆登記的主人（spawn 它的那個 app 行程）是不是還在跑。
///
/// 兩道都要成立才算：pid 還活著（`kill(pid, 0)`），而且那個 pid 的命令列裡
/// 出現得了我們這支執行檔的**檔名**——只看 pid 活不活著會被 pid 回收騙過去。
/// 為什麼比檔名而不是完整路徑（以及這個取捨往哪邊偏）見函式內的說明。
///
/// 「主人就是我自己」回 `false`：收屍只在啟動時、任何 `register` 之前跑，
/// 這時候檔案裡不可能有本輪自己的條目，會對到只可能是 pid 回收的巧合。
fn owner_still_running(owner_pid: i32) -> bool {
    if owner_pid <= 1 || owner_pid == std::process::id() as i32 {
        return false;
    }
    if unsafe { libc::kill(owner_pid, 0) } != 0 {
        return false;
    }
    // 比對用的是**執行檔檔名**，不是完整路徑。
    //
    // 這裡的每一個判斷失誤都要往「主人還活著」偏（＝那一筆不掃＝漏殺），
    // 因為反方向是誤殺。用完整路徑當前綴比對就正好偏錯邊：同一個使用者同時
    // 跑「裝在 /Applications 的正式版」與「開發中的 cargo build 產物」，而
    // single-instance 又失手讓兩邊並存時，兩邊的 `current_exe()` 完全不同，
    // 於是互相判定「對方那個 pid 不是我們的程式、主人已死」，接著把對方
    // 現役的隧道全部 SIGKILL——正是這個機制最不該做的事。
    //
    // 換成檔名包含之後，誤判方向翻過來了：系統上剛好有另一支同名的
    // `traytunnel` 佔著那個回收來的 pid 時會被誤認成「主人還在」，於是那一筆
    // 不掃。代價只是那一輪少收一具屍體，下一次啟動再收；而且要湊齊
    // 「pid 剛好被回收」＋「新主人剛好也叫這個名字」兩件事，機率本來就極低。
    //
    // 比對本身不分大小寫，理由見 [`ps_output_mentions`]。
    let name = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    if name.is_empty() {
        // 問不到自己叫什麼的話，寧可保守：pid 還活著就當主人還在，不要掃
        return true;
    }
    let out = std::process::Command::new("ps")
        .args(["-ww", "-o", "command=", "-p"])
        .arg(owner_pid.to_string())
        .output();
    match out {
        Ok(out) => ps_output_mentions(&String::from_utf8_lossy(&out.stdout), &name),
        // 連 ps 都跑不起來時同樣往「主人還活著」偏
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
    if let Err(e) = unregister_at(&path, std::process::id() as i32, pgid) {
        log::warn!("could not clear supervised process group {pgid}: {e}");
    }
}

/// 啟動時收屍：把上一輪被 `SIGKILL`／當機帶走、卻還活著的 ssh 程序樹清掉。
///
/// **呼叫時機有硬性要求**：必須在任何一條隧道 spawn 之前（上一輪殘留的 ssh 才
/// 來得及把埠讓出來），而且要在本行程任何一次 `register` 之前（那樣檔案裡就
/// 不可能有本輪自己的條目）。`lib.rs` 把它放在 `Builder::setup` 閉包裡。
///
/// 「另一個實例現役的連線」不靠呼叫時機保護，而是靠逐筆的 `ownerPid`
/// （見 [`Entry::owner_pid`]）——single-instance 外掛有一條會讓兩個實例並存的
/// 錯誤分支，時機本身擋不住它。
///
/// 沒有「檔案不在就早退」「登記簿是空的就刪檔早退」這兩道分支，是刻意的：
/// [`read_at`] 讀不到檔案本來就回一份空的登記簿，空登記簿的 [`plan_sweep`] 回
/// 一份空計畫（三個 `len()` 相減是 0，一行日誌都不會記），最後 [`write_or_clear_at`]
/// 對空的登記簿做的正是「把檔案刪掉，本來就沒有也算成功」。兩道早退跟這條主線
/// 一字不差地等價，留著只是同一件事寫兩遍。
pub fn sweep_leftovers() {
    let Some(path) = registry_path() else { return };
    let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let reg = read_at(&path);
    // 一次把全表拿回來，登記簿有幾筆都只問 `ps` 這一次（見 `commands_by_pgid`）
    let by_pgid = commands_by_pgid();
    let plan = plan_sweep(&reg, &mut owner_still_running, &mut |pgid| {
        by_pgid.get(&pgid).cloned().unwrap_or_default()
    });
    for pgid in &plan.doomed {
        log::warn!(
            "killing a process group left behind by a previous run (pgid {pgid}); the last run \
             did not get to clean up (SIGKILL or a crash)"
        );
        kill_group(*pgid);
    }
    if !plan.keep.is_empty() {
        log::warn!(
            "{} recorded process group(s) still belong to a live instance and were left alone",
            plan.keep.len()
        );
    }
    let untouched = reg.entries.len() - plan.doomed.len() - plan.keep.len();
    if untouched > 0 {
        log::info!(
            "{untouched} of {} recorded process group(s) were already gone (or no longer match \
             what we spawned) and were left alone",
            reg.entries.len()
        );
    }
    // 只留下主人還活著的那些條目；主人已死的不論殺沒殺到都清掉，留著只會讓
    // 下一次啟動重問一次同樣的死 pgid。本輪自己的東西會由 `register` 重新寫進來。
    if let Err(e) = write_or_clear_at(&path, &Registry { entries: plan.keep }) {
        log::warn!("could not rewrite the supervised process group registry after sweeping: {e}");
    }
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

    /// 一筆測試用的登記。
    fn entry(pgid: i32, owner_pid: i32, command: &str) -> Entry {
        Entry { pgid, command: command.into(), owner_pid }
    }

    /// 寫出去、讀回來要是同一份；壞掉的內容一律退成空的而不是炸掉。
    #[test]
    fn the_registry_round_trips_and_tolerates_garbage() {
        let reg = Registry {
            entries: vec![
                entry(111, 4242, "ssh -N -L 1080:127.0.0.1:1080 bob@example.com"),
                entry(222, 4242, "ssh -N bob@other"),
            ],
        };
        assert_eq!(parse(&to_json(&reg)), reg);

        assert_eq!(parse("not json at all"), Registry::default());
        assert_eq!(parse(""), Registry::default());
        // 缺欄位不該讓整份讀不回來（`#[serde(default)]`）
        assert_eq!(parse("{}"), Registry::default());
        // 每一筆的 owner 都寫在條目裡，JSON 的鍵是 camelCase
        assert!(to_json(&reg).contains("\"ownerPid\": 4242"), "{}", to_json(&reg));
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
        assert_eq!(reg.entries.len(), 2);
        assert!(reg.entries.iter().all(|e| e.owner_pid == 900));

        // 同一個 pgid 再登記一次是取代，不是長出第二筆
        register_at(&path, 900, 111, "ssh -N bob@a2").expect("重登記要成功");
        let reg = read_at(&path);
        assert_eq!(reg.entries.len(), 2);
        assert_eq!(
            reg.entries.iter().find(|e| e.pgid == 111).map(|e| e.command.as_str()),
            Some("ssh -N bob@a2")
        );

        // 沒登記過的 pgid 退登記是 no-op，不算錯
        unregister_at(&path, 900, 999).expect("退登記不存在的 pgid 不算錯");
        assert_eq!(read_at(&path).entries.len(), 2);

        unregister_at(&path, 900, 111).expect("退登記要成功");
        assert_eq!(read_at(&path).entries.iter().map(|e| e.pgid).collect::<Vec<_>>(), vec![222]);

        unregister_at(&path, 900, 222).expect("最後一筆退登記要成功");
        assert!(!path.exists(), "最後一筆走了就不該再留一份空殼：{}", path.display());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `sweep_leftovers` 拿掉的那兩道早退（「檔案不在」「登記簿是空的」）之所以
    /// 跟主線等價，靠的就是這裡的三件事：讀不到檔案回一份空的登記簿、空的登記簿
    /// 規劃不出任何動作、清空的寫入就是「刪掉檔案，本來沒有也算成功」，而且不會
    /// 順手建出任何東西。
    #[test]
    fn an_absent_registry_reads_as_empty_and_clears_to_nothing() {
        let dir = tempdir("absent");
        let path = dir.join(FILE_NAME);
        assert!(!path.exists(), "前提：這個路徑一開始就沒有檔案");

        assert_eq!(read_at(&path), Registry::default(), "讀不到檔案要回一份空的登記簿");

        let mut never = |_: i32| panic!("空的登記簿不該問任何人");
        let plan =
            plan_sweep(&Registry::default(), &mut never, &mut |_| panic!("空的登記簿不該問 ps"));
        assert_eq!(plan, SweepPlan::default(), "空的登記簿規劃不出任何動作");

        write_or_clear_at(&path, &Registry::default()).expect("清空一份本來就不存在的檔案不算錯");
        assert!(!path.exists(), "不可以憑空生出一份空殼：{}", path.display());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 兩個實例並存時（single-instance 的錯誤分支）的檔案語意：各改各的條目。
    /// 這是 [`Entry::owner_pid`] 那條誤殺鏈第 2、3 步不成立的地方。
    #[test]
    fn one_instance_never_touches_another_instances_entries() {
        let dir = tempdir("twoinstances");
        let path = dir.join(FILE_NAME);

        // A 先登記兩筆
        register_at(&path, 900, 111, "ssh -N bob@a").unwrap();
        register_at(&path, 900, 222, "ssh -N bob@b").unwrap();
        // B 登記自己的一筆——不可以動到 A 的兩筆，也不可以把它們改姓 B
        register_at(&path, 901, 333, "ssh -N bob@c").unwrap();

        let reg = read_at(&path);
        assert_eq!(reg.entries.len(), 3);
        assert_eq!(reg.entries.iter().filter(|e| e.owner_pid == 900).count(), 2);
        assert_eq!(reg.entries.iter().filter(|e| e.owner_pid == 901).count(), 1);

        // B 正常退出：只帶走自己那一筆，A 的兩筆原封不動留著
        unregister_at(&path, 901, 333).unwrap();
        let reg = read_at(&path);
        assert_eq!(reg.entries.iter().map(|e| e.pgid).collect::<Vec<_>>(), vec![111, 222]);
        assert!(reg.entries.iter().all(|e| e.owner_pid == 900));

        // B 也不准替 A 退登記（就算 pgid 猜對了）
        unregister_at(&path, 901, 111).unwrap();
        assert_eq!(read_at(&path).entries.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 寫完（成功或失敗）都不可以在登記簿的資料夾裡留下半成品——留著的話
    /// 使用者的資料夾裡永遠躺著一顆看不懂的東西，而且要等下一次寫入才會被覆寫。
    ///
    /// 「不留半成品」這件事本身現在是 [`crate::config::write_atomic`] 的責任
    /// （原子寫入已經上提到那一份共用實作，見 [`write_at`]），那邊有自己的
    /// 測試釘兩處 `remove_file`。這一條留著、而且**不再寫死暫存檔名**，測的是
    /// 這一層自己的東西：登記簿這條路（含它自己那一段 `create_dir_all`）走完
    /// 之後，這個資料夾裡除了登記簿本身不該多出任何檔案。改成掃整個資料夾而
    /// 不是 `dir.join("supervised-pgids.json.tmp")`，是因為暫存檔怎麼命名已經
    /// 是共用函式的實作細節了，這一層不該再假設它長什麼樣。
    ///
    /// 兩段：成功那一條，以及 **`rename` 失敗**那一條。`fs::write` 寫到一半失敗
    /// （典型是磁碟寫滿）沒有辦法在單元測試裡穩定重現，因此只走得到 rename 這支：
    /// 讓**目的地**是一個資料夾——`create_dir_all(parent)` 看到父資料夾已經在就
    /// 直接成功、`fs::write` 也真的把暫存檔寫出來了，失敗的是最後「把一個檔案
    /// rename 蓋到一個資料夾上」。
    #[test]
    fn writing_leaves_no_leftovers_behind() {
        let dir = tempdir("notmp");
        let path = dir.join(FILE_NAME);

        /// 這個資料夾裡（不含子資料夾）現在有哪些檔案。
        fn files_in(dir: &Path) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .expect("tempdir 要讀得到")
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }

        register_at(&path, 900, 111, "ssh -N bob@a").unwrap();
        assert!(path.is_file());
        assert_eq!(files_in(&dir), vec![FILE_NAME.to_string()], "寫完只該有登記簿本身");

        // rename 失敗那一支：目的地是一個資料夾，前面兩步都會成功
        let dest_is_a_dir = dir.join("dest-is-a-directory");
        std::fs::create_dir_all(&dest_is_a_dir).unwrap();
        assert!(
            write_at(&dest_is_a_dir, &Registry::default()).is_err(),
            "把檔案 rename 蓋到一個資料夾上本來就該失敗——失敗不了的話這一條測不到東西"
        );
        assert_eq!(files_in(&dir), vec![FILE_NAME.to_string()], "寫入失敗也不可以留下半成品");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 命令列比對只認一個方向：`ps` 回報的那一行要以登記的那一行開頭。
    ///
    /// 最後一段是這一版真正要擋的那個誤殺面：`ps` 只印得出 `ssh` 幾個字的
    /// 行程（本機實證：argv[0] 被改成 `ssh` 的 `cat`，`ps -ww -o command=`
    /// 就是一行 `ssh`）**不可以**匹配任何一筆登記——pid 會回收，匹配上就等於
    /// 拿一個舊數字去 SIGKILL 別人的行程群組。
    #[test]
    fn a_command_only_matches_when_ps_shows_at_least_what_we_recorded() {
        let recorded = "ssh -N -o ServerAliveInterval=10 -L 1080:127.0.0.1:1080 bob@example.com";

        // 一模一樣：相符
        assert!(commands_match(recorded, recorded));
        // 前後空白不算差異（`ps` 的欄位會補空白）
        assert!(commands_match(recorded, &format!("  {recorded}  ")));
        // 尾巴多出東西：相符（有些程式會改寫自己的 argv 尾端）
        assert!(commands_match(recorded, &format!("{recorded} extra")));

        // `ps` 只印得出 `ssh`：**不可以**相符
        assert!(
            !commands_match(recorded, "ssh"),
            "ps 只印 ssh 的無關程序若匹配得上，每一筆登記都會被它對到，pid 回收時就是誤殺"
        );
        assert!(!commands_match(recorded, "ssh -N"), "印到一半同樣不算");

        // 完全不相干的行程
        assert!(!commands_match(recorded, "/usr/libexec/some-daemon --serve"));
        // 空字串（`ps` 問不到東西）兩個方向都不算
        assert!(!commands_match(recorded, ""));
        assert!(!commands_match("", "ssh -N bob@a"));
    }

    /// 收屍決策：命令列對得上才殺，對不上或群組已空一律留著。
    #[test]
    fn the_sweep_only_kills_groups_that_still_look_like_what_we_spawned() {
        let reg = Registry {
            entries: vec![
                entry(111, 900, "ssh -N bob@a"),
                entry(222, 900, "ssh -N bob@b"),
                entry(333, 900, "ssh -N bob@c"),
            ],
        };
        let mut dead = |_: i32| false;
        let mut ps = |pgid: i32| match pgid {
            // 領袖還在，命令列一模一樣 → 殺
            111 => vec!["ssh -N bob@a".to_string()],
            // 群組已經空了 → 留著
            222 => vec![],
            // pgid 被回收，現在是別人的群組 → 絕對不能殺
            333 => vec!["/usr/libexec/some-daemon --serve".to_string()],
            _ => unreachable!("測試只問這三個 pgid"),
        };
        let plan = plan_sweep(&reg, &mut dead, &mut ps);
        assert_eq!(plan.doomed, vec![111]);
        // 主人都死了，沒有任何一筆該留在檔案裡
        assert!(plan.keep.is_empty());
    }

    /// 比對的是「群組裡任何一支」而不是只看群組領袖：領袖已經被回收、但
    /// `ps` 全表裡仍有同 pgid 的其他行程時，照樣要收。
    #[test]
    fn a_group_is_killed_when_any_of_its_processes_still_matches() {
        let reg = Registry { entries: vec![entry(111, 900, "ssh -N bob@a")] };
        let mut dead = |_: i32| false;
        // 第一行不是我們登記的那一支，第二行才是——只要有一行對得上就算
        let mut ps = |_: i32| {
            vec!["cloudflared access ssh --hostname a".to_string(), "ssh -N bob@a".to_string()]
        };
        assert_eq!(plan_sweep(&reg, &mut dead, &mut ps).doomed, vec![111]);
    }

    /// **已知漏殺，這裡把它釘成規格**：ssh 自己先退掉、只剩 ProxyCommand 的孫
    /// 程序（cloudflared）還在時，那一支的命令列與登記的 ssh 互不為前綴，比對
    /// 不到，這一組就不收。刻意不為了涵蓋它去放寬比對——放寬的代價是誤殺別人的
    /// 程式，而真正卡住埠的是 ssh 的 `-L`，ssh 不在，埠本來就放掉了。
    #[test]
    fn a_group_with_only_a_grandchild_left_is_a_known_miss() {
        let reg = Registry { entries: vec![entry(111, 900, "ssh -N bob@a")] };
        let mut dead = |_: i32| false;
        let mut ps = |_: i32| vec!["cloudflared access ssh --hostname a".to_string()];
        assert!(plan_sweep(&reg, &mut dead, &mut ps).doomed.is_empty());
    }

    /// 主人還活著的條目一律跳過，而且要**原封不動留在檔案裡**——那是另一個
    /// 實例現役的隧道，它自己退出時才輪得到它退登記。
    #[test]
    fn entries_whose_owner_is_still_alive_are_skipped_and_kept() {
        let a = entry(111, 900, "ssh -N bob@a");
        let b = entry(222, 901, "ssh -N bob@b");
        let reg = Registry { entries: vec![a.clone(), b.clone()] };
        // 900（A）還活著，901（B，上一輪被 SIGKILL 的那個）已經死了
        let mut alive = |pid: i32| pid == 900;
        let mut ps = |_: i32| vec!["ssh -N bob@a".to_string(), "ssh -N bob@b".to_string()];
        let plan = plan_sweep(&reg, &mut alive, &mut ps);
        assert_eq!(plan.doomed, vec![222], "只該收 B 留下的那一組");
        assert_eq!(plan.keep, vec![a], "A 現役的那一筆要留在檔案裡");
    }

    /// 誤殺鏈的完整回放：A 現役、B 曾經並存後被 SIGKILL、C 現在啟動。
    /// C 只准收 B 的殘骸，A 的隧道一根寒毛都不能動。
    #[test]
    fn a_third_instance_never_sweeps_a_live_instances_tunnels() {
        let dir = tempdir("misfirechain");
        let path = dir.join(FILE_NAME);

        // 1. A（pid 900）起兩條隧道
        register_at(&path, 900, 111, "ssh -N bob@a1").unwrap();
        register_at(&path, 900, 222, "ssh -N bob@a2").unwrap();
        // 2. B（pid 901）並存起來，也登記了一條
        register_at(&path, 901, 333, "ssh -N bob@b1").unwrap();
        // 3. B 被 SIGKILL，什麼都沒退登記

        // 4. C 啟動：A 還活著、B 已死
        let reg = read_at(&path);
        let mut alive = |pid: i32| pid == 900;
        let mut ps = |pgid: i32| match pgid {
            111 => vec!["ssh -N bob@a1".to_string()],
            222 => vec!["ssh -N bob@a2".to_string()],
            333 => vec!["ssh -N bob@b1".to_string()],
            _ => unreachable!(),
        };
        let plan = plan_sweep(&reg, &mut alive, &mut ps);
        assert_eq!(plan.doomed, vec![333], "只准收 B 的殘骸");
        assert_eq!(
            plan.keep.iter().map(|e| e.pgid).collect::<Vec<_>>(),
            vec![111, 222],
            "A 現役的兩條要原封不動留著"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `kill(-0)`／`kill(-1)` 是完全不同的廣播範圍（殺自己這一組／殺所有殺得動
    /// 的東西），登記簿是磁碟上的檔案、內容比子程序回報的 pid 更不可信，
    /// 這道下界保護一定要在。
    #[test]
    fn nonsense_pgids_are_never_selected() {
        let reg = Registry {
            entries: vec![
                entry(0, 900, "ssh -N bob@a"),
                entry(1, 900, "ssh -N bob@a"),
                entry(-5, 900, "ssh -N bob@a"),
            ],
        };
        let mut dead = |_: i32| false;
        let mut ps = |_: i32| panic!("下界保護要在問 ps 之前就擋掉");
        assert!(plan_sweep(&reg, &mut dead, &mut ps).doomed.is_empty());
    }

    /// 「主人還在不在」的 `ps` 比對必須**不分大小寫**。macOS 的預設檔案系統不分
    /// 大小寫，而打包產物與 `cargo build` 產物的執行檔名大小寫本來就不必一致
    /// （前者跟 `productName` 走、後者跟 crate 名走）。原樣比對的話，正式版與
    /// dev 版並存時兩邊會互相判定「對方的主人已死」，接著把對方**現役**的隧道
    /// 全部 SIGKILL。
    #[test]
    fn the_owner_check_ignores_executable_name_case() {
        let prod_line = "/Applications/Traytunnel.app/Contents/MacOS/Traytunnel --tray";
        assert!(
            ps_output_mentions(prod_line, "traytunnel"),
            "dev 版（檔名 traytunnel）必須認得出正式版（檔名 Traytunnel）還活著，\
             否則會把正式版現役的隧道當成殘骸掃掉"
        );
        let dev_line = "/Users/bob/dev/tt/target/debug/traytunnel";
        assert!(ps_output_mentions(dev_line, "Traytunnel"), "反方向同樣要成立");

        // 不分大小寫不等於「什麼都算」：完全不相干的行仍然不可以匹配
        assert!(!ps_output_mentions("/usr/libexec/some-daemon --serve", "traytunnel"));
        // `ps` 問不到東西（行程已經不在）會回空字串，那時候不該認為主人還在
        assert!(!ps_output_mentions("", "traytunnel"));
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

    /// `ps -axww -o pid=,pgid=,command=` 的輸出要按 pgid 分好組，而且命令列
    /// 必須**原樣**保留（含裡面的連續空白）——`commands_match` 是逐字前綴比對，
    /// 把空白壓掉就再也對不上我們登記的那一行。
    #[test]
    fn the_ps_table_groups_commands_by_pgid() {
        // 前兩欄靠右對齊、寬度不定，這是 `ps -o pid=,pgid=` 真的會印出來的樣子
        let out = "    1     1 /sbin/launchd\n\
                   \x20 900   900 ssh -N  -L 1080:127.0.0.1:1080 bob@a\n\
                   \x209001   900 cloudflared access ssh --hostname a\n\
                   \x209100  9100 /usr/libexec/some-daemon --serve\n";
        let table = parse_ps_pgid_table(out);

        assert_eq!(
            table.get(&900).map(Vec::as_slice),
            Some(
                &[
                    "ssh -N  -L 1080:127.0.0.1:1080 bob@a".to_string(),
                    "cloudflared access ssh --hostname a".to_string(),
                ][..]
            ),
            "同一個 pgid 的兩支（ssh 與它 ProxyCommand 生的孫程序）要收在一起，\
             而且 `ssh -N  -L` 中間那兩個空白不可以被壓成一個"
        );
        assert_eq!(table.get(&9100).map(Vec::len), Some(1));
        assert_eq!(table.get(&12345), None, "沒出現過的 pgid 就是沒有");

        // 解不開的行整行跳過，不要瞎猜也不要炸掉
        assert!(parse_ps_pgid_table("這一行不是 ps 的輸出\n\n  1\n").is_empty());
    }

    /// 這台機器上真的跑一次 `ps`，確認上面那支解析器吃的格式沒有猜錯：
    /// **本行程自己**一定要出現在自己的 pgid 底下。純唯讀查詢，不動任何東西。
    #[test]
    fn the_ps_table_can_find_this_very_process() {
        let table = commands_by_pgid();
        let me = std::process::id() as i32;
        let my_pgid = unsafe { libc::getpgid(me) };
        assert!(
            table.contains_key(&my_pgid),
            "ps 的全表裡找不到本行程自己的 pgid {my_pgid}——解析器跟真實輸出對不上了"
        );
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
