//! 隧道內 DNS 解析（設計書 §1.5）。目前只有骨架。
//!
//! 走 smoltcp 內建的 `socket-dns`，查詢封包與一般流量走同一個出口。
//! **本機解析器在整條資料路徑上一次都不准出現**（見 §2.2 的洩漏防線）。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use tokio::sync::oneshot;

/// 同時在飛的查詢槽位數，用罄時回 [`ResolveError::Timeout`]
pub const QUERY_SLOTS: usize = 16;

/// 預設查詢逾時（可由 `StackConfig::dns_timeout` 覆寫，見 W5.5）
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// conf 沒寫 `[Interface] DNS`
    NoServers,
    /// 逾時
    Timeout,
    /// 伺服器回了 NXDOMAIN／SERVFAIL，或沒有可用的 A／AAAA
    NotFound,
    /// 名字本身不合法
    InvalidName(String),
}

/// stack 任務內部使用：把 smoltcp 的 dns socket 包成「一次查詢一個 oneshot」的模型。
///
/// 不是獨立任務——它必須跟 `Interface` 住在同一顆任務裡才碰得到 `iface.context()`。
pub(crate) struct Resolver {
    /// QueryHandle → oneshot sender 的對照表（骨架階段先留空殼）
    #[allow(dead_code)]
    pending: (),
}

#[allow(dead_code)]
impl Resolver {
    pub fn new(
        _servers: &[smoltcp::wire::IpAddress],
        _slots: usize,
    ) -> (Self, smoltcp::socket::dns::Socket<'static>) {
        todo!("W5.*：建立 dns socket 與查詢對照表")
    }

    /// 由 `StackCmd::Resolve` 呼叫
    pub fn start(
        &mut self,
        _sock: &mut smoltcp::socket::dns::Socket<'static>,
        _cx: &mut smoltcp::iface::Context,
        _name: &str,
        _reply: oneshot::Sender<Result<Vec<IpAddr>, ResolveError>>,
    ) {
        todo!("W5.1／W5.2：A 與 AAAA 各起一筆查詢")
    }

    /// 每次 poll 之後呼叫，把完成的查詢回覆出去
    pub fn drain(&mut self, _sock: &mut smoltcp::socket::dns::Socket<'static>) {
        todo!("W5.*：把完成的查詢回覆出去")
    }

    /// 逾時掃描
    pub fn expire(&mut self, _now: std::time::Instant) {
        todo!("W5.5：逾時掃描")
    }
}

/// A 與 AAAA 各查一次時的挑選規則：預設 v4 優先，隧道沒有 v4 位址時才用 v6。
pub fn pick(_v4: Vec<Ipv4Addr>, _v6: Vec<Ipv6Addr>, _have_v4: bool) -> Option<IpAddr> {
    todo!("W5.9：v4 優先，隧道無 v4 位址時挑 v6")
}

/// 查詢名正規化：去掉尾端點，並檢查名字本身是否合法（W5.10）
pub fn normalize_name(_name: &str) -> Result<String, ResolveError> {
    todo!("W5.10：`example.com.` 與 `example.com` 等價")
}

#[cfg(test)]
#[path = "dns_tests.rs"]
mod tests;
