//! `wg` 生命週期與狀態的測試——設計書 §5 的 W6 系列（7 條，全部 F）。
//!
//! 比照 `state.rs` 既有的純函式測試風格：守門邏輯抽出來測，**不生 AppHandle**。

use super::*;

use std::collections::BTreeMap;

use crate::config::{Config, Forward, Source, WgProxy};
use crate::state::{self, status, TestView, Worker};

fn fwd(name: &str, local: u16) -> Forward {
    Forward { name: name.into(), local, remote: "10.0.0.5:22".into(), enabled: true }
}

fn cfg_with_proxy() -> Config {
    Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![Source {
            name: "hk".into(),
            host: "hk.example.com".into(),
            user: "bob".into(),
            proxy_command: String::new(),
            forwards: vec![fwd("exit-a", 1080)],
        }],
        wg_proxies: vec![WgProxy {
            name: "ax4200".into(),
            conf_path: "wg/ax4200.conf".into(),
            socks_port: 1085,
            enabled: true,
            forwards: vec![fwd("nas-ssh", 2222), fwd("nas-http", 2280)],
        }],
    }
}

/// W6.1 `CancelGuard` 一 drop，底下的 CancellationToken 就被 cancel。
///
/// 這是 `state.rs` 那句 `rt.job.take()`（拿走即殺掉）對 wg 成立的全部理由。
#[test]
fn dropping_the_guard_cancels_the_task_tree() {
    let token = tokio_util::sync::CancellationToken::new();
    {
        let _guard = CancelGuard(token.clone());
        assert!(!token.is_cancelled(), "還握著的時候不可以先取消");
    }
    assert!(token.is_cancelled(), "guard 一 drop 就要收掉整棵任務樹");
}

/// W6.2 `Worker::Wg` 走 store 的世代守門：世代不符時 guard 當場 drop，
/// 而且不可以蓋掉新世代已經存好的 worker
#[test]
fn a_stale_worker_is_dropped_instead_of_overwriting_the_new_one() {
    let newer = tokio_util::sync::CancellationToken::new();
    let stale = tokio_util::sync::CancellationToken::new();

    // 新世代（7）的 worker 已經就位
    let mut slot = Some((7u64, Worker::Wg(CancelGuard(newer.clone()))));
    // 舊世代（6）的監看迴圈晚一步才來存
    let stored = state::store_worker(&mut slot, 7, 6, Worker::Wg(CancelGuard(stale.clone())));
    assert!(!stored, "世代不符不可以收下");
    assert!(stale.is_cancelled(), "沒被收下的那棵任務樹要當場收乾淨");
    assert!(!newer.is_cancelled(), "新世代的 worker 不可以被蓋掉");
    assert_eq!(slot.as_ref().map(|(g, _)| *g), Some(7));

    // 世代相符就收下
    let ok = tokio_util::sync::CancellationToken::new();
    let mut slot: Option<(u64, Worker)> = None;
    assert!(state::store_worker(&mut slot, 9, 9, Worker::Wg(CancelGuard(ok.clone()))));
    assert!(!ok.is_cancelled());
    assert_eq!(slot.as_ref().map(|(g, _)| *g), Some(9));
}

/// W6.3 收全部：ssh 與 wg 兩種 worker 都要被收掉，兩邊的埠都要回報成 stopped
#[test]
fn draining_covers_both_ssh_and_wg_workers() {
    let wg_token = tokio_util::sync::CancellationToken::new();
    let mut slots: BTreeMap<u16, Option<(u64, Worker)>> = BTreeMap::new();
    slots.insert(1080, Some((1, Worker::Ssh(crate::winsys::Job::new().unwrap()))));
    slots.insert(1085, Some((2, Worker::Wg(CancelGuard(wg_token.clone())))));
    // 本來就沒人在跑的出口不必回報
    slots.insert(2222, None);

    let stopped = state::drain_workers(&mut slots);
    assert_eq!(stopped, vec![1080, 1085], "只回報真的收掉了東西的那些埠");
    assert!(wg_token.is_cancelled(), "wg 的任務樹要一起收掉");
    assert!(slots.values().all(|v| v.is_none()), "全部清空");
}

/// W6.4 握手歲數 → 狀態的映射（設計書 §4.2 的門檻表）
#[test]
fn handshake_age_maps_to_the_right_status() {
    use std::time::Duration;
    assert_eq!(status_for_handshake(None), status::CONNECTING);
    assert_eq!(status_for_handshake(Some(Duration::from_secs(0))), status::CONNECTED);
    assert_eq!(status_for_handshake(Some(Duration::from_secs(179))), status::CONNECTED);
    // REJECT_AFTER_TIME 到了就要老實顯示 reconnecting，
    // 寧可早一點，也不要讓使用者盯著一個假的 connected 而流量石沉大海
    assert_eq!(status_for_handshake(Some(device::REJECT_AFTER)), status::RECONNECTING);
    assert_eq!(status_for_handshake(Some(Duration::from_secs(3600))), status::RECONNECTING);
}

/// W6.5 代理 halt 時，底下所有轉發的埠都要一起被壓成 stopped
#[test]
fn halting_a_proxy_takes_all_of_its_forwards_with_it() {
    let cfg = cfg_with_proxy();
    let mut locals = halted_locals(&cfg, 1085);
    locals.sort_unstable();
    assert_eq!(locals, vec![1085, 2222, 2280]);
    // 別人家的埠不可以被掃到
    assert!(!locals.contains(&1080));
    // 不存在的代理不影響任何人
    assert!(halted_locals(&cfg, 9999).is_empty());
}

/// W6.6 socksPort 與 ssh 出口共用同一份位子邏輯，重複 `start` 不會起第二顆引擎
#[test]
fn a_second_start_on_the_same_socks_port_is_refused() {
    // 前提：socksPort 真的在 locals() 裡，AppState 才會替它開一份執行期狀態
    let cfg = cfg_with_proxy();
    assert!(cfg.locals().contains(&1085), "socksPort 必須併進同一個本地埠鍵空間");

    let mut slot = None;
    let mut seq = 0;
    let mut next = || {
        seq += 1;
        seq
    };
    assert_eq!(state::claim_slot(&mut slot, &mut next), Some(1));
    assert_eq!(state::claim_slot(&mut slot, &mut next), None, "位子有人就不可以再起一顆引擎");
    assert_eq!(seq, 1, "未取得位子時不該消耗世代序號");
}

/// W6.7 代理進入 stale 時自測顯示要被清掉
#[test]
fn a_stale_proxy_loses_its_test_result() {
    let view = || Some(TestView { state: "ok".into(), text: "1.2.3.4  Taipei, TW".into() });
    // 握手陳舊 → reconnecting → 快照不可以再帶著上一輪的出口 IP
    let stale = status_for_handshake(Some(device::REJECT_AFTER));
    assert_eq!(stale, status::RECONNECTING);
    assert!(state::visible_test(stale, view()).is_none());
    // 只有 connected 才帶
    assert!(state::visible_test(status::CONNECTED, view()).is_some());
    // 已經清成 None 的再清一次不會生出新的顯示（呼叫端據此不重複推空事件）
    assert!(state::visible_test(stale, None).is_none());
}
