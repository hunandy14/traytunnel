//! Windows 專用：Job Object 與本地埠 Listen 偵測。
//!
//! ssh 的 ProxyCommand 會再生出 cloudflared 之類的子程序，單純 kill ssh 會留孤兒，
//! 因此把 ssh 放進帶 KILL_ON_JOB_CLOSE 的 job，關掉 handle 就整棵樹一起收掉。
//! 主程式崩潰或被強制結束時 handle 也會被系統關閉，同樣不會留下孤兒。

use std::io;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE, NO_ERROR,
    SYSTEMTIME,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
    TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_SET_VALUE, REG_BINARY, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON,
};

/// 以 isize 保存 handle，讓型別自然是 Send + Sync。
#[derive(Debug)]
pub struct Job(isize);

impl Job {
    pub fn new() -> io::Result<Job> {
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                let err = io::Error::last_os_error();
                CloseHandle(handle);
                return Err(err);
            }
            Ok(Job(handle as isize))
        }
    }

    pub fn assign(&self, process: isize) -> io::Result<()> {
        unsafe {
            if AssignProcessToJobObject(self.0 as HANDLE, process as HANDLE) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0 as HANDLE);
        }
    }
}

/// 本地是否有程序在該埠 Listen（相當於 `Get-NetTCPConnection -State Listen`）。
/// IPv4 與 IPv6 都查，ssh 綁在 ::1 的情況一樣認得。
pub fn is_listening(port: u16) -> bool {
    listening_v4(port) || listening_v6(port)
}

/// dwLocalPort 低兩個位元組是網路位元組序
fn local_port(raw: u32) -> u16 {
    (((raw & 0xff) << 8) | ((raw >> 8) & 0xff)) as u16
}

/// 問長度與真正取表是兩次呼叫，中間表可能又長大了（別的程序剛開了 socket）。
/// 那時第二次會回 ERROR_INSUFFICIENT_BUFFER 並告訴我們新的長度，照著再試一次
/// 就好；重試次數給得夠寬，仍然失敗才放棄。
const TABLE_ATTEMPTS: usize = 4;

/// 取一份 listener 表的原始位元組。
///
/// 緩衝以 u64 為單位配置：MIB_TCPTABLE_OWNER_PID 與 MIB_TCP6TABLE_OWNER_PID
/// 都只由 DWORD 與位元組陣列組成，u64 的對齊必定足夠，指標轉型才不會踩到
/// 未對齊讀取（`Vec<u8>` 只保證 1 位元組對齊）。
fn listener_table(family: u32) -> Option<Vec<u64>> {
    unsafe {
        let mut size: u32 = 0;
        let rc = GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut size,
            0,
            family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if (rc != NO_ERROR && rc != ERROR_INSUFFICIENT_BUFFER) || size == 0 {
            return None;
        }
        for _ in 0..TABLE_ATTEMPTS {
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            let mut len = size;
            let rc = GetExtendedTcpTable(
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                &mut len,
                0,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            );
            match rc {
                NO_ERROR => return Some(buf),
                // 表長大了，照它給的新長度再配一次
                ERROR_INSUFFICIENT_BUFFER if len > size => size = len,
                _ => return None,
            }
        }
        None
    }
}

fn listening_v4(port: u16) -> bool {
    let Some(buf) = listener_table(AF_INET as u32) else {
        return false;
    };
    unsafe {
        let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
        let rows = std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
        rows.iter().any(|r| local_port(r.dwLocalPort) == port)
    }
}

fn listening_v6(port: u16) -> bool {
    let Some(buf) = listener_table(AF_INET6 as u32) else {
        return false;
    };
    unsafe {
        let table = &*(buf.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID);
        let rows = std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
        rows.iter().any(|r| local_port(r.dwLocalPort) == port)
    }
}

/// 本地時間的 `HH:mm:ss`，活動日誌每一行的時間戳。
///
/// 只為了這一個格式扛一整包 chrono 不划算，時區換算交給 Windows 自己做：
/// `GetLocalTime` 回的就是使用者當下時區的牆上時間。
pub fn local_time_hms() -> String {
    unsafe {
        let mut t: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut t);
        format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
    }
}

/// 系統匣圖示這台機器實際要的像素尺寸。
///
/// 100% DPI 是 16，175% 就是 28——Windows 會照這個尺寸向 tray-icon 要圖，
/// 給錯尺寸就由 GDI 拉伸，這是高 DPI 下圖示模糊的根源。
pub fn small_icon_size() -> (u32, u32) {
    metrics(SM_CXSMICON, SM_CYSMICON, (16, 16))
}

/// 視窗「大圖示」這台機器實際要的像素尺寸。
///
/// 工作列的視窗按鈕、Alt+Tab 與 ICON_BIG 取的都是這個尺寸：100% DPI 是 32，
/// 175% 就是 56。Tauri codegen 的 `default_window_icon()` 只給 ICO 的第一層
/// （我們的第一層是 16px），交給 GDI 從 16 拉到 56 就是工作列圖示模糊的原因。
pub fn large_icon_size() -> (u32, u32) {
    metrics(SM_CXICON, SM_CYICON, (32, 32))
}

/// 取一組系統度量，優先走 DPI 版本。
///
/// `GetSystemMetrics` 回的是行程 DPI awareness 脈絡下的值，混合 DPI 的機器上不一定
/// 是目前螢幕要的；`GetSystemMetricsForDpi` 配上 `GetDpiForSystem` 至少能拿到系統
/// DPI 下的正確值。兩者都失敗（回 0 或負數）才退回硬編碼的 100% DPI 尺寸。
fn metrics(cx: i32, cy: i32, fallback: (u32, u32)) -> (u32, u32) {
    unsafe {
        let dpi = GetDpiForSystem();
        let (mut w, mut h) = if dpi > 0 {
            (GetSystemMetricsForDpi(cx, dpi), GetSystemMetricsForDpi(cy, dpi))
        } else {
            (0, 0)
        };
        if w <= 0 || h <= 0 {
            w = GetSystemMetrics(cx);
            h = GetSystemMetrics(cy);
        }
        if w <= 0 || h <= 0 {
            fallback
        } else {
            (w as u32, h as u32)
        }
    }
}

/// 從多層 ICO 的尺寸清單裡挑一層，回傳索引。
///
/// 優先完全相符（完全不縮放）；沒有就取「大於它的最小一層」，讓系統縮小而不是
/// 放大（縮小遠比放大乾淨）；再沒有就退而取最大的一層。
pub fn pick_icon_layer(sizes: &[u32], want: u32) -> Option<usize> {
    if sizes.is_empty() {
        return None;
    }
    let exact = sizes.iter().position(|s| *s == want);
    if exact.is_some() {
        return exact;
    }
    let bigger = sizes
        .iter()
        .enumerate()
        .filter(|(_, s)| **s > want)
        .min_by_key(|(_, s)| **s)
        .map(|(i, _)| i);
    bigger.or_else(|| sizes.iter().enumerate().max_by_key(|(_, s)| **s).map(|(i, _)| i))
}

/// 在 HKCU 底下寫一個字串值，subkey 不存在就建出來。
pub fn write_hkcu_string(subkey: &str, name: &str, data: &str) -> io::Result<()> {
    let payload = wide(data);
    // UTF-16 的位元組數，含結尾的 NUL
    let bytes =
        unsafe { std::slice::from_raw_parts(payload.as_ptr() as *const u8, payload.len() * 2) };
    write_hkcu_value(subkey, name, REG_SZ, bytes)
}

/// 在 HKCU 底下寫一個位元組值（REG_BINARY），subkey 不存在就建出來。
fn write_hkcu_binary(subkey: &str, name: &str, data: &[u8]) -> io::Result<()> {
    write_hkcu_value(subkey, name, REG_BINARY, data)
}

/// 寫值的共同骨架：開（或建）subkey、寫一個值、關 handle。
fn write_hkcu_value(subkey: &str, name: &str, kind: u32, data: &[u8]) -> io::Result<()> {
    let sub = wide(subkey);
    let value = wide(name);
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        let rc = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            0,
            std::ptr::null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        );
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        let rc = RegSetValueExW(key, value.as_ptr(), 0, kind, data.as_ptr(), data.len() as u32);
        RegCloseKey(key);
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
    }
    Ok(())
}

/// 刪掉 HKCU 底下的一個值；值本來就不存在時視同成功（刪除是冪等的）。
pub fn delete_hkcu_value(subkey: &str, name: &str) -> io::Result<()> {
    let sub = wide(subkey);
    let value = wide(name);
    unsafe {
        let rc = RegDeleteKeyValueW(HKEY_CURRENT_USER, sub.as_ptr(), value.as_ptr());
        if rc != ERROR_SUCCESS && rc != ERROR_FILE_NOT_FOUND {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
    }
    Ok(())
}

/// 刪掉 HKCU 底下的一個 subkey（只用在測試收尾）
#[cfg(test)]
pub fn delete_hkcu_key(subkey: &str) -> io::Result<()> {
    use windows_sys::Win32::System::Registry::RegDeleteKeyW;
    let sub = wide(subkey);
    unsafe {
        let rc = RegDeleteKeyW(HKEY_CURRENT_USER, sub.as_ptr());
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
    }
    Ok(())
}

/// 在檔案總管裡開啟並選中一個檔案。
///
/// `explorer.exe /select,<path>` 的逗號後面不能再多一個空白，路徑本身又可能含空白，
/// 所以整段命令列自己組（`raw_arg`）而不是交給 `arg()` 逐段加引號。
/// 檔案還不存在（例如剛被使用者刪掉）時退而開啟它所在的資料夾。
/// explorer 是 GUI 程式，仍加上 CREATE_NO_WINDOW 避免主控台視窗一閃而過。
pub fn reveal_in_explorer(path: &std::path::Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    std::process::Command::new("explorer.exe")
        .raw_arg(explorer_arg(path, path.is_file()))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

/// 用系統預設的瀏覽器開一個網址。
///
/// 走 ShellExecuteW 而不是 `explorer.exe <url>` 或 `cmd /c start`：那兩條都要
/// 另外生一個程序，命令列引號與 `&` 的轉義規則也各有各的坑。ShellExecuteW 是
/// Windows 開啟關聯程式的正規做法（tauri-plugin-updater 自己叫安裝程式用的
/// 也是它），呼叫端傳進來的又只有寫死的常數網址，沒有注入面。
pub fn open_url(url: &str) -> io::Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb = wide("open");
    let file = wide(url);
    let rc = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // 舊式 API：回傳值大於 32 才算成功，小於等於 32 的那個數字本身就是錯誤碼
    let code = rc as isize;
    if code <= 32 {
        return Err(io::Error::other(format!("ShellExecuteW failed with code {code}")));
    }
    Ok(())
}

/// 組 explorer 的命令列。`exists` 拆成參數，路徑組法才測得到。
///
/// 相對路徑的 parent 是空字串（不是 None），直接丟給 explorer 會變成空引號，
/// 因此空的也要收斂成目前目錄。
fn explorer_arg(path: &std::path::Path, exists: bool) -> String {
    if exists {
        return format!("/select,\"{}\"", path.display());
    }
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => std::path::Path::new("."),
    };
    format!("\"{}\"", dir.display())
}

/// Rust 字串轉成結尾帶 NUL 的 UTF-16
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ------------------------------------------------------------------ 開機自啟
//
// 直接操作 HKCU 的 Run 登錄項，不經 tauri-plugin-autostart。
// 值的名稱與內容格式與該外掛相容，既有使用者升級後不必重設。

/// 開機自啟的登錄項所在
const RUN_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/// 工作管理員的「啟動」分頁把使用者的停用寫在這裡，Run 值仍在但不會被執行。
/// 判斷「開機自啟是不是真的開著」必須連它一起看，否則設定頁會顯示 ON 卻沒作用。
const APPROVED_SUBKEY: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";

/// 工作管理員記「啟用」用的那 12 個位元組；判斷只看後 8 個是不是全 0
/// （後 8 位元組是停用時間的 FILETIME，啟用時為 0）
const APPROVED_ENABLED: [u8; 12] = [0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// 開機自啟要寫進登錄的那一行命令。
///
/// 路徑一定要加引號（使用者名稱或安裝路徑含空白時，沒引號的那一版會被 Windows
/// 從空白處截斷），`--tray` 則是讓開機啟動直接縮在系統匣、不彈主視窗。
pub fn autostart_command(exe: &std::path::Path) -> String {
    format!("\"{}\" --tray", exe.display())
}

/// 開機自啟目前是不是真的會生效：Run 值在，而且沒有被工作管理員停用
pub fn autostart_enabled(name: &str) -> bool {
    if read_run_value(name).is_none() {
        return false;
    }
    approved_enabled(name)
}

/// 工作管理員那邊沒有紀錄就當成啟用（大多數情況），有紀錄才看後 8 個位元組
fn approved_enabled(name: &str) -> bool {
    let Some(bytes) = read_hkcu_binary(APPROVED_SUBKEY, name) else {
        return true;
    };
    if bytes.len() < 8 {
        return true;
    }
    bytes.iter().rev().take(8).all(|b| *b == 0)
}

/// 打開開機自啟；順手把工作管理員那邊的停用紀錄也翻回啟用，
/// 否則使用者在設定頁按了 ON 卻依舊不會自啟（工作管理員的停用優先）。
pub fn enable_autostart(name: &str, exe: &std::path::Path) -> io::Result<()> {
    write_hkcu_string(RUN_SUBKEY, name, &autostart_command(exe))?;
    // 這個 subkey 在乾淨的系統上可能還不存在，寫不進去也不影響自啟本身
    if let Err(e) = write_hkcu_binary(APPROVED_SUBKEY, name, &APPROVED_ENABLED) {
        log::warn!("could not update the startup approval entry: {e}");
    }
    Ok(())
}

/// 關掉開機自啟：拿掉 Run 值即可，工作管理員那邊的紀錄留著不動
pub fn disable_autostart(name: &str) -> io::Result<()> {
    delete_hkcu_value(RUN_SUBKEY, name)
}

/// 讀 HKCU 的 Run 登錄值，用來判斷開機自啟項是不是還指向這支執行檔。
pub fn read_run_value(name: &str) -> Option<String> {
    read_hkcu_string(RUN_SUBKEY, name)
}

/// 讀 HKCU 底下的字串值
pub fn read_hkcu_string(subkey: &str, name: &str) -> Option<String> {
    let subkey = wide(subkey);
    let value = wide(name);
    unsafe {
        let mut size: u32 = 0;
        let rc = RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        );
        if rc != ERROR_SUCCESS || size == 0 {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        let rc = RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut size,
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// 讀 HKCU 底下的位元組值（REG_BINARY）
fn read_hkcu_binary(subkey: &str, name: &str) -> Option<Vec<u8>> {
    let subkey = wide(subkey);
    let value = wide(name);
    unsafe {
        let mut size: u32 = 0;
        let rc = RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        );
        if rc != ERROR_SUCCESS || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let rc = RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut size,
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        buf.truncate(size as usize);
        Some(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 圖示工廠產出的層序，測試照著它走
    const LAYERS: [u32; 9] = [16, 20, 24, 28, 32, 48, 64, 128, 256];

    /// 完全相符的層優先，這樣 GDI 完全不用縮放
    #[test]
    fn exact_layer_wins() {
        assert_eq!(pick_icon_layer(&LAYERS, 16), Some(0));
        // 175% DPI 的 28px 有專用層
        assert_eq!(pick_icon_layer(&LAYERS, 28), Some(3));
        assert_eq!(pick_icon_layer(&LAYERS, 32), Some(4));
    }

    /// 視窗大圖示（SM_CXICON）在各 DPI 下都該挑到「不小於它」的層：
    /// 放大會模糊，縮小不會
    #[test]
    fn large_icon_sizes_never_upscale() {
        // 100%／125%／150%／175%／200%／250%／300% 的 SM_CXICON
        let ladder = [(32, 32), (40, 48), (48, 48), (56, 64), (64, 64), (80, 128), (96, 128)];
        for (want, expect) in ladder {
            let idx = pick_icon_layer(&LAYERS, want).expect("一定挑得到一層");
            assert_eq!(LAYERS[idx], expect, "{want}px 挑錯層");
        }
    }

    /// 這台機器兩種圖示尺寸的合理性：大圖示不會比小圖示小，也不會是 0
    #[test]
    fn metrics_are_sane_on_this_machine() {
        let (sw, sh) = small_icon_size();
        let (lw, lh) = large_icon_size();
        assert_eq!(sw, sh, "小圖示應為正方");
        assert_eq!(lw, lh, "大圖示應為正方");
        assert!(sw >= 16 && lw >= 32, "SM_CXSMICON={sw} SM_CXICON={lw}");
        assert!(lw >= sw && lh >= sh, "大圖示不該小於小圖示");
    }

    /// 沒有專用層時寧可讓系統縮小，也不要放大
    #[test]
    fn falls_back_to_the_next_size_up() {
        let sizes = [16, 24, 32];
        assert_eq!(pick_icon_layer(&sizes, 20), Some(1)); // 24 縮到 20
        assert_eq!(pick_icon_layer(&sizes, 28), Some(2)); // 32 縮到 28
    }

    /// 要的比所有層都大時只能拿最大的那層
    #[test]
    fn falls_back_to_the_largest_layer() {
        assert_eq!(pick_icon_layer(&[16, 32, 24], 64), Some(1));
        assert_eq!(pick_icon_layer(&[], 16), None);
    }

    /// 取表那段是手寫 FFI，用一個真的 listener 釘住行為：
    /// 綁得起來的埠一定要被看見，否則 spawn 前的埠檢查就失去意義
    #[test]
    fn detects_a_real_listener() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("綁得起來");
        let port = l.local_addr().unwrap().port();
        assert!(is_listening(port), "剛綁上的 {port} 應該查得到");
    }

    /// 自啟登錄值的內容格式是與 tauri-plugin-autostart 相容的關鍵：
    /// 一定要有 --tray，路徑一定要有引號（安裝路徑含空白時沒引號會被截斷）
    #[test]
    fn autostart_command_quotes_the_path_and_keeps_the_tray_flag() {
        let cmd = autostart_command(Path::new("C:\\Program Files\\app\\traytunnel.exe"));
        assert_eq!(cmd, "\"C:\\Program Files\\app\\traytunnel.exe\" --tray");
        assert!(cmd.ends_with(" --tray"), "少了 --tray 就會開機彈主視窗：{cmd}");
        assert_eq!(cmd.matches('"').count(), 2);
    }

    /// 寫入、讀回、刪掉一個值走完一輪；刪除是冪等的，重刪不算錯
    #[test]
    fn hkcu_value_round_trip() {
        let subkey = format!("Software\\traytunnel-test\\{}", std::process::id());
        write_hkcu_string(&subkey, "cmd", "\"C:\\app\\traytunnel.exe\" --tray").unwrap();
        assert_eq!(
            read_hkcu_string(&subkey, "cmd").as_deref(),
            Some("\"C:\\app\\traytunnel.exe\" --tray")
        );

        write_hkcu_binary(&subkey, "approved", &APPROVED_ENABLED).unwrap();
        assert_eq!(read_hkcu_binary(&subkey, "approved").as_deref(), Some(&APPROVED_ENABLED[..]));

        delete_hkcu_value(&subkey, "cmd").unwrap();
        assert_eq!(read_hkcu_string(&subkey, "cmd"), None);
        // 值已經不在了，再刪一次仍算成功
        delete_hkcu_value(&subkey, "cmd").unwrap();

        delete_hkcu_value(&subkey, "approved").unwrap();
        let _ = delete_hkcu_key(&subkey);
        let _ = delete_hkcu_key("Software\\traytunnel-test");
    }

    /// StartupApproved\Run 是使用者機器上真的那個機碼，測試值一旦留下就是垃圾。
    /// 清理交給 Drop：斷言在哪一行 panic 都照樣會跑，`--nocapture` 或提早 return
    /// 也一樣。名字帶 pid，碰不到真正的 traytunnel 那一筆。
    struct ApprovedValue(String);

    impl ApprovedValue {
        fn new(name: &str) -> Self {
            ApprovedValue(name.to_string())
        }

        fn write(&self, bytes: &[u8]) {
            write_hkcu_binary(APPROVED_SUBKEY, &self.0, bytes).unwrap();
        }

        /// 提早刪掉以驗「沒有紀錄」那一格；刪除是冪等的，Drop 再刪一次也沒事
        fn remove(&self) {
            delete_hkcu_value(APPROVED_SUBKEY, &self.0).unwrap();
        }
    }

    impl Drop for ApprovedValue {
        fn drop(&mut self) {
            let _ = delete_hkcu_value(APPROVED_SUBKEY, &self.0);
        }
    }

    /// 工作管理員的「啟動」分頁把使用者的停用記在 StartupApproved\Run：Run 值還在，
    /// 但開機不會被執行。這裡實際寫進那個 subkey，走真的 approved_enabled 判定，
    /// 三種情形都釘住：啟用（時間戳全 0）、停用（時間戳非 0）、沒有紀錄。
    #[test]
    fn task_manager_disable_is_respected() {
        let name = format!("traytunnel-test-{}", std::process::id());
        let value = ApprovedValue::new(&name);

        // 啟用：後 8 個位元組（停用時間的 FILETIME）全是 0
        value.write(&APPROVED_ENABLED);
        assert!(approved_enabled(&name), "全零時間戳代表使用者沒有停用");

        // 停用：工作管理員把停用當下的 FILETIME 寫進後 8 個位元組
        let disabled: [u8; 12] = [0x03, 0, 0, 0, 0x2b, 0x7c, 0x1d, 0xa9, 0x5f, 0xd4, 0xdb, 0x01];
        value.write(&disabled);
        assert!(!approved_enabled(&name), "非零時間戳代表使用者在工作管理員按了停用");
        // 這個測試名稱沒有對應的 Run 值，整體判定當然也是關的
        assert!(!autostart_enabled(&name));

        // 收尾：測試值刪掉，沒有紀錄時一律當成啟用（乾淨系統的常態）
        value.remove();
        assert!(read_hkcu_binary(APPROVED_SUBKEY, &name).is_none(), "測試值要清乾淨");
        assert!(approved_enabled(&name), "沒有紀錄就當成啟用");
    }

    /// 時間戳的形狀就是日誌行的格式契約：固定八個字元的 HH:mm:ss
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

    #[test]
    fn wide_is_nul_terminated() {
        assert_eq!(wide("ab"), vec![0x61, 0x62, 0]);
        assert_eq!(wide(""), vec![0]);
    }

    /// explorer 的命令列格式嚴格：`/select,` 後面不可以有空白，路徑要自己加引號
    /// （使用者名稱含空白、或裝在 Program Files 底下都會踩到），
    /// 而且引號只能包路徑、不能把 `/select,` 一起包進去。
    #[test]
    fn explorer_selects_the_file_with_a_quoted_path() {
        let path = Path::new("C:\\Users\\Bob Smith\\.traytunnel.toml");
        let arg = explorer_arg(path, true);
        assert_eq!(arg, "/select,\"C:\\Users\\Bob Smith\\.traytunnel.toml\"");
        assert!(arg.starts_with("/select,\""), "逗號後面不可以有空白：{arg}");
    }

    /// 路徑本身含逗號時特別危險：`/select,` 靠的就是逗號分隔，
    /// 沒有引號的話 explorer 會從路徑中間的逗號斷開而選錯東西
    #[test]
    fn explorer_quotes_a_path_containing_commas() {
        let path = Path::new("C:\\Users\\Bob\\Documents, old\\.traytunnel.toml");
        let arg = explorer_arg(path, true);
        assert_eq!(arg, "/select,\"C:\\Users\\Bob\\Documents, old\\.traytunnel.toml\"");
        // 引號恰好包住整條路徑：開頭與結尾各一個，中間不再有
        assert_eq!(arg.matches('"').count(), 2);
        assert!(arg.ends_with('"'));
    }

    /// 檔案不在了就退而開啟它所在的資料夾，一樣要帶引號
    #[test]
    fn explorer_falls_back_to_the_folder() {
        let path = Path::new("C:\\Program Files\\app\\traytunnel.toml");
        assert_eq!(explorer_arg(path, false), "\"C:\\Program Files\\app\"");
        // 連上層資料夾都沒有時不該組出空引號
        assert_eq!(explorer_arg(Path::new("traytunnel.toml"), false), "\".\"");
    }
}
