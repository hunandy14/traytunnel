//! 把 device + stack + 監聽器組裝成一個可啟停的引擎實例（設計書 §1.7）。
//! 目前只有骨架。

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::conf;

pub struct EngineSpec {
    pub name: String,
    pub conf: conf::WgConf,
    pub socks_port: u16,
    /// (name, local, remote)
    pub forwards: Vec<(String, u16, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// 代理本身的狀態，餵給 `set_exit_status_of(socks_port, ..)`
    Proxy(&'static str, Option<String>),
    /// 某個轉發的狀態，餵給 `set_exit_status_of(local, ..)`
    Forward(u16, &'static str, Option<String>),
    Log(String),
}

/// 依序：解析端點 → 起 device → 起 stack → 綁 SOCKS5 監聽 → 綁各轉發監聽。
/// 任何一步失敗都回 `Err`，且已起來的部分會被 `cancel` 收乾淨。
pub async fn spawn(
    _spec: EngineSpec,
    _cancel: CancellationToken,
) -> Result<mpsc::Receiver<EngineEvent>, String> {
    todo!("W4.*：引擎組裝")
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
