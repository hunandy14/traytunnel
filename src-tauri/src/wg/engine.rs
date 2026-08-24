//! 把 device + stack + 監聽器組裝成一個可啟停的引擎實例（設計書 §1.7）。
//! 目前只有骨架。

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::conf;

/// 一條列要引擎替它做什麼。
///
/// **只有兩個 variant**——引擎這一層只認**機制**，而機制就是 §1.2 的那兩種。
/// `probeProxy` 不在這裡：它只決定 supervise 要不要排自測（§5.4），引擎不必
/// 知道，§1.3 的 ③ 與 ④ 走的是同一段程式碼。這正是新編碼的好處——不需要為了
/// 「其中一條會被探測」而在資料流上分岔。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowSpec {
    /// ③④ 轉發：本地埠 → 隧道內的固定目的地
    Forward { local: u16, remote: String },
    /// ⑤ 引擎自建的 SOCKS5 listener
    Socks { local: u16 },
}

impl RowSpec {
    /// 這一條列佔的本地埠。兩個 variant 都有一個，取法不該讓呼叫端 match
    pub fn local(&self) -> u16 {
        match self {
            RowSpec::Forward { local, .. } | RowSpec::Socks { local } => *local,
        }
    }
}

pub struct EngineSpec {
    /// 連線名，同時是引擎的身分與日誌前綴
    pub name: String,
    pub conf: conf::WgConf,
    /// 0..N 條列。零條時 supervise 根本不會呼叫 [`spawn`]（§5.2）
    pub rows: Vec<(String, RowSpec)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// 引擎自己的狀態（握手）。**不直接對外推事件**——supervise 收到後翻譯成
    /// 「底下每一條列的 exit-status」（§5.3 的零新事件）
    Engine(&'static str, Option<String>),
    /// 某一條列的狀態，餵給 `set_exit_status_of(local, ..)`
    Row(u16, &'static str, Option<String>),
    Log(String),
}

/// 依序：解析端點 → 起 device → 起 stack → 逐條列綁監聽器。
///
/// `Forward` 綁 [`super::socks5::serve_forward`]、`Socks` 綁
/// [`super::socks5::serve_socks5`]。**單一列綁不上（埠被佔）只讓那一條進
/// `port_busy`，不讓整顆引擎失敗**；device／stack 起不來才回 `Err`，
/// 且已起來的部分會被 `cancel` 收乾淨。
pub async fn spawn(
    _spec: EngineSpec,
    _cancel: CancellationToken,
) -> Result<mpsc::Receiver<EngineEvent>, String> {
    todo!("W4.*：引擎組裝")
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
