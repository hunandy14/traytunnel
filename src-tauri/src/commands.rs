//! 前端 IPC 指令層：`#[tauri::command]` 的落點，加上系統匣選單也會共用的
//! enable／disable 內部函式。
//!
//! 這一層只做三件事：擋掉不存在的出口與源、把設定改動交給 `AppState::update_config`
//! 落地、成功之後才推事件與拉／停隧道。驗證與正規化一律在 `config` 那邊做完，
//! 這裡不重複判斷，也不自己拼要存進設定的值。

use tauri::{AppHandle, Manager, State};

use crate::config::{self, Config, Source};
use crate::state::{autostart_name, Snapshot, MAIN_WINDOW};
use crate::{close_main, do_exit, tunnel, update, winsys, Shared};

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

/// enable／disable 三對指令存檔成功之後的共同收尾。
///
/// 事件與隧道動作的先後**刻意不對稱**，不可以為了整齊而統一：
/// 連接時先推事件再拉線，介面立刻看得到 connecting，隧道慢慢接上；
/// 中斷時先停線再推事件，介面上不會出現「已停用但還連著」的那一瞬。
fn apply_enabled(st: &Shared, on: bool, start: impl FnOnce(), halt: impl FnOnce()) {
    if on {
        st.emit_config_changed();
        start();
    } else {
        halt();
        st.emit_config_changed();
    }
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

/// 連接／中斷單一出口：先把使用者的選擇（enabled）持久化，成功了才動隧道。
/// 前端指令與系統匣選單共用這裡，不繞 invoke。
pub fn set_exit_enabled(st: &Shared, local: u16, on: bool) {
    if !require_exit(st, local) {
        return;
    }
    if !save(st, |c| {
        if let Some(f) = c.forward_mut(local) {
            f.enabled = on;
        }
    }) {
        // 存檔失敗代表 enabled 沒改成，但兩邊的開關都已經被樂觀翻過去了：
        // 系統匣的勾選是原生選單自己翻的，主視窗的開關是前端先翻的。
        // 全量推一次（emit_config_changed 連同系統匣一起重建）把兩邊都拉回設定裡的
        // 真值——唯讀模式下這條路每次都會走到，只重建系統匣的話介面會一直停在假狀態。
        st.emit_config_changed();
        return;
    }
    apply_enabled(st, on, || tunnel::start(st, local), || tunnel::halt(st, local));
}

#[tauri::command]
pub fn start_exit(state: State<'_, Shared>, local: u16) {
    set_exit_enabled(state.inner(), local, true);
}

#[tauri::command]
pub fn stop_exit(state: State<'_, Shared>, local: u16) {
    set_exit_enabled(state.inner(), local, false);
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
    set_source_enabled(state.inner(), &name, true);
}

/// 中斷一個源底下全部的出口
#[tauri::command]
pub fn stop_source(state: State<'_, Shared>, name: String) {
    set_source_enabled(state.inner(), &name, false);
}

/// 連接／中斷一個源底下全部的出口
fn set_source_enabled(st: &Shared, name: &str, on: bool) {
    if !require_source(st, name) {
        return;
    }
    if !save(st, |c| {
        if let Some(s) = c.source_mut(name) {
            for f in s.forwards.iter_mut() {
                f.enabled = on;
            }
        }
    }) {
        // 同 set_exit_enabled：設定沒改成，但介面的開關已經被樂觀翻過去了，
        // 全量推一次把它們拉回設定裡的真值
        st.emit_config_changed();
        return;
    }
    apply_enabled(st, on, || tunnel::start_source(st, name), || tunnel::halt_source(st, name));
}

/// 全部連接／全部中斷：跨源把 enabled 一起翻過去
pub fn set_all_enabled(st: &Shared, on: bool) {
    if !save(st, |c| {
        for s in c.sources.iter_mut() {
            for f in s.forwards.iter_mut() {
                f.enabled = on;
            }
        }
    }) {
        // 同上。系統匣的 Start／Stop all 還會連帶讓那一列的標籤與整份勾選跟著錯，
        // emit_config_changed 一次把介面與系統匣都重建回真值
        st.emit_config_changed();
        return;
    }
    apply_enabled(st, on, || tunnel::start_enabled(st), || tunnel::halt_all(st));
}

#[tauri::command]
pub fn start_all(state: State<'_, Shared>) {
    set_all_enabled(state.inner(), true);
}

#[tauri::command]
pub fn stop_all(state: State<'_, Shared>) {
    set_all_enabled(state.inner(), false);
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
    let ports = st.with_config(|c| c.locals_of(&name));
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

/// 存檔前的連線測試：拿表單當下（不一定已存檔）的值 spawn 一次性 ssh，
/// async 執行不擋住 UI 執行緒，成功與否＋訊息直接回傳，不走事件。
#[tauri::command]
pub async fn test_connection(
    host: String,
    user: String,
    proxy_command: String,
) -> tunnel::TestConnectionResult {
    tunnel::test_connection(user.trim(), host.trim(), proxy_command.trim()).await
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
    let name = autostart_name(&app);
    let result = if on {
        std::env::current_exe().and_then(|exe| winsys::enable_autostart(&name, &exe))
    } else {
        winsys::disable_autostart(&name)
    };
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

/// 背景檢查更新的開關。
///
/// 關掉之後完全不再連外，順手把已經找到的那一版也從畫面上收掉——使用者說了
/// 不要再被更新的事情打擾，留著那一列只是繼續打擾他。打開則立刻查一次，
/// 不必等到明天的排程。
#[tauri::command]
pub fn set_check_for_updates(state: State<'_, Shared>, on: bool) -> Result<(), String> {
    let st = state.inner();
    st.update_config(|c| c.check_for_updates = Some(on)).map_err(|e| save_error_message(st, e))?;
    st.emit_config_changed();
    st.log(if on { "update checks enabled" } else { "update checks disabled" });
    if on {
        update::check_now(st);
    } else {
        st.set_update(None);
    }
    Ok(())
}

/// 安裝版的「Restart to update」：下載並交棒給 NSIS 安裝程式。
///
/// 正常路徑上這個指令**不會回傳**——安裝程式一起來，這支程式就 exit 了，
/// 所以前端不必為成功的情況做任何收尾。回 Err 才代表這次更新沒能開始。
#[tauri::command]
pub async fn install_update(state: State<'_, Shared>) -> Result<(), String> {
    let st = state.inner().clone();
    update::install(&st).await.inspect_err(|e| st.log(format!("update failed: {e}")))
}

/// 可攜／單檔版的「Download」：開系統瀏覽器到 Releases 頁，剩下的交給使用者。
/// 這條路不下載任何東西，也不會動到執行中的這顆 exe。
#[tauri::command]
pub fn open_releases_page(state: State<'_, Shared>) {
    update::open_releases_page(state.inner());
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
