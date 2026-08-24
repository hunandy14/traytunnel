//! `wg::socks5` 的測試——設計書 §5 的 W2 系列（29 條）。
//!
//! W2.1–W2.18 是純函式（F）；W2.19–W2.29 走真 loopback socket（M），
//! 對面接一個**測試自寫的假 stack**：把 `StackCmd` 通道收下來，照劇本回應。
//!
//! 每一條走 socket 的測試都套 [`IO_TIMEOUT`]：伺服器沒回話時要 FAIL，
//! 不可以整套測試卡在那裡等。

use super::*;

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::wg::stack::{Conn, ConnectError, StackCmd, VirtualPort};

const IO_TIMEOUT: Duration = Duration::from_secs(3);

/// 所有等待都要有上限，否則 todo!() 的骨架會讓整套測試卡死而不是紅
async fn deadline<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(IO_TIMEOUT, f).await.expect("等待逾時：對面沒有回應")
}

// ------------------------------------------------------------ 純函式（F）

/// W2.1 greeting `05 01 00`
#[test]
fn greeting_with_a_single_no_auth_method() {
    assert_eq!(parse_greeting(&[0x05, 0x01, 0x00]).unwrap(), Greeting::Method(METHOD_NO_AUTH));
}

/// W2.2 greeting `05 03 00 01 02`：清單裡有 0x00 就選它
#[test]
fn no_auth_wins_when_it_is_in_the_list() {
    assert_eq!(
        parse_greeting(&[0x05, 0x03, 0x00, 0x01, 0x02]).unwrap(),
        Greeting::Method(METHOD_NO_AUTH)
    );
}

/// W2.3 greeting `05 01 02`：只有 USER/PASS，呼叫端要送 `05 FF`
#[test]
fn a_list_without_no_auth_is_not_acceptable() {
    assert_eq!(parse_greeting(&[0x05, 0x01, 0x02]).unwrap(), Greeting::NoAcceptable);
    assert_eq!(METHOD_NONE, 0xFF);
}

/// W2.4 SOCKS4 的 greeting：錯誤，且**不得**用 SOCKS5 的格式回覆
#[test]
fn socks4_is_refused_without_a_socks5_reply() {
    assert_eq!(parse_greeting(&[0x04, 0x01, 0x00]).unwrap_err(), GreetingError::NotSocks5);
}

/// W2.5 `NMETHODS = 0`
#[test]
fn a_greeting_with_no_methods_is_malformed() {
    assert_eq!(parse_greeting(&[0x05, 0x00]).unwrap_err(), GreetingError::Malformed);
}

/// W2.6 截斷的 greeting 是「需要更多位元組」，不是錯誤
#[test]
fn a_truncated_greeting_asks_for_more_bytes() {
    assert_eq!(parse_greeting(&[0x05]).unwrap(), Greeting::NeedMore);
    assert_eq!(parse_greeting(&[0x05, 0x03, 0x00]).unwrap(), Greeting::NeedMore);
}

/// W2.7 request `ATYP=01`
#[test]
fn request_with_an_ipv4_target() {
    let buf = [0x05, 0x01, 0x00, 0x01, 10, 0, 0, 2, 0x01, 0xBB];
    assert_eq!(parse_request(&buf).unwrap(), Target::Ip(Ipv4Addr::new(10, 0, 0, 2).into(), 443));
}

/// W2.8 request `ATYP=04`
#[test]
fn request_with_an_ipv6_target() {
    let ip = Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1);
    let mut buf = vec![0x05, 0x01, 0x00, 0x04];
    buf.extend_from_slice(&ip.octets());
    buf.extend_from_slice(&443u16.to_be_bytes());
    assert_eq!(parse_request(&buf).unwrap(), Target::Ip(ip.into(), 443));
}

/// W2.9 request `ATYP=03`，名字不含尾端點
#[test]
fn request_with_a_domain_name() {
    let name = b"example.com";
    let mut buf = vec![0x05, 0x01, 0x00, 0x03, name.len() as u8];
    buf.extend_from_slice(name);
    buf.extend_from_slice(&443u16.to_be_bytes());
    assert_eq!(parse_request(&buf).unwrap(), Target::Domain("example.com".into(), 443));
}

/// W2.10 `ATYP=03` 且 len=0
#[test]
fn an_empty_domain_name_is_a_general_failure() {
    let buf = [0x05, 0x01, 0x00, 0x03, 0x00, 0x01, 0xBB];
    assert_eq!(parse_request(&buf).unwrap_err(), Reply::GeneralFailure);
}

/// W2.11 不存在的 ATYP
#[test]
fn an_unknown_address_type_is_reported_as_such() {
    let buf = [0x05, 0x01, 0x00, 0x02, 10, 0, 0, 2, 0x01, 0xBB];
    assert_eq!(parse_request(&buf).unwrap_err(), Reply::AddressTypeNotSupported);
}

/// W2.12 `BIND` 與 `UDP ASSOCIATE` 一律 0x07（R4：明確拒絕而不是靜默失敗）
#[test]
fn bind_and_udp_associate_are_not_supported() {
    for cmd in [0x02u8, 0x03] {
        let buf = [0x05, cmd, 0x00, 0x01, 10, 0, 0, 2, 0x01, 0xBB];
        assert_eq!(parse_request(&buf).unwrap_err(), Reply::CommandNotSupported, "CMD={cmd:#04x}");
    }
}

/// W2.13 request 的 `VER != 05`
#[test]
fn a_request_with_a_wrong_version_is_refused() {
    let buf = [0x04, 0x01, 0x00, 0x01, 10, 0, 0, 2, 0x01, 0xBB];
    assert!(parse_request(&buf).is_err());
}

/// W2.14 port = 0
#[test]
fn port_zero_is_refused() {
    let buf = [0x05, 0x01, 0x00, 0x01, 10, 0, 0, 2, 0x00, 0x00];
    assert!(parse_request(&buf).is_err());
}

/// W2.15 `encode_reply` 的位元組序列逐一比對
#[test]
fn encode_reply_for_an_ipv4_bound_address() {
    let bound: SocketAddr = "10.0.0.2:41000".parse().unwrap();
    assert_eq!(
        encode_reply(Reply::Success, bound),
        vec![0x05, 0x00, 0x00, 0x01, 0x0A, 0x00, 0x00, 0x02, 0xA0, 0x28]
    );
}

/// W2.16 `encode_reply` 對 IPv6 綁定位址
#[test]
fn encode_reply_for_an_ipv6_bound_address() {
    let ip = Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 2);
    let out = encode_reply(Reply::Success, SocketAddr::new(ip.into(), 41000));
    assert_eq!(out[3], 0x04, "ATYP 要是 0x04");
    assert_eq!(out.len(), 4 + 16 + 2);
    assert_eq!(&out[4..20], &ip.octets());
}

/// W2.17 `ConnectError` → 回覆碼的對照
#[test]
fn connect_errors_map_to_the_right_reply_codes() {
    assert_eq!(reply_for(&ConnectError::Refused), Reply::ConnectionRefused);
    assert_eq!(reply_for(&ConnectError::Timeout), Reply::HostUnreachable);
    assert_eq!(reply_for(&ConnectError::NotAllowed), Reply::NotAllowed);
    assert_eq!(reply_for(&ConnectError::NoRoute), Reply::NetworkUnreachable);
    assert_eq!(reply_for(&ConnectError::Exhausted), Reply::GeneralFailure);
}

/// W2.18 網域名超過 255 位元組，協定上根本編不出來
#[test]
fn an_over_long_domain_name_is_refused() {
    let name = vec![b'a'; 256];
    let mut buf = vec![0x05, 0x01, 0x00, 0x03, 0xFF];
    buf.extend_from_slice(&name);
    buf.extend_from_slice(&443u16.to_be_bytes());
    assert!(parse_request(&buf).is_err());
}

// ------------------------------------------------------------ 假 stack（M）

/// 假 stack 的劇本
enum Script {
    /// 每次 Connect 都成功，並把上行資料原樣 echo 回下行
    Echo,
    /// 每次 Connect 都回這個錯
    Fail(ConnectError),
    /// 網域查詢回這個錯
    ResolveFails(crate::wg::dns::ResolveError),
    /// 前 n 條成功，之後一律 Exhausted
    LimitAt(usize),
}

/// 假 stack 的觀測點
#[derive(Default)]
struct Observed {
    /// 「有人呼叫了系統解析器」——真的被翻起來就代表洩漏防線破了
    system_resolver_used: AtomicBool,
    connects: AtomicUsize,
    /// 上行最多同時積了幾塊（W2.26 的反壓斷言）
    max_inflight: AtomicUsize,
}

/// 起一顆假 stack，回傳 (指令 sender, 觀測點, join handle)
fn fake_stack(
    script: Script,
    cancel: tokio_util::sync::CancellationToken,
) -> (tokio::sync::mpsc::Sender<StackCmd>, Arc<Observed>, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<StackCmd>(64);
    let obs = Arc::new(Observed::default());
    let seen = obs.clone();
    let join = tokio::spawn(async move {
        loop {
            let cmd = tokio::select! {
                _ = cancel.cancelled() => break,
                c = rx.recv() => match c { Some(c) => c, None => break },
            };
            match cmd {
                StackCmd::Connect { dst: _, reply } => {
                    let n = seen.connects.fetch_add(1, Ordering::SeqCst);
                    let outcome = match &script {
                        Script::Echo | Script::ResolveFails(_) => Ok(()),
                        Script::Fail(e) => Err(e.clone()),
                        Script::LimitAt(limit) => {
                            if n < *limit {
                                Ok(())
                            } else {
                                Err(ConnectError::Exhausted)
                            }
                        }
                    };
                    match outcome {
                        Err(e) => {
                            let _ = reply.send(Err(e));
                        }
                        Ok(()) => {
                            let (up_tx, mut up_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(
                                crate::wg::stack::CONN_CHANNEL_DEPTH,
                            );
                            let (down_tx, down_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(
                                crate::wg::stack::CONN_CHANNEL_DEPTH,
                            );
                            let conn = Conn {
                                port: VirtualPort(41000 + n as u16),
                                tx: up_tx,
                                rx: down_rx,
                            };
                            let seen2 = seen.clone();
                            tokio::spawn(async move {
                                let mut inflight = 0usize;
                                while let Some(chunk) = up_rx.recv().await {
                                    inflight += 1;
                                    seen2.max_inflight.fetch_max(inflight, Ordering::SeqCst);
                                    if down_tx.send(chunk).await.is_err() {
                                        break;
                                    }
                                    inflight -= 1;
                                }
                            });
                            let _ = reply.send(Ok(conn));
                        }
                    }
                }
                StackCmd::Resolve { name: _, reply } => {
                    let out = match &script {
                        Script::ResolveFails(e) => Err(e.clone()),
                        _ => Ok(vec![std::net::IpAddr::from(Ipv4Addr::new(10, 9, 0, 2))]),
                    };
                    let _ = reply.send(out);
                }
                StackCmd::Listen { .. } | StackCmd::Close { .. } => {}
            }
        }
    });
    (tx, obs, join)
}

/// 綁一個 loopback 監聽埠並把 serve_socks5 起起來
async fn serve(
    script: Script,
) -> (
    SocketAddr,
    Arc<Observed>,
    tokio_util::sync::CancellationToken,
    Vec<tokio::task::JoinHandle<()>>,
) {
    let listener = TcpListener::bind((BIND_ADDR, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let (stack, obs, stack_join) = fake_stack(script, cancel.clone());
    let c = cancel.clone();
    let server = tokio::spawn(async move { serve_socks5(listener, stack, c).await });
    (addr, obs, cancel, vec![server, stack_join])
}

/// 協商 + CONNECT 到一個 IPv4 目的地，回傳伺服器的回覆碼
async fn handshake_and_connect(sock: &mut TcpStream, target: &[u8]) -> u8 {
    deadline(sock.write_all(&[0x05, 0x01, 0x00])).await.unwrap();
    let mut hello = [0u8; 2];
    deadline(sock.read_exact(&mut hello)).await.unwrap();
    assert_eq!(hello, [0x05, 0x00], "協商要選 NO AUTH");
    deadline(sock.write_all(target)).await.unwrap();
    let mut head = [0u8; 4];
    deadline(sock.read_exact(&mut head)).await.unwrap();
    // 把 BND.ADDR/BND.PORT 讀完，免得殘留在串流裡
    let rest = match head[3] {
        0x01 => 4 + 2,
        0x04 => 16 + 2,
        0x03 => {
            let mut len = [0u8; 1];
            deadline(sock.read_exact(&mut len)).await.unwrap();
            len[0] as usize + 2
        }
        other => panic!("回覆的 ATYP 不合法：{other:#04x}"),
    };
    let mut buf = vec![0u8; rest];
    deadline(sock.read_exact(&mut buf)).await.unwrap();
    head[1]
}

fn connect_v4(ip: Ipv4Addr, port: u16) -> Vec<u8> {
    let mut b = vec![0x05, 0x01, 0x00, 0x01];
    b.extend_from_slice(&ip.octets());
    b.extend_from_slice(&port.to_be_bytes());
    b
}

fn connect_domain(name: &str, port: u16) -> Vec<u8> {
    let mut b = vec![0x05, 0x01, 0x00, 0x03, name.len() as u8];
    b.extend_from_slice(name.as_bytes());
    b.extend_from_slice(&port.to_be_bytes());
    b
}

/// W2.19 完整成功路徑：協商 → CONNECT → 雙向 64 KiB → 兩端都收到 EOF
#[tokio::test]
async fn a_full_connect_pumps_both_directions() {
    let (addr, _obs, cancel, _tasks) = serve(Script::Echo).await;
    let mut sock = deadline(TcpStream::connect(addr)).await.unwrap();
    let code = handshake_and_connect(&mut sock, &connect_v4(Ipv4Addr::new(10, 9, 0, 2), 7)).await;
    assert_eq!(code, Reply::Success as u8);

    let payload: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
    let (mut r, mut w) = sock.into_split();
    let sent = payload.clone();
    let writer = tokio::spawn(async move {
        w.write_all(&sent).await.unwrap();
        w.shutdown().await.unwrap();
    });
    let mut got = Vec::new();
    deadline(r.read_to_end(&mut got)).await.unwrap();
    writer.await.unwrap();
    assert_eq!(got, payload, "回來的位元組要與送出的完全一致");
    cancel.cancel();
}

/// W2.20 stack 回 Refused
#[tokio::test]
async fn a_refused_connect_answers_05_05() {
    let (addr, _obs, cancel, _tasks) = serve(Script::Fail(ConnectError::Refused)).await;
    let mut sock = deadline(TcpStream::connect(addr)).await.unwrap();
    let code = handshake_and_connect(&mut sock, &connect_v4(Ipv4Addr::new(10, 9, 0, 2), 9)).await;
    assert_eq!(code, Reply::ConnectionRefused as u8);
    let mut tail = Vec::new();
    deadline(sock.read_to_end(&mut tail)).await.unwrap();
    assert!(tail.is_empty(), "回完錯誤碼就該關掉連線");
    cancel.cancel();
}

/// W2.21 AllowedIPs 擋下 → `05 02`
#[tokio::test]
async fn a_blocked_destination_answers_05_02() {
    let (addr, _obs, cancel, _tasks) = serve(Script::Fail(ConnectError::NotAllowed)).await;
    let mut sock = deadline(TcpStream::connect(addr)).await.unwrap();
    let code = handshake_and_connect(&mut sock, &connect_v4(Ipv4Addr::new(10, 99, 0, 1), 80)).await;
    assert_eq!(code, Reply::NotAllowed as u8);
    cancel.cancel();
}

/// W2.22 隧道內 DNS 回 NotFound → `05 04`
#[tokio::test]
async fn a_name_that_does_not_resolve_answers_05_04() {
    let (addr, _obs, cancel, _tasks) =
        serve(Script::ResolveFails(crate::wg::dns::ResolveError::NotFound)).await;
    let mut sock = deadline(TcpStream::connect(addr)).await.unwrap();
    let code = handshake_and_connect(&mut sock, &connect_domain("nonexistent.invalid", 443)).await;
    assert_eq!(code, Reply::HostUnreachable as u8);
    cancel.cancel();
}

/// W2.23 conf 沒有 DNS 伺服器：回 `05 04`，且**絕不**退回本機解析
#[tokio::test]
async fn without_dns_servers_a_domain_never_falls_back_to_the_system_resolver() {
    let (addr, obs, cancel, _tasks) =
        serve(Script::ResolveFails(crate::wg::dns::ResolveError::NoServers)).await;
    let mut sock = deadline(TcpStream::connect(addr)).await.unwrap();
    let code = handshake_and_connect(&mut sock, &connect_domain("nonexistent.invalid", 443)).await;
    assert_eq!(code, Reply::HostUnreachable as u8);
    assert!(
        !obs.system_resolver_used.load(Ordering::SeqCst),
        "退回本機解析正是這份設計要避免的洩漏"
    );
    cancel.cancel();
}

/// W2.24 客戶端在協商到一半就關閉：任務乾淨結束，不 panic、不洩漏
#[tokio::test]
async fn a_client_that_disappears_mid_handshake_is_cleaned_up() {
    let (addr, _obs, cancel, tasks) = serve(Script::Echo).await;
    {
        let mut sock = deadline(TcpStream::connect(addr)).await.unwrap();
        deadline(sock.write_all(&[0x05])).await.unwrap();
    }
    // 另起一條完整的連線，證明伺服器還活著
    let mut sock = deadline(TcpStream::connect(addr)).await.unwrap();
    let code = handshake_and_connect(&mut sock, &connect_v4(Ipv4Addr::new(10, 9, 0, 2), 7)).await;
    assert_eq!(code, Reply::Success as u8);
    cancel.cancel();
    for t in tasks {
        deadline(t).await.unwrap();
    }
}

/// W2.25 上行半關：隧道側收到 FIN，下行仍可繼續收到資料直到對端也關
#[tokio::test]
async fn a_half_close_upstream_still_lets_the_downstream_drain() {
    let (addr, _obs, cancel, _tasks) = serve(Script::Echo).await;
    let mut sock = deadline(TcpStream::connect(addr)).await.unwrap();
    let code = handshake_and_connect(&mut sock, &connect_v4(Ipv4Addr::new(10, 9, 0, 2), 7)).await;
    assert_eq!(code, Reply::Success as u8);
    let (mut r, mut w) = sock.into_split();
    deadline(w.write_all(b"the last words")).await.unwrap();
    deadline(w.shutdown()).await.unwrap();
    let mut got = Vec::new();
    deadline(r.read_to_end(&mut got)).await.unwrap();
    assert_eq!(got, b"the last words", "半關之後對端送回的最後一段不可以掉");
    cancel.cancel();
}

/// W2.26 上行反壓：對端不讀時記憶體不無限成長，客戶端的 write 最終阻塞
#[tokio::test]
async fn the_upstream_applies_backpressure_instead_of_buffering_forever() {
    let (addr, obs, cancel, _tasks) = serve(Script::Echo).await;
    let mut sock = deadline(TcpStream::connect(addr)).await.unwrap();
    let code = handshake_and_connect(&mut sock, &connect_v4(Ipv4Addr::new(10, 9, 0, 2), 7)).await;
    assert_eq!(code, Reply::Success as u8);
    let (_r, mut w) = sock.into_split();
    // 對面（測試自己）刻意不讀；寫到某個點一定要卡住
    let blocked = tokio::time::timeout(Duration::from_millis(500), async move {
        let chunk = vec![0u8; 64 * 1024];
        loop {
            w.write_all(&chunk).await.unwrap();
        }
    })
    .await;
    assert!(blocked.is_err(), "對端不讀時 write 最終要阻塞，不可以無上限吞進來");
    assert!(
        obs.max_inflight.load(Ordering::SeqCst) <= crate::wg::stack::CONN_CHANNEL_DEPTH + 1,
        "每條連線的在途資料塊要壓在通道深度內"
    );
    cancel.cancel();
}

/// W2.27 cancel：所有連線任務在 100ms 內結束，監聽埠釋放
#[tokio::test]
async fn cancelling_stops_every_task_and_frees_the_port() {
    let (addr, _obs, cancel, tasks) = serve(Script::Echo).await;
    let mut sock = deadline(TcpStream::connect(addr)).await.unwrap();
    let _ = handshake_and_connect(&mut sock, &connect_v4(Ipv4Addr::new(10, 9, 0, 2), 7)).await;
    cancel.cancel();
    for t in tasks {
        tokio::time::timeout(Duration::from_millis(100), t)
            .await
            .expect("取消後 100ms 內要收工")
            .unwrap();
    }
    TcpListener::bind(addr).await.expect("監聽埠要真的被釋放");
}

/// W2.28 連線數到 `max_connections`，第 N+1 條收到 `05 01`
#[tokio::test]
async fn the_connection_beyond_the_limit_gets_a_general_failure() {
    const LIMIT: usize = 4;
    let (addr, _obs, cancel, _tasks) = serve(Script::LimitAt(LIMIT)).await;
    let mut keep = Vec::new();
    for _ in 0..LIMIT {
        let mut s = deadline(TcpStream::connect(addr)).await.unwrap();
        let code = handshake_and_connect(&mut s, &connect_v4(Ipv4Addr::new(10, 9, 0, 2), 7)).await;
        assert_eq!(code, Reply::Success as u8);
        keep.push(s);
    }
    let mut extra = deadline(TcpStream::connect(addr)).await.unwrap();
    let code = handshake_and_connect(&mut extra, &connect_v4(Ipv4Addr::new(10, 9, 0, 2), 7)).await;
    assert_eq!(code, Reply::GeneralFailure as u8);
    cancel.cancel();
}

/// W2.29 `serve_forward`：不經協商，直接 pump 到固定目的地
#[tokio::test]
async fn a_static_forward_pumps_without_any_negotiation() {
    let listener = TcpListener::bind((BIND_ADDR, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let (stack, _obs, _stack_join) = fake_stack(Script::Echo, cancel.clone());
    let c = cancel.clone();
    let server =
        tokio::spawn(async move { serve_forward(listener, stack, "10.9.0.2:7".into(), c).await });

    let mut sock = deadline(TcpStream::connect(addr)).await.unwrap();
    let (mut r, mut w) = sock.split();
    deadline(w.write_all(b"straight through")).await.unwrap();
    let mut got = vec![0u8; b"straight through".len()];
    deadline(r.read_exact(&mut got)).await.unwrap();
    assert_eq!(&got, b"straight through");
    cancel.cancel();
    deadline(server).await.unwrap();
}

// ---------------------------------------------------------------- 回歸（審查補列）

/// `greeting_len` 認得 greeting 到哪裡為止，後面的位元組屬於下一段訊息
#[test]
fn the_greeting_length_marks_where_the_next_message_starts() {
    assert_eq!(greeting_len(&[0x05, 0x01, 0x00]), 3);
    assert_eq!(greeting_len(&[0x05, 0x03, 0x00, 0x01, 0x02]), 5);
    // 後面跟著一份 CONNECT：greeting 只佔前三個位元組
    let mut pipelined = vec![0x05, 0x01, 0x00];
    pipelined.extend_from_slice(&connect_v4(Ipv4Addr::new(10, 9, 0, 2), 7));
    assert_eq!(greeting_len(&pipelined), 3);
}

/// 回歸：客戶端把 greeting、CONNECT **與第一段酬載**貼在同一個 TCP 段裡送出來。
///
/// 協商那一段是「讀到夠為止」，所以它很可能把後面那兩樣也一起讀進緩衝。丟掉
/// 那些位元組的話，接下來的 `read_exact` 會去等一份**已經到了**的請求——
/// 連線就這樣卡到逾時，而且伺服器這一側看起來完全正常。
///
/// 這條測試刻意只寫一次 `write_all`：拆成三次就分成三個段，緩衝裡剛好只有
/// greeting，那正是這個 bug 躲過所有既有測試的原因。
#[tokio::test]
async fn a_client_that_pipelines_everything_into_one_segment_is_served() {
    let (addr, _obs, cancel, _tasks) = serve(Script::Echo).await;
    let mut sock = deadline(TcpStream::connect(addr)).await.unwrap();

    let payload = b"pipelined payload";
    let mut all = vec![0x05, 0x01, 0x00];
    all.extend_from_slice(&connect_v4(Ipv4Addr::new(10, 9, 0, 2), 7));
    all.extend_from_slice(payload);
    deadline(sock.write_all(&all)).await.unwrap();

    // 協商回覆
    let mut hello = [0u8; 2];
    deadline(sock.read_exact(&mut hello)).await.unwrap();
    assert_eq!(hello, [0x05, 0x00]);
    // CONNECT 回覆（ATYP=01 → 4 + 2）
    let mut reply = [0u8; 4];
    deadline(sock.read_exact(&mut reply)).await.unwrap();
    assert_eq!(reply[1], Reply::Success as u8, "貼在同一段裡的 CONNECT 也要被讀到");
    let mut bound = [0u8; 6];
    deadline(sock.read_exact(&mut bound)).await.unwrap();

    // 貼在 CONNECT 後面的酬載也不可以掉，而且要排在後續位元組之前
    deadline(sock.write_all(b" and more")).await.unwrap();
    let mut echoed = vec![0u8; payload.len() + b" and more".len()];
    deadline(sock.read_exact(&mut echoed)).await.unwrap();
    assert_eq!(&echoed, b"pipelined payload and more", "順序與內容都要原封不動");
    cancel.cancel();
}
