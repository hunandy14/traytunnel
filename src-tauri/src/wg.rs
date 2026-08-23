//! WireGuard → 本地 SOCKS5（行程內使用者態隧道）的模組根與生命週期。
//!
//! 對外提供與 `ssh::tunnel` 完全對稱的一組動詞，內部維護每顆代理的監看迴圈，
//! **不含**任何協定細節（設計書 §1.1）。
//!
//! 目前整個模組只有骨架：型別與公開簽名到位讓 crate 編得過，內文一律
//! `todo!()`，行為由 W1～W7 的測試定義。

pub mod conf;
pub mod device;
pub mod dns;
pub mod engine;
pub mod socks5;
pub mod stack;

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::state::AppState;

/// 引擎斷線後的重連間隔，與 `ssh::tunnel::RETRY` 同值同理由
pub const RETRY: Duration = Duration::from_secs(5);

/// 埠佔用預檢的複查間隔，與 `ssh::tunnel::PORT_GRACE` 同值同理由
pub const PORT_GRACE: Duration = Duration::from_millis(500);

/// 一輪連線的取消權杖包裝。
///
/// Drop 時 cancel()，於是 `state.rs` 既有的 `rt.job.take()` 語意（拿走即殺掉）
/// 一字不改就同時涵蓋 ssh 的 Job 與 wg 的任務樹（設計書 §4.2）。
pub struct CancelGuard(pub CancellationToken);

impl Drop for CancelGuard {
    fn drop(&mut self) {
        // W6.1：這裡要 cancel()。骨架階段刻意留空——Drop 裡放 `todo!()` 會在
        // 別的斷言失敗而 unwind 時變成 double panic，直接 abort 整個測試行程，
        // 那樣連「既有測試全綠」都驗不出來。
    }
}

/// 確保這顆 wg 代理有一條引擎在跑；已經有就 no-op。
///
/// 語意與 `ssh::tunnel::start` 一模一樣，包含「不會另起第二條」。
pub fn start(_state: &Arc<AppState>, _socks_port: u16) {
    todo!("W6.6：起引擎，位子邏輯與 ssh 出口共用")
}

/// 停掉這顆代理：遞增世代讓監看迴圈作廢，取消 CancellationToken 收掉整棵任務樹。
/// 不動設定裡的 enabled。
pub fn halt(_state: &Arc<AppState>, _socks_port: u16) {
    todo!("W6.5：代理停掉時底下轉發一併壓成 stopped")
}

/// halt 後立刻 start，套用最新的 .conf 與轉發清單。
pub fn restart(_state: &Arc<AppState>, _socks_port: u16) {
    todo!()
}

pub fn start_enabled(_state: &Arc<AppState>) {
    todo!()
}

pub fn halt_all(_state: &Arc<AppState>) {
    todo!()
}

pub fn reconnect_running(_state: &Arc<AppState>) {
    todo!()
}

/// 存檔前的 .conf 驗證＋真握手測試，回傳型別直接沿用 ssh 那一個。
///
/// 流程：解析 .conf → 綁一個臨時 UDP → 送 handshake initiation → 等握手完成
/// → 立刻拆掉。總上限 15 秒（與 `ssh::tunnel::TEST_TIMEOUT` 同值同理由）。
pub async fn test_conf(_conf_path: &std::path::Path) -> crate::ssh::tunnel::TestConnectionResult {
    todo!("W7.1")
}

/// 只解析不連線，給編輯面板顯示「這份 conf 裡有什麼」。
pub fn inspect_conf(_conf_path: &std::path::Path) -> Result<conf::ConfSummary, String> {
    todo!()
}

/// 握手歲數 → exit-status 字彙的映射（設計書 §4.2 的門檻表，W6.4）。
///
/// `None`→connecting；`Some(< REJECT_AFTER)`→connected；否則 reconnecting。
pub fn status_for_handshake(_age: Option<Duration>) -> &'static str {
    todo!("W6.4")
}

/// 這顆代理停掉時，要一併壓成 stopped 的所有本地埠（socksPort + 底下所有轉發）。
pub fn halted_locals(_cfg: &crate::config::Config, _socks_port: u16) -> Vec<u16> {
    todo!("W6.5")
}
