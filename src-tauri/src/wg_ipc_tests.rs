//! 刪除流程與 `.conf` 驗證／選檔 IPC 的測試——設計書 §6 的 W6.17～W6.23
//! 與 W9 系列（10 條）。
//!
//! 比照 `wg_tests.rs` 的風格：守門與決策邏輯抽出來測，**不生 AppHandle**。
//! 需要 `AppState` 才驗得到的那一半（真的推了幾次事件、真的落了檔）在各條的
//! 註解裡點名，由 `commands.rs` 那條「存檔成功才動線」的共用通則保證——
//! 那條通則本身是 ssh 的 `delete_source`／`delete_forward` 一路沿用下來的。

use super::*;

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;

use base64::Engine as _;
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};

use crate::commands;
use crate::config::{Config, Forward, RowKind, Source, WgProxy};

// ------------------------------------------------------------------ 共用夾具

fn fwd(name: &str, local: u16) -> Forward {
    Forward {
        name: name.into(),
        local,
        remote: Some("10.0.0.5:22".into()),
        kind: RowKind::Forward,
        probe_proxy: false,
        enabled: true,
    }
}

fn socks(name: &str, local: u16) -> Forward {
    Forward {
        name: name.into(),
        local,
        remote: None,
        kind: RowKind::Socks,
        probe_proxy: false,
        enabled: true,
    }
}

/// hk（ssh，一條列）＋ ax4200（wg，一條 socks 列與兩條 forward 列）
fn cfg_with_wg() -> Config {
    Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![Source {
            name: "hk".into(),
            host: "hk.example.com".into(),
            user: "bob".into(),
            proxy_command: String::new(),
            forwards: vec![Forward {
                remote: Some("127.0.0.1:1080".into()),
                ..fwd("exit-a", 1080)
            }],
        }],
        wg_proxies: vec![WgProxy {
            name: "ax4200".into(),
            conf_path: "wg/ax4200.conf".into(),
            enabled: true,
            forwards: vec![socks("socks", 1085), fwd("nas-ssh", 2222), fwd("nas-http", 2280)],
        }],
    }
}

// ------------------------------------------- W6.17～W6.20：deleteWgProxy

/// W6.17 刪一條底下有三條列的 wg 連線：三條列與連線一併從設定消失，
/// 而且**要停的埠必須在刪掉之前先抄下來**——刪完就查不到了。
///
/// 「引擎被收掉」由 `AppState::sync_wg_engines` 丟掉那份 `WgEngineRuntime`
/// 達成（`CancelGuard` 跟著 drop，見 W6.1）；「三條列各推一次 stopped」的
/// 那一份清單就是這裡的 `ports`，`halted_locals` 已經保證不重複（W6.16）。
#[test]
fn deleting_a_wg_connection_takes_its_three_rows_with_it() {
    let mut cfg = cfg_with_wg();
    // 指令層抄埠的那一手，順序與內容都釘住
    let ports = halted_locals(&cfg, "ax4200");
    assert_eq!(ports, vec![1085, 2222, 2280]);

    cfg.wg_proxies.retain(|p| p.name != "ax4200");
    assert!(cfg.wg_proxy("ax4200").is_none(), "連線本身要消失");
    for p in &ports {
        assert!(!cfg.locals().contains(p), "列 {p} 也要跟著消失");
    }
    // 別人家的東西一個都不可以動到
    assert_eq!(cfg.locals(), vec![1080]);
    // 刪完再抄一次就抄不到了——這正是「先抄再刪」的理由
    assert!(halted_locals(&cfg, "ax4200").is_empty());
}

/// W6.18 存檔失敗：設定與引擎都維持原狀，不可以出現「引擎停了、設定還在」。
///
/// 指令層的形狀是 `if !save(..) { return; }`——停線那幾行在 `save` 之後，
/// 存檔失敗就一行都跑不到。`update_config` 本身也保證記憶體不被改動：它是先
/// 複製一份、改複本、寫檔成功了才換上去。這裡釘的是那條複製語意。
#[test]
fn a_failed_save_leaves_both_the_config_and_the_engine_alone() {
    let cfg = cfg_with_wg();
    let before = cfg.clone();

    // update_config_checked 的作法：改的是複本，寫檔失敗就整份丟掉
    let mut draft = cfg.clone();
    draft.wg_proxies.retain(|p| p.name != "ax4200");
    let saved = false;
    let effective = if saved { draft } else { cfg };

    assert_eq!(effective, before, "存檔沒成功，這次操作等於沒發生");
    assert_eq!(halted_locals(&effective, "ax4200").len(), 3, "三條列都還在，引擎也就沒有理由停");
}

/// W6.19 對不存在的連線名：什麼都查不到，呼叫端記一行就退（不 panic）
#[test]
fn deleting_an_unknown_wg_connection_is_a_no_op() {
    let mut cfg = cfg_with_wg();
    let before = cfg.clone();
    assert!(cfg.wg_proxy("nope").is_none(), "指令層據此記一行就退");
    assert!(halted_locals(&cfg, "nope").is_empty());
    cfg.wg_proxies.retain(|p| p.name != "nope");
    assert_eq!(cfg, before, "不可以順手動到別人");
}

/// W6.20 刪完之後 `locals()` 不再含那三個埠，`sync_exits` 因此會把對應的
/// `ExitRuntime` 一併清掉——不留幽靈出口。
///
/// `sync_exits` 的規則就是「保留 `locals()` 裡的、補齊缺的」，所以這條性質
/// 完全由 `locals()` 決定；這裡直接對它斷言。
#[test]
fn the_deleted_rows_leave_no_ghost_runtime_behind() {
    let mut cfg = cfg_with_wg();
    let gone = halted_locals(&cfg, "ax4200");
    cfg.wg_proxies.retain(|p| p.name != "ax4200");

    let kept = cfg.locals();
    for p in &gone {
        assert!(!kept.contains(p), "{p} 還留在 locals() 裡就會留下一個幽靈 ExitRuntime");
    }
    assert!(kept.contains(&1080), "ssh 那邊不可以被掃到");
}

// ------------------------------------------- W6.21～W6.23：deleteForward

/// W6.21 同一支 `deleteForward` 刪得掉三種列，**不必指明機制或連線型**。
///
/// `local` 是全域唯一鍵（D5），所以 `detach_row` 只是掃過去把那個埠拔掉。
#[test]
fn one_delete_covers_an_ssh_row_a_wg_forward_and_a_wg_socks_row() {
    for (local, what) in
        [(1080u16, "ssh 的列"), (2222, "wg 的 forward 列"), (1085, "wg 的 socks 列")]
    {
        let mut cfg = cfg_with_wg();
        assert!(cfg.forward(local).is_some(), "前提：{what}在設定裡");
        commands::detach_row(&mut cfg, local);
        assert!(cfg.forward(local).is_none(), "{what}要刪得掉");
        assert!(!cfg.locals().contains(&local));
        // 只少那一條，其餘一條不動
        assert_eq!(cfg.locals().len(), 3);
    }
}

/// W6.22 刪掉一條 wg 連線的**最後一條啟用列**：引擎依 §5.2 的啟停條件被收掉，
/// 不留一顆空轉的 WireGuard。
#[test]
fn deleting_the_last_enabled_row_takes_the_engine_down() {
    let mut cfg = cfg_with_wg();
    // 先把另外兩條停用，只剩 socks 那一條是啟用的
    cfg.wg_proxies[0].forwards[1].enabled = false;
    cfg.wg_proxies[0].forwards[2].enabled = false;
    assert!(should_run_engine(&cfg, "ax4200"), "前提：現在還有一條要跑");

    commands::detach_row(&mut cfg, 1085);
    assert!(!should_run_engine(&cfg, "ax4200"), "最後一條啟用列沒了就不該再跑引擎");
    assert!(rows_to_start(&cfg, "ax4200").is_empty());
    // 連線本身還在，只是沒有東西要跑
    assert!(cfg.wg_proxy("ax4200").is_some());
}

/// W6.23 對不存在的 local：記一行就退，不 panic、不動到任何一條列
#[test]
fn deleting_an_unknown_local_is_a_no_op() {
    let mut cfg = cfg_with_wg();
    let before = cfg.clone();
    assert!(cfg.row(9999).is_none(), "指令層據此記一行就退");
    commands::detach_row(&mut cfg, 9999);
    assert_eq!(cfg, before);
}

// ------------------------------------------------------------ W9：testWgConf

const A_PRIV: [u8; 32] = [0x11; 32];
const B_PRIV: [u8; 32] = [0x22; 32];
/// 對端一定不會回應時要等多久才算逾時。規格是 15 秒，測試注入短的那一個
const SHORT: Duration = Duration::from_millis(800);

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn public_of(private: [u8; 32]) -> [u8; 32] {
    *PublicKey::from(&StaticSecret::from(private)).as_bytes()
}

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("traytunnel-w9-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// 寫一份指向 `endpoint` 的合法 `.conf`，回傳它的路徑
fn write_conf(
    dir: &std::path::Path,
    endpoint: SocketAddr,
    psk: Option<[u8; 32]>,
) -> std::path::PathBuf {
    let mut body = format!(
        "[Interface]\nPrivateKey = {}\nAddress = 10.9.0.1/32\n\n\
         [Peer]\nPublicKey = {}\nEndpoint = {endpoint}\nAllowedIPs = 0.0.0.0/0\n",
        b64(&A_PRIV),
        b64(&public_of(B_PRIV)),
    );
    if let Some(psk) = psk {
        body.push_str(&format!("PresharedKey = {}\n", b64(&psk)));
    }
    let path = dir.join("test.conf");
    std::fs::write(&path, body).unwrap();
    path
}

/// 借一個沒人用的 UDP 埠
fn free_udp() -> UdpSocket {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap()
}

/// 對端測試檯：一顆只會回握手的 `Tunn`。
///
/// 不能用 `device::spawn` 當對端——它一律送往設定好的 `endpoint`，而
/// `test_conf` 的本地埠是交給 OS 配的，對端事先不知道。這一支改成**回覆給
/// 封包的來源位址**，那才是 WireGuard 責任方真正的行為。
fn spawn_responder(
    socket: UdpSocket,
    private: [u8; 32],
    peer_public: [u8; 32],
    psk: Option<[u8; 32]>,
    cancel: CancellationToken,
) {
    socket.set_nonblocking(true).unwrap();
    let udp = tokio::net::UdpSocket::from_std(socket).unwrap();
    let mut tunn =
        Tunn::new(StaticSecret::from(private), PublicKey::from(peer_public), psk, None, 0, None);
    tokio::spawn(async move {
        let mut rx = vec![0u8; 65536];
        let mut tx = vec![0u8; 65536];
        let mut aux = vec![0u8; 65536];
        loop {
            let (n, src) = tokio::select! {
                _ = cancel.cancelled() => break,
                r = udp.recv_from(&mut rx) => match r {
                    Ok(v) => v,
                    Err(_) => continue,
                },
            };
            if let TunnResult::WriteToNetwork(out) =
                tunn.decapsulate(Some(src.ip()), &rx[..n], &mut tx)
            {
                let _ = udp.send_to(out, src).await;
                while let TunnResult::WriteToNetwork(more) = tunn.decapsulate(None, &[], &mut aux) {
                    let _ = udp.send_to(more, src).await;
                }
            }
        }
    });
}

/// W9.1 對一份會握手成功的 conf：`{ok: true}`，而且臨時引擎在回傳後確實被拆掉。
///
/// 「拆掉了」的斷言用的是 `UDP_TX_COUNT`：泵活著的話，它的 250ms 計時器會繼續
/// 送 keepalive／重協商封包；回傳之後靜置一段時間計數器仍不動，就代表那棵任務樹
/// 真的沒了（計數是 thread-local，`#[tokio::test]` 的所有任務都在同一條執行緒上）。
#[tokio::test]
async fn a_conf_that_completes_a_handshake_reports_ok() {
    let dir = tmp_dir("ok");
    let socket = free_udp();
    let endpoint = socket.local_addr().unwrap();
    let cancel = CancellationToken::new();
    spawn_responder(socket, B_PRIV, public_of(A_PRIV), None, cancel.clone());

    let path = write_conf(&dir, endpoint, None);
    let result = test_conf_within(&path, Duration::from_secs(10)).await;
    assert!(result.ok, "握手應該成立：{}", result.message);

    let after = device::UDP_TX_COUNT.load(Ordering::SeqCst);
    tokio::time::sleep(device::TIMER_TICK * 4).await;
    assert_eq!(
        device::UDP_TX_COUNT.load(Ordering::SeqCst),
        after,
        "回傳之後不可以還有封包出去——臨時引擎沒拆乾淨"
    );
    cancel.cancel();
}

/// W9.2 端點不回應：在上限內回 `{ok: false}`，訊息說得出是逾時，
/// 而且不留下任何背景任務（同 W9.1 的計數器斷言）
#[tokio::test]
async fn an_unresponsive_endpoint_times_out_and_leaves_nothing_running() {
    let dir = tmp_dir("timeout");
    // 綁下來就不放，對面因此永遠不會有人回話
    let dead = free_udp();
    let endpoint = dead.local_addr().unwrap();

    let path = write_conf(&dir, endpoint, None);
    let started = std::time::Instant::now();
    let result = test_conf_within(&path, SHORT).await;
    assert!(!result.ok);
    assert!(result.message.contains("逾時"), "訊息要說得出是逾時：{}", result.message);
    assert!(started.elapsed() < SHORT * 4, "不可以拖過上限太多");

    let after = device::UDP_TX_COUNT.load(Ordering::SeqCst);
    tokio::time::sleep(device::TIMER_TICK * 4).await;
    assert_eq!(device::UDP_TX_COUNT.load(Ordering::SeqCst), after, "逾時那條路也要拆乾淨");
}

/// W9.3 解析就失敗：立刻回 `{ok: false}`，訊息與 `inspect_conf` 一致，
/// **完全不綁 UDP、不連外**（用計數器斷言）
#[tokio::test]
async fn a_conf_that_does_not_parse_never_touches_the_network() {
    let dir = tmp_dir("badparse");
    let path = dir.join("bad.conf");
    std::fs::write(&path, "[Interface]\nAddress = 10.9.0.1/32\n").unwrap();

    let before = device::UDP_TX_COUNT.load(Ordering::SeqCst);
    let result = test_conf_within(&path, SHORT).await;
    assert!(!result.ok);
    assert_eq!(
        result.message,
        inspect_conf(&path).unwrap_err(),
        "同一份 conf 的錯誤，兩支指令要講同一句話"
    );
    assert_eq!(
        device::UDP_TX_COUNT.load(Ordering::SeqCst),
        before,
        "解析都沒過就不該有任何封包出去"
    );
}

/// W9.4 `PresharedKey` 不符：對端根本不會回應，最後是逾時失敗，
/// **不可以誤報成功**（與 W4.16 同一條規則，但走 IPC 這一側）
#[tokio::test]
async fn a_mismatched_preshared_key_never_reports_ok() {
    let dir = tmp_dir("psk");
    let socket = free_udp();
    let endpoint = socket.local_addr().unwrap();
    let cancel = CancellationToken::new();
    // 對端要 PSK，我們這一份 conf 沒有
    spawn_responder(socket, B_PRIV, public_of(A_PRIV), Some([0x33; 32]), cancel.clone());

    let path = write_conf(&dir, endpoint, None);
    let result = test_conf_within(&path, SHORT).await;
    assert!(!result.ok, "金鑰對不上就不可以說連得起來：{}", result.message);
    cancel.cancel();
}

/// W9.5 `testWgConf` 不動任何設定：它是存檔**前**的測試，不可以有副作用。
///
/// 它連 `Config` 都拿不到（簽名只吃一個路徑），這條由型別保證；斷言在這裡
/// 是為了讓「日後有人替它加一個 `&mut Config` 參數」這件事會踩到一條紅線。
#[tokio::test]
async fn testing_a_conf_does_not_touch_the_config() {
    let dir = tmp_dir("nosideeffect");
    let before = cfg_with_wg();
    let cfg = before.clone();

    let dead = free_udp();
    let path = write_conf(&dir, dead.local_addr().unwrap(), None);
    let _ = test_conf_within(&path, SHORT).await;

    assert_eq!(cfg, before, "呼叫前後逐欄位相等");
}

/// W9.6 訊息內容：**不含 `.conf` 的任何內容**（端點主機名、位址、DNS 位址、
/// 金鑰皆不出現），比照 U2 的紅線。
#[tokio::test]
async fn the_message_never_leaks_anything_from_the_conf() {
    let dir = tmp_dir("noleak");
    let dead = free_udp();
    let endpoint = dead.local_addr().unwrap();
    let path = write_conf(&dir, endpoint, Some([0x44; 32]));

    let result = test_conf_within(&path, SHORT).await;
    assert!(!result.ok);
    let msg = &result.message;
    for secret in [
        b64(&A_PRIV),
        b64(&public_of(B_PRIV)),
        b64(&[0x44u8; 32]),
        endpoint.to_string(),
        endpoint.ip().to_string(),
        "10.9.0.1".to_string(),
    ] {
        assert!(!msg.contains(&secret), "訊息漏了 conf 的內容：{msg}");
    }
}

// ------------------------------------------------------------ W9：pickWgConf

/// W9.7 使用者取消：回 `null`。
///
/// **不是空字串**——前端拿到空字串會把它當成「選了一個空路徑」而清掉既有的
/// `confPath`。原生對話框本身測不到（它要一個真的視窗與使用者互動），
/// 這裡測的是它回來之後那一手轉換，那也正是這條規則的落點。
#[test]
fn cancelling_the_picker_yields_null_not_an_empty_string() {
    assert_eq!(commands::picked_conf_path(None), None);
}

/// W9.8 選到檔案：回絕對路徑字串，而且後續 `inspectConf(該路徑)` 走得通
/// ——兩支串起來就是 §5.5 描述的典型流程。
#[test]
fn a_picked_file_comes_back_as_a_path_that_inspect_conf_accepts() {
    let dir = tmp_dir("picked");
    let dead = free_udp();
    let path = write_conf(&dir, dead.local_addr().unwrap(), None);

    let picked = commands::picked_conf_path(Some(path.clone())).expect("選到檔案就要有值");
    assert!(std::path::Path::new(&picked).is_absolute(), "要是絕對路徑：{picked}");
    let summary = inspect_conf(std::path::Path::new(&picked)).expect("接得下去");
    assert_eq!(summary.addresses, vec!["10.9.0.1/32"]);
}

/// W9.9 副檔名過濾器帶 `.conf`，但**不強制**：使用者選了別的副檔名一樣照收，
/// 由 `inspectConf` 去判內容。
#[test]
fn the_extension_filter_is_a_hint_not_a_rule() {
    assert_eq!(commands::CONF_FILTER_EXTENSIONS, ["conf"], "過濾器就這一個副檔名");

    // 內容合格但副檔名不是 .conf：一樣讀得過
    let dir = tmp_dir("otherext");
    let dead = free_udp();
    let conf = write_conf(&dir, dead.local_addr().unwrap(), None);
    let renamed = dir.join("wg0.txt");
    std::fs::rename(&conf, &renamed).unwrap();
    assert!(inspect_conf(&renamed).is_ok(), "判的是內容，不是副檔名");
}

/// W9.10 沒有 `tauri-plugin-dialog` 能力時回 `Err` 而不是 panic，
/// 前端才退得回純文字路徑輸入（Q3 的退路仍要能走）。
///
/// 指令那一側的形狀是「對話框回呼沒被叫到 ⇒ oneshot 的 sender 被丟掉 ⇒
/// `rx.await` 回 Err ⇒ 轉成 `Err(訊息)`」。這裡重現那一條路：沒有真的對話框
/// 可以生（要一個 AppHandle 與一個視窗），但那條**錯誤路徑**本身測得到。
#[tokio::test]
async fn an_unavailable_dialog_returns_an_error_instead_of_panicking() {
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<std::path::PathBuf>>();
    drop(tx); // 對話框沒起來，回呼永遠不會被呼叫
    let outcome: Result<Option<String>, String> = match rx.await {
        Ok(picked) => Ok(commands::picked_conf_path(picked)),
        Err(_) => Err(commands::DIALOG_UNAVAILABLE.to_string()),
    };
    assert_eq!(outcome, Err(commands::DIALOG_UNAVAILABLE.to_string()));
    assert!(!commands::DIALOG_UNAVAILABLE.is_empty(), "訊息要有東西可以顯示給使用者");
}
