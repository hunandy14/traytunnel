//! 系統查詢與系統動作。對照組是 `platform/windows/winsys.rs`。
//!
//! `is_listening`（A 車道）與圖示尺寸／挑層（C 車道）都已在 `agent/feature/macos`
//! 落地（分別見 `feat/macos-process-mgmt`／`feat/macos-tray-window` 合併）。

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- 本地埠偵測

/// 本地是否有程序在該埠 Listen（相當於 Windows 那邊查 `GetExtendedTcpTable`）。
///
/// 手段：對 `127.0.0.1:port` 與 `::1:port` 各自嘗試 `bind`。
/// 這是被動的——全程只跟本機核心的 socket 表打交道，從未對任何位址送出過一個
/// TCP 封包，因此不會對「port 上真的有服務在跑」這件事產生任何 connect 探測
/// 才有的副作用（不會被算進對方的連線數、不會觸發對方的日誌或速率限制）。
///
/// 判定依據：`SO_REUSEADDR`（Rust 的 `TcpListener::bind` 在 Unix 上預設會開）
/// 只放寬「舊連線還卡在 TIME_WAIT」這件事，並不允許兩個 socket 同時
/// `bind`＋`listen` 在同一個位址＋埠上——因此「bind 失敗且錯誤是
/// `AddrInUse`」精準對應「這個位址＋埠上已經有一個 LISTEN 在佔著」；
/// bind 成功則代表沒人佔，順手把這個探測用的 listener 立刻收掉，不留著佔用。
///
/// 兩個位址都查是因為 ssh 的 `-L`（沒指定 bind 位址時）在雙棧主機上一般會同時
/// 綁 `127.0.0.1` 與 `::1`；本專案自己的 SOCKS5 監聽器只綁 `127.0.0.1`
/// （見 `wg::socks5::BIND_ADDR`），但 `is_listening` 是共用門面，兩邊的呼叫端
/// 都吃同一份答案，查全了才不會漏掉 ssh 只綁到 `::1` 的情況。
///
/// 沒有另外選 `lsof` 子程序解析或手刻 `sysctl` 的 pcblist 解析：前者每次呼叫
/// 都是一次 fork/exec 加上核心逐一列舉行程的開銷，這支函式是隧道監看迴圈
/// 每輪都會呼叫的輪詢熱路徑，不值得為了問一個埠的狀態付出那個代價；後者要手刻
/// unsafe 的 `xinpgen`／`xtcpcb` 結構體解析，對應的是 Apple 沒有公開穩定文件的
/// 核心內部格式，維護風險遠高於一組標準函式庫就能做到的 `bind` 探測。
///
/// **與 Windows 版語意上的落差**：`GetExtendedTcpTable` 列的是系統上所有
/// LISTEN 項目，含綁在 `0.0.0.0`／`[::]`（wildcard，涵蓋所有介面）的那些；
/// 這裡的 bind 探測只精確比對 `127.0.0.1`／`::1` 這兩個字面位址，**看不到**
/// 綁在 wildcard 位址上、但同樣會接受 loopback 連線的佔用者——本專案自己的
/// 監聽器（SOCKS5、ssh `-L`）一律只綁字面 loopback，不受影響，但如果哪天
/// 有別的程式改成綁 `0.0.0.0` 佔住同一個埠，這支函式會誤判成「沒人聽」。
///
/// 這個誤判不是沒有安全網：`ssh::tunnel::build_exit_args` 固定帶
/// `ExitOnForwardFailure=yes`（見 `tunnel.rs`），埠真的被佔住時 ssh 自己
/// `bind` 會失敗、直接退出，監看迴圈照樣會在下一輪判定成 disconnected 並重試
/// ——不會是「顯示 connected 但其實沒轉發」這種更難查的錯。這條結論**依賴**
/// 那個 ssh 參數；拿掉它，這裡漏掉的 wildcard 佔用就會變成真正的靜默失敗。
pub fn is_listening(port: u16) -> bool {
    bound_by_someone_else(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        || bound_by_someone_else(SocketAddr::from((Ipv6Addr::LOCALHOST, port)))
}

/// 對單一位址做一次 bind 探測。`Ok` 代表沒人佔，探測用的 listener 隨函式結束
/// 一起 drop 掉；`AddrInUse` 代表已經有人在 LISTEN；其餘錯誤一律當「沒有偵測到
/// LISTEN」，不要把不相干的錯誤誤判成佔用，害 spawn 前的埠檢查卡住一條原本可以
/// 走的隧道。
///
/// 這個「其餘錯誤一律當沒偵測到」本身是一個已知的限制，不是隨手忽略：
/// `AddrNotAvailable`（例如這台機器根本沒有可用的 IPv6 堆疊）是預期中會出現
/// 的訊號，不必聲張；但如果呼叫端把本地埠設在 1024 以下的特權範圍
/// （例如 443／80），這個沒有 root 權限的行程去 `bind` 會拿到
/// `PermissionDenied`，而不是 `AddrInUse`——那個埠即使真的有人在 LISTEN，
/// 這裡也一樣會回 false，讓 `CONNECTED` 判定永遠不觸發。這種錯誤仍然值得
/// 留一筆 `debug` 級的紀錄，讓排查的人查得到「這裡其實沒判斷出結果」，
/// 而不是無聲無息地當成「沒人聽」。
fn bound_by_someone_else(addr: SocketAddr) -> bool {
    match TcpListener::bind(addr) {
        Ok(_listener) => false,
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => true,
        Err(e) => {
            if e.kind() != io::ErrorKind::AddrNotAvailable {
                log::debug!(
                    "is_listening: probing {addr} failed with {e} ({:?}), treating as not listening",
                    e.kind()
                );
            }
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

/// 從一組尺寸裡挑最接近 `want` 的一層，回傳索引。
///
/// 純數字邏輯，不靠任何系統 API，故意與 Windows 版
/// （`platform::windows::winsys::pick_icon_layer`）用同一套演算法：完全相符優先；
/// 沒有就取「大於它的最小一層」（縮小而不是放大，縮小遠比放大乾淨）；再沒有就
/// 退而取最大的一層。macOS 沒有 per-monitor DPI 挑層的問題，這裡的 `want` 只來自
/// `small_icon_size`／`large_icon_size` 這兩個固定值，不會隨螢幕或執行時狀態變動，
/// 因此不必像 Windows 版那樣另外查詢系統度量。
pub fn pick_icon_layer(sizes: &[u32], want: u32) -> Option<usize> {
    if sizes.is_empty() {
        return None;
    }
    if let Some(exact) = sizes.iter().position(|s| *s == want) {
        return Some(exact);
    }
    let bigger = sizes
        .iter()
        .enumerate()
        .filter(|(_, s)| **s > want)
        .min_by_key(|(_, s)| **s)
        .map(|(i, _)| i);
    bigger.or_else(|| sizes.iter().enumerate().max_by_key(|(_, s)| **s).map(|(i, _)| i))
}

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
/// （版本號來自遠端的 latest.json）。呼叫端已經有 [`super::update::release_url`]
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

    /// 圖示工廠產出的層序，測試照著它走（與 `appicon.rs` 內嵌的那顆 ICO 同一份）
    const LAYERS: [u32; 9] = [16, 20, 24, 28, 32, 48, 64, 128, 256];

    /// 完全相符的層優先
    #[test]
    fn exact_layer_wins() {
        assert_eq!(pick_icon_layer(&LAYERS, 16), Some(0));
        assert_eq!(pick_icon_layer(&LAYERS, 64), Some(6));
    }

    /// 沒有專用層時寧可讓系統縮小，也不要放大
    #[test]
    fn falls_back_to_the_next_size_up() {
        assert_eq!(pick_icon_layer(&LAYERS, 44), Some(5)); // 44 -> 48
        assert_eq!(pick_icon_layer(&LAYERS, 20), Some(1));
    }

    /// 要的比所有層都大時只能拿最大的那層；空清單回 None
    #[test]
    fn falls_back_to_the_largest_layer() {
        assert_eq!(pick_icon_layer(&LAYERS, 1024), Some(8));
        assert_eq!(pick_icon_layer(&[], 16), None);
    }

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
