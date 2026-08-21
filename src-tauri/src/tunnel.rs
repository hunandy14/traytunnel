//! 隧道生命週期：spawn ssh、2 秒輪詢判斷連上、斷線固定 5 秒重連。

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::config::Config;
use crate::exits::{probe, ExitTest};
use crate::state::AppState;
use crate::winsys::{is_listening, Job};

/// CREATE_NO_WINDOW，杜絕黑窗一閃
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 連線偵測輪詢間隔
const POLL: Duration = Duration::from_millis(2000);
/// 斷線後固定重連間隔，無退避無上限
const RETRY: Duration = Duration::from_secs(5);

/// 組 ssh 參數，每個 token 獨立傳遞，不做字串拼接。
pub fn build_args(cfg: &Config) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-N".into(),
        "-o".into(),
        "ServerAliveInterval=30".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
    ];
    if !cfg.proxy_command.trim().is_empty() {
        args.push("-o".into());
        args.push(format!("ProxyCommand={}", cfg.proxy_command));
    }
    for f in &cfg.forwards {
        args.push("-L".into());
        args.push(format!("{}:{}", f.local, f.remote));
    }
    args.push(format!("{}@{}", cfg.user, cfg.host));
    args
}

fn spawn_ssh(cfg: &Config) -> std::io::Result<(Child, Job, u32)> {
    let job = Job::new()?;
    let mut cmd = Command::new("ssh");
    cmd.args(build_args(cfg))
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

/// 啟動一輪監看，呼叫端負責先設定 want_run。
pub fn start(state: &Arc<AppState>) {
    let generation = state.next_generation();
    let st = state.clone();
    tauri::async_runtime::spawn(async move { supervise(st, generation).await });
}

/// 停止目前的隧道：世代遞增讓監看迴圈作廢，關 job handle 收掉整棵程序樹。
pub fn stop(state: &Arc<AppState>) {
    state.next_generation();
    state.kill_job();
    state.set_connected(false);
}

/// 設定變更後重新連線
pub fn restart(state: &Arc<AppState>) {
    stop(state);
    if state.want_run() {
        start(state);
    }
}

async fn supervise(state: Arc<AppState>, generation: u64) {
    loop {
        if state.generation() != generation || !state.want_run() {
            return;
        }
        let cfg = state.config();
        if cfg.forwards.is_empty() {
            state.set_status("Stopped", "muted");
            state.log("no forwards configured");
            return;
        }

        state.set_connected(false);
        state.set_status("Connecting...", "amber");

        // spawn 失敗時自己交代重試，不要再補一行「disconnected」
        let mut spawn_failed = false;
        match spawn_ssh(&cfg) {
            Err(e) => {
                spawn_failed = true;
                state.log(format!("tunnel failed to start: {e}, retrying in 5s"));
            }
            Ok((mut child, job, pid)) => {
                state.store_job(generation, job);
                state.log(format!("tunnel starting (pid {pid})"));
                if let Some(stderr) = child.stderr.take() {
                    // ssh 的錯誤訊息只寫進檔案日誌，維持活動區與原版一致
                    tauri::async_runtime::spawn(async move {
                        let mut lines = BufReader::new(stderr).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            log::warn!("ssh: {line}");
                        }
                    });
                }

                let first_port = cfg.forwards[0].local;
                loop {
                    tokio::time::sleep(POLL).await;
                    if state.generation() != generation {
                        return; // 已被 stop/restart 作廢，job 也已關閉
                    }
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) => {}
                    }
                    if !state.connected() && is_listening(first_port) {
                        state.set_connected(true);
                        state.set_status("Connected", "accent");
                        state.log("tunnel up");
                        start_exit_tests(&state);
                    }
                }
                // ssh 退了，順手把 ProxyCommand 生出來的子程序一起收掉
                state.kill_job_of(generation);
            }
        }

        if state.generation() != generation || !state.want_run() {
            return;
        }
        state.set_connected(false);
        if !spawn_failed {
            state.log("disconnected, retrying in 5s");
        }
        state.set_status("Reconnecting...", "amber");
        state.reset_exits();

        let slice = Duration::from_millis(250);
        let mut waited = Duration::ZERO;
        while waited < RETRY {
            tokio::time::sleep(slice).await;
            waited += slice;
            if state.generation() != generation || !state.want_run() {
                return;
            }
        }
    }
}

/// 對每個 forward 平行做出口自測
pub fn start_exit_tests(state: &Arc<AppState>) {
    for f in state.config().forwards {
        let port = f.local;
        if !state.begin_test(port) {
            continue; // 同一個埠已經在測了
        }
        state.set_exit(port, "testing", "testing...");
        let st = state.clone();
        tauri::async_runtime::spawn(async move {
            let result = tauri::async_runtime::spawn_blocking(move || probe(port)).await;
            st.end_test(port);
            match result {
                Ok(ExitTest::Ok(text)) => {
                    st.set_exit(port, "ok", &text);
                    st.log(format!("port {port} : {text}"));
                }
                Ok(ExitTest::Fail(msg)) => {
                    st.set_exit(port, "fail", msg);
                    st.log(format!("port {port} : {msg}"));
                }
                Err(_) => {
                    st.set_exit(port, "fail", "no response");
                    st.log(format!("port {port} : no response"));
                }
            }
        });
    }
    state.log("testing exits...");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Forward;

    fn cfg() -> Config {
        Config {
            host: "h.example.com".into(),
            user: "bob".into(),
            proxy_command: "cloudflared access ssh --hostname %h".into(),
            close_to_tray: true,
            forwards: vec![
                Forward { name: "a".into(), local: 1080, remote: "127.0.0.1:1080".into() },
                Forward { name: "b".into(), local: 1083, remote: "127.0.0.1:1083".into() },
            ],
        }
    }

    #[test]
    fn args_match_original_options() {
        let a = build_args(&cfg());
        assert_eq!(
            a,
            vec![
                "-N",
                "-o",
                "ServerAliveInterval=30",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "ProxyCommand=cloudflared access ssh --hostname %h",
                "-L",
                "1080:127.0.0.1:1080",
                "-L",
                "1083:127.0.0.1:1083",
                "bob@h.example.com",
            ]
        );
    }

    #[test]
    fn empty_proxy_command_is_omitted() {
        let mut c = cfg();
        c.proxy_command = "   ".into();
        let a = build_args(&c);
        assert!(!a.iter().any(|s| s.starts_with("ProxyCommand=")));
        assert_eq!(a.last().unwrap(), "bob@h.example.com");
    }
}
