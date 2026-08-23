//! SOCKS5 伺服器與靜態轉發器（設計書 §1.6）。目前只有骨架。

use tokio::sync::mpsc;
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

/// SOCKS5 監聽器。v1 只綁 127.0.0.1，且只支援 NO AUTH。
pub async fn serve_socks5(
    _listener: tokio::net::TcpListener,
    _stack: mpsc::Sender<stack::StackCmd>,
    _cancel: CancellationToken,
) {
    todo!("W2.19–W2.28：SOCKS5 伺服器")
}

/// 靜態埠轉發：本地埠 → 隧道內固定的 host:port。
///
/// `dst` 是字串，因為 remote 可以寫成隧道內的網域名（每次連線各自解析）。
pub async fn serve_forward(
    _listener: tokio::net::TcpListener,
    _stack: mpsc::Sender<stack::StackCmd>,
    _dst: String,
    _cancel: CancellationToken,
) {
    todo!("W2.29／W4.14：靜態埠轉發")
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
pub fn parse_greeting(_buf: &[u8]) -> Result<Greeting, GreetingError> {
    todo!("W2.1–W2.6")
}

/// 解析 request（VER/CMD/RSV/ATYP/ADDR/PORT）
pub fn parse_request(_buf: &[u8]) -> Result<Target, Reply> {
    todo!("W2.7–W2.14／W2.18")
}

/// 組回覆封包
pub fn encode_reply(_reply: Reply, _bound: std::net::SocketAddr) -> Vec<u8> {
    todo!("W2.15／W2.16")
}

/// `ConnectError` → SOCKS5 回覆碼的對照，單獨一支才測得到
pub fn reply_for(_err: &stack::ConnectError) -> Reply {
    todo!("W2.17")
}

/// `ResolveError` → SOCKS5 回覆碼的對照（W2.22／W2.23／W5.4／W5.6）
pub fn reply_for_resolve(_err: &super::dns::ResolveError) -> Reply {
    todo!("W2.22／W2.23：NotFound/NoServers→0x04，其餘→0x01")
}
