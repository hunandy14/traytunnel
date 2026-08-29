//! 系統查詢與系統動作。對照組是 `platform/windows/winsys.rs`。
//!
//! `is_listening`（A 車道）與圖示尺寸／挑層（C 車道）都已在 `agent/feature/macos`
//! 落地（分別見 `feat/macos-process-mgmt`／`feat/macos-tray-window` 合併）。

use std::io;
use std::path::{Path, PathBuf};

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

/// 寫出（或覆寫）LaunchAgent plist，**下次登入生效**。
///
/// 不呼叫 `launchctl load`：`RunAtLoad = true` 的 job 一被 load 就會當場再跑一次
/// `<exe> --tray`，多開一個實例（理由整段寫在本節開頭）。覆寫檔案本身就足以更新
/// 登記內容——launchd 是在下次登入時才讀這個資料夾的。
pub fn enable_autostart(name: &str, exe: &Path) -> io::Result<()> {
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

    use crate::platform::process_tests::{poll_until, DEADLINE};

    /// wildcard（`0.0.0.0`）監聽者必須被看得到——這是舊版 bind 探測看不到、
    /// 這次改成被動查詢要修的語意缺口（既有的三條埠偵測契約測試在
    /// `platform/process_tests.rs`，只查字面 `127.0.0.1`／`::1`，不動；這裡
    /// 補的是 macOS 這邊 wildcard 這個額外語意，因此另外掛在這支實作自己的
    /// 測試模組底下）。用 OS 配發的 ephemeral 埠（bind 0 再讀實際埠號），
    /// 不寫死任何埠號；查表跟核心更新之間可能隔一拍，所以是輪詢＋期限，不是
    /// 綁完就當場問一次——輪詢機制借用 `process_tests` 那份（`poll_until`／
    /// `DEADLINE` 升成 `pub(super)`），不必自己另外手刻一份一模一樣的迴圈。
    #[test]
    fn a_wildcard_listener_is_visible() {
        let listener = TcpListener::bind(("0.0.0.0", 0)).expect("要綁得起來");
        let port = listener.local_addr().expect("listener 一定有本地位址").port();

        let seen = poll_until(DEADLINE, || is_listening(port));
        drop(listener);

        assert!(
            seen,
            "0.0.0.0:{port} 上有 TcpListener 在 LISTEN，is_listening 卻遲遲沒回 true\
             ——舊版的 bind 探測只精確比對字面 loopback 位址，看不到 wildcard 監聽者，\
             這正是這次要修的語意缺口"
        );
    }

    // `local_time_is_a_fixed_width_hms` 與 Windows 逐字相同，已搬到
    // `platform::process_tests`（跨平台契約容器），不在這裡重複一份。

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
        assert!(xml.contains("<true/>"), "RunAtLoad 要是 true");

        let cmd = read_program_arguments(&xml).expect("要讀得回 ProgramArguments");
        assert_eq!(cmd, format!("{} --tray", exe.display()));
        assert!(cmd.ends_with(" --tray"), "少了 --tray 就會開機彈主視窗：{cmd}");
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

    // `metrics_are_sane_on_this_machine` 與 Windows 逐字相同（只差一句失敗訊息
    // 的措辭），已搬到 `platform::process_tests`（跨平台契約容器），不在這裡
    // 重複一份。
}
