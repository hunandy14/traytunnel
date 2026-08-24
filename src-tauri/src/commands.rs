//! 前端 IPC 指令層：`#[tauri::command]` 的落點，加上系統匣選單也會共用的
//! enable／disable 內部函式。
//!
//! 這一層只做三件事：擋掉不存在的出口與源、把設定改動交給 `AppState::update_config`
//! 落地、成功之後才推事件與拉／停隧道。驗證與正規化一律在 `config` 那邊做完，
//! 這裡不重複判斷，也不自己拼要存進設定的值。

use tauri::{AppHandle, Manager, State};

use crate::config::{self, Config, ConnKind, RowKind, Source, WgProxy};
use crate::state::{autostart_name, Snapshot, UpdateInfo, MAIN_WINDOW};
use crate::{close_main, do_exit, tunnel, update, wg, winsys, Shared};

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

/// 這條列該由 ssh 還是 wg 那一套動詞去啟停。
///
/// `local` 是全域唯一鍵，IPC 那一層不必（也不該）知道機制——路由集中在這裡，
/// 免得每一支指令各自判斷一次還判得不一樣。
fn row_conn_kind(st: &Shared, local: u16) -> Option<ConnKind> {
    st.with_config(|c| c.row(local).map(|(conn, _)| conn.kind()))
}

/// 起單一列，依它所屬連線的型別分流
fn start_row(st: &Shared, local: u16) {
    match row_conn_kind(st, local) {
        Some(ConnKind::Wg) => wg::start_row(st, local),
        Some(ConnKind::Ssh) => tunnel::start(st, local),
        None => {}
    }
}

/// 停單一列，依它所屬連線的型別分流
fn halt_row(st: &Shared, local: u16) {
    match row_conn_kind(st, local) {
        Some(ConnKind::Wg) => wg::halt_row(st, local),
        Some(ConnKind::Ssh) => tunnel::halt(st, local),
        None => {}
    }
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
    apply_enabled(st, on, || start_row(st, local), || halt_row(st, local));
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
    match row_conn_kind(st, local) {
        // wg 的一條列不能單獨重接：它的監聽器是引擎那棵任務樹的一部分（§5.2）
        Some(ConnKind::Wg) => wg::start_row(st, local),
        Some(ConnKind::Ssh) => tunnel::restart(st, local),
        None => {}
    }
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
        config::apply_source_enabled(c, name, on);
    }) {
        // 同 set_exit_enabled：設定沒改成，但介面的開關已經被樂觀翻過去了，
        // 全量推一次把它們拉回設定裡的真值
        st.emit_config_changed();
        return;
    }
    apply_enabled(st, on, || tunnel::start_source(st, name), || tunnel::halt_source(st, name));
}

/// 全部連接／全部中斷：跨連線、跨連線型把 enabled 一起翻過去。
///
/// 這一支**刻意比 `set_wg_enabled` 粗**：使用者按的是「全部」，那就是所有列
/// 加所有連線一起翻，不保留任何逐列意圖。`set_wg_enabled` 那條「不碰列的
/// enabled」的規則管的是單一連線的總開關，兩者要的是不同的東西。
pub fn set_all_enabled(st: &Shared, on: bool) {
    if !save(st, |c| {
        for s in c.sources.iter_mut() {
            for f in s.forwards.iter_mut() {
                f.enabled = on;
            }
        }
        for p in c.wg_proxies.iter_mut() {
            p.enabled = on;
            for f in p.forwards.iter_mut() {
                f.enabled = on;
            }
        }
    }) {
        // 同上。系統匣的 Start／Stop all 還會連帶讓那一列的標籤與整份勾選跟著錯，
        // emit_config_changed 一次把介面與系統匣都重建回真值
        st.emit_config_changed();
        return;
    }
    apply_enabled(
        st,
        on,
        || {
            tunnel::start_enabled(st);
            wg::start_enabled(st);
        },
        || {
            tunnel::halt_all(st);
            wg::halt_all(st);
        },
    );
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
    let written = st.update_config_checked(|c| {
        // 便宜的重驗：上面那次驗證與這次寫入之間 cfg 鎖是放開的，兩個同時進來的
        // 新增可以雙雙通過驗證，再一前一後 push 進兩筆同名的源。這一次是在鎖裡做的
        if let Some(err) =
            config::validate_source(&c.sources, original_name.as_deref(), &target, &host, &user)
        {
            return Err(err);
        }
        match original_name.as_deref() {
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
        }
        Ok(())
    });
    match written {
        Err(e) => return Some(save_error_message(st, e)),
        Ok(Err(err)) => return Some(err),
        Ok(Ok(())) => {}
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

/// 從設定裡的所有連線（兩型都算）拔掉這個本地埠的列
fn detach_row(c: &mut Config, local: u16) {
    for s in c.sources.iter_mut() {
        s.forwards.retain(|f| f.local != local);
    }
    for p in c.wg_proxies.iter_mut() {
        p.forwards.retain(|f| f.local != local);
    }
}

/// 把一筆準備好的列掛進指定連線
fn attach_row(c: &mut Config, connection: &str, kind: ConnKind, row: config::Forward) {
    match kind {
        ConnKind::Ssh => {
            if let Some(s) = c.source_mut(connection) {
                s.forwards.push(row);
            }
        }
        ConnKind::Wg => {
            if let Some(p) = c.wg_proxy_mut(connection) {
                p.forwards.push(row);
            }
        }
    }
}

/// 呼叫端說的連線型別；沒說時由連線名自己問出來。
///
/// **相容期**：前端過渡墊片同時送 `source` 與 `connection` 兩個鍵、而且可能
/// 還沒帶 `connectionKind`，這裡把兩種形狀都接住。前端拆掉墊片之後，
/// `source` 與這段推斷可以一起移除。
fn resolve_conn_kind(st: &Shared, declared: Option<&str>, connection: &str) -> ConnKind {
    match declared {
        Some("wg") => ConnKind::Wg,
        Some("ssh") => ConnKind::Ssh,
        _ if st.with_config(|c| c.wg_proxy(connection).is_some()) => ConnKind::Wg,
        _ => ConnKind::Ssh,
    }
}

/// 兩支 upsert（`upsertForward`／`upsertWgSocks`）共用的落地流程。
///
/// 差別只在各自帶入固定的 `kind` 與各自的必填欄位，驗證與唯一性檢查因此
/// 只有一份實作（§5.5）。
///
/// 參數多是因為它就是 [`config::RowInput`] 的那一組欄位再加一個 `st`；
/// 收成結構只是把同一串欄位換個地方寫，不會少掉任何一個。
#[allow(clippy::too_many_arguments)]
fn upsert_row(
    st: &Shared,
    connection: &str,
    conn_kind: ConnKind,
    original_local: Option<u16>,
    name: &str,
    local: u16,
    remote: Option<&str>,
    kind: RowKind,
    probe_proxy: bool,
) -> Option<String> {
    // 值本身之外的欄位每次都一樣，只有 name／remote 會在正規化後換一份字串
    let base = config::RowInput {
        connection,
        conn_kind,
        original_local,
        name,
        local,
        remote,
        kind,
        probe_proxy,
    };
    // 抄原本的 enabled、正規化與驗證，全部看同一份設定
    let prepared = st.with_config(|c| {
        // 新增的列比照設定檔缺省值視為 enabled，加完就直接連；編輯則沿用原本的選擇
        let was_enabled = match original_local {
            Some(orig) => c.forward(orig).is_some_and(|f| f.enabled),
            None => true,
        };
        config::prepare_forward(c, &base, was_enabled)
    });
    let row = match prepared {
        Ok(f) => f,
        Err(err) => return Some(err),
    };
    let was_enabled = row.enabled;
    let row_name = row.name.clone();

    let written = st.update_config_checked(|c| {
        // 同 upsert_source 的理由：驗證與寫入之間 cfg 鎖是放開的，兩個同時進來的
        // 新增可以雙雙通過驗證，再一前一後 push 進兩筆佔著同一個本地埠的列。
        // 這裡只重驗唯一性那一段（值本身已經正規化過了），成本是幾個整數比較
        let recheck = config::RowInput { name: &row.name, remote: row.remote.as_deref(), ..base };
        if let Some(err) = config::validate_forward(c, &recheck) {
            return Err(err);
        }
        if let Some(orig) = original_local {
            // 先從原本的連線拔掉，再掛進目標連線，同連線編輯也走同一條路
            detach_row(c, orig);
        }
        attach_row(c, connection, conn_kind, row.clone());
        Ok(())
    });
    match written {
        Err(e) => return Some(save_error_message(st, e)),
        Ok(Err(err)) => return Some(err),
        Ok(Ok(())) => {}
    }

    // 存檔成功之後才停掉舊的那條線（換埠或換連線時舊埠也才會放掉）。存檔失敗時
    // 什麼都還沒動，隧道照舊跑著，不會出現「線停了、設定沒改成」的錯位。
    if let Some(orig) = original_local {
        if orig != local {
            tunnel::halt(st, orig);
        }
    }

    st.emit_config_changed();
    st.log_from(
        connection,
        match original_local {
            Some(_) => format!("{row_name} updated"),
            None => format!("{row_name} added"),
        },
    );
    if was_enabled {
        start_row(st, local);
    }
    None
}

/// 新增或編輯 `forward` 列（SSH 與 WG 共用），originalLocal 為 None 代表新增；
/// 回傳 None 代表成功。
///
/// `source` 是相容期的舊鍵名，與 `connection` 同義（見 [`resolve_conn_kind`]）。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn upsert_forward(
    state: State<'_, Shared>,
    source: Option<String>,
    connection: Option<String>,
    connection_kind: Option<String>,
    original_local: Option<u16>,
    name: String,
    local: u16,
    remote: String,
    probe_proxy: Option<bool>,
) -> Option<String> {
    let st = state.inner();
    let connection = connection.or(source).unwrap_or_default();
    let conn_kind = resolve_conn_kind(st, connection_kind.as_deref(), &connection);
    upsert_row(
        st,
        &connection,
        conn_kind,
        original_local,
        &name,
        local,
        Some(remote.as_str()),
        RowKind::Forward,
        probe_proxy.unwrap_or(false),
    )
}

/// 新增或編輯 `socks` 列（WG 專屬——後端會拒絕 ssh 連線名，W3.38）
#[tauri::command]
pub fn upsert_wg_socks(
    state: State<'_, Shared>,
    connection: String,
    original_local: Option<u16>,
    name: String,
    local: u16,
) -> Option<String> {
    upsert_row(
        state.inner(),
        &connection,
        ConnKind::Wg,
        original_local,
        &name,
        local,
        // socks 列沒有目的地，也不帶 probeProxy（它恆測）
        None,
        RowKind::Socks,
        false,
    )
}

/// 刪任何一種列，運行中的先停掉。
///
/// `local` 是全域唯一鍵，**同一支指令不必指明機制或連線型**（W6.21）。
#[tauri::command]
pub fn delete_forward(state: State<'_, Shared>, local: u16) {
    let st = state.inner();
    let names =
        st.with_config(|c| c.row(local).map(|(conn, f)| (conn.name().to_string(), f.name.clone())));
    let Some((cname, fname)) = names else {
        // 不存在的 local：記一行就退，不 panic（W6.23）
        st.log(format!("port {local} : no such exit"));
        return;
    };
    let kind = row_conn_kind(st, local);
    // 同 delete_source：先存檔成功才停線，存檔失敗時隧道維持原狀
    if !save(st, |c| detach_row(c, local)) {
        return;
    }
    match kind {
        // 列已經不在設定裡了，`wg::halt_row` 查不到所屬連線，得直接對連線動手：
        // 引擎要用新的列清單重建，而剩下零條啟用的列時它會依 §5.2 收掉（W6.22）
        Some(ConnKind::Wg) => {
            st.set_exit_status(local, crate::state::status::STOPPED, None);
            wg::restart(st, &cname);
        }
        _ => tunnel::halt(st, local),
    }
    st.emit_config_changed();
    st.log_from(&cname, format!("{fname} deleted"));
}

// ------------------------------------------------------- WireGuard 連線層（§5.5）

/// 新增或編輯 WG 連線，originalName 為 None 代表新增；回傳 None 代表成功。
///
/// 注意：**沒有 socksPort**——SOCKS5 埠是底下的一條 `socks` 列（§1.3）。
#[tauri::command]
pub fn upsert_wg_proxy(
    state: State<'_, Shared>,
    original_name: Option<String>,
    name: String,
    conf_path: String,
) -> Option<String> {
    let st = state.inner();
    let name = name.trim().to_string();
    let conf_path = conf_path.trim().to_string();
    if let Some(err) = st
        .with_config(|c| config::validate_wg_proxy(c, original_name.as_deref(), &name, &conf_path))
    {
        return Some(err);
    }

    let written = st.update_config_checked(|c| {
        // 便宜的重驗，理由同 upsert_source：這一次是在 cfg 鎖裡做的
        if let Some(err) = config::validate_wg_proxy(c, original_name.as_deref(), &name, &conf_path)
        {
            return Err(err);
        }
        match original_name.as_deref() {
            Some(orig) => {
                if let Some(p) = c.wg_proxy_mut(orig) {
                    p.name = name.clone();
                    p.conf_path = conf_path.clone();
                }
            }
            // 新連線底下還沒有任何列，所以也還沒有東西要跑；enabled 沿用預設的
            // true，使用者按下總開關（或加了第一條列）時才真的起引擎
            None => c.wg_proxies.push(WgProxy {
                name: name.clone(),
                conf_path: conf_path.clone(),
                enabled: true,
                forwards: Vec::new(),
            }),
        }
        Ok(())
    });
    match written {
        Err(e) => return Some(save_error_message(st, e)),
        Ok(Err(err)) => return Some(err),
        Ok(Ok(())) => {}
    }

    st.emit_config_changed();
    st.log_from(
        &name,
        match original_name.as_deref() {
            Some(_) => "connection updated",
            None => "WireGuard connection added",
        },
    );
    // 改名之後舊名的引擎已經被 sync_exits 收掉了；換了 conf 就要用新的那一份重來
    if original_name.is_some() {
        wg::restart(st, &name);
    }
    None
}

/// 刪 WG 連線，底下所有列一併刪掉，運行中的先停（W6.17～W6.20）
#[tauri::command]
pub fn delete_wg_proxy(state: State<'_, Shared>, name: String) {
    let st = state.inner();
    if st.with_config(|c| c.wg_proxy(&name).is_none()) {
        st.log(format!("no such WireGuard connection: {name}")); // W6.19
        return;
    }
    // 要停的埠得在刪掉之前先抄下來，刪完就查不到了
    let ports = st.with_config(|c| wg::halted_locals(c, &name));
    // 先存檔成功才停線（W6.18）：反過來做的話，存檔失敗就會留下「引擎停了、
    // 設定還在而且是 enabled」的錯位狀態
    if !save(st, |c| c.wg_proxies.retain(|p| p.name != name)) {
        return;
    }
    // 引擎那一份執行期狀態已經被 sync_exits 連同 CancelGuard 一起丟掉了；
    // 這裡只補推各列的 stopped，讓介面與系統匣立刻跟上（W6.17）
    for local in ports {
        st.set_exit_status(local, crate::state::status::STOPPED, None);
    }
    st.emit_config_changed();
    st.log(format!("WireGuard connection {name} deleted"));
}

/// 連線層的引擎總開關（§5.5 第 3 支）。
///
/// 與 ssh 的 `set_source_enabled` **刻意不對稱**：只改連線自己的 `enabled`，
/// 底下各列的意圖一個都不碰。存檔成功才動引擎，步驟由
/// [`wg::wg_enabled_steps`] 決定（那一串抽出來才測得到，W6.13／W6.14）。
#[tauri::command]
pub fn set_wg_enabled(state: State<'_, Shared>, name: String, on: bool) {
    let st = state.inner();
    if st.with_config(|c| c.wg_proxy(&name).is_none()) {
        st.log(format!("no such WireGuard connection: {name}")); // W6.15
        return;
    }
    // 無條件落檔並推 config-changed，就算值沒變也一樣：介面的開關已經被樂觀
    // 翻過去了，不推的話它會停在一個設定裡沒有的狀態
    let saved = save(st, |c| {
        config::apply_wg_enabled(c, &name, on);
    });
    let has_enabled_row = st.with_config(|c| wg::should_run_engine(c, &name));
    for step in wg::wg_enabled_steps(&name, on, saved, has_enabled_row) {
        match step {
            wg::WgEnabledStep::EmitConfigChanged => st.emit_config_changed(),
            wg::WgEnabledStep::StartEngine(conn) => wg::start(st, &conn),
            wg::WgEnabledStep::HaltEngine(conn) => wg::halt(st, &conn),
        }
    }
    if saved {
        st.log_from(&name, if on { "engine started" } else { "engine stopped" });
    }
}

/// 輕量 conf 解析，**不握手、不連外、不起引擎**（§5.5 第 4 支）。
///
/// 給編輯面板在「選檔當下」即時顯示這份 conf 裡有什麼；解析失敗時回 Err(訊息)，
/// 面板就地把錯誤掛在 confPath 欄位上。金鑰一概不在回傳值裡。
#[tauri::command]
pub fn inspect_conf(
    state: State<'_, Shared>,
    conf_path: String,
) -> Result<wg::conf::ConfSummary, String> {
    let st = state.inner();
    let dir = st.path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
    wg::inspect_conf(&config::resolve_conf_path(&dir, &conf_path))
}

/// 存檔前的 .conf 測試：解析 + 真握手，15 秒上限（§5.5 第 5 支）。
///
/// 與 `inspect_conf` 並存、各司其職：那一支是選檔當下的即時回饋（毫秒級、
/// 純本機），這一支是使用者主動按下的完整驗證（會連外、會等握手）。
/// 回傳型別直接沿用 ssh 的 `TestConnectionResult`。
#[tauri::command]
pub async fn test_wg_conf(
    state: State<'_, Shared>,
    conf_path: String,
) -> Result<tunnel::TestConnectionResult, ()> {
    let dir =
        state.inner().path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
    Ok(wg::test_conf(&config::resolve_conf_path(&dir, &conf_path)).await)
}

/// 原生檔案選擇器，選 `.conf`；取消時回 null（§5.5 第 6 支）。
///
/// 副檔名過濾器只是**提示**，不強制：使用者選了別的副檔名一樣照收，
/// 內容合不合格由 `inspect_conf` 去判（W9.9）。
#[tauri::command]
pub async fn pick_wg_conf(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("WireGuard configuration", &["conf"])
        .set_title("Select a WireGuard .conf")
        .pick_file(move |picked| {
            let _ = tx.send(picked);
        });
    // 對話框開不起來（沒有 dialog 能力、沒有視窗）時 sender 會被丟掉：
    // 回 Err 而不是 panic，前端就退回純文字路徑輸入（W9.10／Q3 的退路）
    let picked = rx.await.map_err(|_| "file dialog is unavailable".to_string())?;
    Ok(picked.map(|p| p.to_string()))
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
/// 關掉之後完全不再連外，並把已經找到的那一版也從畫面上收掉——使用者既然選擇
/// 不再接收更新提示，留著那一列等同繼續提示。打開則立刻查一次，
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

/// 使用者主動按下的「Check now」。
///
/// **不受背景檢查開關管**：那個開關管的是自動連外，親手按下這顆鈕就是對這一次
/// 連外的明示同意。結果直接回傳，讓按鈕呈現得出 Up to date 與 Check failed
/// 那兩個瞬態——Err 就是失敗，Ok(None) 就是已經最新。
#[tauri::command]
pub async fn check_for_updates_now(state: State<'_, Shared>) -> Result<Option<UpdateInfo>, String> {
    let st = state.inner().clone();
    update::check_manually(&st).await
}

/// 某一版的 release 頁：發佈說明與該版的下載資產都在那一頁上。
/// 可攜／單檔版的「Get vX.Y.Z」與下拉的「View release notes」共用這個指令，
/// version 給 None 時退回 releases/latest。
#[tauri::command]
pub fn open_release_page(state: State<'_, Shared>, version: Option<String>) {
    update::open_release_page(state.inner(), version.as_deref());
}

/// 下拉的「Download from Releases」：開系統瀏覽器到 Releases 列表頁，
/// 剩下的交給使用者。這條路不下載任何東西，也不會動到執行中的這顆 exe。
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
