//! 前端 IPC 指令層：`#[tauri::command]` 的落點，加上系統匣選單也會共用的
//! enable／disable 內部函式。
//!
//! 這一層只做三件事：擋掉不存在的出口與源、把設定改動交給 `AppState::update_config`
//! 落地、成功之後才推事件與拉／停隧道。驗證與正規化一律在 `config` 那邊做完，
//! 這裡不重複判斷，也不自己拼要存進設定的值。

use tauri::{AppHandle, Manager, State};

use crate::config::{self, Config, ConnKind, RowKind, Source, WgProxy};
use crate::platform::{self, update};
use crate::state::{autostart_name, Snapshot, UpdateInfo, MAIN_WINDOW};
use crate::{close_main, do_exit, tunnel, wg, Shared};

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

/// 「這一次連線是使用者要的」這件事，在活動日誌上留一行。
///
/// 為什麼要有它：日誌裡原本只看得到「ssh starting (pid …)」那一類**結果**，
/// 看不出這條線是開機時自動拉起來的、還是使用者剛剛親手按的。更新重啟後那個
/// 十秒空窗（Defender 掃描新落地的執行檔）就是靠這個差別才判得出來——沒有這一行，
/// 「使用者按了但沒反應」與「開機自動連線還沒輪到」在日誌上長得一模一樣。
///
/// 只在 `on` 為真時記：中斷那條路本來就會留下 stopped 狀態，不缺這一行。
fn log_connect_requested(st: &Shared, local: u16) {
    let name =
        st.with_config(|c| c.row(local).map(|(conn, f)| (conn.name().to_string(), f.name.clone())));
    match name {
        Some((conn, row)) => st.log_from(&conn, format!("{row} : connect requested")),
        None => st.log(format!("port {local} : connect requested")),
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
    if on {
        log_connect_requested(st, local);
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
///
/// **ssh 列的例外**：來源被主卡總開關關著時不補這個 true。那條列本來就連
/// 不上（`tunnel::start` 會被 `row_source_enabled` 擋下），若照樣把 enabled
/// 悄悄改成 true，使用者其實想留著停用的一條列會在來源重新打開的那一刻
/// 憑空復活——這是在改寫使用者的逐列意圖，不是「重接」該做的事。直接跳過、
/// 記一行日誌交代原因。**wg 的列不受影響**：wg 連線本身就有
/// `should_run_engine` 那一關，補 true 是既有且刻意的行為，這裡不去動它。
#[tauri::command]
pub fn restart_exit(state: State<'_, Shared>, local: u16) {
    let st = state.inner();
    if !require_exit(st, local) {
        return;
    }
    if matches!(row_conn_kind(st, local), Some(ConnKind::Ssh))
        && !st.with_config(|c| config::row_source_enabled(c, local))
    {
        st.log_exit(local, format!("port {local} : source is switched off, not restarting"));
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

/// 源層級的連線總開關（W6.12 起與 `set_wg_enabled` 同一套語意）：只改
/// `Source.enabled`，底下各列的意圖一個都不碰。存檔成功才動隧道——
/// 開時只拉起列自己也 enabled 的那些（`tunnel::start_source`），
/// 關時把底下所有列一起停掉（`tunnel::halt_source`），列的 enabled 原樣留著，
/// 下次打開才能照使用者原本的選擇恢復。
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
    if on {
        let count = st.with_config(|c| c.enabled_locals_of(name).len());
        st.log_from(name, format!("connect requested for {count} exit(s)"));
    }
    apply_enabled(st, on, || tunnel::start_source(st, name), || tunnel::halt_source(st, name));
}

/// 全部連接／全部中斷：跨連線、跨連線型把 enabled 一起翻過去，**連線層與列層
/// 兩型都翻**（`Source.enabled`／`WgProxy.enabled` 都跟著 `on` 走）。
///
/// 這一支**刻意比 `set_wg_enabled`／`set_source_enabled` 粗**：使用者按的是
/// 「全部」，那就是所有連線加所有列一起翻，不保留任何逐列或逐連線意圖。
/// 那兩支「只動連線總開關、不碰列的 enabled」的規則管的是單一連線的總開關，
/// 兩者要的是不同的東西——這裡若漏翻 `Source.enabled`，關掉的連線會被卡在
/// `enabled_locals()` 永遠為空的狀態，Connect all 因此完全失效。
pub fn set_all_enabled(st: &Shared, on: bool) {
    if !save(st, |c| config::apply_all_enabled(c, on)) {
        // 同上。系統匣的 Start／Stop all 還會連帶讓那一列的標籤與整份勾選跟著錯，
        // emit_config_changed 一次把介面與系統匣都重建回真值
        st.emit_config_changed();
        return;
    }
    if on {
        let count = st.with_config(|c| c.enabled_locals().len());
        st.log(format!("connect requested for all {count} exit(s)"));
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
            // 新的源底下還沒有任何出口；enabled 沿用預設的 true，比照
            // upsert_wg_proxy 新增 wg 連線的規則
            None => c.sources.push(Source {
                name: target.clone(),
                host: host.clone(),
                user: user.clone(),
                proxy_command: proxy_command.clone(),
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

/// 從設定裡的所有連線（兩型都算）拔掉這個本地埠的列。
///
/// `local` 是全域唯一鍵，所以刪一條列不需要知道它是什麼機制、掛在哪一型連線
/// 底下（W6.21）——掃過去、拔掉、結束。
pub(crate) fn detach_row(c: &mut Config, local: u16) {
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
        // 引擎要用新的列清單重建，而剩下零條啟用的列時它會依 §5.2 收掉（W6.22）。
        //
        // 這裡**不必**、也沒辦法再對 `local` 寫一次 stopped：`save` 成功時
        // `sync_exits` 已經連同它的 `ExitRuntime` 一起清掉了，`set_exit_status`
        // 只改既存項，寫下去是 no-op。介面那一側由下面的 `emit_config_changed`
        // 負責——快照裡本來就沒有這一條列了。
        Some(ConnKind::Wg) => wg::restart(st, &cname),
        _ => tunnel::halt(st, local),
    }
    st.emit_config_changed();
    st.log_from(&cname, format!("{fname} deleted"));
}

// ------------------------------------------------------- WireGuard 連線層（§5.5）

/// 新增或編輯 WG 連線，originalName 為 None 代表新增；回傳 None 代表成功。
///
/// 注意：**沒有 socksPort**——SOCKS5 埠是底下的一條 `socks` 列（§1.3）。
///
/// `mtu` 是選填的隧道 MTU 覆寫（省略／null＝照 `.conf`，見 `wg::effective_mtu`）。
///
/// **新建**時可能順手附一條預設的 SOCKS5 列，規則見 [`config::default_socks_row`]。
#[tauri::command]
pub fn upsert_wg_proxy(
    state: State<'_, Shared>,
    original_name: Option<String>,
    name: String,
    conf_path: String,
    mtu: Option<usize>,
) -> Option<String> {
    let st = state.inner();
    let name = name.trim().to_string();
    let conf_path = conf_path.trim().to_string();
    if let Some(err) = st.with_config(|c| {
        config::validate_wg_proxy(c, original_name.as_deref(), &name, &conf_path, mtu)
    }) {
        return Some(err);
    }

    // 附贈預設 SOCKS5 列的執行期條件，**在 cfg 鎖外先問完**：`is_listening` 在
    // 兩個平台上都要問一趟系統（Windows 是一次系統呼叫的全表列舉，macOS 是
    // 幾次本地 bind 探測），沒有理由讓設定鎖撐在那裡等它。編輯路徑永不附贈，
    // 所以那一路連問都不問。
    //
    // 這個答案有兩處刻意記在這裡的不精確：
    //   * 兩個平台查的範圍不完全等價於「只有我們自己會綁的 127.0.0.1」——
    //     Windows 掃的是全介面（含 0.0.0.0 與 ::），別人綁在某張外部網卡的
    //     1080 一樣會被算成「忙」而讓我們不附贈，這是保守方向；macOS 目前只
    //     精確比對 127.0.0.1／::1 這兩個字面 loopback 位址，對綁在
    //     0.0.0.0／[::] 這種 wildcard 位址的佔用者不可見（見
    //     `platform::macos::sys::is_listening` 的說明），方向反過來、是
    //     「可能誤判成沒人聽」——但我們自己的監聽器一律只綁字面 loopback，
    //     不受影響。
    //   * `is_listening` 查詢失敗時一律回 false（＝當作沒人在聽）。那是查不到
    //     答案，不是查到「空」；真撞上了會在附贈的列上以 port_busy 現形，
    //     那條路徑本來就有完整的錯誤顯示。
    let socks_port_listening =
        original_name.is_none() && platform::is_listening(config::DEFAULT_SOCKS_PORT);

    let written = st.update_config_checked(|c| {
        // 便宜的重驗，理由同 upsert_source：這一次是在 cfg 鎖裡做的
        if let Some(err) =
            config::validate_wg_proxy(c, original_name.as_deref(), &name, &conf_path, mtu)
        {
            return Err(err);
        }
        // 兩條臂各自算出「這一次有沒有附贈預設 SOCKS5 列」，match 當運算式用，
        // 閉包只有這一個出口
        let added_default = match original_name.as_deref() {
            Some(orig) => {
                if let Some(p) = c.wg_proxy_mut(orig) {
                    p.name = name.clone();
                    p.conf_path = conf_path.clone();
                    // 清空欄位就是把覆寫拿掉，回去照 .conf——所以這裡是無條件
                    // 指派而不是 `if let Some`
                    p.mtu = mtu;
                }
                // 編輯永不附贈：這一臂結構上就沒有那條路，規則不必再寫一次
                false
            }
            // 新連線的 enabled 沿用預設的 true。底下有沒有東西要跑，看 1080 淨不
            // 淨空——淨空就附一條預設 SOCKS5 列，否則連線底下一條列都沒有，
            // 使用者加了第一條列時才真的起引擎
            None => {
                c.wg_proxies.push(WgProxy {
                    name: name.clone(),
                    conf_path: conf_path.clone(),
                    enabled: true,
                    mtu,
                    forwards: Vec::new(),
                });
                // 先推連線再問附贈：那一筆是用手建列同一條 prepare_forward 造的，
                // 而它要查得到所屬連線才驗得過（W3.37）
                match config::default_socks_row(c, &name, socks_port_listening) {
                    Some(row) => {
                        // 上一行才推進去的那一條，查不到是不可能的
                        c.wg_proxy_mut(&name).expect("剛推進去的連線一定找得到").forwards.push(row);
                        true
                    }
                    None => false,
                }
            }
        };
        Ok(added_default)
    });
    let added_default = match written {
        Err(e) => return Some(save_error_message(st, e)),
        Ok(Err(err)) => return Some(err),
        Ok(Ok(added)) => added,
    };

    st.emit_config_changed();
    st.log_from(
        &name,
        match original_name.as_deref() {
            Some(_) => "connection updated",
            None => "WireGuard connection added",
        },
    );
    if added_default {
        st.log_from(
            &name,
            format!("default SOCKS5 proxy added on port {}", config::DEFAULT_SOCKS_PORT),
        );
    } else if socks_port_listening {
        // 沒附贈而且原因是執行期探測時**一定要留一行**：那是三個條件裡唯一一個
        // 事後看不出痕跡的。設定層撞埠使用者在列表上就看得到佔用者，但「剛剛
        // 刪掉的那條連線的監聽器還沒放掉 1080」只在這一瞬間為真，下一秒再建一次
        // 就成功了——沒有這一行，同樣的操作兩次不同結果會完全無跡可循。
        st.log_from(
            &name,
            format!("skipped default SOCKS5: port {} is in use", config::DEFAULT_SOCKS_PORT),
        );
    }
    // 改名之後舊名的引擎已經被 sync_exits 收掉了；換了 conf 就要用新的那一份重來
    if original_name.is_some() {
        wg::restart(st, &name);
    } else if added_default && st.wg_conf_error(&name).is_none() {
        // 附上的列與手建列無異，起線就走手建列同一條路（`start_row` 依連線型別
        // 分流），不直接戳 wg::start——今天兩者等價，但這條路徑不該自己長一份。
        //
        // `.conf` 壞掉時**不起**：`wg::start` 會擋下來並記一行 `cannot start: …`，
        // 而使用者這一步只是新增了一條連線、根本沒要求連線，替他生一則他沒要求的
        // 錯誤只是噪音。列照樣附著，等他把 conf 修好、按下總開關由既有流程接手。
        start_row(st, config::DEFAULT_SOCKS_PORT);
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
    // 先存檔成功才停線（W6.18）：反過來做的話，存檔失敗就會留下「引擎停了、
    // 設定還在而且是 enabled」的錯位狀態
    if !save(st, |c| c.wg_proxies.retain(|p| p.name != name)) {
        return;
    }
    // 存檔成功那一刻 `sync_exits` 就把這條連線的引擎（連同 `CancelGuard`，
    // 於是整棵任務樹與所有列的監聽器）與各列的 `ExitRuntime` 一起丟掉了，
    // 這裡不必再逐列停一次——那些項目已經不在，寫什麼都是 no-op。
    // 介面與系統匣由下面這一手全量重建（W6.17／W6.20）
    st.emit_config_changed();
    st.log(format!("WireGuard connection {name} deleted"));
}

/// 連線層的引擎總開關（§5.5 第 3 支）。
///
/// 自 W6.12 起與 ssh 的 `set_source_enabled` 是同一套語意：只改連線自己的
/// `enabled`，底下各列的意圖一個都不碰。差別只在 ssh 沒有引擎這個執行實體，
/// 因此不需要「零列就不留空轉引擎」這一關（W6.14），也就沒有對應的
/// `steps` 抽出來測——`set_source_enabled` 直接呼叫 `tunnel::start_source`／
/// `halt_source` 即可。存檔成功才動引擎，步驟由 [`wg::wg_enabled_steps`]
/// 決定（那一串抽出來才測得到，W6.13／W6.14）。
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
        .add_filter(CONF_FILTER_LABEL, &CONF_FILTER_EXTENSIONS)
        .set_title("Select a WireGuard .conf")
        .pick_file(move |picked| {
            let _ = tx.send(picked);
        });
    // 對話框開不起來（沒有 dialog 能力、沒有視窗）時 sender 會被丟掉：
    // 回 Err 而不是 panic，前端就退回純文字路徑輸入（W9.10／Q3 的退路）
    let picked = rx.await.map_err(|_| DIALOG_UNAVAILABLE.to_string())?;
    Ok(picked_conf_path(picked.and_then(|p| p.into_path().ok())))
}

/// 檔案對話框的副檔名過濾器。**只是提示，不強制**：使用者選了別的副檔名一樣
/// 照收，內容合不合格由 `inspect_conf` 去判（W9.9）。
pub(crate) const CONF_FILTER_LABEL: &str = "WireGuard configuration";
pub(crate) const CONF_FILTER_EXTENSIONS: [&str; 1] = ["conf"];

/// 對話框叫不起來時交回前端的訊息（W9.10）
pub(crate) const DIALOG_UNAVAILABLE: &str = "file dialog is unavailable";

/// 對話框選到的東西 → IPC 的回傳值。
///
/// 取消時是 `None`，**不是空字串**：前端拿到空字串會把它當成「使用者選了一個
/// 空路徑」而清掉既有的 `confPath`（W9.7）。
pub(crate) fn picked_conf_path(picked: Option<std::path::PathBuf>) -> Option<String> {
    picked.map(|p| p.to_string_lossy().into_owned())
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
        std::env::current_exe().and_then(|exe| platform::enable_autostart(&name, &exe))
    } else {
        platform::disable_autostart(&name)
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

// ---------------------------------------------------------------- 開外部程式
//
// 底下三支都是「請系統開一個東西給使用者看」，共同的紀律是**不可以在指令函式
// 裡同步等它**：Tauri 的同步指令跑在主執行緒上，而這幾條路底下是
// `open`（macOS）／`ShellExecuteW`（Windows），冷啟一個 Finder 視窗或瀏覽器
// 動輒一到三秒——那段時間整個 UI 會凍住。
//
// 作法是把阻塞那一段丟到 `spawn_blocking`（阻塞 I/O 專用的執行緒池，不佔
// tokio 的工作執行緒），指令本身立刻返回，成敗照舊記進活動日誌。刻意**不**
// 改成 `async fn` 去 await 它：這三支的回傳值前端一個都沒有用（`ipc.ts` 一律
// `invoke<void>`），await 只會把「promise 什麼時候 resolve」跟「系統視窗什麼
// 時候真的開出來」綁在一起，換不到任何東西；而帶 `State<'_, _>` 的 async 指令
// 又被 Tauri 逼著回一個永遠是 `Ok` 的 `Result`（`Result<(), ()>` 還會撞上
// `clippy::result_unit_err`）。
//
// 這一段是共用核心，沒有任何 `cfg`：「怎麼開」在 `platform` 那一層，
// 兩個平台的實作都不必為此改動。

/// 在檔案總管裡開啟設定檔所在資料夾，並選中設定檔本身
#[tauri::command]
pub fn open_config_dir(state: State<'_, Shared>) {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(e) = platform::reveal_in_file_manager(&st.path) {
            st.log(format!("could not open the config folder: {e}"));
        }
    });
}

/// 自動更新的總開關（設定頁的「Automatic updates」）。
///
/// 關掉之後完全不再連外，已經找到的那一版從畫面上收掉，**已經下載好躺在暫存區的
/// 那一份也一起丟掉**。最後這一件不是順手做的：套用更新那條路跑在設定檔載入
/// 之前（見 `update::discard_staged`），它看不到這個開關，所以「關掉之後不會再
/// 被自動更新」這個承諾只能靠現在就把標記清掉來兌現。
///
/// 打開則立刻查一次，不必等到明天的排程。
#[tauri::command]
pub fn set_automatic_updates(state: State<'_, Shared>, on: bool) -> Result<(), String> {
    let st = state.inner();
    st.update_config(|c| c.check_for_updates = Some(on)).map_err(|e| save_error_message(st, e))?;
    st.emit_config_changed();
    st.log(if on { "automatic updates enabled" } else { "automatic updates disabled" });
    if on {
        update::check_now(st);
    } else {
        st.set_update(None);
        update::discard_staged(st);
    }
    Ok(())
}

/// 設定頁與系統匣的「Restart to update」：把已經下載好的那一版現在就裝上去。
///
/// 正常路徑上這個指令**不會回傳**——安裝程式一起來，這支程式就 exit 了，
/// 所以前端不必為成功的情況做任何收尾。回 Err 才代表這次更新沒能開始。
///
/// 走 `spawn_blocking` 而不是直接做：`apply_now` 要把十幾 MB 的安裝檔整個讀進來
/// 算一次 SHA-256（落地之後有沒有被動過，只有這一關驗得出來）。同步指令是在
/// Tauri 的執行緒池上跑的，把那幾百毫秒的整檔讀取留在上面會擋住其他 IPC。
#[tauri::command]
pub async fn apply_update(state: State<'_, Shared>) -> Result<(), String> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        update::apply_now(&st).inspect_err(|e| st.log(format!("update failed: {e}")))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 使用者主動按下的「Check for updates／Check now」。
///
/// **不受自動更新開關管**：那個開關管的是自動連外，親手按下這顆鈕就是對這一次
/// 連外的明示同意。結果直接回傳，讓按鈕呈現得出 Up to date 與 Check failed
/// 那兩個瞬態——Err 就是失敗，Ok(None) 就是已經最新。
#[tauri::command]
pub async fn check_for_updates_now(state: State<'_, Shared>) -> Result<Option<UpdateInfo>, String> {
    let st = state.inner().clone();
    update::check_manually(&st).await
}

/// 設定頁那顆綠色主鈕：把新版裝上去。
///
/// 與 `apply_update` 的分工是「暫存區裡有沒有東西」：那一支只認已經下載好的
/// 那一份（系統匣的「Restart to update」走它），這一支則從頭走完整條
/// ——沒下載就下載，下載好了就交棒。自動更新關著時能更新的只有這條路。
///
/// 正常路徑上這個指令**不會回傳**——安裝程式一起來，這支程式就 exit 了，
/// 所以前端不必為成功的情況做任何收尾。回 Err 才代表這次更新沒能開始。
#[tauri::command]
pub async fn install_update(state: State<'_, Shared>) -> Result<(), String> {
    let st = state.inner().clone();
    update::install(&st).await.inspect_err(|e| st.log(format!("update failed: {e}")))
}

/// 某一版的 release 頁：發佈說明與該版的下載資產都在那一頁上。
/// 可攜／單檔版的「Get vX.Y.Z」與下拉的「View release notes」共用這個指令，
/// version 給 None 時退回 releases/latest。
#[tauri::command]
pub fn open_release_page(state: State<'_, Shared>, version: Option<String>) {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        update::open_release_page(&st, version.as_deref());
    });
}

/// 下拉的「Download from Releases」：開系統瀏覽器到 Releases 列表頁，
/// 剩下的交給使用者。這條路不下載任何東西，也不會動到執行中的這顆 exe。
#[tauri::command]
pub fn open_releases_page(state: State<'_, Shared>) {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || update::open_releases_page(&st));
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
