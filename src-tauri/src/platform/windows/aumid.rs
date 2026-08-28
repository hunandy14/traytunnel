//! 通知掛名（AppUserModelID）自註冊。
//!
//! Windows 的 toast 是掛在 AUMID 底下的，程式沒有自己的 AUMID 時
//! tauri-plugin-notification 會退回 notify-rust 的 PowerShell AUMID，通知就會
//! 顯示成「Windows PowerShell」。要讓系統認得自訂 AUMID，MS 的文件明確要求
//! 開始選單裡必須有一個帶 `System.AppUserModel.ID` 屬性的捷徑，因此這裡做三件事：
//!
//! 1. `SetCurrentProcessExplicitAppUserModelID`，把本行程掛到自己的 AUMID 上
//! 2. 開始選單缺捷徑（或捷徑指到別的執行檔）時，用 IShellLinkW + IPropertyStore
//!    建立／更新一份帶 AUMID 的捷徑
//! 3. 寫 `HKCU\Software\Classes\AppUserModelId\{aumid}`，補上 DisplayName 與 IconUri，
//!    通知中心才會顯示程式名稱與圖示——IconUri 一定要指到圖片檔（ico/png），指到 exe
//!    不會渲染，因此這裡把內嵌的 PNG 落地到 `%LOCALAPPDATA%\{aumid}\icon.png`
//!
//! 三步都要在建立 UI 之前跑完。

use std::io;
use std::path::{Path, PathBuf};

use windows::core::{Interface, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemAlloc, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, SetCurrentProcessExplicitAppUserModelID, ShellLink};

// 連模組本身一起帶進來：底下 `mod tests` 是 `use super::*`，在那裡
// `super::` 指的是 aumid 自己，走不到 platform::windows 的 winsys
use super::winsys::{self, wide};

/// PKEY_AppUserModel_ID，propkey.h 裡的固定值，windows crate 的 metadata 沒有生出來
const PKEY_APP_USER_MODEL_ID: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3), pid: 5 };

const AUMID_CLASS_ROOT: &str = "Software\\Classes\\AppUserModelId";

/// 通知圖示：內嵌方形 PNG，AUMID 的 IconUri 與 toast 的 appLogoOverride 都吃這一份，
/// 只嵌這一次，不再另外複製一份到別的模組。
const APP_ICON_PNG: &[u8] = include_bytes!("../../../icons/128x128.png");

/// 圖示落地的路徑：可攜版沒有安裝目錄，只能寫進使用者自己的 LOCALAPPDATA。
pub fn icon_file_path(aumid: &str) -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join(aumid).join("icon.png"))
}

/// 把圖示 bytes 寫到磁碟：內容跟現有檔案一樣就不重寫，回傳這次是不是真的寫了檔。
fn write_icon_file(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if std::fs::read(path).map(|existing| existing == bytes).unwrap_or(false) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)?;
    Ok(true)
}

/// 開始選單裡「使用者自己的」程式資料夾，捷徑就放這一層
fn start_menu_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs"),
    )
}

/// 捷徑檔的完整路徑
pub fn shortcut_path(product: &str) -> Option<PathBuf> {
    Some(start_menu_dir()?.join(format!("{product}.lnk")))
}

/// 捷徑要不要重寫：檔案不在、讀不到目標、或目標不是現在這支執行檔都要重寫。
///
/// 路徑比較不分大小寫（Windows 檔名不分大小寫），也不管尾端空白。
pub fn shortcut_is_stale(target: Option<&Path>, exe: &Path) -> bool {
    match target {
        None => true,
        Some(t) => {
            let a = t.to_string_lossy().trim().to_lowercase();
            let b = exe.to_string_lossy().trim().to_lowercase();
            a != b
        }
    }
}

/// COM 在主執行緒上通常已經由視窗框架初始化過，這裡重複呼叫拿到
/// S_FALSE／RPC_E_CHANGED_MODE 都不算錯，忽略即可。
fn ensure_com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

/// 把字串包成 VT_LPWSTR 的 PROPVARIANT；字串必須是 CoTaskMemAlloc 出來的，
/// PropVariantClear 才收得掉。
unsafe fn propvariant_string(s: &str) -> Option<PROPVARIANT> {
    let data = wide(s);
    let bytes = data.len() * 2;
    let mem = unsafe { CoTaskMemAlloc(bytes) } as *mut u16;
    if mem.is_null() {
        return None;
    }
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), mem, data.len()) };
    let mut pv = PROPVARIANT::default();
    unsafe {
        let inner = &mut *pv.Anonymous.Anonymous;
        inner.vt = VT_LPWSTR;
        inner.Anonymous.pwszVal = PWSTR(mem);
    }
    Some(pv)
}

/// 讀回一個既有的捷徑
fn load_shortcut(lnk: &Path) -> Option<IShellLinkW> {
    if !lnk.exists() {
        return None;
    }
    ensure_com();
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        let path = wide(&lnk.to_string_lossy());
        file.Load(PCWSTR(path.as_ptr()), STGM_READ).ok()?;
        Some(link)
    }
}

/// 讀既有捷徑指向的執行檔
fn read_shortcut_target(lnk: &Path) -> Option<PathBuf> {
    let link = load_shortcut(lnk)?;
    unsafe {
        let mut buf = [0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if len == 0 {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }
}

/// 讀既有捷徑上的 AUMID 屬性，驗證捷徑真的帶著它
pub fn read_shortcut_aumid(lnk: &Path) -> Option<String> {
    let link = load_shortcut(lnk)?;
    unsafe {
        let store: IPropertyStore = link.cast().ok()?;
        let mut pv = store.GetValue(&PKEY_APP_USER_MODEL_ID).ok()?;
        let inner = &*pv.Anonymous.Anonymous;
        let out = if inner.vt == VT_LPWSTR && !inner.Anonymous.pwszVal.is_null() {
            inner.Anonymous.pwszVal.to_string().ok()
        } else {
            None
        };
        let _ = PropVariantClear(&mut pv);
        out
    }
}

/// 建立或更新帶 AUMID 的開始選單捷徑，回傳 true 代表這次真的寫了檔。
pub fn ensure_shortcut(lnk: &Path, exe: &Path, aumid: &str, description: &str) -> io::Result<bool> {
    // 指向對的執行檔還不夠，屬性也得在：既有捷徑可能沒帶 AUMID 屬性
    let pointing_here = !shortcut_is_stale(read_shortcut_target(lnk).as_deref(), exe);
    if pointing_here && read_shortcut_aumid(lnk).as_deref() == Some(aumid) {
        return Ok(false);
    }
    if let Some(parent) = lnk.parent() {
        std::fs::create_dir_all(parent)?;
    }
    ensure_com();
    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(to_io)?;
        let exe_w = wide(&exe.to_string_lossy());
        link.SetPath(PCWSTR(exe_w.as_ptr())).map_err(to_io)?;
        link.SetIconLocation(PCWSTR(exe_w.as_ptr()), 0).map_err(to_io)?;
        if let Some(dir) = exe.parent() {
            let dir_w = wide(&dir.to_string_lossy());
            let _ = link.SetWorkingDirectory(PCWSTR(dir_w.as_ptr()));
        }
        let desc = wide(description);
        let _ = link.SetDescription(PCWSTR(desc.as_ptr()));

        // 捷徑要帶 System.AppUserModel.ID，Windows 才會認這個 AUMID
        let store: IPropertyStore = link.cast().map_err(to_io)?;
        let mut pv =
            propvariant_string(aumid).ok_or_else(|| io::Error::other("CoTaskMemAlloc 失敗"))?;
        let set = store.SetValue(&PKEY_APP_USER_MODEL_ID, &pv);
        let commit = set.and_then(|_| store.Commit());
        let _ = PropVariantClear(&mut pv);
        commit.map_err(to_io)?;

        let file: IPersistFile = link.cast().map_err(to_io)?;
        let lnk_w = wide(&lnk.to_string_lossy());
        file.Save(PCWSTR(lnk_w.as_ptr()), true).map_err(to_io)?;
    }
    Ok(true)
}

/// 註冊 AUMID 的顯示名稱與圖示，通知中心的分組才會顯示成程式本身。
///
/// IconUri 必須指到圖片檔：先把內嵌 PNG 落地（冪等，內容沒變就不重寫），再讓
/// IconUri 指過去。找不到 LOCALAPPDATA 這種邊緣情況就退回 exe 路徑，至少不會讓
/// 整段註冊失敗。
pub fn register_aumid(aumid: &str, display_name: &str, exe: &Path) -> io::Result<()> {
    let subkey = format!("{AUMID_CLASS_ROOT}\\{aumid}");
    winsys::write_hkcu_string(&subkey, "DisplayName", display_name)?;
    let icon_uri = match icon_file_path(aumid) {
        Some(path) => {
            write_icon_file(&path, APP_ICON_PNG)?;
            path.to_string_lossy().into_owned()
        }
        None => exe.to_string_lossy().into_owned(),
    };
    winsys::write_hkcu_string(&subkey, "IconUri", &icon_uri)?;
    Ok(())
}

fn to_io(e: windows::core::Error) -> io::Error {
    io::Error::other(e.to_string())
}

/// 三步一次做完，回傳給上層記日誌用的說明。建 UI 之前呼叫。
pub fn prepare(aumid: &str, product: &str, exe: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    unsafe {
        let id = wide(aumid);
        match SetCurrentProcessExplicitAppUserModelID(PCWSTR(id.as_ptr())) {
            Ok(()) => notes.push(format!("notification app id set to {aumid}")),
            Err(e) => notes.push(format!("could not set notification app id: {e}")),
        }
    }
    match shortcut_path(product) {
        Some(lnk) => match ensure_shortcut(&lnk, exe, aumid, product) {
            Ok(true) => notes.push("start menu shortcut created for notifications".into()),
            Ok(false) => {}
            Err(e) => notes.push(format!("could not write start menu shortcut: {e}")),
        },
        None => notes.push("could not locate the start menu folder".into()),
    }
    if let Err(e) = register_aumid(aumid, product, exe) {
        notes.push(format!("could not register notification app id: {e}"));
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 捷徑不存在、或指到別的執行檔（例如執行檔換了位置）都要重寫
    #[test]
    fn stale_when_missing_or_pointing_elsewhere() {
        let exe = Path::new(r"C:\Apps\Traytunnel\traytunnel.exe");
        assert!(shortcut_is_stale(None, exe));
        assert!(shortcut_is_stale(Some(Path::new(r"C:\Old\traytunnel.exe")), exe));
        assert!(shortcut_is_stale(Some(Path::new(r"C:\Apps\Traytunnel\other.exe")), exe));
    }

    /// 指到同一支執行檔就不要每次啟動都重寫檔案
    #[test]
    fn fresh_when_pointing_at_the_same_exe() {
        let exe = Path::new(r"C:\Apps\Traytunnel\traytunnel.exe");
        assert!(!shortcut_is_stale(Some(exe), exe));
        // Windows 檔名不分大小寫，殼層讀回來的路徑常常是全大寫
        assert!(!shortcut_is_stale(Some(Path::new(r"C:\APPS\TRAYTUNNEL\TRAYTUNNEL.EXE")), exe));
        // 讀回來的字串尾端有空白也不算不同
        assert!(!shortcut_is_stale(Some(Path::new(r"C:\Apps\Traytunnel\traytunnel.exe ")), exe));
    }

    /// 捷徑固定放使用者自己的開始選單，檔名就是產品名
    #[test]
    fn shortcut_lands_in_the_user_start_menu() {
        let p = shortcut_path("Traytunnel").expect("測試環境要有 APPDATA");
        let s = p.to_string_lossy().replace('/', "\\");
        assert!(s.ends_with("\\Microsoft\\Windows\\Start Menu\\Programs\\Traytunnel.lnk"), "{s}");
    }

    /// 真的把本行程掛到一個 AUMID 上，確認 shell32 這支呼叫在這台機器上會成功
    #[test]
    fn process_aumid_can_be_set() {
        let id = wide("com.traytunnel.desktop.test");
        unsafe {
            SetCurrentProcessExplicitAppUserModelID(PCWSTR(id.as_ptr()))
                .expect("SetCurrentProcessExplicitAppUserModelID 應該要成功");
        }
    }

    /// 真的寫一次 HKCU\Software\Classes\AppUserModelId\{id} 再讀回來，最後清掉。
    /// IconUri 必須指到落地的 PNG 檔，不是 exe——toast 只吃圖片檔。
    #[test]
    fn aumid_registry_entry_round_trips() {
        let id = format!("com.traytunnel.desktop.test{}", std::process::id());
        let exe = std::env::current_exe().unwrap();
        let icon_path = icon_file_path(&id).expect("測試環境要有 LOCALAPPDATA");
        let _ = std::fs::remove_dir_all(icon_path.parent().unwrap());

        register_aumid(&id, "Traytunnel Test", &exe).expect("寫登錄檔應該要成功");

        let sub = format!("{AUMID_CLASS_ROOT}\\{id}");
        assert_eq!(
            winsys::read_hkcu_string(&sub, "DisplayName").as_deref(),
            Some("Traytunnel Test")
        );
        assert_eq!(
            winsys::read_hkcu_string(&sub, "IconUri").as_deref(),
            Some(icon_path.to_string_lossy().as_ref())
        );
        assert!(icon_path.exists(), "IconUri 指到的圖示檔要真的寫出來");
        assert_eq!(
            std::fs::read(&icon_path).unwrap(),
            APP_ICON_PNG,
            "落地的圖示內容要跟內嵌的 PNG 一致"
        );

        winsys::delete_hkcu_key(&sub).expect("測試收尾要刪得掉");
        assert!(winsys::read_hkcu_string(&sub, "DisplayName").is_none());
        let _ = std::fs::remove_dir_all(icon_path.parent().unwrap());
    }

    /// 圖示檔冪等：內容沒變就不該重寫；內容真的不同（模擬版本升級）才要重寫。
    #[test]
    fn icon_file_write_is_idempotent() {
        let id = format!("com.traytunnel.desktop.icon.test{}", std::process::id());
        let path = icon_file_path(&id).expect("測試環境要有 LOCALAPPDATA");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(write_icon_file(&path, APP_ICON_PNG).unwrap(), "第一次要真的寫檔");
        assert_eq!(std::fs::read(&path).unwrap(), APP_ICON_PNG);

        assert!(!write_icon_file(&path, APP_ICON_PNG).unwrap(), "內容沒變就不該重寫");

        assert!(write_icon_file(&path, b"different bytes").unwrap(), "內容變了要重寫");
        assert_eq!(std::fs::read(&path).unwrap(), b"different bytes");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// 真的建一份捷徑再讀回來：目標與 AUMID 屬性都要對得上，
    /// 而且第二次呼叫不該重寫檔案。寫在暫存資料夾，不碰真的開始選單。
    #[test]
    fn shortcut_round_trips_target_and_aumid() {
        let dir =
            std::env::temp_dir().join(format!("traytunnel-lnk-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lnk = dir.join("Traytunnel Test.lnk");
        let exe = std::env::current_exe().unwrap();
        let aumid = "com.traytunnel.desktop.test";

        assert!(ensure_shortcut(&lnk, &exe, aumid, "Traytunnel Test").unwrap());
        assert!(lnk.exists());
        assert_eq!(read_shortcut_aumid(&lnk).as_deref(), Some(aumid));
        let target = read_shortcut_target(&lnk).expect("捷徑要讀得回目標");
        assert!(!shortcut_is_stale(Some(&target), &exe), "讀回來的是 {target:?}，期望 {exe:?}");

        // 已經指對了就不要每次啟動都重寫
        assert!(!ensure_shortcut(&lnk, &exe, aumid, "Traytunnel Test").unwrap());

        // 捷徑帶的是別的（或沒有）AUMID 時要補寫回去
        assert!(ensure_shortcut(&lnk, &exe, "com.traytunnel.other", "Traytunnel Test").unwrap());
        assert_eq!(read_shortcut_aumid(&lnk).as_deref(), Some("com.traytunnel.other"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
