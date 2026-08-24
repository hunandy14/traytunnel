//! 隧道內 DNS 解析（設計書 §1.5、§2.5）。
//!
//! 走 smoltcp 內建的 `socket-dns`，查詢封包與一般流量走同一個出口。
//! **本機解析器在整條資料路徑上一次都不准出現**（見 §2.2 的洩漏防線）。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

use smoltcp::socket::dns::{GetQueryResultError, QueryHandle, Socket, StartQueryError};
use smoltcp::wire::{DnsQueryType, IpAddress};
use tokio::sync::oneshot;

/// 同時在飛的查詢槽位數，用罄時回 [`ResolveError::Timeout`]
pub const QUERY_SLOTS: usize = 16;

/// 預設查詢逾時（可由 `StackConfig::dns_timeout` 覆寫，見 W5.5）
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 一個查詢名最長 253 個字元（RFC 1035 的 255 減掉頭尾的長度位元組）
const MAX_NAME_LEN: usize = 253;

/// 一個標籤最長 63 個字元
const MAX_LABEL_LEN: usize = 63;

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

type Reply = oneshot::Sender<Result<Vec<IpAddr>, ResolveError>>;

/// 一次 `StackCmd::Resolve` 對應的兩筆 smoltcp 查詢（A 與 AAAA）。
///
/// 兩筆都有結論（成功、失敗或逾時）之後才回覆呼叫端——只等到其中一筆的話，
/// 「A 沒有但 AAAA 有」的名字會被誤判成查不到。
struct Query {
    /// 還在飛的 A 查詢；有結論後設回 `None`（smoltcp 的槽位這時已經自己釋放）
    a: Option<QueryHandle>,
    /// 還在飛的 AAAA 查詢
    aaaa: Option<QueryHandle>,
    v4: Vec<Ipv4Addr>,
    v6: Vec<Ipv6Addr>,
    reply: Reply,
    deadline: Instant,
}

impl Query {
    fn settled(&self) -> bool {
        self.a.is_none() && self.aaaa.is_none()
    }
}

/// stack 任務內部使用：把 smoltcp 的 dns socket 包成「一次查詢一個 oneshot」的模型。
///
/// 不是獨立任務——它必須跟 `Interface` 住在同一顆任務裡才碰得到 `iface.context()`。
pub(crate) struct Resolver {
    /// 還沒回覆的查詢；上限是 `slots`，用罄時新的查詢立刻回 `Timeout`
    pending: Vec<Query>,
    slots: usize,
    timeout: Duration,
    /// 這條隧道有沒有 v4 位址——決定 [`pick`] 的優先順序
    have_v4: bool,
}

impl Resolver {
    /// `slots` 是**同時在飛的解析數**；每一筆解析要用掉 smoltcp 的兩個查詢槽
    /// （A 與 AAAA），所以 socket 開的是 `slots * 2`。
    ///
    /// 比設計書的 `new(servers, slots)` 多兩個參數：逾時要能注入（W5.5 不然
    /// 得跑滿 5 秒），而 `have_v4` 是 [`pick`] 的必要輸入，只有 stack 知道。
    pub fn new(
        servers: &[IpAddress],
        slots: usize,
        timeout: Duration,
        have_v4: bool,
    ) -> (Self, Socket<'static>) {
        let queries: Vec<Option<smoltcp::socket::dns::DnsQuery>> =
            (0..slots * 2).map(|_| None).collect();
        let socket = Socket::new(servers, queries);
        let resolver = Resolver { pending: Vec::new(), slots, timeout, have_v4 };
        (resolver, socket)
    }

    /// 由 `StackCmd::Resolve` 呼叫
    pub fn start(
        &mut self,
        sock: &mut Socket<'static>,
        cx: &mut smoltcp::iface::Context,
        name: &str,
        reply: Reply,
    ) {
        let name = match normalize_name(name) {
            Ok(n) => n,
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        };
        // 槽位用罄等同暫時性失敗（§2.5）：立刻說一聲，不可以讓呼叫端掛在那裡
        if self.pending.len() >= self.slots {
            log::debug!("wg dns: all {} query slots busy, refusing {name}", self.slots);
            let _ = reply.send(Err(ResolveError::Timeout));
            return;
        }

        let a = match sock.start_query(cx, &name, DnsQueryType::A) {
            Ok(h) => h,
            Err(e) => {
                let _ = reply.send(Err(start_error(e, &name)));
                return;
            }
        };
        let aaaa = match sock.start_query(cx, &name, DnsQueryType::Aaaa) {
            Ok(h) => h,
            Err(e) => {
                sock.cancel_query(a);
                let _ = reply.send(Err(start_error(e, &name)));
                return;
            }
        };

        self.pending.push(Query {
            a: Some(a),
            aaaa: Some(aaaa),
            v4: Vec::new(),
            v6: Vec::new(),
            reply,
            deadline: Instant::now() + self.timeout,
        });
    }

    /// 每次 poll 之後呼叫，把完成的查詢回覆出去
    pub fn drain(&mut self, sock: &mut Socket<'static>) {
        let have_v4 = self.have_v4;
        self.pending.retain_mut(|q| {
            collect(sock, &mut q.a, &mut q.v4, &mut q.v6);
            collect(sock, &mut q.aaaa, &mut q.v4, &mut q.v6);
            if !q.settled() {
                return true;
            }
            let answer = ordered(std::mem::take(&mut q.v4), std::mem::take(&mut q.v6), have_v4);
            // 兩筆都有結論卻一個位址都沒有：NXDOMAIN、SERVFAIL，或只有
            // CNAME／其他記錄——對呼叫端而言都是「查不到」（W5.4）
            let result = if answer.is_empty() { Err(ResolveError::NotFound) } else { Ok(answer) };
            reply_and_drop(q, result);
            false
        });
    }

    /// 逾時掃描。
    ///
    /// 比設計書多一個 `sock`：逾時的查詢必須 `cancel_query` 把 smoltcp 的槽位
    /// 還回去，不然槽位只出不進，跑久了每一次解析都會失敗。
    pub fn expire(&mut self, sock: &mut Socket<'static>, now: Instant) {
        self.pending.retain_mut(|q| {
            if now < q.deadline {
                return true;
            }
            for handle in [q.a.take(), q.aaaa.take()].into_iter().flatten() {
                sock.cancel_query(handle);
            }
            reply_and_drop(q, Err(ResolveError::Timeout));
            false
        });
    }
}

/// 把已經有結論的那一筆收掉：成功就把位址分進 v4／v6，失敗就只是清掉 handle。
///
/// smoltcp 的 `get_query_result` 在拿到結果（成功或失敗）的當下就把槽位釋放了，
/// 所以拿到之後一定要把 handle 設成 `None`——再問一次會 panic。
fn collect(
    sock: &mut Socket<'static>,
    slot: &mut Option<QueryHandle>,
    v4: &mut Vec<Ipv4Addr>,
    v6: &mut Vec<Ipv6Addr>,
) {
    let Some(handle) = *slot else { return };
    match sock.get_query_result(handle) {
        Err(GetQueryResultError::Pending) => {}
        Err(GetQueryResultError::Failed) => *slot = None,
        Ok(addrs) => {
            *slot = None;
            for addr in addrs {
                match addr {
                    IpAddress::Ipv4(a) => v4.push(a),
                    IpAddress::Ipv6(a) => v6.push(a),
                }
            }
        }
    }
}

/// `oneshot::Sender` 不能從 `&mut` 借出來送，所以用一顆一定會被丟掉的替身換出來
fn reply_and_drop(q: &mut Query, result: Result<Vec<IpAddr>, ResolveError>) {
    let (spare, _) = oneshot::channel();
    let reply = std::mem::replace(&mut q.reply, spare);
    let _ = reply.send(result);
}

fn start_error(err: StartQueryError, name: &str) -> ResolveError {
    match err {
        // 上面已經先擋過一次 `slots`，這裡只可能是 A 起得來、AAAA 起不來的
        // 邊界情況；語意與槽位用罄相同：暫時性失敗
        StartQueryError::NoFreeSlot => ResolveError::Timeout,
        StartQueryError::InvalidName | StartQueryError::NameTooLong => {
            ResolveError::InvalidName(name.to_string())
        }
    }
}

/// A 與 AAAA 各查一次時的挑選規則：預設 v4 優先，隧道沒有 v4 位址時才用 v6。
///
/// 正式路徑走的是清單版 [`ordered`]（第一個連不上時還留得住備援位址）；
/// 這一支是同一條規則的單值版，由 W5.9 直接釘住。
#[allow(dead_code)]
pub fn pick(v4: Vec<Ipv4Addr>, v6: Vec<Ipv6Addr>, have_v4: bool) -> Option<IpAddr> {
    ordered(v4, v6, have_v4).into_iter().next()
}

/// [`pick`] 的清單版：偏好的那一族排前面。
///
/// `resolve_target` 取的是 `first()`，所以順序就是選擇；其餘的留著，未來要做
/// 「第一個連不上就換下一個」時不必再查一次。
fn ordered(v4: Vec<Ipv4Addr>, v6: Vec<Ipv6Addr>, have_v4: bool) -> Vec<IpAddr> {
    let v4 = v4.into_iter().map(IpAddr::V4);
    let v6 = v6.into_iter().map(IpAddr::V6);
    if have_v4 {
        v4.chain(v6).collect()
    } else {
        v6.chain(v4).collect()
    }
}

/// 查詢名正規化：去掉尾端點，並檢查名字本身是否合法（W5.10）
pub fn normalize_name(name: &str) -> Result<String, ResolveError> {
    let bad = || ResolveError::InvalidName(name.to_string());
    // `example.com.` 與 `example.com` 是同一個名字（根的那一個點不算標籤）
    let trimmed = name.strip_suffix('.').unwrap_or(name);
    if trimmed.is_empty() || trimmed.len() > MAX_NAME_LEN {
        return Err(bad());
    }
    for label in trimmed.split('.') {
        if label.is_empty() || label.len() > MAX_LABEL_LEN {
            return Err(bad());
        }
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
#[path = "dns_tests.rs"]
mod tests;
