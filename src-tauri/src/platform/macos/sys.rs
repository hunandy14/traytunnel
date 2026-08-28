//! 系統查詢與系統動作。對照組是 `platform/windows/winsys.rs`。
//!
//! `is_listening`／圖示尺寸與挑層是 A／C 車道的範疇，仍是 stub。

use std::io;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- 本地埠偵測

/// W3：Windows 走 `GetExtendedTcpTable`，macOS 這邊要另找一條
/// （libproc 的 `proc_pidfdinfo`，或退而求其次去 parse `lsof -nP -iTCP -sTCP:LISTEN`）。
///
/// 這一支**絕對不可以**先回 `false` 頂著：連線判定與 spawn 前的埠檢查都吃它的答案，
/// 一律回 false 會讓每一條隧道都停在 connecting、而且埠被佔用時照樣硬 spawn。
pub fn is_listening(_port: u16) -> bool {
    todo!("W3: macOS 的本地 listener 偵測尚未實作")
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
// 因此直接照 launchd 慣例管 plist：語意等同外掛的 `MacosLauncher::LaunchAgent`
// 模式（`RunAtLoad` + `launchctl load/unload`），零新增依賴。

/// LaunchAgent plist 所在資料夾：`~/Library/LaunchAgents`。
///
/// 只有這一支（與 [`launchctl_load`]／[`launchctl_unload`]）會碰真的使用者家目錄
/// 與真的 launchd——底下的 `*_at` 系列全部改吃呼叫端傳進來的 `base: &Path`，
/// 測試才能打 tempdir 而不必污染實機或 CI runner（獨立審查 2026-08-29 阻擋缺陷：
/// 預設測試輪不准碰真實系統）。
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

/// 開機自啟目前是不是真的登記著：plist 檔案在就算數。純檔案 I/O，不碰 launchctl，
/// `base` 讓測試可以傳 tempdir。
///
/// 與 Windows 的 `autostart_enabled` 不完全對稱：Windows 那邊還會再看工作管理員
/// 的 StartupApproved 停用紀錄，macOS（13 起）的「登入項目」系統設定也有等價的
/// 使用者停用機制，但要偵測它得挖 `SMAppService` 的狀態或系統的背景任務管理
/// 資料庫，複雜度與這一輪的範疇不成比例，先留給之後補（見 PR 說明的偏離事項）。
fn autostart_enabled_at(base: &Path, name: &str) -> bool {
    plist_path_in(base, name).is_file()
}

/// 讀 plist 裡的 `ProgramArguments`，用來判斷開機自啟項是不是還指向這支執行檔。
/// 純檔案 I/O，不碰 launchctl。
fn read_autostart_command_at(base: &Path, name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(plist_path_in(base, name)).ok()?;
    read_program_arguments(&contents)
}

/// 把 LaunchAgent plist 寫進 `base` 資料夾，回傳寫出去的路徑。純檔案 I/O，
/// 不碰 launchctl——要不要、什麼時候 `load` 是呼叫端（[`enable_autostart`]）的事。
fn write_autostart_plist_at(base: &Path, name: &str, exe: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(base)?;
    let label = plist_label(name);
    let path = base.join(format!("{label}.plist"));
    std::fs::write(&path, plist_contents(&label, exe))?;
    Ok(path)
}

/// 從 `base` 資料夾刪掉 plist；冪等（本來就沒有檔案也算成功）。純檔案 I/O，
/// 不碰 launchctl。
fn remove_autostart_plist_at(base: &Path, name: &str) -> io::Result<()> {
    match std::fs::remove_file(plist_path_in(base, name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// `launchctl load -w`，讓剛寫好的 plist 立即生效（不必等下次登入）。
/// 薄函式，唯一目的是把「真的呼叫 launchctl」這件事收在一支好認的名字底下，
/// 預設測試輪完全不呼叫它——只有 [`enable_autostart`] 與手動的 `#[ignore]`
/// 實機測試會碰到。
fn launchctl_load(path: &Path) -> io::Result<()> {
    let status =
        std::process::Command::new("launchctl").arg("load").arg("-w").arg(path).status()?;
    if !status.success() {
        return Err(io::Error::other(format!("launchctl load exited with {status}")));
    }
    Ok(())
}

/// `launchctl unload`，失敗（例如根本沒登記過）不當一回事——收尾用，
/// 不該讓「本來就沒登記」變成錯誤。同樣是薄函式，預設測試輪不呼叫它。
fn launchctl_unload(path: &Path) {
    let _ = std::process::Command::new("launchctl").arg("unload").arg(path).output();
}

/// 開機自啟目前是不是真的登記著。
pub fn autostart_enabled(name: &str) -> bool {
    launch_agents_dir().is_some_and(|dir| autostart_enabled_at(&dir, name))
}

/// 讀登記的命令，用來判斷開機自啟項是不是還指向這支執行檔。
pub fn read_autostart_command(name: &str) -> Option<String> {
    read_autostart_command_at(&launch_agents_dir()?, name)
}

/// 寫出 LaunchAgent plist 並 `launchctl load` 讓它立即生效（不必等下次登入）。
///
/// 先 `unload` 舊的（若有）再覆寫再 `load`：直接覆寫檔案不會讓 launchd 認得
/// 內容已經變了，重新登記才會讓新的 `ProgramArguments`（例如執行檔搬過位置後
/// 的自癒）真正生效。`unload` 失敗（例如根本沒登記過）不當一回事，只有最後的
/// `load` 失敗才回錯——那才是使用者真正在意的「自啟到底有沒有打開」。
pub fn enable_autostart(name: &str, exe: &Path) -> io::Result<()> {
    let dir = launch_agents_dir()
        .ok_or_else(|| io::Error::other("could not resolve $HOME for ~/Library/LaunchAgents"))?;
    launchctl_unload(&plist_path_in(&dir, name));
    let path = write_autostart_plist_at(&dir, name, exe)?;
    launchctl_load(&path)
}

/// `launchctl unload` 並刪掉 plist；兩者都是冪等操作，跟 Windows
/// `disable_autostart` 刪 Run 值的冪等語意一致。
pub fn disable_autostart(name: &str) -> io::Result<()> {
    let Some(dir) = launch_agents_dir() else {
        // 問不到 $HOME，沒有地方可能登記過，視同已經是關的
        return Ok(());
    };
    let path = plist_path_in(&dir, name);
    if path.is_file() {
        launchctl_unload(&path);
    }
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

/// 在 Finder 裡開啟並選中一個檔案，對應 Windows 的 `explorer.exe /select,`。
pub fn reveal_in_file_manager(path: &Path) -> io::Result<()> {
    let exists = path.exists();
    let target = reveal_target(path, exists);
    let mut cmd = std::process::Command::new("open");
    if exists {
        cmd.arg("-R");
    }
    cmd.arg(&target).spawn().map(|_| ())
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
    /// `launchctl load -w`、讀回命令、`launchctl unload`、刪檔。**會動真的
    /// launchd**（`-w` 還會在 launchd 的 override 資料庫留下紀錄），比照
    /// `wg_live_tests`／`exits::live_probe` 的慣例刻意 `#[ignore]`，預設測試輪
    /// 不跑，只有手動指定才會跑：
    ///
    /// cargo test --lib -- --ignored --nocapture live_autostart
    ///
    /// 比照 Windows `hkcu_value_round_trip`，測試名稱帶 pid 避免撞到使用者真正
    /// 的登記項，收尾一定會把測試用的 plist 清掉；但中途 assert 失敗仍可能跳過
    /// 收尾，這正是這條不准留在預設測試輪的理由。
    #[test]
    #[ignore]
    fn live_autostart_round_trips_through_launchd() {
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
}
