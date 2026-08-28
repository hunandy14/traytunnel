//! 系統查詢與系統動作的 stub。對照組是 `platform/windows/winsys.rs`。

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};
use std::path::Path;

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

/// W3：Windows 是 `GetLocalTime`。macOS 沒有等價的 Win32 呼叫，
/// 要嘛拉一顆時間 crate，要嘛自己走 `libc::localtime_r`。
pub fn local_time_hms() -> String {
    todo!("W3: macOS 的本地時間戳尚未實作")
}

// ---------------------------------------------------------------- 圖示與 DPI

/// W3：macOS 的系統匣圖示是 template image，尺寸規則與 Windows 的
/// `SM_CXSMICON` 完全不同（點數固定、由 backing scale 決定像素）。
pub fn small_icon_size() -> (u32, u32) {
    todo!("W3: macOS 的系統匣圖示尺寸尚未決定")
}

/// W3：macOS 沒有「視窗大圖示」這個概念（Dock 圖示走 app bundle 的 icns）。
pub fn large_icon_size() -> (u32, u32) {
    todo!("W3: macOS 的視窗圖示尺寸尚未決定")
}

/// W3：挑層規則本身是純邏輯，但要挑的是哪一種資源（ICO 層還是 icns）
/// 得等 macOS 的圖示方案定案。
pub fn pick_icon_layer(_sizes: &[u32], _want: u32) -> Option<usize> {
    todo!("W3: macOS 的圖示挑層尚未實作")
}

// ---------------------------------------------------------------- 開機自啟

/// W3：macOS 走 `~/Library/LaunchAgents/<label>.plist`
/// （或 `SMAppService`，看最低支援版本怎麼訂）。
pub fn autostart_enabled(_name: &str) -> bool {
    todo!("W3: macOS 的開機自啟尚未實作")
}

/// W3：對應 Windows 讀 HKCU Run 值，macOS 這邊是讀 plist 裡的
/// `ProgramArguments`，回一行給自癒邏輯比對。
pub fn read_autostart_command(_name: &str) -> Option<String> {
    todo!("W3: macOS 的開機自啟尚未實作")
}

/// W3：寫出 LaunchAgent plist 並 `launchctl load`（或 `SMAppService::register`）。
pub fn enable_autostart(_name: &str, _exe: &Path) -> io::Result<()> {
    Err(io::Error::other("autostart is not implemented on macOS yet"))
}

/// W3：`launchctl unload` 並刪掉 plist。
pub fn disable_autostart(_name: &str) -> io::Result<()> {
    Err(io::Error::other("autostart is not implemented on macOS yet"))
}

// ---------------------------------------------------------------- 開外部程式

/// W3：`open -R <path>`（或 `NSWorkspace::activateFileViewerSelecting`）。
///
/// 「開瀏覽器」不在這裡：那件事只有 [`super::update`] 用得到，一併留給它。
pub fn reveal_in_file_manager(_path: &Path) -> io::Result<()> {
    Err(io::Error::other("revealing a file in Finder is not implemented on macOS yet"))
}
