//! wg-quick `.conf` 解析（設計書 §1.2）。
//!
//! 目前只有骨架：型別與公開簽名到位讓 crate 編得過，內文一律 `todo!()`，
//! 行為由 W1 系列的測試（`conf_tests.rs`）定義。

use std::net::IpAddr;
use std::path::Path;

/// `[Interface] MTU` 省略時的預設值
pub const DEFAULT_MTU: usize = 1420;

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
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!("W1.29：固定印 REDACTED，不得洩漏任何金鑰位元組")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpNet {
    pub addr: IpAddr,
    pub prefix: u8,
}

#[derive(Debug)]
pub struct WgConf {
    /// `[Interface] PrivateKey`
    pub private_key: SecretKey,
    /// `[Interface] Address`，可多筆；不帶前綴時 v4 補 /32、v6 補 /128
    pub addresses: Vec<IpNet>,
    /// `[Interface] DNS`，只收得下 IP 字面值，其餘（搜尋網域）跳過並警告
    pub dns: Vec<IpAddr>,
    /// `[Interface] MTU`，省略時 [`DEFAULT_MTU`]
    pub mtu: usize,
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
    pub mtu: usize,
    pub keepalive: Option<u16>,
    pub warnings: Vec<String>,
}

/// 解析一份 wg-quick 設定檔的內容。
///
/// 錯誤訊息**不得包含**輸入字串的任何金鑰片段（W1.10）。
pub fn parse(_raw: &str) -> Result<WgConf, String> {
    todo!("W1.*：wg-quick 解析")
}

/// 從檔案讀進來再 [`parse`]，含 BOM 剝除（W1.28）
pub fn load(_path: &Path) -> Result<WgConf, String> {
    todo!("W1.28：讀檔 + strip_bom + parse")
}

impl WgConf {
    pub fn summary(&self) -> ConfSummary {
        todo!("W1.30：只帶非機密欄位的摘要")
    }
}

#[cfg(test)]
#[path = "conf_tests.rs"]
mod tests;
