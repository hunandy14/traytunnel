//! 隧道生命週期：每個出口一條獨立的 ssh 連線，各自 spawn、各自 2 秒輪詢判斷
//! 連上、各自在斷線後固定 5 秒重連，彼此不互相拖累。

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::config::{Forward, Source};
use crate::exits::{probe, ExitTest};
use crate::state::{status, test_state, AppState};
use crate::winsys::{is_listening, Job};

/// CREATE_NO_WINDOW，避免主控台視窗一閃而過
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 連線偵測輪詢間隔
const POLL: Duration = Duration::from_millis(2000);
/// 斷線後固定重連間隔，無退避無上限；埠被佔用時也是每 5 秒重查一次
const RETRY: Duration = Duration::from_secs(5);
/// 自己的舊連線剛被收掉時，本地埠可能還殘留幾百毫秒才真的放掉，
/// 判定 port_busy 前先給這段緩衝再看一次，免得重接時誤報佔用
const PORT_GRACE: Duration = Duration::from_millis(500);
/// 連線測試的總上限，涵蓋 spawn 到程序退出的整段等待；
/// ssh 自己的 ConnectTimeout 管不到 ProxyCommand 卡住的情況，因此在這裡設一道總上限。
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// 組單一出口的 ssh 參數，每個 token 獨立傳遞，不做字串拼接。
/// 連線參數一律取自這個出口所屬的源。
///
/// ServerAlive 這一對決定「多久才發現線斷了」：ssh 每 Interval 秒沒收到資料就
/// 送一次探測，連續 CountMax 次沒回應才判定斷線。10 × 2 大約是 20-30 秒。
///
/// 這個值刻意壓得比常見的預設緊。Wi-Fi 斷掉、筆電休眠醒來這類情況下，TCP
/// 本身不會馬上知道對面沒了，這條 ssh 會維持在「看起來還連著」的狀態；
/// 本程式的連線判斷看的是本地埠有沒有在 listen，所以那段時間介面顯示 connected、
/// 使用者的流量卻全部石沉大海。原本的 30 × 3 要 90-120 秒才會進重連，
/// 那是使用者最容易誤以為「程式壞了」的一段。
///
/// 代價是每個出口每 10 秒多一個幾十位元組的探測封包，在這個用途上可以忽略。
pub fn build_exit_args(src: &Source, f: &Forward) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-N".into(),
        "-o".into(),
        "ServerAliveInterval=10".into(),
        "-o".into(),
        "ServerAliveCountMax=2".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
    ];
    if !src.proxy_command.trim().is_empty() {
        args.push("-o".into());
        args.push(format!("ProxyCommand={}", src.proxy_command));
    }
    args.push("-L".into());
    args.push(format!("{}:{}", f.local, f.remote));
    args.push(format!("{}@{}", src.user, src.host));
    args
}

/// 組連線測試用的 ssh 參數：一次性登入即退出，不建立任何轉發。
/// token 一樣逐個獨立傳遞，ProxyCommand 的處理與 build_exit_args 一致，
/// 只是不帶 -N -L，改用 BatchMode 避免卡在互動提示、ConnectTimeout 讓 ssh
/// 自己先設一道逾時。
pub fn build_test_args(user: &str, host: &str, proxy_command: &str) -> Vec<String> {
    let mut args: Vec<String> =
        vec!["-o".into(), "BatchMode=yes".into(), "-o".into(), "ConnectTimeout=10".into()];
    if !proxy_command.trim().is_empty() {
        args.push("-o".into());
        args.push(format!("ProxyCommand={proxy_command}"));
    }
    args.push(format!("{user}@{host}"));
    args.push("exit".into());
    args
}

/// spawn 前的埠檢查：埠已經被別人佔住就不要盲目 spawn，
/// 回傳 Some(detail) 代表要進 port_busy 狀態。
pub fn port_busy_detail(local: u16, listening: bool) -> Option<String> {
    if listening {
        Some(format!("Local port {local} is already in use by another process."))
    } else {
        None
    }
}

fn spawn_ssh(src: &Source, f: &Forward) -> std::io::Result<(Child, Job, u32)> {
    let job = Job::new()?;
    let mut cmd = Command::new("ssh");
    cmd.args(build_exit_args(src, f))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true);
    let child = cmd.spawn()?;
    let pid = child.id().unwrap_or(0);
    if let Some(handle) = child.raw_handle() {
        if let Err(e) = job.assign(handle as isize) {
            log::warn!("assign ssh to job object failed: {e}");
        }
    }
    Ok((child, job, pid))
}

/// 存檔前的連線測試結果：ok 為 false 時 message 是給使用者看的失敗原因。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestConnectionResult {
    pub ok: bool,
    pub message: String,
}

impl TestConnectionResult {
    fn ok() -> Self {
        TestConnectionResult { ok: true, message: "Connected".into() }
    }

    fn fail(message: impl Into<String>) -> Self {
        TestConnectionResult { ok: false, message: message.into() }
    }
}

/// stderr 逐行讀完，回傳最後一行非空白內容；ssh 的失敗原因（DNS 解析失敗、
/// 逾時、金鑰被拒……）都在這一行，原樣顯示給使用者就分辨得出來，不必自己再分類。
async fn last_meaningful_line(stderr: tokio::process::ChildStderr) -> String {
    let mut last = String::new();
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if !line.trim().is_empty() {
            last = line;
        }
    }
    last
}

/// 存檔前的手動連線測試：拿表單當下的值 spawn 一次性 ssh（不建立任何轉發），
/// 用來讓使用者在存檔前就知道 host／user／ProxyCommand 對不對。
/// 探測程序一樣掛進 Job Object，函式結束（含逾時）時隨著 Job 一起收掉，不留孤兒。
pub async fn test_connection(user: &str, host: &str, proxy_command: &str) -> TestConnectionResult {
    let job = match Job::new() {
        Ok(j) => j,
        Err(e) => return TestConnectionResult::fail(format!("failed to prepare job object: {e}")),
    };

    let mut cmd = Command::new("ssh");
    cmd.args(build_test_args(user, host, proxy_command))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return TestConnectionResult::fail(format!("failed to start ssh: {e}")),
    };
    if let Some(handle) = child.raw_handle() {
        if let Err(e) = job.assign(handle as isize) {
            log::warn!("test_connection: assign ssh to job object failed: {e}");
        }
    }

    let stderr_task =
        child.stderr.take().map(|s| tauri::async_runtime::spawn(last_meaningful_line(s)));

    let result = match tokio::time::timeout(TEST_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => {
            let last_line = match stderr_task {
                Some(task) => task.await.unwrap_or_default(),
                None => String::new(),
            };
            if status.success() {
                TestConnectionResult::ok()
            } else if last_line.is_empty() {
                TestConnectionResult::fail(format!("ssh exited with {status}"))
            } else {
                TestConnectionResult::fail(last_line)
            }
        }
        Ok(Err(e)) => TestConnectionResult::fail(format!("failed to wait for ssh: {e}")),
        Err(_) => {
            let _ = child.kill().await;
            TestConnectionResult::fail("connection test timed out after 15s")
        }
    };
    // job 在這裡被丟掉：無論成功、失敗還是逾時，整棵程序樹（含 ProxyCommand
    // 生出來的子程序，例如 cloudflared）都會跟著關閉的 handle 一起收掉。
    drop(job);
    result
}

/// 啟動單一出口的監看迴圈；出口不在設定裡就什麼都不做。
/// 呼叫端負責先把 enabled 寫進設定。
///
/// 語意是「確保這個出口有一條線在跑」：已經有監看迴圈時直接 no-op，
/// 不會另起一條。否則 start_all 打在已連線的出口上會讓新迴圈掃到舊 ssh
/// 還佔著的埠，誤報 5 秒的 port_busy。要換新設定請走 halt 再 start。
pub fn start(state: &Arc<AppState>, local: u16) {
    if state.with_config(|c| c.forward(local).is_none()) {
        return;
    }
    let Some(generation) = state.claim_supervisor(local) else {
        return; // 已經有一條線在跑
    };
    let st = state.clone();
    tauri::async_runtime::spawn(async move {
        supervise(&st, local, generation).await;
        st.release_supervisor(local, generation);
    });
}

/// 停掉單一出口：世代遞增讓監看迴圈作廢，關 job handle 收掉整棵程序樹。
/// 不動設定裡的 enabled，重啟流程才能沿用。
pub fn halt(state: &Arc<AppState>, local: u16) {
    state.next_generation(local);
    state.kill_job(local);
    state.clear_exit_test(local);
    state.set_exit_status(local, status::STOPPED, None);
}

/// 重接單一出口：halt 已經把世代遞增並當場騰出監看位子，
/// 緊接著的 start 不必等舊迴圈醒來就能接手，不會多開第二條 ssh。
pub fn restart(state: &Arc<AppState>, local: u16) {
    halt(state, local);
    start(state, local);
}

/// 啟動所有源的所有 enabled 出口（程式啟動與 start_all 都走這裡）
pub fn start_enabled(state: &Arc<AppState>) {
    for local in state.with_config(|c| c.enabled_locals()) {
        start(state, local);
    }
}

/// 停掉所有源的所有出口
pub fn halt_all(state: &Arc<AppState>) {
    for local in state.with_config(|c| c.locals()) {
        halt(state, local);
    }
}

/// 啟動單一源底下所有 enabled 的出口
pub fn start_source(state: &Arc<AppState>, source: &str) {
    for local in state.with_config(|c| c.enabled_locals_of(source)) {
        start(state, local);
    }
}

/// 停掉單一源底下所有出口
pub fn halt_source(state: &Arc<AppState>, source: &str) {
    for local in state.with_config(|c| c.locals_of(source)) {
        halt(state, local);
    }
}

/// 重接單一源底下運行中（enabled）的出口，該源的連線欄位改變時用
pub fn restart_running_in_source(state: &Arc<AppState>, source: &str) {
    for local in state.with_config(|c| c.enabled_locals_of(source)) {
        restart(state, local);
    }
}

/// 分段等待，中途世代作廢就立刻回 false
async fn wait_alive(state: &Arc<AppState>, local: u16, generation: u64, total: Duration) -> bool {
    let slice = Duration::from_millis(250);
    let mut waited = Duration::ZERO;
    while waited < total {
        tokio::time::sleep(slice).await;
        waited += slice;
        if !state.generation_alive(local, generation) {
            return false;
        }
    }
    true
}

/// 單一出口的監看迴圈。
///
/// 這裡的每一次狀態寫入都走 `set_exit_status_of` 帶著自己那一代的號碼：迴圈算出
/// 一個狀態到真正寫下去之間隔著埠掃描、spawn、搶 cfg 鎖的等待，halt 有足夠的時間
/// 插進來換掉世代。沒有守門的話，這條已經作廢的迴圈會把 halt 剛壓下的 stopped
/// 蓋回 connected，而且不會再有事件來糾正——迴圈下一圈就退出了。
/// 迴圈中間那幾道 `generation_alive` 是提早收工用的，不是守門：真正的守門在寫入
/// 那一刻、與寫入同一把鎖內完成。
async fn supervise(state: &Arc<AppState>, local: u16, generation: u64) {
    loop {
        if !state.generation_alive(local, generation) {
            return;
        }
        // 只複製這個出口與它所屬的源，不為了兩筆資料深拷貝整份設定
        let Some((src, f)) =
            state.with_config(|c| c.locate(local).map(|(s, f)| (s.clone(), f.clone())))
        else {
            return; // 出口已經被刪掉
        };
        let sname = src.name.as_str();
        // 這裡刻意不看 f.enabled：停止的唯一訊號是 halt 的世代遞增。
        // 中斷是「先寫 enabled=false 再 halt」兩步，中間那個微秒窗口若讓
        // 迴圈自己因為 enabled=false 就退出，會在沒有遞增世代的情況下把
        // 監看位子還掉，剛好插進來的 start 就會被這個早退吃掉一次 claim。

        // spawn 前先看埠是不是已經被系統上的其他程序佔住
        let mut busy = port_busy_detail(local, is_listening(local));
        if busy.is_some() {
            tokio::time::sleep(PORT_GRACE).await;
            if !state.generation_alive(local, generation) {
                return;
            }
            busy = port_busy_detail(local, is_listening(local));
        }
        if let Some(detail) = busy {
            state.set_exit_status_of(local, generation, status::PORT_BUSY, Some(detail));
            state.log_from(sname, format!("{} : local port {local} busy, retrying in 5s", f.name));
            if !wait_alive(state, local, generation, RETRY).await {
                return;
            }
            continue;
        }

        state.set_exit_status_of(local, generation, status::CONNECTING, None);

        // spawn 失敗的分支自己記下重試訊息，不再補一行「disconnected」
        let mut spawn_failed = false;
        match spawn_ssh(&src, &f) {
            Err(e) => {
                spawn_failed = true;
                state.set_exit_status_of(local, generation, status::ERROR, Some(e.to_string()));
                state.log_from(
                    sname,
                    format!("{} : failed to start ssh: {e}, retrying in 5s", f.name),
                );
            }
            Ok((mut child, job, pid)) => {
                state.store_job(local, generation, job);
                state.log_from(sname, format!("{} : ssh starting (pid {pid})", f.name));
                if let Some(stderr) = child.stderr.take() {
                    // ssh stderr 噪音大，只寫進檔案日誌，不進活動區——
                    // 活動區只保留本程式自身的事件
                    let name = format!("{sname}/{}", f.name);
                    tauri::async_runtime::spawn(async move {
                        let mut lines = BufReader::new(stderr).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            log::warn!("ssh[{name}]: {line}");
                        }
                    });
                }

                loop {
                    tokio::time::sleep(POLL).await;
                    if !state.generation_alive(local, generation) {
                        // 已被 halt/restart 作廢。halt 的 kill_job 有可能跑在
                        // store_job 之前（先清到空的，才輪到我們把 job 放進去），
                        // 那一手就會讓這條 ssh 連同 ProxyCommand 的孫程序留著沒人收，
                        // 所以離開前自己再收一次；世代不符時 kill_job_of 是 no-op，
                        // 不會誤殺已經接手的新一輪連線
                        state.kill_job_of(local, generation);
                        return;
                    }
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) => {}
                    }
                    if !state.is_connected(local) && is_listening(local) {
                        state.set_exit_status_of(local, generation, status::CONNECTED, None);
                        state.log_from(sname, format!("{} : up", f.name));
                        test_exit(state, local);
                    }
                }
                // ssh 退了，順手把 ProxyCommand 生出來的子程序一起收掉
                state.kill_job_of(local, generation);
            }
        }

        if !state.generation_alive(local, generation) {
            return;
        }
        state.clear_exit_test(local);
        if !spawn_failed {
            state.log_from(sname, format!("{} : disconnected, retrying in 5s", f.name));
            state.set_exit_status_of(local, generation, status::RECONNECTING, None);
        }
        if !wait_alive(state, local, generation, RETRY).await {
            return;
        }
    }
}

/// 對單一出口做自測，只有連上的出口才測。
pub fn test_exit(state: &Arc<AppState>, local: u16) {
    // 憑證要在任何其他檢查之前先取。自測在背景非同步進行，探測期間使用者可能
    // 已經中斷或重接了這個出口，晚到的結果靠憑證擋在門外——但號碼要是等到
    // is_connected／begin_test 之後才讀，halt 剛好插在中間時讀到的就是 halt
    // 換過的**新**號碼，之後那道檢查一路都會成立，守門形同虛設，
    // 一份對舊連線做的探測結果就這樣寫進了新連線。
    let token = state.test_token(local);
    if !state.is_connected(local) {
        state.log_exit(local, format!("port {local} : not connected, cannot test"));
        return;
    }
    if !state.begin_test(local, token) {
        return; // 同一輪連線已經在測了
    }
    // 連「testing...」這個佔位也走憑證版：從取憑證到寫下去之間一樣有窗口，
    // 沒守門的話 halt 剛清乾淨的自測欄會被這一手寫回一個永遠不會有結果的
    // testing（它的結果稍後會被憑證擋掉），介面就這樣一直轉下去
    state.set_exit_test_of(local, token, test_state::TESTING, "testing...");
    let st = state.clone();
    tauri::async_runtime::spawn(async move {
        let result = tauri::async_runtime::spawn_blocking(move || probe(local)).await;
        st.end_test(local, token);
        if !st.test_alive(local, token) {
            return;
        }
        let (state_name, text) = match result {
            Ok(ExitTest::Ok(text)) => (test_state::OK, text),
            Ok(ExitTest::Fail(msg)) => (test_state::FAIL, msg.to_string()),
            Err(_) => (test_state::FAIL, "no response".to_string()),
        };
        st.set_exit_test_of(local, token, state_name, &text);
        st.log_exit(local, format!("port {local} : {text}"));
    });
}

/// 對所有目前連線中的出口逐一重接（跨源）。停用中的出口不在名單裡，
/// 維持停用，不會被這個動作拉起來。對應托盤根層的「Reconnect all」。
pub fn reconnect_all(state: &Arc<AppState>) {
    let ports: Vec<u16> =
        state.with_config(|c| c.locals()).into_iter().filter(|p| state.is_running(*p)).collect();
    if ports.is_empty() {
        state.log("no running exit to reconnect");
        return;
    }
    for p in ports {
        restart(state, p);
    }
    state.log("reconnecting exits...");
}

/// 對單一源底下所有目前連線中的出口逐一重接，停用中的維持停用。
/// 對應托盤子選單的「Reconnect」；主視窗 ⋯ 選單的同名動作是前端逐條呼叫
/// restart_exit，不會走到這裡。
pub fn reconnect_source(state: &Arc<AppState>, source: &str) {
    let ports: Vec<u16> = state
        .with_config(|c| c.locals_of(source))
        .into_iter()
        .filter(|p| state.is_running(*p))
        .collect();
    if ports.is_empty() {
        state.log_from(source, "no running exit to reconnect");
        return;
    }
    for p in ports {
        restart(state, p);
    }
    state.log_from(source, "reconnecting exits...");
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::Config;

    fn cfg() -> Config {
        Config {
            close_to_tray: true,
            check_for_updates: None,
            sources: vec![
                Source {
                    name: "hk".into(),
                    host: "h.example.com".into(),
                    user: "bob".into(),
                    proxy_command: "cloudflared access ssh --hostname %h".into(),
                    forwards: vec![
                        Forward {
                            name: "a".into(),
                            local: 1080,
                            remote: "127.0.0.1:1080".into(),
                            enabled: true,
                        },
                        Forward {
                            name: "b".into(),
                            local: 1083,
                            remote: "127.0.0.1:1083".into(),
                            enabled: false,
                        },
                    ],
                },
                Source {
                    name: "tw".into(),
                    host: "t.example.com".into(),
                    user: "alice".into(),
                    proxy_command: String::new(),
                    forwards: vec![Forward {
                        name: "c".into(),
                        local: 1090,
                        remote: "127.0.0.1:1090".into(),
                        enabled: true,
                    }],
                },
            ],
        }
    }

    /// 每條連線只背自己那一組 -L，其餘旗標與原本單一連線時完全相同
    #[test]
    fn args_carry_only_one_forward() {
        let c = cfg();
        let s = &c.sources[0];
        let a = build_exit_args(s, &s.forwards[0]);
        assert_eq!(
            a,
            vec![
                "-N",
                "-o",
                "ServerAliveInterval=10",
                "-o",
                "ServerAliveCountMax=2",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "ProxyCommand=cloudflared access ssh --hostname %h",
                "-L",
                "1080:127.0.0.1:1080",
                "bob@h.example.com",
            ]
        );
        let b = build_exit_args(s, &s.forwards[1]);
        assert_eq!(b.iter().filter(|s| *s == "-L").count(), 1);
        assert!(b.contains(&"1083:127.0.0.1:1083".to_string()));
        assert!(!b.contains(&"1080:127.0.0.1:1080".to_string()));
    }

    /// 每個出口的 ssh 參數都取自它所屬的源，不會串到別的源去
    #[test]
    fn args_come_from_the_owning_source() {
        let c = cfg();
        for local in c.locals() {
            let (src, f) = c.locate(local).unwrap();
            let args = build_exit_args(src, f);
            assert_eq!(args.last().unwrap(), &format!("{}@{}", src.user, src.host));
        }
        // tw 的出口不帶 hk 的 ProxyCommand
        let (tw, f) = c.locate(1090).unwrap();
        let args = build_exit_args(tw, f);
        assert_eq!(args.last().unwrap(), "alice@t.example.com");
        assert!(!args.iter().any(|s| s.starts_with("ProxyCommand=")));
        // hk 的出口才有
        let (hk, f) = c.locate(1080).unwrap();
        assert!(build_exit_args(hk, f)
            .iter()
            .any(|s| s == "ProxyCommand=cloudflared access ssh --hostname %h"));
    }

    #[test]
    fn empty_proxy_command_is_omitted() {
        let mut c = cfg();
        c.sources[0].proxy_command = "   ".into();
        let s = &c.sources[0];
        let a = build_exit_args(s, &s.forwards[0]);
        assert!(!a.iter().any(|s| s.starts_with("ProxyCommand=")));
        assert_eq!(a.last().unwrap(), "bob@h.example.com");
    }

    /// 斷線偵測窗口是規格：Interval × CountMax 決定使用者要盯著一個假的
    /// connected 多久。Wi-Fi 斷掉時 TCP 本身不會馬上知道，全靠這一對把時間壓下來，
    /// 被放寬回去的話畫面又會停在「連著卻不通」好幾分鐘
    #[test]
    fn the_keepalive_window_stays_under_half_a_minute() {
        let c = cfg();
        let s = &c.sources[0];
        let args = build_exit_args(s, &s.forwards[0]);
        let value = |key: &str| {
            args.iter()
                .find_map(|a| a.strip_prefix(key))
                .unwrap_or_else(|| panic!("少了 {key}"))
                .parse::<u32>()
                .expect("值要是數字")
        };
        let interval = value("ServerAliveInterval=");
        let count = value("ServerAliveCountMax=");
        assert!(interval * count <= 30, "偵測窗口 {interval}x{count} 秒太寬");
        assert!(interval >= 5, "探測太密只是白費封包");
    }

    #[test]
    fn port_busy_only_when_something_is_listening() {
        assert!(port_busy_detail(1080, false).is_none());
        let detail = port_busy_detail(1080, true).expect("佔用時要給 detail");
        assert!(detail.contains("1080"));
    }

    /// 連線測試不帶 -N -L，只做一次性登入即退出，且要有 BatchMode／ConnectTimeout
    #[test]
    fn test_args_are_a_one_shot_login_without_any_forward() {
        let args = build_test_args("bob", "h.example.com", "");
        assert_eq!(
            args,
            vec!["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "bob@h.example.com", "exit"]
        );
        assert!(!args.contains(&"-N".to_string()));
        assert!(!args.iter().any(|a| a == "-L"));
    }

    /// 表單有填 ProxyCommand 就照 build_exit_args 的作法帶一個 -o ProxyCommand=
    #[test]
    fn test_args_carry_proxy_command_when_present() {
        let args =
            build_test_args("alice", "t.example.com", "cloudflared access ssh --hostname %h");
        assert_eq!(
            args,
            vec![
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ProxyCommand=cloudflared access ssh --hostname %h",
                "alice@t.example.com",
                "exit",
            ]
        );
    }

    /// 空白（或只有空白）的 ProxyCommand 一樣要省略，跟 build_exit_args 一致
    #[test]
    fn test_args_omit_blank_proxy_command() {
        let args = build_test_args("bob", "h.example.com", "   ");
        assert!(!args.iter().any(|a| a.starts_with("ProxyCommand=")));
        assert_eq!(
            args,
            vec!["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "bob@h.example.com", "exit"]
        );
    }
}
