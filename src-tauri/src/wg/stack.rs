//! smoltcp 介面與 poll 迴圈（設計書 §1.4）。
//!
//! 一個介面、一個 `SocketSet`，TCP 與 DNS 同住（onetun 是 TCP 一個介面、UDP 一個
//! 介面，各配一個虛擬裝置）。目的地路由**只靠一條 default route**，不像 onetun
//! 那樣把每個目的地 IP 都掛成介面自己的位址——SOCKS5 的目的地是任意的，那一招
//! 不可用（D2）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::pin::Pin;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smoltcp::iface::{Interface, PollResult, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, IpEndpoint};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt, StreamMap};
use tokio_util::sync::CancellationToken;

use super::{conf, dns};

/// 每顆引擎的連線上限預設值
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;

/// 每條連線 rx/tx 各配的 TCP 緩衝（onetun 是 64 KiB，這裡減半）
pub const SOCKET_BUFFER: usize = 32 * 1024;

/// 虛擬埠的配置範圍
pub const VPORT_RANGE: std::ops::RangeInclusive<u16> = 1024..=60999;

/// 每條連線上下行通道的深度（反壓的來源）
pub const CONN_CHANNEL_DEPTH: usize = 4;

/// `StackCmd` 通道的深度
const CMD_CHANNEL_DEPTH: usize = 64;

/// 建立連線的上限。對面回 RST 時我們立刻就知道（W4.7），這條門檻只擋
/// 「封包進了黑洞」那一種——不設的話那條連線會永遠掛著
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// 下行通道塞住（呼叫端不讀）時的重試間隔。
///
/// 這是反壓期間唯一會定期空轉的地方：通道有空位是別條任務的事，smoltcp 的
/// `poll_delay` 不會替我們醒過來，因此在被塞住的那段期間用一個很短的節拍回頭看。
const BACKPRESSURE_POLL: Duration = Duration::from_millis(5);

/// 有連線正在等待建立時的 poll 節拍（讓連線逾時掃得到）
const CONNECTING_POLL: Duration = Duration::from_millis(100);

/// 沒事做時最長的睡眠。smoltcp 大多會給出更短的 `poll_delay`，這只是天花板
const IDLE_POLL: Duration = Duration::from_secs(1);

/// 一條虛擬連線的兩端通道
pub struct Conn {
    pub port: VirtualPort,
    /// 上行：呼叫端 → 隧道
    pub tx: mpsc::Sender<bytes::Bytes>,
    /// 下行：隧道 → 呼叫端
    pub rx: mpsc::Receiver<bytes::Bytes>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// 對端 RST／連不上
    Refused,
    /// 逾時
    Timeout,
    /// 目的地不在 AllowedIPs 內
    NotAllowed,
    /// 目的地的 IP 版本本隧道沒有位址
    NoRoute,
    /// 虛擬埠或連線數用罄
    Exhausted,
}

pub enum StackCmd {
    Connect {
        dst: IpEndpoint,
        reply: oneshot::Sender<Result<Conn, ConnectError>>,
    },
    /// 隧道內 DNS 查詢，socks5h 語意的落點
    Resolve {
        name: String,
        reply: oneshot::Sender<Result<Vec<std::net::IpAddr>, dns::ResolveError>>,
    },
    /// 測試用（並為未來的反向轉發預留）：在隧道內位址上開一個被動監聽
    Listen {
        endpoint: IpEndpoint,
        accept: mpsc::Sender<Conn>,
    },
    Close {
        port: VirtualPort,
    },
}

pub struct StackConfig {
    pub addresses: Vec<smoltcp::wire::IpCidr>,
    pub dns_servers: Vec<smoltcp::wire::IpAddress>,
    pub mtu: usize,
    pub allowed_ips: Vec<conf::IpNet>,
    pub max_connections: usize,
    /// DNS 查詢逾時。
    ///
    /// 設計書 §1.5 寫死 5 秒，但 §5 W5.5 要求可注入以縮短測試，因此做成參數。
    pub dns_timeout: Duration,
}

pub fn spawn(
    cfg: StackConfig,
    outbound: mpsc::Sender<Vec<u8>>,
    inbound: mpsc::Receiver<Vec<u8>>,
    cancel: CancellationToken,
) -> StackHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel(CMD_CHANNEL_DEPTH);
    let join = tokio::spawn(run(cfg, outbound, inbound, cmd_rx, cancel));
    StackHandle { cmd: cmd_tx, join }
}

pub struct StackHandle {
    pub cmd: mpsc::Sender<StackCmd>,
    pub join: tokio::task::JoinHandle<()>,
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub struct VirtualPort(pub(crate) u16);

impl VirtualPort {
    pub fn num(&self) -> u16 {
        self.0
    }
}

// ---------------------------------------------------------------- 虛擬裝置

/// `impl smoltcp::phy::Device`：收包是一條 `VecDeque`，由 poll 迴圈在收到
/// `inbound` 封包時 push；送包直接 `try_send` 進 device 任務的通道。
///
/// 通道滿了就**丟包**：這是 IP 層，丟包由上層的 TCP 重送處理，比阻塞整個
/// poll 迴圈好得多。
struct VirtualDevice {
    mtu: usize,
    rx: VecDeque<Vec<u8>>,
    tx: mpsc::Sender<Vec<u8>>,
}

impl Device for VirtualDevice {
    type RxToken<'a>
        = VirtualRxToken
    where
        Self: 'a;
    type TxToken<'a>
        = VirtualTxToken
    where
        Self: 'a;

    fn receive(
        &mut self,
        _timestamp: smoltcp::time::Instant,
    ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let buffer = self.rx.pop_front()?;
        Some((VirtualRxToken { buffer }, VirtualTxToken { tx: self.tx.clone() }))
    }

    fn transmit(&mut self, _timestamp: smoltcp::time::Instant) -> Option<Self::TxToken<'_>> {
        Some(VirtualTxToken { tx: self.tx.clone() })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut cap = DeviceCapabilities::default();
        cap.medium = Medium::Ip;
        cap.max_transmission_unit = self.mtu;
        // 送出時照算，收進來時不驗：這條「線」是 WireGuard，每一個位元組都已經
        // 過了 AEAD 的驗證，IP／UDP 的 16 位元和再算一次買不到任何完整性，卻會
        // 把對端偷懶沒填 checksum 的封包（測試檯的假 DNS 就是這樣送的）
        // 靜默丟掉。TCP 維持預設的雙向，那一層的 checksum 由 smoltcp 自己
        // 兩端一致地處理。
        cap.checksum.ipv4 = smoltcp::phy::Checksum::Tx;
        cap.checksum.udp = smoltcp::phy::Checksum::Tx;
        cap
    }
}

struct VirtualRxToken {
    buffer: Vec<u8>,
}

impl smoltcp::phy::RxToken for VirtualRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.buffer)
    }
}

struct VirtualTxToken {
    tx: mpsc::Sender<Vec<u8>>,
}

impl smoltcp::phy::TxToken for VirtualTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0u8; len];
        let result = f(&mut buffer);
        if self.tx.try_send(buffer).is_err() {
            log::debug!("wg stack: outbound queue full, dropping packet");
        }
        result
    }
}

// ---------------------------------------------------------------- poll 迴圈

/// 上行資料流。`None` 是「呼叫端那一頭關了」的哨兵——`StreamMap` 在子串流結束時
/// 只是把它移除，不會通知，半關（W2.25／W4.6）就偵測不到，因此自己接一顆。
type UpStream = Pin<Box<dyn Stream<Item = Option<Bytes>> + Send>>;

fn up_stream(rx: mpsc::Receiver<Bytes>) -> UpStream {
    Box::pin(ReceiverStream::new(rx).map(Some).chain(tokio_stream::once(None)))
}

/// 一條已配好虛擬埠的連線
struct Entry {
    handle: SocketHandle,
    /// 下行：隧道 → 呼叫端。對端關掉接收半邊時整個丟掉，呼叫端的 `rx` 就收到 EOF
    down: Option<mpsc::Sender<Bytes>>,
    /// socket 的 tx 緩衝一次吃不完時剩下的那一塊
    pending: VecDeque<Bytes>,
    /// 反壓期間從 `StreamMap` 拔下來寄放的上行串流
    parked: Option<UpStream>,
    /// 呼叫端已經關掉上行
    up_done: bool,
    /// 已經對 socket 呼叫過 close()
    closed: bool,
    /// 還沒回覆的 `Connect`
    connect: Option<(oneshot::Sender<Result<Conn, ConnectError>>, Conn)>,
    deadline: Option<Instant>,
}

struct Listener {
    endpoint: IpEndpoint,
    accept: mpsc::Sender<Conn>,
    handle: SocketHandle,
}

async fn run(
    cfg: StackConfig,
    outbound: mpsc::Sender<Vec<u8>>,
    mut inbound: mpsc::Receiver<Vec<u8>>,
    mut cmd_rx: mpsc::Receiver<StackCmd>,
    cancel: CancellationToken,
) {
    let mut device = VirtualDevice { mtu: cfg.mtu, rx: VecDeque::new(), tx: outbound };

    let mut iface_cfg = smoltcp::iface::Config::new(HardwareAddress::Ip);
    // 不引入 rand：虛擬介面的序號種子只要每次啟動不同就夠了
    iface_cfg.random_seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut iface = Interface::new(iface_cfg, &mut device, smoltcp::time::Instant::now());

    // D2：位址一律以 /32、/128 掛，路由**只靠一條 default route**
    let addresses: Vec<IpCidr> =
        cfg.addresses.iter().map(|c| IpCidr::new(c.address(), host_prefix(&c.address()))).collect();
    iface.update_ip_addrs(|slots| {
        for cidr in &addresses {
            if slots.push(*cidr).is_err() {
                log::warn!("wg stack: too many interface addresses, dropping {cidr}");
            }
        }
    });
    // 入站封包本來就是寄給我們的位址，開著 any_ip 只會多收垃圾
    iface.set_any_ip(false);

    if !skip_default_route() {
        // R11／D2：`Medium::Ip` 下 `has_neighbor()` 等同「`route()` 有回值」，
        // 而 `route()` 只在 `in_same_network()` 或路由表命中時有回值。沒有這一行，
        // TCP 的 SYN 會**靜默地**送不出去——沒有錯誤、沒有日誌，只是不動。
        // 閘道位址本身在 `Medium::Ip` 下不會被使用（沒有鄰居探索），它存在的
        // 唯一意義就是讓 `route()` 回 Some。W4.2 專門釘這一行。
        let routes = iface.routes_mut();
        let _ = routes.add_default_ipv4_route(std::net::Ipv4Addr::new(0, 0, 0, 1));
        let _ = routes.add_default_ipv6_route(std::net::Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1));
    }

    let mut sockets = SocketSet::new(Vec::new());

    // DNS 走 smoltcp 內建的 socket-dns，與一般流量共用同一個出口。
    // `[Interface] DNS` 為空時**連 socket 都不開**：網域請求一律回 NoServers，
    // 絕不退回本機解析（§2.2 的第二道洩漏防線）。
    let mut resolver = if cfg.dns_servers.is_empty() {
        None
    } else {
        let have_v4 = addresses.iter().any(|c| matches!(c.address(), IpAddress::Ipv4(_)));
        let (r, sock) =
            dns::Resolver::new(&cfg.dns_servers, dns::QUERY_SLOTS, cfg.dns_timeout, have_v4);
        let handle = sockets.add(sock);
        Some((r, handle))
    };

    let mut entries: HashMap<VirtualPort, Entry> = HashMap::new();
    let mut listeners: Vec<Listener> = Vec::new();
    let mut up_streams: StreamMap<VirtualPort, UpStream> = StreamMap::new();
    let mut ports = PortAllocator::default();
    let mut next_poll: Option<tokio::time::Instant> = Some(tokio::time::Instant::now());

    loop {
        // `biased` 的順序有意義：真事件排在計時器**之前**。反過來的話，
        // `next_poll` 一旦被設成「立刻」，select 就永遠只挑得到那一路，
        // 指令與封包會活活餓死。計時器排最後不會漏事——每一圈結束都會 poll。
        tokio::select! {
            biased;

            _ = cancel.cancelled() => break,

            packet = inbound.recv() => {
                match packet {
                    Some(packet) => device.rx.push_back(packet),
                    None => break,
                }
            }

            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                handle_cmd(
                    cmd,
                    &cfg,
                    &mut iface,
                    &mut sockets,
                    &mut entries,
                    &mut listeners,
                    &mut up_streams,
                    &mut ports,
                    &mut resolver,
                );
            }

            Some((port, item)) = up_streams.next() => {
                if let Some(entry) = entries.get_mut(&port) {
                    match item {
                        Some(chunk) => entry.pending.push_back(chunk),
                        None => entry.up_done = true,
                    }
                }
            }

            _ = wait_until(next_poll) => {}
        }

        let now = smoltcp::time::Instant::now();
        let polled = iface.poll(now, &mut device, &mut sockets) == PollResult::SocketStateChanged;
        if let Some((resolver, handle)) = resolver.as_mut() {
            let sock = sockets.get_mut::<smoltcp::socket::dns::Socket>(*handle);
            resolver.drain(sock);
            resolver.expire(sock, Instant::now());
        }
        let service = service_sockets(
            &mut sockets,
            &mut entries,
            &mut listeners,
            &mut up_streams,
            &mut ports,
        );

        next_poll = if polled || service.progressed || !device.rx.is_empty() {
            Some(tokio::time::Instant::now())
        } else {
            let hint = iface
                .poll_delay(now, &sockets)
                .map(|d| Duration::from_micros(d.total_micros()))
                .unwrap_or(IDLE_POLL)
                .min(if service.backpressured {
                    BACKPRESSURE_POLL
                } else if service.connecting {
                    CONNECTING_POLL
                } else {
                    IDLE_POLL
                });
            Some(tokio::time::Instant::now() + hint)
        };
    }
}

async fn wait_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn host_prefix(addr: &IpAddress) -> u8 {
    match addr {
        IpAddress::Ipv4(_) => 32,
        IpAddress::Ipv6(_) => 128,
    }
}

fn to_std(addr: &IpAddress) -> IpAddr {
    match addr {
        IpAddress::Ipv4(a) => IpAddr::V4(*a),
        IpAddress::Ipv6(a) => IpAddr::V6(*a),
    }
}

fn same_version(a: &IpAddress, b: &IpAddress) -> bool {
    matches!(
        (a, b),
        (IpAddress::Ipv4(_), IpAddress::Ipv4(_)) | (IpAddress::Ipv6(_), IpAddress::Ipv6(_))
    )
}

/// 虛擬埠配置：`1024..=60999` 的循環計數器 + 佔用表。
///
/// 不需要 `rand`——虛擬埠只在本引擎內部有意義，不面對外部攻擊者。
struct PortAllocator {
    next: u16,
    used: HashSet<u16>,
}

impl Default for PortAllocator {
    fn default() -> Self {
        PortAllocator { next: *VPORT_RANGE.start(), used: HashSet::new() }
    }
}

impl PortAllocator {
    fn take(&mut self) -> Option<VirtualPort> {
        let span = (*VPORT_RANGE.end() - *VPORT_RANGE.start()) as usize + 1;
        for _ in 0..span {
            let candidate = self.next;
            self.next =
                if candidate >= *VPORT_RANGE.end() { *VPORT_RANGE.start() } else { candidate + 1 };
            if self.used.insert(candidate) {
                return Some(VirtualPort(candidate));
            }
        }
        None
    }

    fn give_back(&mut self, port: VirtualPort) {
        self.used.remove(&port.0);
    }
}

fn new_tcp_socket() -> tcp::Socket<'static> {
    tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0u8; SOCKET_BUFFER]),
        tcp::SocketBuffer::new(vec![0u8; SOCKET_BUFFER]),
    )
}

#[allow(clippy::too_many_arguments)]
fn handle_cmd(
    cmd: StackCmd,
    cfg: &StackConfig,
    iface: &mut Interface,
    sockets: &mut SocketSet<'static>,
    entries: &mut HashMap<VirtualPort, Entry>,
    listeners: &mut Vec<Listener>,
    up_streams: &mut StreamMap<VirtualPort, UpStream>,
    ports: &mut PortAllocator,
    resolver: &mut Option<(dns::Resolver, SocketHandle)>,
) {
    match cmd {
        StackCmd::Connect { dst, reply } => {
            // Q2：AllowedIPs 是**出口過濾器**（真 WireGuard 的 cryptokey routing
            // 就是這個語意），擋下時明確回一個碼，比靜默黑洞好除錯得多
            if !allowed(&cfg.allowed_ips, &to_std(&dst.addr)) {
                let _ = reply.send(Err(ConnectError::NotAllowed));
                return;
            }
            let Some(local) = cfg.addresses.iter().find(|c| same_version(&c.address(), &dst.addr))
            else {
                let _ = reply.send(Err(ConnectError::NoRoute));
                return;
            };
            if entries.len() >= cfg.max_connections {
                let _ = reply.send(Err(ConnectError::Exhausted));
                return;
            }
            let Some(port) = ports.take() else {
                let _ = reply.send(Err(ConnectError::Exhausted));
                return;
            };

            let mut socket = new_tcp_socket();
            if socket.connect(iface.context(), dst, (local.address(), port.0)).is_err() {
                ports.give_back(port);
                let _ = reply.send(Err(ConnectError::NoRoute));
                return;
            }
            let handle = sockets.add(socket);

            let (up_tx, up_rx) = mpsc::channel::<Bytes>(CONN_CHANNEL_DEPTH);
            let (down_tx, down_rx) = mpsc::channel::<Bytes>(CONN_CHANNEL_DEPTH);
            up_streams.insert(port, up_stream(up_rx));
            entries.insert(
                port,
                Entry {
                    handle,
                    down: Some(down_tx),
                    pending: VecDeque::new(),
                    parked: None,
                    up_done: false,
                    closed: false,
                    connect: Some((reply, Conn { port, tx: up_tx, rx: down_rx })),
                    deadline: Some(Instant::now() + CONNECT_TIMEOUT),
                },
            );
        }

        StackCmd::Resolve { name, reply } => match resolver.as_mut() {
            // §2.2 第二道洩漏防線：沒有隧道內的 DNS 伺服器就直接說沒有，
            // **絕不**退回本機解析器
            None => {
                let _ = reply.send(Err(dns::ResolveError::NoServers));
            }
            Some((resolver, handle)) => {
                let sock = sockets.get_mut::<smoltcp::socket::dns::Socket>(*handle);
                let cx = iface.context();
                resolver.start(sock, cx, &name, reply);
            }
        },

        StackCmd::Listen { endpoint, accept } => {
            let mut socket = new_tcp_socket();
            if socket.listen(endpoint).is_err() {
                log::warn!("wg stack: cannot listen on {endpoint}");
                return;
            }
            let handle = sockets.add(socket);
            listeners.push(Listener { endpoint, accept, handle });
        }

        StackCmd::Close { port } => {
            if let Some(entry) = entries.get_mut(&port) {
                let socket = sockets.get_mut::<tcp::Socket>(entry.handle);
                socket.close();
                entry.closed = true;
            }
        }
    }
}

fn allowed(nets: &[conf::IpNet], ip: &IpAddr) -> bool {
    // 空清單只可能來自「conf 明寫了一個空的 AllowedIPs」——解析器對缺鍵的情況
    // 補的是全開（W1.16），所以這裡照字面擋住
    nets.iter().any(|n| n.contains(ip))
}

#[derive(Default)]
struct ServiceOutcome {
    /// 這一輪真的搬動了東西，馬上再轉一圈
    progressed: bool,
    /// 有連線因為下行通道塞住而讀不動，要用短節拍回頭看
    backpressured: bool,
    /// 有連線還在等建立，逾時掃描需要節拍
    connecting: bool,
}

fn service_sockets(
    sockets: &mut SocketSet<'static>,
    entries: &mut HashMap<VirtualPort, Entry>,
    listeners: &mut [Listener],
    up_streams: &mut StreamMap<VirtualPort, UpStream>,
    ports: &mut PortAllocator,
) -> ServiceOutcome {
    let mut out = ServiceOutcome::default();
    let now = Instant::now();
    let mut done: Vec<VirtualPort> = Vec::new();

    for (port, entry) in entries.iter_mut() {
        let socket = sockets.get_mut::<tcp::Socket>(entry.handle);

        if entry.connect.is_some() {
            if socket.may_send() {
                let (reply, conn) = entry.connect.take().expect("just checked");
                let _ = reply.send(Ok(conn));
                out.progressed = true;
            } else if !socket.is_active() {
                // 對面回了 RST（W4.7）——立刻收手，不要掛到逾時
                let (reply, _) = entry.connect.take().expect("just checked");
                let _ = reply.send(Err(ConnectError::Refused));
                done.push(*port);
                continue;
            } else if entry.deadline.is_some_and(|d| now >= d) {
                let (reply, _) = entry.connect.take().expect("just checked");
                let _ = reply.send(Err(ConnectError::Timeout));
                socket.abort();
                done.push(*port);
                continue;
            } else {
                out.connecting = true;
                continue;
            }
        }

        // ---- 上行：把 pending 灌進 socket 的 tx 緩衝
        while socket.can_send() {
            let Some(chunk) = entry.pending.front() else { break };
            match socket.send_slice(chunk) {
                Ok(0) => break,
                Ok(sent) => {
                    out.progressed = true;
                    if sent < chunk.len() {
                        let rest = entry.pending.front_mut().expect("just peeked");
                        let _ = rest.split_to(sent);
                        break;
                    }
                    entry.pending.pop_front();
                }
                Err(_) => break,
            }
        }
        if entry.pending.is_empty() {
            // 反壓解除：把寄放的上行串流放回 map
            if let Some(stream) = entry.parked.take() {
                up_streams.insert(*port, stream);
            }
            // 呼叫端的上行已經關了且全部灌完 → 半關（smoltcp 的 close() 會把
            // FIN 排在剩餘資料之後，所以最後一段不會掉，W2.25／W4.6）
            if entry.up_done && !entry.closed {
                socket.close();
                entry.closed = true;
                out.progressed = true;
            }
        } else if entry.parked.is_none() {
            // socket 吃不下了：把這條上行從 map 拔掉，呼叫端的 `send().await`
            // 就會卡住，反壓一路傳回本地 TCP 連線
            if let Some(stream) = up_streams.remove(port) {
                entry.parked = Some(stream);
            }
        }

        // ---- 下行：有位子才讀，沒位子就這一輪不讀（smoltcp 的接收視窗自然關上）
        let mut caller_gone = false;
        while socket.can_recv() {
            let Some(down) = entry.down.as_ref() else { break };
            let permit = match down.try_reserve() {
                Ok(permit) => permit,
                Err(mpsc::error::TrySendError::Full(())) => {
                    out.backpressured = true;
                    break;
                }
                // 呼叫端整個不見了
                Err(mpsc::error::TrySendError::Closed(())) => {
                    caller_gone = true;
                    break;
                }
            };
            let data =
                socket.recv(|buf| (buf.len(), Bytes::copy_from_slice(buf))).unwrap_or_default();
            if data.is_empty() {
                break;
            }
            permit.send(data);
            out.progressed = true;
        }
        if caller_gone {
            entry.down = None;
        }

        // 對面關了接收半邊且緩衝抽乾 → 丟掉下行 sender，呼叫端讀到 EOF。
        // 還在三向交握途中的 socket（被動監聽剛被接起來時是 SynReceived）
        // 也一樣答 may_recv() == false，要先排除掉，不然連線一成立就被判 EOF。
        let handshaking = matches!(
            socket.state(),
            tcp::State::Listen | tcp::State::SynSent | tcp::State::SynReceived
        );
        if !handshaking && !socket.may_recv() && !socket.can_recv() && entry.down.take().is_some() {
            out.progressed = true;
        }

        // 收乾淨了才收工：`TimeWait`／`Closed` 的 rx 緩衝裡可能還留著對面最後
        // 送來的那一段——下行通道塞住的那一輪尤其明顯。先送完再拆，不然
        // 尾巴幾 KiB 會靜靜地不見（W4.5 那條 8 連線測試就是這樣抓到的）。
        if !socket.is_open() {
            if !socket.can_recv() || entry.down.is_none() {
                done.push(*port);
            } else {
                out.backpressured = true;
            }
        }
    }

    for port in done {
        if let Some(entry) = entries.remove(&port) {
            sockets.remove(entry.handle);
        }
        up_streams.remove(&port);
        ports.give_back(port);
    }

    // ---- 被動監聽：連上了就換成一條連線，並馬上補一個新的 listener 繼續收
    for listener in listeners.iter_mut() {
        let socket = sockets.get_mut::<tcp::Socket>(listener.handle);
        if socket.state() == tcp::State::Listen || !socket.is_active() {
            continue;
        }
        let Some(port) = ports.take() else {
            socket.abort();
            continue;
        };
        let (up_tx, up_rx) = mpsc::channel::<Bytes>(CONN_CHANNEL_DEPTH);
        let (down_tx, down_rx) = mpsc::channel::<Bytes>(CONN_CHANNEL_DEPTH);
        let conn = Conn { port, tx: up_tx, rx: down_rx };
        if listener.accept.try_send(conn).is_err() {
            socket.abort();
            ports.give_back(port);
            continue;
        }
        up_streams.insert(port, up_stream(up_rx));
        entries.insert(
            port,
            Entry {
                handle: listener.handle,
                down: Some(down_tx),
                pending: VecDeque::new(),
                parked: None,
                up_done: false,
                closed: false,
                connect: None,
                deadline: None,
            },
        );

        let mut fresh = new_tcp_socket();
        if fresh.listen(listener.endpoint).is_err() {
            log::warn!("wg stack: cannot re-arm listener on {}", listener.endpoint);
            continue;
        }
        listener.handle = sockets.add(fresh);
        out.progressed = true;
    }

    out
}

// ---------------------------------------------------------------- 測試開關

/// W4.2 專用的回歸開關：把 D2 的 default route 關掉。
///
/// `Medium::Ip` 下 `has_neighbor()` 等同「`route()` 有回值」，沒有 default route
/// 的話 TCP 的 SYN 會**靜默地**送不出去。那一行是整份設計最容易被順手「簡化」
/// 掉又不會馬上壞的一行，因此把它做成測試可切換的旗標來釘住。
///
/// 旗標本身是 **thread-local** 的：`cargo test` 預設平行跑，而 `#[tokio::test]`
/// 用的是 current-thread runtime——換成全域旗標的話，W4.2 一翻開關就會同時把
/// 其他正在跑的 W4 測試的路由拔掉。對呼叫端而言介面不變（`.store(值, 順序)`）。
#[cfg(test)]
pub(crate) struct SkipDefaultRoute;

#[cfg(test)]
impl SkipDefaultRoute {
    pub(crate) fn store(&self, value: bool, _order: std::sync::atomic::Ordering) {
        SKIP_DEFAULT_ROUTE_LOCAL.with(|c| c.set(value));
    }
}

#[cfg(test)]
thread_local! {
    static SKIP_DEFAULT_ROUTE_LOCAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) static SKIP_DEFAULT_ROUTE: SkipDefaultRoute = SkipDefaultRoute;

#[cfg(test)]
fn skip_default_route() -> bool {
    SKIP_DEFAULT_ROUTE_LOCAL.with(|c| c.get())
}

#[cfg(not(test))]
fn skip_default_route() -> bool {
    false
}
