//! Windows 專用：Job Object 與本地埠 Listen 偵測。
//!
//! ssh 的 ProxyCommand 會再生出 cloudflared 之類的子程序，單純 kill ssh 會留孤兒，
//! 因此把 ssh 放進帶 KILL_ON_JOB_CLOSE 的 job，關掉 handle 就整棵樹一起收掉。
//! 主程式崩潰或被強制結束時 handle 也會被系統關閉，同樣不會留下孤兒。

use std::io;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE, NO_ERROR,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
    TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
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

/// 本地是否有程序在該埠 Listen（等同原版的 Get-NetTCPConnection -State Listen）。
/// IPv4 與 IPv6 都查，ssh 綁在 ::1 的情況一樣認得。
pub fn is_listening(port: u16) -> bool {
    listening_v4(port) || listening_v6(port)
}

/// dwLocalPort 低兩個位元組是網路位元組序
fn local_port(raw: u32) -> u16 {
    (((raw & 0xff) << 8) | ((raw >> 8) & 0xff)) as u16
}

fn listener_table(family: u32) -> Option<Vec<u8>> {
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
        let mut buf = vec![0u8; size as usize];
        let rc = GetExtendedTcpTable(
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut size,
            0,
            family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if rc != NO_ERROR {
            return None;
        }
        Some(buf)
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

/// 系統匣圖示這台機器實際要的像素尺寸。
///
/// 100% DPI 是 16，175% 就是 28——Windows 會照這個尺寸向 tray-icon 要圖，
/// 給錯尺寸就由 GDI 拉伸，高 DPI 下糊掉的根源。
pub fn small_icon_size() -> (u32, u32) {
    metrics(SM_CXSMICON, SM_CYSMICON, (16, 16))
}

/// 視窗「大圖示」這台機器實際要的像素尺寸。
///
/// 工作列的視窗按鈕、Alt+Tab 與 ICON_BIG 取的都是這個尺寸：100% DPI 是 32，
/// 175% 就是 56。Tauri codegen 的 `default_window_icon()` 只給 ICO 的第一層
/// （我們的第一層是 16px），交給 GDI 從 16 拉到 56 就是工作列圖示糊掉的原因。
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
    let sub = wide(subkey);
    let value = wide(name);
    let payload = wide(data);
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
        let rc = RegSetValueExW(
            key,
            value.as_ptr(),
            0,
            REG_SZ,
            payload.as_ptr() as *const u8,
            (payload.len() * 2) as u32,
        );
        RegCloseKey(key);
        if rc != ERROR_SUCCESS {
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
/// explorer 是 GUI 程式，仍加上 CREATE_NO_WINDOW 杜絕黑窗一閃。
pub fn reveal_in_explorer(path: &std::path::Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    std::process::Command::new("explorer.exe")
        .raw_arg(explorer_arg(path, path.is_file()))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
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

/// 讀 HKCU 的 Run 登錄值，用來判斷開機自啟項是不是還指向這支執行檔。
pub fn read_run_value(name: &str) -> Option<String> {
    const SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    read_hkcu_string(SUBKEY, name)
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
        // 175% DPI 的 28px 現在有專用層
        assert_eq!(pick_icon_layer(&LAYERS, 28), Some(3));
        assert_eq!(pick_icon_layer(&LAYERS, 32), Some(4));
    }

    /// 視窗大圖示（SM_CXICON）在各 DPI 下都該挑到「不小於它」的層，
    /// 放大才會糊，縮小不會
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

    #[test]
    fn wide_is_nul_terminated() {
        assert_eq!(wide("ab"), vec![0x61, 0x62, 0]);
        assert_eq!(wide(""), vec![0]);
    }

    /// explorer 的命令列很挑：`/select,` 後面不可以有空白，路徑要自己加引號
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
