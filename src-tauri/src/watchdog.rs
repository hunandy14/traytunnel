//! 啟動自癒看門狗：開機後幾秒複查一次，該在跑卻沒在跑的隧道自己補踢一腳。
//!
//! 為什麼需要它：0.5.3 → 0.5.4 的更新實測過一次，更新重啟後第一次 spawn ssh
//! 卡了大約十秒（Defender 對剛落地的新執行檔做完整掃描），那段時間裡監看迴圈
//! 已經退出、位子已經還掉，而沒有任何路徑會再踢它一次——使用者看到的是
//! 「更新完隧道就沒了，要自己去按重連」。自動更新把重啟變成常態之後，
//! 這個空窗只會更常被踩到，所以補一道複查。
//!
//! 它刻意做得很笨：**只看監看位子在不在**，不看連線狀態、不看錯誤、不重試。
//! 「有位子但連不上」是監看迴圈自己的事（它本來就會退避重試），看門狗要補的
//! 只有「連迴圈都不在了」這一種洞。判斷本身是純函式（[`missing`]），
//! 測得到，不必真的起一條 ssh。

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

/// 該在名單裡、卻不在場的那些。
///
/// 兩型連線共用這一支：ssh 的身分是本地埠，wg 的身分是連線名（§5.2），
/// 除了型別以外要問的是同一個問題。
pub fn missing<T: PartialEq + Clone>(expected: &[T], present: &[T]) -> Vec<T> {
    expected.iter().filter(|e| !present.contains(e)).cloned().collect()
}

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
    // 只算 ssh 源底下的列。wg 的列沒有自己的監看迴圈（統一由引擎那一條代管），
    // 拿它們去問 `has_supervisor` 永遠是 false，會變成每次都誤報
    let want = st.with_config(|c| c.enabled_ssh_locals());
    let have: Vec<u16> = want.iter().copied().filter(|p| st.has_supervisor(*p)).collect();
    for local in missing(&want, &have) {
        let name = st.with_config(|c| c.source_name_of(local).map(str::to_string));
        match name {
            Some(source) => st.log_from(&source, format!("port {local} : watchdog restart")),
            None => st.log(format!("port {local} : watchdog restart")),
        }
        tunnel::start(st, local);
    }

    // ---- wg：該跑引擎的連線就該有一個監看位子 ----
    // 壞掉的 `.conf` 不算：`wg::start` 對它們一律拒絕並記一行，
    // 每次啟動都在日誌裡多印一次同樣的錯只是噪音
    let want: Vec<String> = st.with_config(|c| {
        c.wg_proxies
            .iter()
            .filter(|p| wg::should_run_engine(c, &p.name))
            .map(|p| p.name.clone())
            .collect()
    });
    let want: Vec<String> =
        want.into_iter().filter(|name| st.wg_conf_error(name).is_none()).collect();
    let have: Vec<String> = want.iter().filter(|n| st.wg_has_supervisor(n)).cloned().collect();
    for conn in missing(&want, &have) {
        st.log_from(&conn, "watchdog restart");
        wg::start(st, &conn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 該在的都在時什麼都不做——看門狗絕不可以在正常啟動時多踢任何一腳，
    /// 那會變成每次開機都把剛連好的線再斷一次
    #[test]
    fn nothing_is_kicked_when_everything_is_supervised() {
        assert_eq!(missing(&[1080u16, 1081], &[1080, 1081]), Vec::<u16>::new());
        assert_eq!(missing::<u16>(&[], &[]), Vec::<u16>::new());
    }

    /// 缺的那幾個原樣回傳，順序照 expected（日誌行才跟設定檔同序）
    #[test]
    fn only_the_missing_ones_come_back() {
        assert_eq!(missing(&[1080u16, 1081, 1082], &[1081]), vec![1080, 1082]);
        assert_eq!(missing(&[1080u16, 1081], &[]), vec![1080, 1081]);
    }

    /// 在場名單裡多出不該有的東西不影響判斷：看門狗只回答「名單上少了誰」
    #[test]
    fn extra_entries_in_the_present_list_are_ignored() {
        assert_eq!(missing(&[1080u16], &[1080, 9999]), Vec::<u16>::new());
        assert_eq!(missing(&[1080u16, 1081], &[9999]), vec![1080, 1081]);
    }

    /// wg 那半邊用的是連線名，同一支函式要照樣成立
    #[test]
    fn connection_names_work_the_same_way() {
        let want = vec!["hk".to_string(), "tw".to_string()];
        assert_eq!(missing(&want, &["tw".to_string()]), vec!["hk".to_string()]);
        assert_eq!(missing(&want, &want), Vec::<String>::new());
    }
}
