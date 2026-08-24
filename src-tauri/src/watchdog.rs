//! 啟動自癒看門狗：開機後幾秒複查一次，該在跑卻沒在跑的隧道自己補踢一腳。
//!
//! 為什麼需要它：0.5.3 → 0.5.4 的更新實測過一次，更新重啟後第一次 spawn ssh
//! 卡了大約十秒（Defender 對剛落地的新執行檔做完整掃描），那段時間裡監看迴圈
//! 已經退出、位子已經還掉，而沒有任何路徑會再踢它一次——使用者看到的是
//! 「更新完隧道就沒了，要自己去按重連」。自動更新把重啟變成常態之後，
//! 這個空窗只會更常被踩到，所以補一道複查。
//!
//! 它刻意做得很笨：**只看監看位子在不在**，不看連線狀態、不看錯誤、不重試，
//! 而且只在啟動後跑這一次。「有位子但連不上」是監看迴圈自己的事（它本來就會
//! 退避重試），看門狗要補的只有「連迴圈都不在了」這一種洞。

use std::sync::Arc;
use std::time::Duration;

use crate::state::AppState;
use crate::{tunnel, wg};

/// 啟動後隔這麼久複查。
///
/// 要比 spawn 真的會花的時間長（Defender 掃描實測約 10 秒），又不能長到
/// 使用者早就自己按過重連了。12 秒是「掃描最壞情況剛過、人還在等第一條線」
/// 的那個位置。只複查這一次：常駐期間的斷線由監看迴圈自己處理，
/// 看門狗要補的是啟動那一次。
const DELAY: Duration = Duration::from_secs(12);

/// 排一次啟動複查。整支程式只呼叫一次，就在 `start_enabled` 之後。
pub fn spawn(state: &Arc<AppState>) {
    let st = state.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(DELAY).await;
        sweep(&st);
    });
}

/// 複查一次並補踢。同步的，全部只是查表與 spawn，不會擋住任何東西。
fn sweep(st: &Arc<AppState>) {
    // 使用者在這 12 秒內按了 Exit，或更新交棒已經開始：不要在退出途中又把線拉起來
    if st.is_exiting() {
        return;
    }

    // ---- ssh：一條 enabled 的出口就該有一個監看位子 ----
    //
    // 只算 ssh 源底下的列。wg 的列沒有自己的監看迴圈（統一由引擎那一條代管），
    // 拿它們去問 `has_supervisor` 永遠是 false，會變成每次都誤報。
    //
    // 清單與名字在**同一次** with_config 裡取完：分兩次的話中間放掉了 cfg 鎖，
    // 設定可以在那個空檔被改掉，於是拿到的名字對不上剛剛那份清單。
    let orphans: Vec<(u16, Option<String>)> = st.with_config(|c| {
        c.enabled_ssh_locals()
            .into_iter()
            .filter(|local| !st.has_supervisor(*local))
            .map(|local| (local, c.source_name_of(local).map(str::to_string)))
            .collect()
    });
    for (local, source) in orphans {
        match source {
            Some(name) => st.log_from(&name, format!("port {local} : watchdog restart")),
            None => st.log(format!("port {local} : watchdog restart")),
        }
        tunnel::start(st, local);
    }

    // ---- wg：該跑引擎的連線就該有一個監看位子 ----
    //
    // 「該跑」的定義與 `wg::start_enabled` 共用 `wg::wants_engine`：准入條件
    // （底下有該跑的列、而且 `.conf` 讀得過）只有一個出處，兩邊不會各判各的。
    for conn in wg::wants_engine(st).into_iter().filter(|c| !st.wg_has_supervisor(c)) {
        st.log_from(&conn, "watchdog restart");
        wg::start(st, &conn);
    }
}
