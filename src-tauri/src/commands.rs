//! 前端 IPC 指令層：`#[tauri::command]` 的落點，加上系統匣選單也會共用的
//! enable／disable 內部函式。
//!
//! 這一層只做三件事：擋掉不存在的出口與源、把設定改動交給 `AppState::update_config`
//! 落地、成功之後才推事件與拉／停隧道。驗證與正規化一律在 `config` 那邊做完，
//! 這裡不重複判斷，也不自己拼要存進設定的值。

use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::config::{self, Config, Source};
use crate::state::{Snapshot, MAIN_WINDOW};
use crate::{close_main, do_exit, tunnel, winsys, Shared};

/// 存檔失敗時回給前端的訊息開頭，回傳字串的那幾個指令共用同一份字面值
const SAVE_FAILED: &str = "Failed to save settings";

/// 設定檔寫入失敗一律讓使用者看得到，且記憶體狀態不會被改掉
fn report_save_error(state: &Shared, e: &std::io::Error) {
    state.log(format!("failed to save settings: {e}"));
}

/// 存檔失敗且要把原因交回前端時走這裡：記一行到活動日誌，並組出對話框要顯示的訊息
fn save_error_message(st: &Shared, e: std::io::Error) -> String {
    report_save_error(st, &e);
    format!("{SAVE_FAILED}:\n{e}")
}

/// 存檔，失敗時記一行並回 false。
///
/// 指令層的通則：設定沒存成功就什麼都不要做——隧道不停、事件不推，因為
/// `update_config` 回 Err 時記憶體裡的設定也沒被改動，這次操作等於沒發生。
fn save(st: &Shared, edit: impl FnOnce(&mut Config)) -> bool {
    match st.update_config(edit) {
        Ok(()) => true,
        Err(e) => {
            report_save_error(st, &e);
            false
        }
    }
}

// ---------------------------------------------------------------- 前端指令

#[tauri::command]
pub fn get_state(state: State<'_, Shared>) -> Snapshot {
    state.snapshot()
}

/// 出口不存在時記一行就回，回傳 false 代表沒有這個出口
fn require_exit(st: &Shared, local: u16) -> bool {
    if st.with_config(|c| c.forward(local).is_none()) {
        st.log(format!("port {local} : no such exit"));
        return false;
    }
    true
}

/// 源不存在時記一行就回
pub fn require_source(st: &Shared, name: &str) -> bool {
    if st.with_config(|c| c.source(name).is_none()) {
        st.log(format!("no such source: {name}"));
        return false;
    }
    true
}

/// 連接單一出口：記住使用者的選擇（enabled=true）後再拉線。
/// 前端指令與系統匣選單共用這裡，不繞 invoke。
pub fn enable_exit(st: &Shared, local: u16) {
    if !require_exit(st, local) {
        return;
    }
    if !save(st, |c| {
        if let Some(f) = c.forward_mut(local) {
            f.enabled = true;
        }
    }) {
        // 存檔失敗代表 enabled 沒改成，但系統匣的勾選已經被原生選單自己翻掉了，
        // 重建一次把它拉回設定裡的真值
        st.refresh_tray();
        return;
    }
    st.emit_config_changed();
    tunnel::start(st, local);
}

/// 中斷單一出口：enabled=false 並持久化，重開程式也不會自己連回來
pub fn disable_exit(st: &Shared, local: u16) {
    if !require_exit(st, local) {
        return;
    }
    if !save(st, |c| {
        if let Some(f) = c.forward_mut(local) {
            f.enabled = false;
        }
    }) {
        // 同上：勾選已被原生選單翻掉，設定卻沒改成，重建把它拉回真值
        st.refresh_tray();
        return;
    }
    tunnel::halt(st, local);
    st.emit_config_changed();
}

#[tauri::command]
pub fn start_exit(state: State<'_, Shared>, local: u16) {
    enable_exit(state.inner(), local);
}

#[tauri::command]
pub fn stop_exit(state: State<'_, Shared>, local: u16) {
    disable_exit(state.inner(), local);
}

/// 重接單一出口：halt 後立刻 start，套用最新設定。
/// 停用中的出口按重接視同要它連起來，順手把 enabled 補成 true。
#[tauri::command]
pub fn restart_exit(state: State<'_, Shared>, local: u16) {
    let st = state.inner();
    if !require_exit(st, local) {
        return;
    }
    let enabled = st.with_config(|c| c.forward(local).is_some_and(|f| f.enabled));
    if !enabled {
        if !save(st, |c| {
            if let Some(f) = c.forward_mut(local) {
                f.enabled = true;
            }
        }) {
            return;
        }
        st.emit_config_changed();
    }
    st.log_exit(local, format!("port {local} : restarting"));
    tunnel::restart(st, local);
}

/// 連接一個源底下全部的出口
#[tauri::command]
pub fn start_source(state: State<'_, Shared>, name: String) {
    let st = state.inner();
    if !require_source(st, &name) {
        return;
    }
    if !save(st, |c| {
        if let Some(s) = c.source_mut(&name) {
            for f in s.forwards.iter_mut() {
                f.enabled = true;
            }
        }
    }) {
        return;
    }
    st.emit_config_changed();
    tunnel::start_source(st, &name);
}

/// 中斷一個源底下全部的出口
#[tauri::command]
pub fn stop_source(state: State<'_, Shared>, name: String) {
    let st = state.inner();
    if !require_source(st, &name) {
        return;
    }
    if !save(st, |c| {
        if let Some(s) = c.source_mut(&name) {
            for f in s.forwards.iter_mut() {
                f.enabled = false;
            }
        }
    }) {
        return;
    }
    tunnel::halt_source(st, &name);
    st.emit_config_changed();
}

/// 全部連接：跨源把 enabled 全開再拉線
pub fn enable_all(st: &Shared) {
    if !save(st, set_all_enabled(true)) {
        return;
    }
    st.emit_config_changed();
    tunnel::start_enabled(st);
}

/// 全部中斷
pub fn disable_all(st: &Shared) {
    if !save(st, set_all_enabled(false)) {
        return;
    }
    tunnel::halt_all(st);
    st.emit_config_changed();
}

#[tauri::command]
pub fn start_all(state: State<'_, Shared>) {
    enable_all(state.inner());
}

#[tauri::command]
pub fn stop_all(state: State<'_, Shared>) {
    disable_all(state.inner());
}

fn set_all_enabled(on: bool) -> impl FnOnce(&mut Config) {
    move |c: &mut Config| {
        for s in c.sources.iter_mut() {
            for f in s.forwards.iter_mut() {
                f.enabled = on;
            }
        }
    }
}

/// 新增或編輯連線源，originalName 為 None 代表新增；回傳 None 代表成功。
/// 改到連線欄位時會重接這個源底下運行中的出口。
#[tauri::command]
pub fn upsert_source(
    state: State<'_, Shared>,
    original_name: Option<String>,
    name: String,
    host: String,
    user: String,
    proxy_command: String,
) -> Option<String> {
    let st = state.inner();
    let name = name.trim().to_string();
    let host = host.trim().to_string();
    let user = user.trim().to_string();
    let proxy_command = proxy_command.trim().to_string();
    // 驗證與「連線欄位有沒有真的變」看的是同一份設定，一次讀完
    let (invalid, changed) = st.with_config(|c| {
        let invalid =
            config::validate_source(&c.sources, original_name.as_deref(), &name, &host, &user);
        // 連線欄位有沒有真的變，決定要不要把這個源的出口重接一輪
        let changed = match original_name.as_deref().and_then(|n| c.source(n)) {
            Some(old) => old.host != host || old.user != user || old.proxy_command != proxy_command,
            None => false,
        };
        (invalid, changed)
    });
    if let Some(err) = invalid {
        return Some(err);
    }

    let target = name.clone();
    if let Err(e) = st.update_config(|c| match original_name.as_deref() {
        Some(orig) => {
            if let Some(s) = c.source_mut(orig) {
                s.name = target.clone();
                s.host = host.clone();
                s.user = user.clone();
                s.proxy_command = proxy_command.clone();
            }
        }
        // 新的源底下還沒有任何出口
        None => c.sources.push(Source {
            name: target.clone(),
            host: host.clone(),
            user: user.clone(),
            proxy_command: proxy_command.clone(),
            forwards: Vec::new(),
        }),
    }) {
        return Some(save_error_message(st, e));
    }

    st.emit_config_changed();
    st.log_from(
        &name,
        match original_name {
            Some(_) => "source updated",
            None => "source added",
        },
    );
    if changed {
        st.log_from(&name, "connection settings changed, restarting running exits");
        tunnel::restart_running_in_source(st, &name);
    }
    None
}

/// 刪源，底下的出口先全部停掉；刪到零源也是允許的
#[tauri::command]
pub fn delete_source(state: State<'_, Shared>, name: String) {
    let st = state.inner();
    if !require_source(st, &name) {
        return;
    }
    // 先存檔成功才停線。反過來做的話，存檔失敗就會留下「隧道已經停了、設定裡卻還
    // 在而且是 enabled」的錯位狀態。要停的埠得在刪掉之前先抄下來，刪完就查不到了。
    let ports: Vec<u16> = st.with_config(|c| {
        c.source(&name).map(|s| s.forwards.iter().map(|f| f.local).collect()).unwrap_or_default()
    });
    if !save(st, |c| c.sources.retain(|s| s.name != name)) {
        return;
    }
    for p in ports {
        tunnel::halt(st, p);
    }
    st.emit_config_changed();
    st.log(format!("source {name} deleted"));
}

/// 新增或編輯出口，originalLocal 為 None 代表新增；回傳 None 代表成功。
/// source 是這個出口要掛進去的源，編輯時也可以藉此把出口搬到別的源。
#[tauri::command]
pub fn upsert_forward(
    state: State<'_, Shared>,
    source: String,
    original_local: Option<u16>,
    name: String,
    local: u16,
    remote: String,
) -> Option<String> {
    let st = state.inner();
    // 查源、抄原本的 enabled、正規化與驗證，全部看同一份設定
    let prepared = st.with_config(|c| {
        if c.source(&source).is_none() {
            return Err(format!("no such connection: {source}"));
        }
        // 新增的出口比照設定檔缺省值視為 enabled，加完就直接連；編輯則沿用原本的選擇
        let was_enabled = match original_local {
            Some(orig) => c.forward(orig).is_some_and(|f| f.enabled),
            None => true,
        };
        // 正規化與驗證都在 config 那邊做完，這裡只負責把它給的那一筆原樣存下去
        config::prepare_forward(&c.sources, original_local, &name, local, &remote, was_enabled)
    });
    let forward = match prepared {
        Ok(f) => f,
        Err(err) => return Some(err),
    };
    let was_enabled = forward.enabled;
    let name = forward.name.clone();

    if let Err(e) = st.update_config(|c| {
        if let Some(orig) = original_local {
            // 先從原本的源拔掉，再掛進目標源，同源編輯也走同一條路
            for s in c.sources.iter_mut() {
                s.forwards.retain(|f| f.local != orig);
            }
        }
        if let Some(s) = c.source_mut(&source) {
            s.forwards.push(forward.clone());
        }
    }) {
        return Some(save_error_message(st, e));
    }

    // 存檔成功之後才停掉舊的那條線（換埠或換源時舊埠也才會放掉）。存檔失敗時
    // 什麼都還沒動，隧道照舊跑著，不會出現「線停了、設定沒改成」的錯位。
    if let Some(orig) = original_local {
        tunnel::halt(st, orig);
    }

    st.emit_config_changed();
    st.log_from(
        &source,
        match original_local {
            Some(_) => format!("{name} updated"),
            None => format!("{name} added"),
        },
    );
    if was_enabled {
        tunnel::start(st, local);
    }
    None
}

/// 刪出口，運行中的先停掉
#[tauri::command]
pub fn delete_forward(state: State<'_, Shared>, local: u16) {
    let st = state.inner();
    let names = st.with_config(|c| c.locate(local).map(|(s, f)| (s.name.clone(), f.name.clone())));
    let Some((sname, fname)) = names else {
        st.log(format!("port {local} : no such exit"));
        return;
    };
    // 同 delete_source：先存檔成功才停線，存檔失敗時隧道維持原狀
    if !save(st, |c| {
        for s in c.sources.iter_mut() {
            s.forwards.retain(|f| f.local != local);
        }
    }) {
        return;
    }
    tunnel::halt(st, local);
    st.emit_config_changed();
    st.log_from(&sname, format!("{fname} deleted"));
}

#[tauri::command]
pub fn test_exit(state: State<'_, Shared>, local: u16) {
    tunnel::test_exit(state.inner(), local);
}

#[tauri::command]
pub fn set_close_to_tray(state: State<'_, Shared>, on: bool) -> Result<(), String> {
    let st = state.inner();
    st.update_config(|c| c.close_to_tray = on).map_err(|e| save_error_message(st, e))?;
    st.emit_config_changed();
    st.log(if on { "close hides to tray" } else { "close exits app" });
    Ok(())
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, state: State<'_, Shared>, on: bool) -> Result<(), String> {
    let st = state.inner();
    let result = if on { app.autolaunch().enable() } else { app.autolaunch().disable() };
    result.map_err(|e| format!("Failed to change autostart:\n{e}"))?;
    st.log(if on { "autostart enabled" } else { "autostart disabled" });
    st.emit_config_changed();
    Ok(())
}

/// 這次執行實際生效的設定檔完整路徑，設定頁的 About 直接顯示它
#[tauri::command]
pub fn get_config_path(state: State<'_, Shared>) -> String {
    state.path.to_string_lossy().into_owned()
}

/// 在檔案總管裡開啟設定檔所在資料夾，並選中設定檔本身
#[tauri::command]
pub fn open_config_dir(state: State<'_, Shared>) {
    let st = state.inner();
    if let Err(e) = winsys::reveal_in_explorer(&st.path) {
        st.log(format!("could not open the config folder: {e}"));
    }
}

#[tauri::command]
pub fn window_close(state: State<'_, Shared>) {
    close_main(state.inner());
}

#[tauri::command]
pub fn window_minimize(app: AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN_WINDOW) {
        let _ = w.minimize();
    }
}

#[tauri::command]
pub fn exit_app(state: State<'_, Shared>) {
    do_exit(state.inner());
}
