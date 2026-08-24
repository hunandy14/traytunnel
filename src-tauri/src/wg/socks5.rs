//! SOCKS5 伺服器與靜態轉發器（設計書 §1.6）。
//!
//! 兩支監聽器共用同一套資料泵，差別只在 `serve_socks5` 多了一段協商、
//! `serve_forward` 的目的地固定。網域名一律交給隧道內的 DNS（socks5h 語意）——
//! 這個檔案裡**不存在**任何對系統解析器的呼叫，由 W1.31 的 grep 型測試釘住。

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::stack;

/// v1 只綁 loopback。
///
/// 要開放 `0.0.0.0` 就**必須同時做 SOCKS5 認證**，否則等於在區網開一個開放代理
/// （R9）。寫成常數就是為了避免日後被順手改成可設定。
pub const BIND_ADDR: std::net::Ipv4Addr = std::net::Ipv4Addr::LOCALHOST;

/// 協商階段只接受 NO AUTH
pub const METHOD_NO_AUTH: u8 = 0x00;

/// 沒有可接受的方法時要回的 `05 FF`
pub const METHOD_NONE: u8 = 0xFF;

/// SOCKS5 版本位元組
const VERSION: u8 = 0x05;

/// 只支援 CONNECT
const CMD_CONNECT: u8 = 0x01;

/// 一次從本地 TCP 連線讀進來的上限
const PUMP_CHUNK: usize = 16 * 1024;

/// SOCKS5 監聽器。v1 只綁 127.0.0.1，且只支援 NO AUTH。
pub async fn serve_socks5(
    listener: tokio::net::TcpListener,
    stack: mpsc::Sender<stack::StackCmd>,
    cancel: CancellationToken,
) {
    accept_loop(listener, cancel, move |sock, cancel| {
        let stack = stack.clone();
        async move { handle_socks5(sock, stack, cancel).await }
    })
    .await;
}

/// 靜態埠轉發：本地埠 → 隧道內固定的 host:port。
///
/// `dst` 是字串，因為 remote 可以寫成隧道內的網域名（每次連線各自解析）。
pub async fn serve_forward(
    listener: tokio::net::TcpListener,
    stack: mpsc::Sender<stack::StackCmd>,
    dst: String,
    cancel: CancellationToken,
) {
    accept_loop(listener, cancel, move |sock, cancel| {
        let stack = stack.clone();
        let dst = dst.clone();
        async move {
            let Ok(endpoint) = resolve_target(&parse_dst(&dst)?, &stack).await else {
                return None;
            };
            let conn = connect(&stack, endpoint).await.ok()?;
            pump(sock, conn, cancel).await;
            Some(())
        }
    })
    .await;
}

/// 監聽迴圈：一條連線一顆 tokio 任務，`cancel` 一取消全體收工。
async fn accept_loop<F, Fut>(
    listener: tokio::net::TcpListener,
    cancel: CancellationToken,
    handler: F,
) where
    F: Fn(TcpStream, CancellationToken) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Option<()>> + Send + 'static,
{
    loop {
        let accepted = tokio::select! {
            _ = cancel.cancelled() => break,
            r = listener.accept() => r,
        };
        let Ok((sock, _peer)) = accepted else { continue };
        let fut = handler(sock, cancel.clone());
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {}
                _ = fut => {}
            }
        });
    }
    // listener 在這裡 drop，監聽埠當場釋放（W2.27／W4.10）
}

async fn handle_socks5(
    mut sock: TcpStream,
    stack: mpsc::Sender<stack::StackCmd>,
    cancel: CancellationToken,
) -> Option<()> {
    // ---- 協商
    let mut buf = Vec::with_capacity(64);
    loop {
        match parse_greeting(&buf) {
            Ok(Greeting::Method(method)) => {
                sock.write_all(&[VERSION, method]).await.ok()?;
                break;
            }
            Ok(Greeting::NoAcceptable) => {
                let _ = sock.write_all(&[VERSION, METHOD_NONE]).await;
                return None;
            }
            // SOCKS4 之類：連 SOCKS5 的回覆格式都不該用，直接關掉（W2.4）
            Err(_) => return None,
            Ok(Greeting::NeedMore) => {
                let mut chunk = [0u8; 64];
                let n = sock.read(&mut chunk).await.ok()?;
                if n == 0 {
                    return None;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
        }
    }

    // ---- 請求。逐段讀「剛好那麼多」位元組，讓 parse_request 拿到的是一份完整
    // 且不多不少的請求（W2.18 靠長度不符擋掉編不出來的超長網域名）
    let mut req = vec![0u8; 4];
    sock.read_exact(&mut req).await.ok()?;
    let want = match request_len(&req) {
        Ok(n) => n,
        Err(reply) => {
            let _ = sock.write_all(&encode_reply(reply, unspecified())).await;
            return None;
        }
    };
    if want > req.len() {
        let mut rest = vec![0u8; want - req.len()];
        sock.read_exact(&mut rest).await.ok()?;
        req.extend_from_slice(&rest);
        // ATYP=03 的長度要等讀到 DOMAIN LEN 才算得出來，因此可能要再補一次
        if let Ok(total) = request_len(&req) {
            if total > req.len() {
                let mut more = vec![0u8; total - req.len()];
                sock.read_exact(&mut more).await.ok()?;
                req.extend_from_slice(&more);
            }
        }
    }

    let target = match parse_request(&req) {
        Ok(t) => t,
        Err(reply) => {
            let _ = sock.write_all(&encode_reply(reply, unspecified())).await;
            return None;
        }
    };

    // ---- 目的地。DOMAINNAME 一律交給隧道內的 DNS（socks5h 語意的唯一落點）
    let endpoint = match resolve_target(&target, &stack).await {
        Ok(e) => e,
        Err(reply) => {
            let _ = sock.write_all(&encode_reply(reply, unspecified())).await;
            return None;
        }
    };

    let conn = match connect(&stack, endpoint).await {
        Ok(c) => c,
        Err(e) => {
            let _ = sock.write_all(&encode_reply(reply_for(&e), unspecified())).await;
            return None;
        }
    };

    // BND.ADDR/BND.PORT 填這條連線在隧道內的落點。多數客戶端不看，但寫成
    // 0.0.0.0:0 會踩到少數嚴格的實作
    let bound = std::net::SocketAddr::new(to_std_ip(&endpoint.addr), conn.port.num());
    sock.write_all(&encode_reply(Reply::Success, bound)).await.ok()?;

    pump(sock, conn, cancel).await;
    Some(())
}

fn unspecified() -> std::net::SocketAddr {
    std::net::SocketAddr::from(([0u8, 0, 0, 0], 0))
}

fn to_std_ip(addr: &smoltcp::wire::IpAddress) -> std::net::IpAddr {
    match addr {
        smoltcp::wire::IpAddress::Ipv4(a) => std::net::IpAddr::V4(*a),
        smoltcp::wire::IpAddress::Ipv6(a) => std::net::IpAddr::V6(*a),
    }
}

/// `serve_forward` 的 `remote` 字串 → [`Target`]。
///
/// 這裡刻意只做字串拆解：主機名是不是解得開，是隧道內 DNS 的事。
fn parse_dst(dst: &str) -> Option<Target> {
    if let Ok(addr) = dst.parse::<std::net::SocketAddr>() {
        return Some(Target::Ip(addr.ip(), addr.port()));
    }
    let (host, port) = dst.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    if host.is_empty() || port == 0 {
        return None;
    }
    Some(Target::Domain(host.trim_matches(['[', ']']).to_string(), port))
}

/// `Target` → 隧道內的 `IpEndpoint`。網域名走 [`stack::StackCmd::Resolve`]。
async fn resolve_target(
    target: &Target,
    stack: &mpsc::Sender<stack::StackCmd>,
) -> Result<smoltcp::wire::IpEndpoint, Reply> {
    match target {
        Target::Ip(ip, port) => {
            Ok(smoltcp::wire::IpEndpoint::new(smoltcp::wire::IpAddress::from(*ip), *port))
        }
        Target::Domain(name, port) => {
            let (tx, rx) = oneshot::channel();
            stack
                .send(stack::StackCmd::Resolve { name: name.clone(), reply: tx })
                .await
                .map_err(|_| Reply::GeneralFailure)?;
            let ips = rx
                .await
                .map_err(|_| Reply::GeneralFailure)?
                .map_err(|e| reply_for_resolve(&e))?;
            let ip = ips.first().ok_or(Reply::HostUnreachable)?;
            Ok(smoltcp::wire::IpEndpoint::new(smoltcp::wire::IpAddress::from(*ip), *port))
        }
    }
}

async fn connect(
    stack: &mpsc::Sender<stack::StackCmd>,
    dst: smoltcp::wire::IpEndpoint,
) -> Result<stack::Conn, stack::ConnectError> {
    let (tx, rx) = oneshot::channel();
    stack
        .send(stack::StackCmd::Connect { dst, reply: tx })
        .await
        .map_err(|_| stack::ConnectError::Exhausted)?;
    rx.await.map_err(|_| stack::ConnectError::Exhausted)?
}

/// 任務跑完就把子任務收掉，不留孤兒
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// 雙向資料泵。
///
/// 上行 EOF 時對隧道側的 socket 做**半關**（丟掉 `Conn::tx`，stack 會在把剩餘
/// 位元組灌完之後才 `close()`），不是直接 abort——否則對端會少收最後一段資料。
async fn pump(sock: TcpStream, conn: stack::Conn, cancel: CancellationToken) {
    let stack::Conn { tx, mut rx, .. } = conn;
    let (mut reader, mut writer) = sock.into_split();

    let up_cancel = cancel.clone();
    let up = tokio::spawn(async move {
        let mut buf = vec![0u8; PUMP_CHUNK];
        loop {
            let read = tokio::select! {
                _ = up_cancel.cancelled() => break,
                r = reader.read(&mut buf) => r,
            };
            match read {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(Bytes::copy_from_slice(&buf[..n])).await.is_err() {
                        break;
                    }
                }
            }
        }
        // tx 在這裡 drop = 上行 EOF
    });
    let _guard = AbortOnDrop(up);

    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => break,
            c = rx.recv() => c,
        };
        let Some(chunk) = chunk else { break };
        if writer.write_all(&chunk).await.is_err() {
            break;
        }
    }
    let _ = writer.shutdown().await;
}

// ---- 純函式，方便逐條測試（見 §5 W2.x）----

#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Ip(std::net::IpAddr, u16),
    Domain(String, u16),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[repr(u8)]
pub enum Reply {
    Success = 0x00,
    GeneralFailure = 0x01,
    NotAllowed = 0x02,
    NetworkUnreachable = 0x03,
    HostUnreachable = 0x04,
    ConnectionRefused = 0x05,
    TtlExpired = 0x06,
    CommandNotSupported = 0x07,
    AddressTypeNotSupported = 0x08,
}

/// greeting 的解析結果。
///
/// 設計書 §1.6 把簽名寫成 `Result<u8, Reply>`，但那個形狀表達不出 W2.6 的
/// 「需要更多位元組」與 W2.3 的「無可接受方法」——後者要回 `05 FF` 而不是
/// 一般的 Reply 碼，前者根本不是錯誤。因此拆成這一組型別，語意對得上 §5。
#[derive(Debug, PartialEq, Eq)]
pub enum Greeting {
    /// 位元組還不夠，呼叫端續讀（W2.6）
    NeedMore,
    /// 協商成功，選定的方法
    Method(u8),
    /// 清單裡沒有 `0x00`，呼叫端送 `05 FF` 後關閉（W2.3）
    NoAcceptable,
}

/// greeting 階段的錯誤。
///
/// 刻意不是 [`Reply`]：這個階段還沒進到 request，**不得**用 SOCKS5 的回覆格式
/// 回話（W2.4 的 SOCKS4 案例就是靠這條擋住）。
#[derive(Debug, PartialEq, Eq)]
pub enum GreetingError {
    /// `VER != 0x05`（例如 SOCKS4），直接關閉連線
    NotSocks5,
    /// `NMETHODS == 0` 之類的壞格式
    Malformed,
}

/// 解析 greeting（VER/NMETHODS/METHODS）
pub fn parse_greeting(buf: &[u8]) -> Result<Greeting, GreetingError> {
    if buf.is_empty() {
        return Ok(Greeting::NeedMore);
    }
    if buf[0] != VERSION {
        return Err(GreetingError::NotSocks5);
    }
    if buf.len() < 2 {
        return Ok(Greeting::NeedMore);
    }
    let count = buf[1] as usize;
    if count == 0 {
        return Err(GreetingError::Malformed);
    }
    if buf.len() < 2 + count {
        return Ok(Greeting::NeedMore);
    }
    if buf[2..2 + count].contains(&METHOD_NO_AUTH) {
        Ok(Greeting::Method(METHOD_NO_AUTH))
    } else {
        Ok(Greeting::NoAcceptable)
    }
}

/// 一則請求編碼完的**確切**長度。
///
/// 這一支是 [`parse_request`] 的長度契約：伺服器照它讀「剛好那麼多」位元組，
/// 解析器則要求緩衝的長度與它相符——多一個位元組都算編錯（W2.18）。
fn request_len(buf: &[u8]) -> Result<usize, Reply> {
    if buf.len() < 4 {
        return Ok(4);
    }
    if buf[0] != VERSION {
        return Err(Reply::GeneralFailure);
    }
    if buf[1] != CMD_CONNECT {
        return Err(Reply::CommandNotSupported);
    }
    match buf[3] {
        0x01 => Ok(4 + 4 + 2),
        0x04 => Ok(4 + 16 + 2),
        0x03 => {
            if buf.len() < 5 {
                return Ok(5);
            }
            let len = buf[4] as usize;
            if len == 0 {
                return Err(Reply::GeneralFailure);
            }
            Ok(5 + len + 2)
        }
        _ => Err(Reply::AddressTypeNotSupported),
    }
}

/// 解析 request（VER/CMD/RSV/ATYP/ADDR/PORT）
pub fn parse_request(buf: &[u8]) -> Result<Target, Reply> {
    let want = request_len(buf)?;
    if buf.len() != want {
        return Err(Reply::GeneralFailure);
    }
    let (target, port) = match buf[3] {
        0x01 => {
            let octets: [u8; 4] = buf[4..8].try_into().expect("length checked");
            let port = u16::from_be_bytes([buf[8], buf[9]]);
            (Target::Ip(std::net::IpAddr::from(octets), port), port)
        }
        0x04 => {
            let octets: [u8; 16] = buf[4..20].try_into().expect("length checked");
            let port = u16::from_be_bytes([buf[20], buf[21]]);
            (Target::Ip(std::net::IpAddr::from(octets), port), port)
        }
        0x03 => {
            let len = buf[4] as usize;
            let name = std::str::from_utf8(&buf[5..5 + len])
                .map_err(|_| Reply::GeneralFailure)?
                // 尾端點與不帶點等價（W2.9／W5.10）
                .trim_end_matches('.')
                .to_string();
            if name.is_empty() {
                return Err(Reply::GeneralFailure);
            }
            let port = u16::from_be_bytes([buf[5 + len], buf[6 + len]]);
            (Target::Domain(name, port), port)
        }
        _ => return Err(Reply::AddressTypeNotSupported),
    };
    if port == 0 {
        return Err(Reply::GeneralFailure);
    }
    Ok(target)
}

/// 組回覆封包
pub fn encode_reply(reply: Reply, bound: std::net::SocketAddr) -> Vec<u8> {
    let mut out = vec![VERSION, reply as u8, 0x00];
    match bound.ip() {
        std::net::IpAddr::V4(ip) => {
            out.push(0x01);
            out.extend_from_slice(&ip.octets());
        }
        std::net::IpAddr::V6(ip) => {
            out.push(0x04);
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&bound.port().to_be_bytes());
    out
}

/// `ConnectError` → SOCKS5 回覆碼的對照，單獨一支才測得到
pub fn reply_for(err: &stack::ConnectError) -> Reply {
    match err {
        stack::ConnectError::Refused => Reply::ConnectionRefused,
        stack::ConnectError::Timeout => Reply::HostUnreachable,
        stack::ConnectError::NotAllowed => Reply::NotAllowed,
        stack::ConnectError::NoRoute => Reply::NetworkUnreachable,
        stack::ConnectError::Exhausted => Reply::GeneralFailure,
    }
}

/// `ResolveError` → SOCKS5 回覆碼的對照（W2.22／W2.23／W5.4／W5.6）
pub fn reply_for_resolve(err: &super::dns::ResolveError) -> Reply {
    match err {
        // 沒有隧道內的 DNS 伺服器與查無此名，對客戶端而言都是「到不了那台主機」。
        // 前者**絕不**退回本機解析（§2.2 的第二道洩漏防線）
        super::dns::ResolveError::NoServers | super::dns::ResolveError::NotFound => {
            Reply::HostUnreachable
        }
        super::dns::ResolveError::Timeout | super::dns::ResolveError::InvalidName(_) => {
            Reply::GeneralFailure
        }
    }
}

#[cfg(test)]
#[path = "socks5_tests.rs"]
mod tests;
