//! wg-quick `.conf` 解析（設計書 §1.2）。
//!
//! 只吃**標準 wg-quick**：`[Interface]`／`[Peer]` 全部支援，wg-quick 專屬的主機
//! 路由與腳本鍵（`PostUp` 這一類）**認得、跳過、記一行，永不執行**，wireproxy 的
//! 擴充段一律容忍跳過。行為逐條由 W1 系列的測試（`conf_tests.rs`）定義。

use std::net::IpAddr;
use std::path::Path;

/// 應用層的 MTU 預設值：**`.conf` 沒寫 MTU、使用者也沒在介面上覆寫**時才輪到它。
///
/// 這裡刻意**不是** wg-quick 的 1420。1420 是「以太網 1500 減掉 WireGuard 表頭」
/// 算出來的理想值，只在整條路徑都跑得動 1500 位元組時成立；一旦中間有 PPPoE、
/// 隧道套隧道或某一跳把 ICMP「需要分片」丟掉，大封包就會靜默黑洞——網頁載一半、
/// 連線卡住，而握手與小封包全都正常，最難查的那一類故障。
///
/// 1280 是 Tailscale 與 Mullvad 這類量產客戶端共同採用的保守值（也是 IPv6 規範
/// 要求每一條鏈路都必須通過的最小 MTU），拿它當「什麼都不知道時的預設」，代價是
/// 大流量下多一點分包開銷，換來的是預設組態不會黑洞。要吞吐量的人可以在連線
/// 表單的 MTU 欄位往上調。
///
/// PM 裁決 2026-08-24：預設值從解析器移到應用層，並由 1420 降為 1280。
pub const APP_DEFAULT_MTU: usize = 1280;

/// MTU 的合法範圍（W1.18）
pub const MTU_RANGE: std::ops::RangeInclusive<usize> = 576..=9000;

/// 私鑰／預共享金鑰的容器。
///
/// `Debug` 一律印固定字串，序列化**不實作**（絕不可能被 serde 帶去前端），
/// Drop 交給 x25519-dalek 的 zeroize。欄位是 `pub(crate)` 而不是 `pub`：
/// 引擎與測試檯要造得出金鑰，但這個型別本身仍是模組外唯一的入口。
pub struct SecretKey(pub(crate) boringtun::x25519::StaticSecret);

/// `SecretKey` 的 Debug 輸出，W1.29 釘住它不含任何金鑰位元組
pub const REDACTED: &str = "Key(<redacted>)";

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // W1.29：固定字串。這裡**絕對不可以**改成 derive 或印出任何欄位——
        // `WgConf` 的 Debug 會連帶把它印進日誌與 panic 訊息。
        f.write_str(REDACTED)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpNet {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl IpNet {
    /// 這個位址的「全長」前綴：v4 是 32、v6 是 128
    pub fn host_prefix(addr: &IpAddr) -> u8 {
        match addr {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        }
    }

    /// 這個網段涵不涵蓋某個位址（`AllowedIPs` 的出口過濾器，設計書 Q2）
    pub fn contains(&self, other: &IpAddr) -> bool {
        match (self.addr, other) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                prefix_eq(&net.octets(), &ip.octets(), self.prefix)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                prefix_eq(&net.octets(), &ip.octets(), self.prefix)
            }
            _ => false,
        }
    }
}

impl std::fmt::Display for IpNet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// `AllowedIPs` 的出口過濾器：這個位址准不准進隧道（設計書 Q2、§2.2 防線二）。
///
/// **只有這一份實作**：stack 的出站過濾與 MTU 探測的目標挑選都走這裡。
/// 兩邊各寫一份的話，遲早會出現「stack 擋得住、探測封包卻繞過去了」的後門。
///
/// 空清單只可能來自「conf 明寫了一個空的 `AllowedIPs`」——解析器對缺鍵的情況
/// 補的是全開（W1.16），所以這裡照字面擋住。
pub fn allowed(nets: &[IpNet], ip: &IpAddr) -> bool {
    nets.iter().any(|n| n.contains(ip))
}

/// 兩串位元組的前 `bits` 個位元相不相等
fn prefix_eq(a: &[u8], b: &[u8], bits: u8) -> bool {
    let bits = bits as usize;
    let full = bits / 8;
    if a[..full] != b[..full] {
        return false;
    }
    let rest = bits % 8;
    if rest == 0 {
        return true;
    }
    let mask = 0xffu8 << (8 - rest);
    a[full] & mask == b[full] & mask
}

#[derive(Debug)]
pub struct WgConf {
    /// `[Interface] PrivateKey`
    pub private_key: SecretKey,
    /// `[Interface] Address`，可多筆；不帶前綴時 v4 補 /32、v6 補 /128
    pub addresses: Vec<IpNet>,
    /// `[Interface] DNS`，只收得下 IP 字面值，其餘（搜尋網域）跳過並警告
    pub dns: Vec<IpAddr>,
    /// `[Interface] MTU`，**省略時 None**。
    ///
    /// 這裡不在解析階段補預設：`.conf` 到底有沒有明寫 MTU 是上層要用到的資訊
    /// （優先序是「介面覆寫 ＞ conf 明寫 ＞ [`APP_DEFAULT_MTU`]」，見
    /// `wg::plan_mtu`），一補預設就把「明寫 1280」與「沒寫」黏成同一件事。
    pub mtu: Option<usize>,
    /// `[Interface] ListenPort`，省略時 0（讓 OS 配）
    pub listen_port: u16,
    /// `[Peer] PublicKey`
    pub peer_public_key: [u8; 32],
    /// `[Peer] PresharedKey`，省略時 None
    pub preshared_key: Option<[u8; 32]>,
    /// `[Peer] Endpoint` 的**原始字串**（可能是 FQDN），每次重連時才解析
    pub endpoint: String,
    /// `[Peer] AllowedIPs`；省略時視為 `[0.0.0.0/0, ::/0]`
    pub allowed_ips: Vec<IpNet>,
    /// `[Peer] PersistentKeepalive`，省略或 0 時 None
    pub keepalive: Option<u16>,
    /// 解析過程中被忽略的東西，逐行進活動日誌
    pub warnings: Vec<String>,
}

/// 給前端／編輯面板看的摘要：**只含非機密欄位**，金鑰一概不在其中。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfSummary {
    pub endpoint: String,
    pub addresses: Vec<String>,
    pub dns: Vec<String>,
    pub allowed_ips: Vec<String>,
    /// `.conf` 明寫的 MTU；沒寫就是 None（**不代入應用層預設**，同 [`WgConf::mtu`]）
    pub mtu: Option<usize>,
    pub keepalive: Option<u16>,
    pub warnings: Vec<String>,
}

/// wg-quick 專屬的主機路由／腳本鍵：**認得、跳過、記一行，永不執行**（W1.22）。
///
/// 「不執行」才是這一份清單存在的理由——`PostUp` 這類鍵若被當成命令跑起來，
/// 一份 `.conf` 就成了任意程式碼執行的入口。因此解析器對它們只做三件事：
/// 認得、跳過、留一行警告，`WgConf` 裡**沒有任何欄位**承載它們的值。
const IGNORED_INTERFACE_KEYS: [&str; 7] =
    ["table", "fwmark", "saveconfig", "preup", "postup", "predown", "postdown"];

/// wireproxy 的擴充段：容忍跳過、不匯入（設計書 D4）
const IGNORED_SECTIONS: [&str; 8] = [
    "socks5",
    "tcpclienttunnel",
    "tcpservertunnel",
    "stdiotunnel",
    "http",
    "sni",
    "resolve",
    "udpproxytunnel",
];

fn strip_bom(raw: &str) -> &str {
    raw.strip_prefix('\u{feff}').unwrap_or(raw)
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Section {
    /// 還沒遇到任何區段（wireproxy 的根層鍵落在這裡）
    Root,
    Interface,
    Peer,
    /// 認得但不匯入，或完全不認得
    Ignored,
}

/// 解析一份 wg-quick 設定檔的內容。
///
/// 錯誤訊息**不得包含**輸入字串的任何金鑰片段（W1.10）：金鑰壞掉時只講「哪一個
/// 鍵、壞在哪一類」，一個位元組都不回放。
pub fn parse(raw: &str) -> Result<WgConf, String> {
    let raw = strip_bom(raw);
    let mut warnings: Vec<String> = Vec::new();
    let mut section = Section::Root;
    let mut interfaces = 0usize;
    let mut peers = 0usize;

    let mut private_key: Option<boringtun::x25519::StaticSecret> = None;
    let mut addresses: Vec<IpNet> = Vec::new();
    let mut dns: Vec<IpAddr> = Vec::new();
    let mut mtu: Option<usize> = None;
    let mut listen_port: u16 = 0;

    let mut peer_public_key: Option<[u8; 32]> = None;
    let mut preshared_key: Option<[u8; 32]> = None;
    let mut endpoint: Option<String> = None;
    let mut allowed_ips: Option<Vec<IpNet>> = None;
    let mut keepalive: Option<u16> = None;

    for line in raw.lines() {
        let line = line.trim();
        // W1.6：整行註解與空行。**行內註解不在此列**（W1.7），wg-quick 也不吃。
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if let Some(rest) = line.strip_prefix('[') {
            let Some(end) = rest.find(']') else {
                return Err(format!("區段標頭少了結尾的 `]`：{line}"));
            };
            let name = rest[..end].trim().to_string();
            let lower = name.to_ascii_lowercase();
            section = match lower.as_str() {
                "interface" => {
                    interfaces += 1;
                    if interfaces > 1 {
                        return Err("這份 .conf 有多個 [Interface] 區段".into());
                    }
                    Section::Interface
                }
                "peer" => {
                    peers += 1;
                    if peers > 1 {
                        return Err("這份 .conf 有多個 [Peer] 區段：v1 只支援單一 peer".into());
                    }
                    Section::Peer
                }
                s if IGNORED_SECTIONS.contains(&s) => {
                    warnings.push(format!(
                        "已忽略 wireproxy 擴充段 [{name}]：監聽埠與轉發清單一律以 traytunnel 的設定為準"
                    ));
                    Section::Ignored
                }
                _ => {
                    warnings.push(format!("已忽略不認得的區段 [{name}]"));
                    Section::Ignored
                }
            };
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            warnings.push(format!("已忽略無法解析的一行（缺少 `=`）：{}", key_of(line)));
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        let lower = key.to_ascii_lowercase();

        match section {
            // W1.25：出現在任何區段之前的鍵（涵蓋 wireproxy 根層的 WGConfig）——
            // 跳過並警告，**絕不遞迴載入外部檔案**
            Section::Root => warnings.push(format!(
                "已忽略區段之前的鍵 {key}：這份 .conf 只讀 [Interface] 與 [Peer]"
            )),
            Section::Ignored => {}
            Section::Interface => match lower.as_str() {
                "privatekey" => {
                    private_key = Some(boringtun::x25519::StaticSecret::from(decode_key(
                        value,
                        "PrivateKey",
                    )?));
                }
                "address" => {
                    for item in split_list(value) {
                        addresses.push(parse_ip_net(item, false, "Address")?);
                    }
                }
                "dns" => {
                    for item in split_list(value) {
                        match item.parse::<IpAddr>() {
                            Ok(ip) => dns.push(ip),
                            // wg-quick 允許在 DNS 裡寫搜尋網域，那是給系統解析器
                            // 用的，使用者態隧道沒有地方擺，跳過並留一行（W1.15）
                            Err(_) => warnings
                                .push(format!("已忽略 DNS 裡的搜尋網域 {item}：只收 IP 位址")),
                        }
                    }
                }
                "mtu" => {
                    let n: usize =
                        value.parse().map_err(|_| "MTU 不是一個整數".to_string())?;
                    if !MTU_RANGE.contains(&n) {
                        return Err(format!(
                            "MTU {n} 超出合法範圍 {}..={}",
                            MTU_RANGE.start(),
                            MTU_RANGE.end()
                        ));
                    }
                    mtu = Some(n);
                }
                "listenport" => {
                    listen_port =
                        value.parse().map_err(|_| "ListenPort 不是一個合法的埠號".to_string())?;
                }
                s if IGNORED_INTERFACE_KEYS.contains(&s) => warnings.push(format!(
                    "已忽略 [Interface] 的 {key}：wg-quick 的主機路由／腳本鍵對使用者態代理沒有意義，一律不執行"
                )),
                _ => warnings.push(format!("已忽略 [Interface] 不認得的鍵 {key}")),
            },
            Section::Peer => match lower.as_str() {
                "publickey" => peer_public_key = Some(decode_key(value, "PublicKey")?),
                "presharedkey" => preshared_key = Some(decode_key(value, "PresharedKey")?),
                "endpoint" => endpoint = Some(check_endpoint(value)?),
                "allowedips" => {
                    let mut nets = Vec::new();
                    for item in split_list(value) {
                        nets.push(parse_ip_net(item, true, "AllowedIPs")?);
                    }
                    allowed_ips = Some(nets);
                }
                "persistentkeepalive" => {
                    let n: u16 = value
                        .parse()
                        .map_err(|_| "PersistentKeepalive 不是一個整數".to_string())?;
                    keepalive = if n == 0 { None } else { Some(n) };
                }
                _ => warnings.push(format!("已忽略 [Peer] 不認得的鍵 {key}")),
            },
        }
    }

    if interfaces == 0 {
        return Err("這份 .conf 沒有 [Interface] 區段".into());
    }
    if peers == 0 {
        return Err("這份 .conf 沒有 [Peer] 區段：v1 只支援單一 peer".into());
    }

    let private_key = private_key.ok_or("[Interface] 缺少 PrivateKey")?;
    let peer_public_key = peer_public_key.ok_or("[Peer] 缺少 PublicKey")?;
    let endpoint = endpoint.ok_or("[Peer] 缺少 Endpoint")?;

    Ok(WgConf {
        private_key: SecretKey(private_key),
        addresses,
        dns,
        // 沒寫就是 None：預設值由 `wg::plan_mtu` 在組引擎時才決定
        mtu,
        listen_port,
        peer_public_key,
        preshared_key,
        endpoint,
        // 省略 AllowedIPs 視為全開，與 wg 一致（W1.16）
        allowed_ips: allowed_ips.unwrap_or_else(|| {
            vec![
                IpNet { addr: "0.0.0.0".parse().unwrap(), prefix: 0 },
                IpNet { addr: "::".parse().unwrap(), prefix: 0 },
            ]
        }),
        keepalive,
        warnings,
    })
}

/// 沒有 `=` 的那一行拿前面幾個字當識別用，**不回放整行**（可能是半行金鑰）
fn key_of(line: &str) -> String {
    line.split_whitespace().next().unwrap_or("").to_string()
}

/// 逗號分隔清單，逐項 trim、空項跳過（W1.7 的規則 7）
fn split_list(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').map(str::trim).filter(|s| !s.is_empty())
}

/// 標準 base64、解碼後恰好 32 位元組。
///
/// 錯誤訊息只講鍵名與失敗的種類，**一個輸入位元組都不回放**（W1.10／W1.34）。
fn decode_key(value: &str, key: &'static str) -> Result<[u8; 32], String> {
    // 這個 use 刻意留在函式內：放在模組頂層的話，`conf_tests.rs` 的
    // `use super::*;` 會把它一併帶進去，測試檔自己那一行就變成重複匯入
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .map_err(|_| format!("{key} 不是合法的 base64"))?;
    <[u8; 32]>::try_from(raw.as_slice()).map_err(|_| format!("{key} 解碼後不是 32 位元組"))
}

/// `Address`（前綴可省）與 `AllowedIPs`（前綴必填）共用的一支
fn parse_ip_net(item: &str, require_prefix: bool, key: &'static str) -> Result<IpNet, String> {
    match item.rsplit_once('/') {
        Some((addr, prefix)) => {
            let addr: IpAddr =
                addr.parse().map_err(|_| format!("{key} 的 {item} 不是合法的位址"))?;
            let prefix: u8 =
                prefix.parse().map_err(|_| format!("{key} 的 {item} 前綴長度不是一個整數"))?;
            if prefix > IpNet::host_prefix(&addr) {
                return Err(format!("{key} 的 {item} 前綴長度超出範圍"));
            }
            Ok(IpNet { addr, prefix })
        }
        None => {
            if require_prefix {
                // wg 本身也要求 AllowedIPs 帶前綴：`10.0.0.1` 與 `10.0.0.1/32`
                // 在分流設定裡差很多，不猜（W1.17）
                return Err(format!("{key} 的 {item} 必須帶前綴長度（例如 /24）"));
            }
            let addr: IpAddr =
                item.parse().map_err(|_| format!("{key} 的 {item} 不是合法的位址"))?;
            let prefix = IpNet::host_prefix(&addr);
            Ok(IpNet { addr, prefix })
        }
    }
}

/// `Endpoint` 只做形狀檢查：有沒有 `:port`、port 在不在 1..=65535。
///
/// **主機名不在這裡解析**（W1.27）——留到每次重連前才用系統解析器解，動態 DNS
/// 的端點才跟得上。回傳的是原字串。
fn check_endpoint(value: &str) -> Result<String, String> {
    let bad = || format!("Endpoint `{}` 的格式不對，要寫成 host:port", redact_shape(value));
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        // `[fd00::1]:51820`
        let (host, rest) = rest.split_once(']').ok_or_else(bad)?;
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(bad());
        }
        let port = rest.strip_prefix(':').ok_or_else(bad)?;
        (host.to_string(), port)
    } else {
        let (host, port) = value.rsplit_once(':').ok_or_else(bad)?;
        // 裸 IPv6 沒有中括號的話 rsplit 會切在位址中間，那本來就不是合法寫法
        if host.is_empty() || host.contains(':') || host.split_whitespace().count() != 1 {
            return Err(bad());
        }
        (host.to_string(), port)
    };
    let _ = host;
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u32 = port.parse().map_err(|_| bad())?;
    if !(1..=65535).contains(&n) {
        return Err(bad());
    }
    Ok(value.to_string())
}

/// 端點不是機密，但也沒必要把整串連同雜訊回放進日誌；只留頭 64 個字元
fn redact_shape(value: &str) -> String {
    value.chars().take(64).collect()
}

/// 從檔案讀進來再 [`parse`]，含 BOM 剝除（W1.28）。
///
/// 解析錯誤原樣往上傳，`inspect_conf` 與 `parse` 因此是同一句訊息（W1.34）。
pub fn load(path: &Path) -> Result<WgConf, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("讀不到 .conf 檔案 {}：{e}", path.display()))?;
    parse(&raw)
}

impl WgConf {
    pub fn summary(&self) -> ConfSummary {
        ConfSummary {
            endpoint: self.endpoint.clone(),
            addresses: self.addresses.iter().map(|n| n.to_string()).collect(),
            dns: self.dns.iter().map(|ip| ip.to_string()).collect(),
            allowed_ips: self.allowed_ips.iter().map(|n| n.to_string()).collect(),
            mtu: self.mtu,
            keepalive: self.keepalive,
            warnings: self.warnings.clone(),
        }
    }
}

#[cfg(test)]
#[path = "conf_tests.rs"]
mod tests;
