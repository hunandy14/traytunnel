//! 系統查詢與系統動作。對照組是 `platform/windows/winsys.rs`。
//!
//! `is_listening`（A 車道）與圖示尺寸／挑層（C 車道）都已在 `agent/feature/macos`
//! 落地（分別見 `feat/macos-process-mgmt`／`feat/macos-tray-window` 合併）。

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- GUI 啟動的 PATH
//
// **這一節在 Windows 上沒有對應物，也不需要**：Windows 的 GUI 行程是由
// Explorer／登錄檔的 Run 值啟動的，環境變數（含 `PATH`）一路從使用者的 session
// 繼承下來，跟從 `cmd` 敲一次指令拿到的是同一份。macOS 不是——launchd 給 GUI
// 行程（Finder 雙擊、`open`、我們自己寫的 LaunchAgent）的 `PATH` 是一份最小集
// `/usr/bin:/bin:/usr/sbin:/sbin`，使用者在 `.zshrc`／`.zprofile` 裡加的東西
// 一概不在裡面。
//
// 這對本程式是**致命**的，不是「有點不方便」：ssh 的 `ProxyCommand`（預設值就是
// `cloudflared access ssh --hostname %h`）是交給 `/bin/sh -c` 跑的，而 Homebrew
// 把 `cloudflared` 裝在 `/opt/homebrew/bin`——不在最小集裡。於是 GUI 啟動的實例
// 每一條隧道都在 `sh: cloudflared: not found` 上失敗、進五秒一輪的重連迴圈，
// 而從終端機啟動（繼承使用者 PATH）的同一支執行檔卻完全正常，兩者症狀差異大到
// 使用者根本不會往 PATH 想。實測見 PR 說明。
//
// 社群標準解是 tauri-apps 自己的 `fix-path-env` crate：啟動時跑一次登入 shell、
// 把使用者真正的環境變數讀回來。**這裡不用那顆 crate**，改用它的同一套手法自己
// 做一小份，理由三條：
//
//   1. 它**沒有發布到 crates.io**（`cargo` 只能用 `git = "https://github.com/
//      tauri-apps/fix-path-env-rs"` 拉），版本號還停在 `0.0.0`。把一顆 git 依賴
//      放進發佈用的相依樹，等於放棄版本語意、`cargo audit` 與可重現建置。
//   2. 它**沒有逾時保護**：一路 `Command::output()` 等下去，登入 shell 的 rc
//      檔卡住（oh-my-zsh 的自動更新是最有名的一例，它自己還特地設
//      `DISABLE_AUTO_UPDATE` 來擋）就等於整支 app 永遠停在啟動的第一行。
//   3. 它把**整份環境**（`env` 的每一行）都灌回行程裡，我們只需要 `PATH` 一個鍵。
//
// 於是這裡只保留那套手法真正有價值的部分（登入 shell ＋ 標記夾出結果），另外補上
// 它缺的三件事：逾時（連同殺掉整個行程群組）、「只動 `PATH`」，以及**輸出不走管線**
// ——那顆 crate 的 `Command::output()` 會在使用者 rc 檔有 `some-daemon &` 時一路等到
// 那支背景程序死掉（管線的 EOF 要所有寫端關閉），逾時預算完全被繞過，詳見
// [`ask_shell_for_path`]。

/// launchd 沒有另外設定時，給 GUI 行程的預設 `PATH`（`man launchd.plist` 的
/// `EnvironmentVariables`／`launchctl config user path` 都是在改它）。
/// 現在的 `PATH` 完全落在這一組裡面，就是「這次是 GUI 啟動」的訊號。
const LAUNCHD_DEFAULT_PATH: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// 等登入 shell 回答的上限。給得寬（rc 檔厚的機器上一次互動式登入要好幾百毫秒），
/// 但一定要有——這是 `fix-path-env` 缺的那一格。逾時就放棄修正，維持原樣啟動：
/// 隧道會失敗，但 app 起得來、日誌上有話說，比永遠卡在第一行好。
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// 輪詢登入 shell 有沒有結束的間隔。
const LOGIN_SHELL_TICK: Duration = Duration::from_millis(20);

/// 問不到 `SHELL`（或它不是絕對路徑）時要跑哪一支。macOS 10.15 起的預設登入 shell。
const DEFAULT_LOGIN_SHELL: &str = "/bin/zsh";

/// 把 `PATH` 從互動式 shell 的雜訊（rc 檔自己的 echo、提示字元、顏色碼）裡夾出來的標記。
///
/// 首字元刻意是 `>`／`<` 這種**不可能出現在識別字裡**的字元：標記若以底線或字母
/// 開頭，任何把它接在變數後面的寫法（`$PATH__traytunnel_path_end__`）都會被 shell
/// 當成一個更長的變數名。現在的取法根本不讓 shell 展開變數（見
/// [`ask_shell_for_path`] 用的是 `printenv`），這一層是第二道保險，擋的是「日後
/// 有人把腳本改回 `$PATH` 拼接」。
const PATH_BEGIN: &str = ">>>traytunnel-path>>>";
const PATH_END: &str = "<<<traytunnel-path<<<";

/// 目前這個 `PATH` 是不是「GUI 啟動才會拿到的那份最小集」——每一段都落在
/// [`LAUNCHD_DEFAULT_PATH`] 裡面就算。
///
/// 有這道閘，從終端機／`cargo run` 啟動（PATH 已經是使用者的那一份）時整段
/// 修正完全不會跑，省下一次登入 shell 的開銷，也不會在開發流程上多出任何變數。
/// 空的 `PATH`（連最小集都沒有）同樣回 `true`：那比最小集更該修。
fn path_is_the_gui_minimum(path: &str) -> bool {
    path.split(':')
        .filter(|d| !d.is_empty())
        .all(|d| LAUNCHD_DEFAULT_PATH.contains(&d.trim_end_matches('/')))
}

/// 從登入 shell 的輸出裡把兩個標記中間那一段夾出來。
///
/// 互動式 shell（`-i`）的 rc 檔什麼都可能往 stdout 印，所以不能直接把整份輸出
/// 當成 `PATH`。夾出來之後再濾掉控制字元：合法的路徑不含它們，而 rc 檔的顏色碼
/// 有機會黏在標記外圍以外的地方。
fn extract_marked_path(stdout: &str) -> Option<String> {
    let start = stdout.find(PATH_BEGIN)? + PATH_BEGIN.len();
    let end = stdout[start..].find(PATH_END)? + start;
    let value: String = stdout[start..end].chars().filter(|c| !c.is_control()).collect();
    let value = value.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// 登入 shell 給的 `PATH` 補上系統目錄，缺哪個補哪個（接在後面，使用者自己的
/// 順序優先）。
///
/// 這是一道「不准把事情弄得更糟」的保險：正常的登入 shell 一定會從 `/etc/paths`
/// 拿到這四個目錄，但這裡吃的是使用者 rc 檔的輸出——真的有人把 `PATH` 整個覆寫掉
/// 的話，我們不可以因為「修正」而讓 `ssh`（`/usr/bin/ssh`）本身也找不到。
fn with_system_dirs(login_path: &str) -> String {
    let mut out: Vec<&str> = login_path.split(':').filter(|d| !d.is_empty()).collect();
    for dir in LAUNCHD_DEFAULT_PATH {
        if !out.iter().any(|d| d.trim_end_matches('/') == dir) {
            out.push(dir);
        }
    }
    out.join(":")
}

/// 要問哪一支 shell：`SHELL` 有值**而且是絕對路徑**才用它，否則退回
/// [`DEFAULT_LOGIN_SHELL`]。純函式。
///
/// 絕對路徑這一關不是形式主義：`Command::new` 拿到一個相對名字時會照 `PATH` 去找，
/// 而這支函式的整個存在理由就是「現在的 `PATH` 是壞的」——用一份壞掉的 `PATH` 去
/// 解析要跑哪支 shell，最好的情況是找不到，最壞的情況是找到當前工作目錄底下同名的
/// 別的東西。問不出一個可信的絕對路徑時，跑系統預設的那一支才是對的。
fn resolve_login_shell(from_env: Option<&str>) -> String {
    match from_env {
        Some(s) if Path::new(s).is_absolute() => s.to_string(),
        _ => DEFAULT_LOGIN_SHELL.to_string(),
    }
}

/// 跑一次使用者的登入 shell，把它的 `PATH` 問回來。逾時或任何一步失敗都回 `None`。
fn login_shell_path() -> Option<String> {
    ask_shell_for_path(&resolve_login_shell(std::env::var("SHELL").ok().as_deref()))
}

/// [`login_shell_path`] 的本體，shell 由呼叫端指定（測試才餵得進一支假的）。
///
/// `-ilc`（互動＋登入＋執行一行）是 `fix-path-env` 用的同一組旗標，也是這件事的
/// 社群慣例：`.zprofile`（登入）與 `.zshrc`（互動）兩份都得跑過，使用者的 `PATH`
/// 才會完整——大多數人是在 `.zshrc` 裡加 Homebrew 的。
///
/// ## stdout 一定要導到檔案，不可以是管線
///
/// 這是覆審擋下來的一個真缺陷。管線的讀端要等**所有**寫端關閉才收得到 EOF，而
/// 子程序 spawn 出來的孫程序會繼承同一支寫端：使用者的 rc 檔只要有一句
/// `some-daemon &`（或 `nohup … &`），登入 shell 自己秒退，`wait_with_output()`
/// 卻會一路等到那支背景程序死掉為止——覆審者實測 25.3 秒，而預算是 5 秒。
/// 這條路跑在**任何 UI 之前**，症狀就是「雙擊圖示，什麼都沒發生」。
///
/// （這裡原本的註解寫「輸出遠小於管線緩衝區，不會死鎖」——那句話診斷錯了病因：
/// 卡住的不是緩衝區滿，是**寫端沒關**。輸出再小也一樣卡。）
///
/// 導到一個暫存檔就沒有這回事：檔案沒有 EOF 語意，`read_to_string` 讀的是「此刻
/// 檔案裡有什麼」，於是**逾時迴圈就是唯一的上界**。rc 檔生出來的背景程序照樣繼承
/// 那支 fd，但它之後往一個已經被我們刪掉的 inode 寫，誰都不影響。
///
/// ## 為什麼是 `printenv` 而不是 `"$PATH"`
///
/// 讓 shell 展開變數會被 shell 的語法綁死：`${PATH}` 在 fish 直接是語法錯誤，
/// 而 `"$PATH"` 在 fish 是**用空白**接起來的（`PATH` 在 fish 是 list 變數），
/// 拿回來的字串根本不是冒號分隔的 `PATH`——這種失敗還是靜默的。改成讓 shell 去
/// 跑 `/usr/bin/printenv PATH`，印的是它**匯出給子程序**的那一份，sh／bash／zsh／
/// ksh／dash／fish 一律是冒號分隔的同一個答案，我們這邊一個字都不必展開。
///
/// （已知例外：`tcsh` 不收合併寫的 `-ilc`，它要求 `-l` 單獨當第一個參數。那是
/// `-ilc` 這個社群慣例本身的限制，`fix-path-env` 也一樣；`SHELL` 是 tcsh 的人
/// 會走到「問不到」那一支——照原樣啟動、日誌留一行，不會卡住也不會更糟。）
///
/// 成敗只看「標記在不在」，不看退出碼：互動式 shell 的退出碼是它 rc 檔最後一個
/// 命令的結果，跟「我們有沒有問到答案」沒有關係。
fn ask_shell_for_path(shell: &str) -> Option<String> {
    let tmp = std::env::temp_dir().join(format!(
        "traytunnel-login-path-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let found = ask_shell_for_path_into(shell, &tmp);
    // 成功、逾時、失敗都要清掉，暫存檔不留在 /tmp
    let _ = std::fs::remove_file(&tmp);
    found
}

/// [`ask_shell_for_path`] 扣掉暫存檔清理的那一段，拆開只為了讓清理有唯一一個出口。
fn ask_shell_for_path_into(shell: &str, tmp: &Path) -> Option<String> {
    let script =
        format!("/bin/echo '{PATH_BEGIN}'; /usr/bin/printenv PATH; /bin/echo '{PATH_END}'");

    let mut cmd = std::process::Command::new(shell);
    cmd.args(["-ilc", &script])
        // oh-my-zsh 的自動更新會在互動式啟動時停下來問人（`fix-path-env` 也特地
        // 設這個變數擋它）。逾時保護接得住，但能不觸發就不要觸發
        .env("DISABLE_AUTO_UPDATE", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(tmp).ok()?))
        .stderr(Stdio::null());
    // 自成一個行程群組，逾時時才收得掉「shell 自己＋它 rc 檔生出來的東西」整棵樹
    // ——只 kill shell 的話，卡住它的那支孫程序會留下來
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);

    let mut child = cmd.spawn().ok()?;
    let pgid = child.id() as i32;
    let deadline = Instant::now() + LOGIN_SHELL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // 整組收掉再 wait，不留殭屍也不留孤兒
                    super::pgids::kill_group(pgid);
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(LOGIN_SHELL_TICK);
            }
            Err(_) => return None,
        }
    }
    extract_marked_path(&std::fs::read_to_string(tmp).ok()?)
}

/// GUI（Finder／`open`／LaunchAgent）啟動時把 `PATH` 換成使用者登入 shell 的那一份。
///
/// 回傳要補進活動日誌的行——這支函式跑在 `tauri_plugin_log` 裝上全域 logger
/// **之前**（它必須是整支程式最早的動作之一，見下面那段），那時 `log::info!`
/// 是丟進黑洞的，所以比照 `prepare_notifications` 的作法把話帶回去給 `setup` 記。
///
/// **呼叫時機是規格的一部分**：`std::env::set_var` 改的是整個行程共用的環境區塊，
/// 而 `getenv`／`setenv` 不是執行緒安全的。必須在**任何執行緒被生出來之前**呼叫，
/// 也就是 `tauri::Builder` 之前——`lib.rs::run()` 的最前面。這也剛好是「在任何
/// 一次 spawn ssh 之前」的必要條件。
pub fn fix_gui_launch_path() -> Vec<String> {
    let current = std::env::var("PATH").unwrap_or_default();
    if !path_is_the_gui_minimum(&current) {
        // 終端機／開發啟動：PATH 本來就是使用者的那一份，什麼都不必做，也不必記
        return Vec::new();
    }
    let Some(login) = login_shell_path() else {
        return vec![
            "PATH is the launchd default; asking the login shell for the real one failed, \
             tools installed by Homebrew (e.g. cloudflared for ProxyCommand) may not be found"
                .into(),
        ];
    };
    let fixed = with_system_dirs(&login);
    if fixed == current {
        return Vec::new();
    }
    std::env::set_var("PATH", &fixed);
    vec![format!("PATH was the launchd default, replaced with the login shell PATH: {fixed}")]
}

// ---------------------------------------------------------------- 本地埠偵測

/// 本地是否有程序在該埠 Listen（相當於 Windows 那邊查 `GetExtendedTcpTable`）。
///
/// 手段：唯讀查詢 libproc 記著的行程／socket 表（`listeners` crate 底下
/// 實際呼叫的是 `proc_pidinfo`／`proc_pidfdinfo`，見 Cargo.toml 裡這顆依賴
/// 的說明），列出系統上每一個行程、展開它的 fd、挑出型別是 socket 的那些，
/// 逐一問 fd 的 socket 資訊，從中找有沒有一項 TCP／LISTEN 落在這個埠上。
/// 全程只讀核心已經記著的表，從未對任何位址 `bind` 或 `connect`，因此完全
/// 被動：不會佔用埠、不會跟任何人的 `bind` 搶，也不會對「port 上真的有服務
/// 在跑」這件事產生 connect 探測才有的副作用。
///
/// **這支函式在 2026-08-29 之前是主動 bind 探測**（對 `127.0.0.1`／`::1`
/// 各嘗試 `bind` 一次，bind 不上代表有人在聽）；獨立審查在本機實測踩到那個
/// 做法的真實競態：`tunnel::supervise` 每 2 秒輪詢一次，若正好落在 ssh 完成
/// 認證、準備自己 `bind -L` 的時間窗探測，兩邊都開 `SO_REUSEADDR` 也擋不住
/// 「第二個 bind 撞上 `AddrInUse`」——探測會把 ssh 正要用的位址擠掉一個
/// （雙位址 bind 時可能變成只綁到一族，連線卻照樣回報 `CONNECTED`；
/// `AddressFamily inet` 只綁一族時，`ExitOnForwardFailure` 更會讓 ssh 直接
/// 判定失敗、觸發重連迴圈）。改成唯讀查詢後這個時窗不存在了——這裡從來不去
/// `bind` 任何位址，自然不會跟 ssh 搶；同一個理由也讓下面兩個舊語意缺口
/// 一併修掉：
///   * 綁在 `0.0.0.0`／`[::]`（wildcard，涵蓋所有介面）的監聽者現在看得到了
///     ——舊版只精確比對 `127.0.0.1`／`::1` 這兩個字面位址；libproc 查的是
///     核心記著的 socket 表，不管綁的是字面 loopback 還是 wildcard 都一樣讀
///     得到（見下方測試 `a_wildcard_listener_is_visible`）。
///   * 特權埠（<1024）不再誤判成「沒人聽」——舊版靠自己 `bind` 探測，沒有
///     root 權限時去 `bind` 一個 <1024 的埠一定拿到 `PermissionDenied`，
///     跟「這個埠真的有人在聽」是同一種錯誤外觀、分辨不出來；新版完全不
///     `bind`，只是讀已經存在的 fd 表，同一個 uid 底下的行程無論綁在哪個埠
///     都讀得到，不再受 bind 權限限制。
///
/// **已知限制（不同 uid 的行程）**：libproc 對「不是自己也不是 root」的行程
/// 之 fd 資訊有存取限制——實測（見 PR 說明）在這台機器上，同一個 uid 底下、
/// 甚至是完全獨立的另一支子行程（不是查詢者自己）綁的埠一律讀得到；但 root
/// 擁有的行程（例如 launchd 隨選啟動、uid 0 的 `sshd`，監聽在 `*.22`）在
/// 沒有 root 權限時完全查不到，即使 `netstat`（讀 sysctl 的 pcblist，不受
/// 這層限制）看得到。這是 libproc 這條路徑本身的限制，不是這支函式沒做完；
/// 而且只在「別的使用者或 root 擁有的程序剛好佔住我們要用的那個埠」這種少見
/// 情境才會現形——本專案自己會綁的監聽者（SOCKS5、我們 spawn 出來的 ssh）
/// 一律跟查詢者同一個 uid，不受影響。
pub fn is_listening(port: u16) -> bool {
    match listeners::get_all() {
        Ok(all) => all.iter().any(|l| {
            l.protocol == listeners::Protocol::TCP
                && l.state == listeners::SocketState::Listen
                && l.socket.port() == port
        }),
        Err(e) => {
            // 查表本身失敗（理論上只有系統呼叫層級的異常才會到這裡），比照舊版
            // 「查不到答案就當沒偵測到」的保守方向：不要把「問不到」誤判成
            // 「有人佔著」，害 spawn 前的埠檢查卡住一條原本可以走的隧道；
            // 留一筆 debug 紀錄讓排查的人查得到「這裡其實沒判斷出結果」。
            log::debug!(
                "is_listening: querying the local listener table failed: {e}, treating as not listening"
            );
            false
        }
    }
}

// ---------------------------------------------------------------- 時間

/// 本地時間的 `HH:mm:ss`，活動日誌每一行的時間戳。對應 Windows 的 `GetLocalTime`。
///
/// 不新拉一顆時間 crate（`chrono`／`time`）只為了這一個格式：`libc::localtime_r`
/// 已經是牆上時間（本地時區），時區換算交給 libc 自己做，跟 Windows 那邊
/// `GetLocalTime` 直接回本地時間是同一個語意。`libc` 本來就在相依樹裡
/// （tokio 等間接帶進來），這裡只是把它升成直接依賴。
pub fn local_time_hms() -> String {
    // localtime_r 是執行緒安全版（不像 localtime 共用一份靜態緩衝），
    // 這支可能被日誌路徑從多個地方呼叫，安全版本才不會有資料競爭
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

// ---------------------------------------------------------------- 圖示與 DPI

/// 系統匣圖示（NSStatusItem）的挑層目標尺寸。
///
/// macOS 沒有 Windows 那種隨 DPI 變動的 `SM_CXSMICON`：tray-icon 這顆底層 crate
/// 把選單列圖示的顯示高度寫死在 18pt（見其 `platform_impl/macos/mod.rs` 的
/// `icon_height`），跟這裡塞進去的點陣圖實際像素尺寸無關——那個高度是事後用
/// `NSImage::setSize` 定死的，點陣圖的像素數只決定 Retina 下夠不夠清晰。因此只要
/// 給一張解析度夠高的正方形圖即可：Apple 選單列圖示的建議尺寸是 22×22pt，
/// 2x（Retina）算下來是 44×44px，這裡就回這個目標尺寸。
///
/// macOS 系統匣現在的主要圖示是 [`crate::appicon::tray_icon_template`]（另外
/// 一份純黑＋透明的 template PNG，見 `assets/gen-tray-template.py`），不會走這裡的
/// ICO 挑層；這一支純粹是 `appicon::tray_icon()`（template 圖載不到時的退路）要用
/// 的「想要哪個 ICO 層」目標尺寸。
pub fn small_icon_size() -> (u32, u32) {
    (44, 44)
}

/// 「視窗大圖示」在 macOS 沒有 Windows 工作列按鈕那種對應物——Dock 圖示是
/// app bundle 自帶的 `icon.icns`，跟這支函式的回傳值無關。這裡沿用與 Windows 版
/// 同樣的比例關係（大圖示是小圖示的兩倍），單純讓 `appicon::window_icon()`
/// （`win.set_icon` 的來源；即使這個呼叫在 macOS 上是否真的顯示不影響這裡的職責）
/// 挑到一層夠大、不失真的圖。64 剛好是內嵌 ICO 現成的一層，挑得到就不必再讓系統
/// 縮放。
pub fn large_icon_size() -> (u32, u32) {
    (64, 64)
}

// 「從一組尺寸裡挑最接近 want 的一層」（pick_icon_layer）不靠任何系統 API，是純
// 數字邏輯，原本這裡與 Windows 版（`platform::windows::winsys`）各自維護一份逐字
// 相同的演算法，已上提到唯一的呼叫端 `crate::appicon`，不再由這個模組提供。

// ---------------------------------------------------------------- 開機自啟
//
// 直接管理 `~/Library/LaunchAgents/<label>.plist`（launchd 慣例），不經
// tauri-plugin-autostart。
//
// 偏離說明：官方外掛的 Rust API（`ManagerExt::autolaunch()`）是 `tauri::Manager`
// 的擴充方法，要拿 `AppHandle`／`App` 才叫得到；但 platform 介面這四支是自由函式
// （`name: &str`／`exe: &Path`），跟 Windows 那邊直接寫 HKCU 的形狀一致，也是
// `heal_autostart`／`commands::set_autostart` 已經在用的簽章，不能為了 macOS
// 一個平台去動共用介面或 Windows 那半邊。往下挖一層會發現外掛自己也只是把
// `auto_launch::AutoLaunch`（一顆完全不依賴 Manager 的 crate）包成 Tauri managed
// state 而已；追加整顆外掛只換來一份用不到的 Tauri command／state 掛載，划不來。
//
// **這一組只寫檔案，一次都不呼叫 `launchctl`**——這正是 `auto_launch` 0.5 的
// `macos.rs`（也就是外掛底下真正在做事的那一層）在 LaunchAgent 模式下的作法：
// enable 就是把 plist 寫進 `~/Library/LaunchAgents`，disable 就是把它刪掉，
// 生效時機是**下一次登入**（launchd 登入時自己讀那個資料夾）。
//
// 為什麼不「順手 load 一下讓它立即生效」——那正是這一版修掉的兩個缺陷：
//
// 1. **`launchctl unload` 會殺掉 app 自己。** 這份 plist 的 `RunAtLoad` 是 true、
//    `ProgramArguments` 就是 `[<exe>, --tray]`，所以「登入時自啟進來」的那個
//    實例**本身就是這個 job 的行程**。對它 `unload`，launchd 會對我們自己送
//    SIGTERM：app 無預警死亡，`do_exit`／`kill_all_jobs` 一行都不會跑，ssh 子程序
//    （自成 pgid）當場變孤兒繼續佔著 `-L` 的本地埠，而 `unload` 之後那句刪檔
//    也永遠執行不到——下次登入照樣自啟、開關照樣顯示 ON，等於這個「關閉自啟」
//    的動作除了把 app 弄死以外什麼都沒做。
// 2. **`launchctl load -w` 會當場多開一個實例。** `RunAtLoad = true` 的 job 一被
//    load 就立刻執行 `<exe> --tray`，它被 single-instance 外掛轉成對主實例的
//    `show_main`——使用者只是在設定頁打開一個開關，畫面卻自己跳出來。
//
// 立即生效換來的好處（省下一次登入）遠不值這兩個代價，何況「開機自啟」這件事
// 本來就是在講下一次開機。Windows 那邊寫 HKCU 的 Run 值同樣是下次登入才生效，
// 兩個平台的語意因此也是對齊的。

/// LaunchAgent plist 所在資料夾：`~/Library/LaunchAgents`。
///
/// 只有這一支會碰真的使用者家目錄——底下的 `*_at` 系列全部改吃呼叫端傳進來的
/// `base: &Path`，測試才能打 tempdir 而不必污染實機或 CI runner（獨立審查
/// 2026-08-29 阻擋缺陷：預設測試輪不准碰真實系統）。
fn launch_agents_dir() -> Option<PathBuf> {
    super::paths::home_dir().map(|h| h.join("Library").join("LaunchAgents"))
}

/// 把任意 `name`（目前實際呼叫端一律是 productName，例如 "traytunnel"）轉成
/// launchd 慣用的 reverse-DNS 風格 label，同時保證能安全當檔名：非英數字元
/// 一律收斂成 `-`，大小寫也一併正規化，避免同一個邏輯名稱因大小寫不同
/// 而對到兩份不同的 plist。
fn plist_label(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    format!("com.traytunnel.autostart.{slug}")
}

/// plist 在給定 `base` 資料夾底下的完整路徑。純函式，`base` 是 `~/Library/LaunchAgents`
/// 或測試用的 tempdir，這一層不知道也不在乎是哪一種。
fn plist_path_in(base: &Path, name: &str) -> PathBuf {
    base.join(format!("{}.plist", plist_label(name)))
}

/// plist 是 XML，這幾個字元在 `<string>` 內容裡必須轉義——執行檔路徑理論上不會
/// 含這些字元，但寧可正確也不要賭使用者的安裝路徑裡沒有奇怪字元。
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// `--tray` 讓開機啟動直接縮在系統匣、不彈主視窗——與 Windows
/// `winsys::autostart_command` 的 `--tray` 是同一個約定。
///
/// ## 為什麼一定要有 `AbandonProcessGroup`
///
/// `launchd.plist(5)` 對這個鍵的原文是：
///
/// > **AbandonProcessGroup** \<boolean\>
/// > When a job dies, launchd kills any remaining processes with the same
/// > process group ID as the job. Setting this key to true disables that
/// > behavior.
///
/// 也就是說**預設**行為是「job 一死，launchd 順手把同 pgid 的殘餘程序也殺掉」。
/// 這條規則會直接砸掉應用內更新：開機自啟進來的那個實例就是這個 job 的行程，
/// 更新走到最後是 `app.restart()`——先 `spawn` 一個新實例、再讓自己 `exit`。
/// 新實例是從舊實例 fork 出來的，**繼承同一個 pgid**，於是舊實例一 exit，
/// launchd 就把剛生出來、還在初始化的新實例一起連坐殺掉：使用者按下
/// 「Restart to update」之後，程式直接消失，而且只有「這一次是開機自啟進來的」
/// 才會這樣，從 Finder 手動開的那次完全正常——症狀差異大到極難查。
///
/// 設成 true 只關掉 launchd 那一手連坐，不影響本程式自己的收尾：ssh 程序樹是由
/// [`super::spawn::ProcessSupervisor`] 明確 `killpg` 收的（自成 pgid，本來就不
/// 屬於這個 job 的 pgid），三道防線一個都沒少。
fn plist_contents(label: &str, exe: &Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{label}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{exe}</string>\n\
         \t\t<string>--tray</string>\n\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>AbandonProcessGroup</key>\n\
         \t<true/>\n\
         </dict>\n\
         </plist>\n",
        label = xml_escape(label),
        exe = xml_escape(&exe.display().to_string()),
    )
}

/// 從 plist 內容裡挖出 `ProgramArguments` 陣列，用空白接回一行，
/// 給自癒邏輯的 `.contains(exe_path)` 比對用（語意對應 Windows 的
/// `read_run_value`：回的是登記的那一行命令）。
///
/// 刻意手剝字串而不是拉一顆 plist 解析 crate：格式是我們自己寫出來的
/// （見 [`plist_contents`]），欄位固定兩個 `<string>`，不需要一整套泛用解析器。
fn read_program_arguments(contents: &str) -> Option<String> {
    let key_at = contents.find("<key>ProgramArguments</key>")?;
    let array_start = contents[key_at..].find("<array>")? + key_at + "<array>".len();
    let array_end = contents[array_start..].find("</array>")? + array_start;
    let body = &contents[array_start..array_end];

    let mut parts = Vec::new();
    let mut rest = body;
    while let Some(s) = rest.find("<string>") {
        let after = &rest[s + "<string>".len()..];
        let e = after.find("</string>")?;
        parts.push(xml_unescape(&after[..e]));
        rest = &after[e + "</string>".len()..];
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// 開機自啟目前是不是真的登記著：plist 檔案在就算數。純檔案 I/O，
/// `base` 讓測試可以傳 tempdir。
///
/// 與 Windows 的 `autostart_enabled` 不完全對稱：Windows 那邊還會再看工作管理員
/// 的 StartupApproved 停用紀錄，macOS（13 起）的「登入項目」系統設定也有等價的
/// 使用者停用機制，但要偵測它得挖 `SMAppService` 的狀態或系統的背景任務管理
/// 資料庫，複雜度與這一輪的範疇不成比例，先留給之後補（README「已知限制」
/// 有對使用者的說明）。
fn autostart_enabled_at(base: &Path, name: &str) -> bool {
    plist_path_in(base, name).is_file()
}

/// 讀 plist 裡的 `ProgramArguments`，用來判斷開機自啟項是不是還指向這支執行檔。
/// 純檔案 I/O。
fn read_autostart_command_at(base: &Path, name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(plist_path_in(base, name)).ok()?;
    read_program_arguments(&contents)
}

/// 把 LaunchAgent plist 寫進 `base` 資料夾，回傳寫出去的路徑。
/// 覆寫既有檔案就是「更新登記內容」（自癒改寫 `ProgramArguments` 走的正是這條），
/// launchd 下次登入讀到的自然是新的那一份。
fn write_autostart_plist_at(base: &Path, name: &str, exe: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(base)?;
    let path = plist_path_in(base, name);
    std::fs::write(&path, plist_contents(&plist_label(name), exe))?;
    Ok(path)
}

/// 從 `base` 資料夾刪掉 plist；冪等（本來就沒有檔案也算成功）。
fn remove_autostart_plist_at(base: &Path, name: &str) -> io::Result<()> {
    match std::fs::remove_file(plist_path_in(base, name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// 開機自啟目前是不是真的登記著。
pub fn autostart_enabled(name: &str) -> bool {
    launch_agents_dir().is_some_and(|dir| autostart_enabled_at(&dir, name))
}

/// 讀登記的命令，用來判斷開機自啟項是不是還指向這支執行檔。
pub fn read_autostart_command(name: &str) -> Option<String> {
    read_autostart_command_at(&launch_agents_dir()?, name)
}

/// App Translocation 的掛載點記號。
///
/// macOS（10.12 起）對「帶著隔離標記、又還沒被搬進正式位置」的 app 會做
/// Gatekeeper 路徑隨機化：從 dmg 視窗或 `~/Downloads` 直接雙擊時，系統不是原地
/// 執行那顆 app，而是把它掛成一份**唯讀的隨機路徑影本**再跑，`current_exe()`
/// 於是長成
///
/// ```text
/// /private/var/folders/<x>/<y>/T/AppTranslocation/<uuid>/d/Traytunnel.app/Contents/MacOS/traytunnel
/// ```
///
/// 這個路徑是**這一次執行才存在的**：app 結束、掛載點消失，下次登入時它不存在。
/// 比對整段 `/AppTranslocation/` 而不是只比 `AppTranslocation`，是為了確保比到的
/// 是一整層路徑元件，不會被某個剛好叫 `MyAppTranslocationTool` 的資料夾騙過去。
fn is_app_translocated(exe: &Path) -> bool {
    exe.to_string_lossy().contains("/AppTranslocation/")
}

/// 從 App Translocation 的唯讀影本跑起來時，寫開機自啟一律拒絕，錯誤訊息直接
/// 是給使用者看的處理方式（`commands::set_autostart` 原樣往前端送）。
fn translocation_refusal() -> io::Error {
    io::Error::other(
        "Traytunnel is running from a temporary read-only copy made by macOS App Translocation, \
         so the path it would record here no longer exists at the next login. Move Traytunnel.app \
         into the Applications folder, open it from there, and turn this on again.",
    )
}

/// 寫出（或覆寫）LaunchAgent plist，**下次登入生效**。
///
/// 不呼叫 `launchctl load`：`RunAtLoad = true` 的 job 一被 load 就會當場再跑一次
/// `<exe> --tray`，多開一個實例（理由整段寫在本節開頭）。覆寫檔案本身就足以更新
/// 登記內容——launchd 是在下次登入時才讀這個資料夾的。
///
/// **App Translocation 底下一律拒絕寫入**（見 [`is_app_translocated`]）。呼叫端
/// 傳進來的 `exe` 一律是 `current_exe()`，而在那個模式下它是一條這次執行才存在的
/// 隨機掛載點路徑：寫進 plist 等於登記一個下次登入必定不存在的執行檔，開關顯示
/// ON、實際永遠啟動不到任何東西。更糟的是自癒那條路（`lib.rs::heal_autostart`）
/// ——使用者本來有一份指向 `/Applications` 的**好** plist，只要哪天從 dmg 直接
/// 開一次，自癒就會「發現登記的命令跟現在的執行檔對不上」而主動把它覆寫成那條
/// 暫時路徑，把原本好好的自啟弄壞。所以拒絕要放在這一層：`heal_autostart` 走的
/// 也是這支函式，這一擋同時關掉手動開關與自癒兩條路，共用核心那邊不必加 cfg，
/// 只要照原樣把錯誤往上送。
///
/// 為什麼不「自己還原成真正的原始路徑」：把 translocated 路徑換算回原始位置只有
/// Security.framework 的 `SecTranslocateCreatePathForURL`／
/// `SecTranslocateCreateOriginalPathForURL` 那一組 SPI 做得到，它們不在公開 API
/// 裡；而且就算換算得回來，那個位置（`~/Downloads`、dmg 掛載點）本來就不是 app
/// 該長住的地方。老實回一句「請先搬進應用程式資料夾」才是對的答案，README 的
/// 安裝說明講的也是同一件事。
pub fn enable_autostart(name: &str, exe: &Path) -> io::Result<()> {
    if is_app_translocated(exe) {
        log::warn!(
            "refusing to write the login item: this run is an App Translocation copy ({}), \
             the path would not exist at the next login",
            exe.display()
        );
        return Err(translocation_refusal());
    }
    let dir = launch_agents_dir()
        .ok_or_else(|| io::Error::other("could not resolve $HOME for ~/Library/LaunchAgents"))?;
    write_autostart_plist_at(&dir, name, exe).map(|_| ())
}

/// 刪掉 plist，**下次登入生效**；冪等，跟 Windows `disable_autostart` 刪 Run 值的
/// 冪等語意一致。
///
/// 同樣不呼叫 `launchctl unload`：這個行程很可能**就是**那個 job 的行程，unload
/// 等於請 launchd 把我們自己殺掉（理由整段寫在本節開頭）。刪檔就夠了。
pub fn disable_autostart(name: &str) -> io::Result<()> {
    let Some(dir) = launch_agents_dir() else {
        // 問不到 $HOME，沒有地方可能登記過，視同已經是關的
        return Ok(());
    };
    remove_autostart_plist_at(&dir, name)
}

// ---------------------------------------------------------------- 開外部程式

/// 決定 `open` 要打開的目標：檔案存在就是它本身（配 `-R` 選中它），不存在時
/// （例如設定檔還沒建出來）退而開啟它所在的資料夾；連上層資料夾都沒有時退回
/// 目前目錄。`exists` 拆成參數，路徑組法才測得到（比照 Windows `explorer_arg`）。
fn reveal_target(path: &Path, exists: bool) -> PathBuf {
    if exists {
        return path.to_path_buf();
    }
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// 跑一次 `open` 並等它結束。
///
/// **一定要 `status()`（等於 fork＋wait），不可以只 `spawn()`。** `open` 只是把
/// 請求交給 LaunchServices 就立刻退出，本身不是長命程序；但這支程式沒有任何
/// 地方會去 `wait` 它——tokio 的 reaper 只認得 `tokio::process` spawn 出來的
/// 子程序，`std::process::Child` 被 drop 時預設**不**回收（`Child::drop` 明文
/// 寫著「不會 wait，可能留下殭屍」）。於是每按一次「開啟設定檔資料夾」或
/// 「Download from Releases」就在行程表裡積一隻 `<defunct>`，常駐幾天下來
/// 就是一整排。等它退出還順帶換來一個好處：`open` 失敗（檔案不存在、沒有
/// 對應的處理程式）現在會變成一個真的 `Err` 往上回，呼叫端本來就有記日誌的
/// 分支，以前那條路是完全靜默的。
///
/// **代價：這支函式會阻塞**——`open` 自己雖然很快就退出，但它退出前要等
/// LaunchServices 把目標程式叫起來，冷啟一個 Finder 視窗或瀏覽器要一到三秒。
/// 因此呼叫端不可以在主執行緒上等它，`commands.rs` 那三支指令一律把它丟進
/// `spawn_blocking`（同一份紀律的完整說明寫在那裡）。
fn run_open(cmd: &mut std::process::Command) -> io::Result<()> {
    let status = cmd.status()?;
    if !status.success() {
        return Err(io::Error::other(format!("open exited with {status}")));
    }
    Ok(())
}

/// 在 Finder 裡開啟並選中一個檔案，對應 Windows 的 `explorer.exe /select,`。
pub fn reveal_in_file_manager(path: &Path) -> io::Result<()> {
    let exists = path.exists();
    let target = reveal_target(path, exists);
    let mut cmd = std::process::Command::new("open");
    if exists {
        cmd.arg("-R");
    }
    cmd.arg(&target);
    run_open(&mut cmd)
}

/// 用系統預設瀏覽器開一個網址，對應 Windows 的 `winsys::open_url`（ShellExecuteW）。
///
/// 刻意**不**放進 `platform::mod` 那份跨平台門面：唯一的呼叫端是更新那條路
/// （`update::open_release_page`／`open_releases_page`），而 update 整個子模組本來
/// 就是平台各自提供的，門面上再開一個沒有共用核心會用的洞只是多一個死角
/// （`platform/mod.rs` 的 `reveal_in_file_manager` 那一段就是在講這件事）。
///
/// 只放行 `https://`：`open` 的第一個位置參數什麼都收——`file:///`、
/// 自訂 scheme、甚至一個本地路徑都會照開，而網址在這條路上是拼出來的
/// （版本號來自遠端的 latest.json）。呼叫端已經有 [`crate::platform::update_common::release_url`]
/// 那一層過濾，這裡是第二道，兩道都在才擋得住「有人日後多開一個呼叫端」。
pub fn open_url(url: &str) -> io::Result<()> {
    if !url.starts_with("https://") {
        return Err(io::Error::other(format!("refusing to open a non-https url: {url}")));
    }
    run_open(std::process::Command::new("open").arg(url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    /// wildcard（`0.0.0.0`）監聽者必須被看得到——這是舊版 bind 探測看不到、
    /// 這次改成被動查詢要修的語意缺口（既有的三條埠偵測契約測試在
    /// `platform/process_tests.rs`，只查字面 `127.0.0.1`／`::1`，不動；這裡
    /// 補的是 macOS 這邊 wildcard 這個額外語意，因此另外掛在這支實作自己的
    /// 測試模組底下）。用 OS 配發的 ephemeral 埠（bind 0 再讀實際埠號），
    /// 不寫死任何埠號；查表跟核心更新之間可能隔一拍，所以是輪詢＋期限，不是
    /// 綁完就當場問一次——紀律同 `process_tests.rs`。
    #[test]
    fn a_wildcard_listener_is_visible() {
        let listener = TcpListener::bind(("0.0.0.0", 0)).expect("要綁得起來");
        let port = listener.local_addr().expect("listener 一定有本地位址").port();

        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = false;
        while Instant::now() < deadline {
            if is_listening(port) {
                seen = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(listener);

        assert!(
            seen,
            "0.0.0.0:{port} 上有 TcpListener 在 LISTEN，is_listening 卻遲遲沒回 true\
             ——舊版的 bind 探測只精確比對字面 loopback 位址，看不到 wildcard 監聽者，\
             這正是這次要修的語意缺口"
        );
    }

    /// 「這次是不是 GUI 啟動」的判定：launchd 的最小集（含順序打亂、少幾個、
    /// 尾隨斜線、完全空的）都算，一旦冒出任何一個不在最小集裡的目錄就不算。
    #[test]
    fn the_launchd_default_path_is_recognised() {
        assert!(path_is_the_gui_minimum("/usr/bin:/bin:/usr/sbin:/sbin"));
        assert!(path_is_the_gui_minimum("/bin:/usr/bin"), "少幾個、順序不同一樣是最小集");
        assert!(path_is_the_gui_minimum("/usr/bin/:/bin"), "尾隨斜線不該讓判定失手");
        assert!(path_is_the_gui_minimum(""), "連 PATH 都沒有比最小集更該修");

        assert!(
            !path_is_the_gui_minimum("/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"),
            "使用者的 PATH（Homebrew 在裡面）不可以被當成 GUI 啟動，否則每次從終端機\
             啟動都要多跑一次登入 shell"
        );
        assert!(!path_is_the_gui_minimum("/usr/local/bin:/usr/bin"));
    }

    /// 互動式 shell 的 rc 檔什麼都可能往 stdout 印，標記中間那一段才是答案。
    #[test]
    fn the_login_shell_path_is_extracted_from_the_noise() {
        let noisy = format!(
            "Last login: Fri\n\u{1b}[32mwelcome\u{1b}[0m\n{PATH_BEGIN}/opt/homebrew/bin:/usr/bin{PATH_END}\n"
        );
        assert_eq!(
            extract_marked_path(&noisy).as_deref(),
            Some("/opt/homebrew/bin:/usr/bin"),
            "rc 檔的問候語與顏色碼都不可以混進 PATH"
        );

        // 沒有標記（shell 根本沒跑到我們那一行）就老實回 None，不要拿雜訊當 PATH
        assert_eq!(extract_marked_path("zsh: command not found"), None);
        assert_eq!(extract_marked_path(&format!("{PATH_BEGIN}   {PATH_END}")), None, "空的不算");
    }

    /// 要跑哪一支 shell：`SHELL` 必須是**絕對路徑**才採信。相對名字會讓
    /// `Command::new` 照 `PATH` 去找，而這條路存在的理由就是「現在的 `PATH`
    /// 是壞的」——用壞掉的 `PATH` 去找 shell，最壞會找到工作目錄底下同名的
    /// 別的東西。
    #[test]
    fn only_an_absolute_shell_path_is_trusted() {
        assert_eq!(resolve_login_shell(Some("/bin/bash")), "/bin/bash");
        assert_eq!(resolve_login_shell(Some("/opt/homebrew/bin/fish")), "/opt/homebrew/bin/fish");

        for bogus in [Some("zsh"), Some("./zsh"), Some(""), None] {
            assert_eq!(
                resolve_login_shell(bogus),
                DEFAULT_LOGIN_SHELL,
                "{bogus:?} 不是絕對路徑，必須退回系統預設的 shell"
            );
        }
    }

    /// **逾時必須涵蓋整段**（覆審擋下的缺陷）。
    ///
    /// 假的登入 shell 模擬使用者 rc 檔裡一句 `some-daemon &`：先把一支 `sleep 25`
    /// 丟到背景（它繼承同一支 stdout），再印出標記、正常退出。
    ///
    /// stdout 若是管線，`wait_with_output()` 要等**所有**寫端關閉才收得到 EOF，
    /// 於是這裡會卡滿 25 秒——遠超過 5 秒預算，而且這條路跑在任何 UI 之前，
    /// 症狀是「雙擊圖示什麼都沒發生」。導到暫存檔之後沒有 EOF 這回事，
    /// 逾時迴圈就是唯一的上界，shell 一退出就讀得到答案。
    ///
    /// 兩個斷言缺一不可：**沒有卡住**（< 6 秒），而且**真的問到了 PATH**
    /// （不是靠逾時放棄換來的快）。
    #[test]
    fn a_background_process_in_the_rc_file_cannot_blow_the_timeout() {
        let dir = std::env::temp_dir().join(format!(
            "traytunnel-test-fakeshell-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("要建得起 tempdir");
        let shell = dir.join("fake-login-shell");
        let bg_pid = dir.join("background.pid");

        // 忽略 -ilc 與腳本，照自己的劇本跑：背景程序 → 印標記 → 退出
        std::fs::write(
            &shell,
            format!(
                "#!/bin/sh\n\
                 sleep 25 &\n\
                 echo $! > '{pid}'\n\
                 echo '{PATH_BEGIN}'\n\
                 echo '/opt/homebrew/bin:/usr/bin'\n\
                 echo '{PATH_END}'\n\
                 exit 0\n",
                pid = bg_pid.display(),
            ),
        )
        .expect("寫得出假 shell");
        std::fs::set_permissions(&shell, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("要設得起執行權限");

        let started = Instant::now();
        let found = ask_shell_for_path(&shell.to_string_lossy());
        let elapsed = started.elapsed();

        // 先把背景那支收掉再斷言，測試失敗也不會在機器上留一支 sleep
        if let Ok(pid) = std::fs::read_to_string(&bg_pid) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            elapsed < Duration::from_secs(6),
            "rc 檔留下的背景程序把逾時預算撐爆了（花了 {elapsed:?}，上限是 \
             {LOGIN_SHELL_TIMEOUT:?}）——stdout 又走回管線了嗎？"
        );
        assert_eq!(
            found.as_deref(),
            Some("/opt/homebrew/bin:/usr/bin"),
            "不能只是『沒卡住』，還要真的問到 PATH；問不到代表是逾時放棄換來的快"
        );
    }

    /// 修正過的 PATH 一定要含系統目錄：使用者把 PATH 整個覆寫掉時，不可以因為
    /// 「修正」反而讓 `/usr/bin/ssh` 自己都找不到。
    #[test]
    fn the_fixed_path_always_keeps_the_system_directories() {
        let fixed = with_system_dirs("/opt/homebrew/bin");
        for dir in LAUNCHD_DEFAULT_PATH {
            assert!(fixed.split(':').any(|d| d == dir), "{dir} 應該被補回來：{fixed}");
        }
        assert!(fixed.starts_with("/opt/homebrew/bin"), "使用者自己的順序要在前面：{fixed}");

        // 本來就齊全時不重複補，也不改順序
        let already = "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin";
        assert_eq!(with_system_dirs(already), already);
        // 尾隨斜線的寫法算同一個目錄，不該再補一份
        assert_eq!(
            with_system_dirs("/usr/bin/:/bin/:/usr/sbin/:/sbin/"),
            "/usr/bin/:/bin/:/usr/sbin/:/sbin/"
        );
    }

    /// 時間戳的形狀就是日誌行的格式契約：固定八個字元的 HH:mm:ss。
    /// 對照 Windows `winsys::local_time_is_a_fixed_width_hms`。
    #[test]
    fn local_time_is_a_fixed_width_hms() {
        let ts = local_time_hms();
        assert_eq!(ts.len(), 8, "{ts}");
        let parts: Vec<&str> = ts.split(':').collect();
        assert_eq!(parts.len(), 3, "{ts}");
        let bounds = [24, 60, 60];
        for (p, max) in parts.iter().zip(bounds) {
            assert_eq!(p.len(), 2, "每段都要補到兩位：{ts}");
            assert!(p.parse::<u32>().unwrap() < max, "{ts}");
        }
    }

    /// label 一定要是安全的檔名：非英數字元收斂成 `-`，大小寫正規化。
    #[test]
    fn plist_label_is_filename_safe() {
        assert_eq!(plist_label("traytunnel"), "com.traytunnel.autostart.traytunnel");
        assert_eq!(plist_label("Traytunnel"), "com.traytunnel.autostart.traytunnel");
        assert_eq!(
            plist_label("traytunnel-test 123"),
            "com.traytunnel.autostart.traytunnel-test-123"
        );
    }

    /// plist 內容寫出去、再用 `read_program_arguments` 讀回來要是同一行命令；
    /// 純函式測試，不碰真的檔案系統或 launchctl。
    #[test]
    fn plist_round_trips_program_arguments() {
        let exe = Path::new("/Applications/Traytunnel.app/Contents/MacOS/traytunnel");
        let xml = plist_contents("com.traytunnel.autostart.traytunnel", exe);
        assert!(xml.contains("<key>Label</key>"));
        // 兩個布林鍵各自釘住，不再只斷言「檔案裡有一個 <true/>」——現在有兩個，
        // 那種寫法會讓其中一個掉了也照樣綠
        assert!(
            xml.contains("<key>RunAtLoad</key>\n\t<true/>"),
            "RunAtLoad 要是 true，否則登入時根本不會被啟動：{xml}"
        );

        let cmd = read_program_arguments(&xml).expect("要讀得回 ProgramArguments");
        assert_eq!(cmd, format!("{} --tray", exe.display()));
        assert!(cmd.ends_with(" --tray"), "少了 --tray 就會開機彈主視窗：{cmd}");
    }

    /// `AbandonProcessGroup` 是規格，不是可有可無的調味：`launchd.plist(5)` 明定
    /// 「job 一死，launchd 會把同 pgid 的殘餘程序也殺掉」，而應用內更新的
    /// `app.restart()` 正是「spawn 一個同 pgid 的新實例、然後自己 exit」——沒有
    /// 這個鍵，開機自啟進來的那個實例一更新，新舊兩個實例會一起被 launchd 收走。
    #[test]
    fn the_plist_tells_launchd_not_to_kill_the_process_group() {
        let xml = plist_contents(
            "com.traytunnel.autostart.traytunnel",
            Path::new("/Applications/Traytunnel.app/Contents/MacOS/traytunnel"),
        );
        assert!(
            xml.contains("<key>AbandonProcessGroup</key>\n\t<true/>"),
            "少了 AbandonProcessGroup，自啟實例做應用內更新時會被 launchd 連坐殺掉：{xml}"
        );
    }

    /// 路徑含 XML 特殊字元時要轉義，讀回來又要能正確還原——不然使用者裝在
    /// 一個含 `&` 的資料夾名稱底下時，plist 會是一份壞掉的 XML。
    #[test]
    fn plist_escapes_and_unescapes_special_characters() {
        let exe = Path::new("/Users/bob & alice/Traytunnel.app/Contents/MacOS/traytunnel");
        let xml = plist_contents("com.traytunnel.autostart.traytunnel", exe);
        assert!(!xml.contains("bob & alice"), "裸的 & 不是合法 XML");
        assert!(xml.contains("bob &amp; alice"));

        let cmd = read_program_arguments(&xml).unwrap();
        assert_eq!(cmd, format!("{} --tray", exe.display()));
    }

    /// 沒有 `ProgramArguments` 陣列（或不是我們自己寫出來的格式）時老實回 None，
    /// 不要瞎猜。
    #[test]
    fn read_program_arguments_is_none_for_unrelated_content() {
        assert_eq!(read_program_arguments("<plist></plist>"), None);
        assert_eq!(read_program_arguments(""), None);
    }

    /// 把一份 plist 餵給系統自己的 `plutil`，回 (成功嗎, stdout)。
    /// `-` 代表從 stdin 讀（見 `plutil(1)`）。
    fn plutil(args: &[&str], xml: &str) -> (bool, String) {
        use std::io::Write;

        let mut child = std::process::Command::new("plutil")
            .args(args)
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("macOS 上一定有 plutil");
        child
            .stdin
            .take()
            .expect("剛設成 piped")
            .write_all(xml.as_bytes())
            .expect("plist 很小，寫得進去");
        let out = child.wait_with_output().expect("plutil 要回得來");
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// 這份 plist 的 XML 是**手寫**的（[`plist_contents`] 是一串 `format!`，
    /// 刻意不拉一顆 plist 解析／產生 crate，理由在 [`read_program_arguments`]）。
    /// 手寫就要有人替它把關格式，而最權威的把關者就是系統自己的 `plutil`
    /// ——launchd 讀這個檔案用的是同一套解析器。
    ///
    /// 兩段：整份 lint 過得了，以及讓 `plutil` 自己把那三個鍵讀出來（不是我們
    /// 用 `contains` 看字串長得像不像，而是真的被解析成正確的型別與值）。
    #[test]
    fn the_plist_is_what_launchd_will_actually_parse() {
        let exe = Path::new("/Applications/Traytunnel.app/Contents/MacOS/traytunnel");
        let xml = plist_contents("com.traytunnel.autostart.traytunnel", exe);

        let (ok, _) = plutil(&["-lint"], &xml);
        assert!(ok, "plutil 認不得我們寫出來的 plist：\n{xml}");

        // `raw` 讓布林印成 true／false 而不是 <true/>
        assert_eq!(
            plutil(&["-extract", "AbandonProcessGroup", "raw", "-o", "-"], &xml).1,
            "true",
            "AbandonProcessGroup 要真的被解析成布林 true（M3）"
        );
        assert_eq!(plutil(&["-extract", "RunAtLoad", "raw", "-o", "-"], &xml).1, "true");
        assert_eq!(
            plutil(&["-extract", "ProgramArguments.1", "raw", "-o", "-"], &xml).1,
            "--tray",
            "第二個 argv 要是 --tray，否則開機會彈主視窗"
        );
    }

    /// App Translocation 的偵測：路徑裡有整整一層 `AppTranslocation` 才算，
    /// 名字裡剛好含這個字串的一般資料夾不算。
    #[test]
    fn app_translocation_paths_are_recognised() {
        assert!(is_app_translocated(Path::new(
            "/private/var/folders/9x/abc/T/AppTranslocation/8B1F-4/d/Traytunnel.app/Contents/MacOS/traytunnel"
        )));

        assert!(!is_app_translocated(Path::new(
            "/Applications/Traytunnel.app/Contents/MacOS/traytunnel"
        )));
        assert!(
            !is_app_translocated(Path::new("/Users/bob/AppTranslocationNotes/traytunnel")),
            "只是名字裡含這個字串的資料夾不是掛載點"
        );
        assert!(!is_app_translocated(Path::new(
            "/Users/bob/dev/traytunnel/target/debug/traytunnel"
        )));
    }

    /// 從 App Translocation 的唯讀影本跑起來時，`enable_autostart` 一定要拒絕：
    /// 那條路徑下次登入不存在，寫進去等於「開關顯示 ON、其實永遠啟動不到」。
    /// 自癒（`lib.rs::heal_autostart`）走的也是這支函式，因此同一擋也讓它不會把
    /// 使用者原本指向 /Applications 的好 plist 覆寫掉。
    ///
    /// 這一條不碰檔案系統：拒絕發生在解析 `~/Library/LaunchAgents` 之前，
    /// 所以就算 `$HOME` 是真的家目錄也不會寫出任何東西。
    #[test]
    fn autostart_is_refused_under_app_translocation() {
        let translocated = Path::new(
            "/private/var/folders/9x/abc/T/AppTranslocation/8B1F-4/d/Traytunnel.app/Contents/MacOS/traytunnel",
        );
        let err = enable_autostart("traytunnel-test-translocation", translocated)
            .expect_err("App Translocation 底下必須拒絕寫入");
        let msg = err.to_string();
        assert!(msg.contains("App Translocation"), "訊息要說得出原因：{msg}");
        assert!(
            msg.contains("Applications folder"),
            "訊息要直接告訴使用者怎麼處理（搬進應用程式資料夾）：{msg}"
        );
    }

    /// 檔案存在就選中它本身；不存在就退而開啟上層資料夾；
    /// 連上層資料夾都沒有（相對路徑、無 parent）時退回目前目錄。
    #[test]
    fn reveal_target_falls_back_to_the_parent_folder() {
        let file = Path::new("/Users/bob/Library/Application Support/traytunnel.toml");
        assert_eq!(reveal_target(file, true), file);
        assert_eq!(reveal_target(file, false), Path::new("/Users/bob/Library/Application Support"));
        assert_eq!(reveal_target(Path::new("traytunnel.toml"), false), Path::new("."));
    }

    /// 開機自啟的檔案管理（不含 launchctl）走一輪完整往返：寫 plist、讀出已登記、
    /// 讀回命令、刪檔、讀出未登記——全部打 tempdir。**不碰真的
    /// `~/Library/LaunchAgents`，也不呼叫 launchctl**（獨立審查 2026-08-29 阻擋
    /// 缺陷：預設測試輪不准碰真實系統，只有手動的 `live_autostart_round_trips_
    /// through_launchd` 才准）。
    #[test]
    fn autostart_files_round_trip_in_a_tempdir() {
        let base = std::env::temp_dir().join(format!(
            "traytunnel-test-launchagents-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let name = "traytunnel-test";
        let exe = Path::new("/Applications/Traytunnel.app/Contents/MacOS/traytunnel");

        // 乾淨狀態：tempdir 一開始就是空的
        assert!(!autostart_enabled_at(&base, name));
        assert_eq!(read_autostart_command_at(&base, name), None);

        let path = write_autostart_plist_at(&base, name, exe).expect("寫 plist 應該要成功");
        assert!(path.is_file());
        assert!(autostart_enabled_at(&base, name));
        let cmd = read_autostart_command_at(&base, name).expect("寫完要讀得回登記的命令");
        assert_eq!(cmd, format!("{} --tray", exe.display()));

        remove_autostart_plist_at(&base, name).expect("刪除應該要成功");
        assert!(!autostart_enabled_at(&base, name));
        assert_eq!(read_autostart_command_at(&base, name), None);

        // 刪除是冪等的，重複呼叫不算錯
        remove_autostart_plist_at(&base, name).expect("重複刪除仍要成功");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 開機自啟一輪完整的實機操作：寫真的 `~/Library/LaunchAgents/<label>.plist`、
    /// 讀回命令、刪檔。**會動使用者真的家目錄**，比照 `wg_live_tests`／
    /// `exits::live_probe` 的慣例刻意 `#[ignore]`，預設測試輪不跑，只有手動指定
    /// 才會跑：
    ///
    /// cargo test --lib -- --ignored --nocapture live_autostart
    ///
    /// 這一條**不再往返 launchd**：`enable_autostart`／`disable_autostart` 現在
    /// 只寫／刪檔案，一次都不呼叫 `launchctl`（理由見「開機自啟」那一節開頭
    /// ——`unload` 會殺掉 app 自己，`load -w` 會當場多開一個實例）。因此能測、
    /// 也只該測的就是檔案語意；沿用舊名字反而會讓人以為還有一段 launchd 往返
    /// 沒被驗到，於是改名。留著它而不是刪掉，是因為它仍然是唯一一條會走
    /// `launch_agents_dir()`（真的 `$HOME`）的路——`autostart_files_round_trip_in_a_tempdir`
    /// 打的是 tempdir，驗不到「家目錄解析＋建資料夾」這一段。
    ///
    /// 比照 Windows `hkcu_value_round_trip`，測試名稱帶 pid 避免撞到使用者真正
    /// 的登記項，收尾一定會把測試用的 plist 清掉；但中途 assert 失敗仍可能跳過
    /// 收尾，這正是這條不准留在預設測試輪的理由。
    #[test]
    #[ignore]
    fn live_autostart_round_trips_through_the_launch_agents_folder() {
        let name = format!("traytunnel-test-{}", std::process::id());
        let exe = std::env::current_exe().expect("測試需要拿得到自己的執行檔路徑");

        // 乾淨狀態：這個測試名稱本來就不該登記過
        assert!(!autostart_enabled(&name));
        assert_eq!(read_autostart_command(&name), None);

        enable_autostart(&name, &exe).expect("啟用開機自啟應該要成功");
        assert!(autostart_enabled(&name));
        let cmd = read_autostart_command(&name).expect("啟用後要讀得回登記的命令");
        assert!(
            cmd.to_lowercase().contains(&exe.to_string_lossy().to_lowercase()),
            "登記的命令要指向測試自己的執行檔：{cmd}"
        );
        assert!(cmd.ends_with(" --tray"));

        disable_autostart(&name).expect("停用開機自啟應該要成功");
        assert!(!autostart_enabled(&name));
        assert_eq!(read_autostart_command(&name), None);

        // 停用是冪等的，重複呼叫不算錯
        disable_autostart(&name).expect("重複停用仍要成功");
    }

    // `pick_icon_layer` 本身的測試（完全相符優先、沒有專用層時的退讓方向、超過
    // 最大層與空清單）已隨函式本體搬到 `crate::appicon`，與 Windows 版合併保留
    // 兩邊的斷言資料，不在這裡重複一份。

    /// 這台機器兩種圖示尺寸的合理性：與 Windows 版
    /// `metrics_are_sane_on_this_machine` 同樣的斷言，只是這裡的值是固定常數
    #[test]
    fn metrics_are_sane_on_this_machine() {
        let (sw, sh) = small_icon_size();
        let (lw, lh) = large_icon_size();
        assert_eq!(sw, sh, "小圖示應為正方");
        assert_eq!(lw, lh, "大圖示應為正方");
        assert!(sw >= 16 && lw >= 32, "small={sw} large={lw}");
        assert!(lw >= sw && lh >= sh, "大圖示不該小於小圖示");
    }
}
