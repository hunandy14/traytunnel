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
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
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

/// 讀 HKCU 的 Run 登錄值，用來判斷開機自啟項是不是還指向這支執行檔。
pub fn read_run_value(name: &str) -> Option<String> {
    const SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    let subkey: Vec<u16> = SUBKEY.encode_utf16().chain(std::iter::once(0)).collect();
    let value: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
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
