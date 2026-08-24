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

/// 確保這條 wg 連線有一條引擎在跑；已經有就 no-op。
///
/// 語意與 `ssh::tunnel::start` 一模一樣，包含「不會另起第二條」。
///
/// **身分是連線的 `name` 而不是某個埠**：一條連線有 0..N 條列（§1.2），
/// 沒有哪個埠代表得了它。底下一條啟用的列都沒有時直接 no-op，不起引擎（§5.2）。
pub fn start(_state: &Arc<AppState>, _conn: &str) {
    todo!("W6.6／W6.8：起引擎，位子邏輯與 ssh 出口共用")
}

/// 停掉這條連線：遞增世代讓監看迴圈作廢，取消 CancellationToken 收掉整棵任務樹
/// （引擎 + 所有列的監聽器）。不動設定裡的 enabled。
pub fn halt(_state: &Arc<AppState>, _conn: &str) {
    todo!("W6.5：連線停掉時底下所有列一併壓成 stopped")
}

/// halt 後立刻 start，套用最新的 .conf 與列清單。
pub fn restart(_state: &Arc<AppState>, _conn: &str) {
    todo!()
}

/// 起／停單一列，不動引擎（引擎已在跑時才有意義）。
/// `start_exit`／`stop_exit` 這幾支 IPC 打在 wg 的列上時走這裡。
pub fn start_row(_state: &Arc<AppState>, _local: u16) {
    todo!("§2.1：起單一列的監聽器")
}

pub fn halt_row(_state: &Arc<AppState>, _local: u16) {
    todo!("§2.1：停單一列的監聽器")
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
///
/// **不握手、不解析主機名**（W1.33）：端點寫一個解不出來的名字也照樣回摘要，
/// 那是重連時才要做的事。錯誤訊息與 [`conf::parse`] 是同一句（W1.34），金鑰
/// 一個位元組都不會出現在裡面。
pub fn inspect_conf(conf_path: &std::path::Path) -> Result<conf::ConfSummary, String> {
    conf::load(conf_path).map(|c| c.summary())
}

/// 握手歲數 → exit-status 字彙的映射（設計書 §4.2 的門檻表，W6.4）。
///
/// `None`→connecting；`Some(< REJECT_AFTER)`→connected；否則 reconnecting。
pub fn status_for_handshake(_age: Option<Duration>) -> &'static str {
    todo!("W6.4")
}

/// 這條連線停掉時，要一併壓成 stopped 的所有本地埠（底下每一條列各一次）。
pub fn halted_locals(_cfg: &crate::config::Config, _conn: &str) -> Vec<u16> {
    todo!("W6.5／W6.16")
}

/// 這條連線現在該啟動哪些列（W6.11）：**連線 enabled 且列 enabled**。
///
/// 連線層與列層是兩個獨立的意圖，`AND` 起來才是「這條列現在該不該跑」（§5.5）。
pub fn rows_to_start(_cfg: &crate::config::Config, _conn: &str) -> Vec<u16> {
    todo!("W6.11")
}

/// 要不要替這條連線起一顆引擎（§5.2 的啟停條件，W6.8／W6.14）。
///
/// 零列或全部停用的連線不需要跑一顆 WireGuard——沒有任何東西會用到它，
/// 留一顆空轉的引擎只是白白吃著 UDP 埠與一個計時器。
pub fn should_run_engine(_cfg: &crate::config::Config, _conn: &str) -> bool {
    todo!("W6.8／W6.14")
}

/// 引擎狀態 → 底下各列的狀態（W6.9）。
///
/// 「埠被佔住只影響那一條列」是與 ssh 不同的地方，而且是刻意的：ssh 一個出口
/// 就是一條連線，埠被佔就整條起不來；WG 一條隧道底下有多條列，其中一條的埠
/// 被佔沒有理由拖垮其他列（§5.2）。
pub fn row_statuses(
    _rows: &[u16],
    _engine: &'static str,
    _busy: &[u16],
) -> Vec<(u16, &'static str)> {
    todo!("W6.9")
}

/// `set_wg_enabled` 會做的事，依序（W6.13）。
///
/// 抽成一串步驟才測得到「存檔成功才動引擎」與 `apply_enabled` 那條刻意的
/// 不對稱：連接時先推事件再拉線（介面立刻看得到 connecting），中斷時先停線
/// 再推事件（不會出現「已停用但還連著」的那一瞬）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WgEnabledStep {
    EmitConfigChanged,
    StartEngine(String),
    HaltEngine(String),
}

/// * `saved` 為 false（存檔失敗）：引擎維持原狀，只推一次 `emit_config_changed`
///   把樂觀翻過去的開關拉回真值（沿用 `commands.rs::apply_enabled` 的通則）。
/// * `on` 為 true 但底下零條 enabled 的列：設定寫入成功，但**引擎不啟動**（W6.14）。
pub fn wg_enabled_steps(
    _conn: &str,
    _on: bool,
    _saved: bool,
    _has_enabled_row: bool,
) -> Vec<WgEnabledStep> {
    todo!("W6.13／W6.14")
}

#[cfg(test)]
#[path = "wg_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "wg_live_tests.rs"]
mod live_tests;
