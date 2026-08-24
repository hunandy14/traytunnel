//! `wg` 生命週期與狀態的測試——設計書 §6 的 W6 系列（16 條，全部 F），
//! 檔尾續編 W6.17～W6.18：MTU 生效優先序（PM 裁決 2026-08-24）。
//!
//! 比照 `state.rs` 既有的純函式測試風格：守門邏輯抽出來測，**不生 AppHandle**。

use super::*;

use std::collections::BTreeMap;

use crate::config::{
    apply_source_enabled, apply_wg_enabled, should_probe, Config, Forward, RowKind, Source, WgProxy,
};
use crate::state::{self, status, TestView, Worker};

fn fwd(name: &str, local: u16) -> Forward {
    Forward {
        name: name.into(),
        local,
        remote: Some("10.0.0.5:22".into()),
        kind: RowKind::Forward,
        probe_proxy: false,
        enabled: true,
    }
}

fn socks(name: &str, local: u16) -> Forward {
    Forward {
        name: name.into(),
        local,
        remote: None,
        kind: RowKind::Socks,
        probe_proxy: false,
        enabled: true,
    }
}

/// hk（ssh，一條列）＋ ax4200（wg，一條 socks 列與兩條 forward 列）
fn cfg_with_wg() -> Config {
    Config {
        close_to_tray: true,
        check_for_updates: None,
        sources: vec![Source {
            name: "hk".into(),
            host: "hk.example.com".into(),
            user: "bob".into(),
            proxy_command: String::new(),
            forwards: vec![Forward {
                remote: Some("127.0.0.1:1080".into()),
                ..fwd("exit-a", 1080)
            }],
        }],
        wg_proxies: vec![WgProxy {
            name: "ax4200".into(),
            conf_path: "wg/ax4200.conf".into(),
            enabled: true,
            mtu: None,
            forwards: vec![socks("socks", 1085), fwd("nas-ssh", 2222), fwd("nas-http", 2280)],
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
    // 本來就沒人在跑的列不必回報
    slots.insert(2222, None);

    let stopped = state::drain_workers(&mut slots);
    assert_eq!(stopped, vec![1080, 1085], "只回報真的收掉了東西的那些埠");
    assert!(wg_token.is_cancelled(), "wg 的任務樹要一起收掉");
    assert!(slots.values().all(|v| v.is_none()), "全部清空");
}

/// W6.4 握手歲數 → 狀態的映射（設計書 §5.2 的門檻表）
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

/// W6.5 連線 halt 時，底下**所有列**的埠都要一起被壓成 stopped
#[test]
fn halting_a_connection_takes_all_of_its_rows_with_it() {
    let cfg = cfg_with_wg();
    let mut locals = halted_locals(&cfg, "ax4200");
    locals.sort_unstable();
    assert_eq!(locals, vec![1085, 2222, 2280]);
    // 別人家的埠不可以被掃到
    assert!(!locals.contains(&1080));
    // 不存在的連線不影響任何人
    assert!(halted_locals(&cfg, "nope").is_empty());
}

/// W6.6 連線名與 ssh 出口共用同一份位子邏輯，重複 `start` 不會起第二顆引擎
#[test]
fn a_second_start_on_the_same_connection_is_refused() {
    // 前提：wg 的列真的在 locals() 裡，AppState 才會替它們開執行期狀態（D5）
    let cfg = cfg_with_wg();
    for local in [1085u16, 2222, 2280] {
        assert!(cfg.locals().contains(&local), "wg 的列必須併進同一個本地埠鍵空間");
    }

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

/// W6.7 引擎 stale 時，所有**被探測的列**其自測顯示與 protocol 快取都要清掉，
/// 而且不重複推空事件
#[test]
fn a_stale_engine_clears_every_probed_row() {
    let cfg = cfg_with_wg();
    let view = || {
        Some(TestView {
            state: "ok".into(),
            text: "1.2.3.4  Taipei, TW".into(),
            protocol: Some("socks5".into()),
        })
    };
    // 握手陳舊 → reconnecting → 快照不可以再帶著上一輪的出口 IP 與徽章
    let stale = status_for_handshake(Some(device::REJECT_AFTER));
    assert_eq!(stale, status::RECONNECTING);
    assert!(state::visible_test(stale, view()).is_none());
    // 只有 connected 才帶
    assert!(state::visible_test(status::CONNECTED, view()).is_some());
    // 已經清成 None 的再清一次不會生出新的顯示（呼叫端據此不重複推空事件）
    assert!(state::visible_test(stale, None).is_none());

    // 要被清的就是 should_probe 為真的那些列：socks 列恆真，純轉發不進場
    let probed: Vec<u16> = cfg.wg_proxies[0]
        .forwards
        .iter()
        .filter(|f| should_probe(f.kind, f.probe_proxy))
        .map(|f| f.local)
        .collect();
    assert_eq!(probed, vec![1085]);
}

/// W6.8 引擎啟停條件：零列或全部停用的連線**不起引擎**
#[test]
fn an_engine_only_runs_when_at_least_one_row_is_enabled() {
    let mut cfg = cfg_with_wg();
    assert!(should_run_engine(&cfg, "ax4200"));

    for f in cfg.wg_proxies[0].forwards.iter_mut() {
        f.enabled = false;
    }
    assert!(!should_run_engine(&cfg, "ax4200"), "全部停用就沒有東西會用到這條隧道");

    cfg.wg_proxies[0].forwards.clear();
    assert!(!should_run_engine(&cfg, "ax4200"), "零列一樣不起");

    assert!(!should_run_engine(&cfg, "nope"), "不存在的連線當然不起");
}

/// W6.9 單一列的埠被佔：**只有那一條**進 port_busy，其餘照常
///
/// 這是與 ssh 不同的地方，而且是刻意的（§5.2）。
#[test]
fn a_busy_port_only_affects_its_own_row() {
    let rows = [1085u16, 2222, 2280];
    let got = row_statuses(&rows, status::CONNECTED, &[2222]);
    assert_eq!(
        got,
        vec![(1085, status::CONNECTED), (2222, status::PORT_BUSY), (2280, status::CONNECTED),]
    );
    // 沒有人被佔時全部照引擎的狀態走
    let got = row_statuses(&rows, status::RECONNECTING, &[]);
    assert!(got.iter().all(|(_, s)| *s == status::RECONNECTING));
}

// -------------------------------------------------- set_wg_enabled（§5.5）

/// W6.10 `set_wg_enabled(name, false)`：只改連線自己的 enabled，
/// **三條列的 enabled 一個都沒被改**。這是這一支最重要的一條。
#[test]
fn turning_a_wg_connection_off_never_touches_its_rows() {
    let mut cfg = cfg_with_wg();
    cfg.wg_proxies[0].forwards[1].enabled = false; // true / false / true

    assert!(apply_wg_enabled(&mut cfg, "ax4200", false));
    assert!(!cfg.wg_proxies[0].enabled);
    let flags: Vec<bool> = cfg.wg_proxies[0].forwards.iter().map(|f| f.enabled).collect();
    assert_eq!(flags, vec![true, false, true], "列的逐條意圖要原封不動");
}

/// W6.11 再打開：只有原本 enabled = true 的那兩條列被啟動
#[test]
fn turning_it_back_on_only_starts_the_rows_the_user_left_enabled() {
    let mut cfg = cfg_with_wg();
    cfg.wg_proxies[0].forwards[1].enabled = false;
    apply_wg_enabled(&mut cfg, "ax4200", false);
    assert!(rows_to_start(&cfg, "ax4200").is_empty(), "連線關著就一條都不起");

    assert!(apply_wg_enabled(&mut cfg, "ax4200", true));
    assert_eq!(rows_to_start(&cfg, "ax4200"), vec![1085, 2280], "中間那條停用的仍是 stopped");
}

/// W6.12 對比：ssh 的 `set_source_enabled` 逐條把 forward 的 enabled 寫掉。
///
/// **行為刻意與 W6.10 不同**——這條測試存在的目的就是釘住這個不對稱，
/// 避免日後有人「順手對齊」。理由見 §5.5 那張表。
#[test]
fn the_ssh_connection_switch_is_deliberately_asymmetric() {
    let mut cfg = cfg_with_wg();
    cfg.sources[0].forwards.push(Forward { enabled: false, ..fwd("db", 5432) });

    assert!(apply_source_enabled(&mut cfg, "hk", false));
    assert!(
        cfg.sources[0].forwards.iter().all(|f| !f.enabled),
        "ssh 沒有「連線」這個執行實體，要停就只能逐條停"
    );

    assert!(apply_source_enabled(&mut cfg, "hk", true));
    assert!(
        cfg.sources[0].forwards.iter().all(|f| f.enabled),
        "重新打開時全部列都會起——因為剛剛全被寫成 true"
    );
}

/// W6.13 落檔順序：先存檔成功才動引擎；存檔失敗時引擎維持原狀，
/// 並推一次 `emit_config_changed` 把樂觀翻過去的開關拉回真值
#[test]
fn the_engine_only_moves_after_the_save_succeeded() {
    use WgEnabledStep::*;
    // 連接：先推事件再拉線，介面立刻看得到 connecting
    assert_eq!(
        wg_enabled_steps("ax4200", true, true, true),
        vec![EmitConfigChanged, StartEngine("ax4200".into())]
    );
    // 中斷：先停線再推事件，不會出現「已停用但還連著」的那一瞬
    assert_eq!(
        wg_enabled_steps("ax4200", false, true, true),
        vec![HaltEngine("ax4200".into()), EmitConfigChanged]
    );
    // 存檔失敗：引擎一動都不動，只把介面拉回真值
    assert_eq!(wg_enabled_steps("ax4200", true, false, true), vec![EmitConfigChanged]);
    assert_eq!(wg_enabled_steps("ax4200", false, false, true), vec![EmitConfigChanged]);
}

/// W6.14 `set_wg_enabled(name, true)` 但底下零條 enabled 的列：
/// 設定寫入成功，但**引擎不啟動**，不留下一顆空轉的 WireGuard
#[test]
fn turning_on_a_connection_with_no_enabled_rows_starts_nothing() {
    let mut cfg = cfg_with_wg();
    for f in cfg.wg_proxies[0].forwards.iter_mut() {
        f.enabled = false;
    }
    assert!(apply_wg_enabled(&mut cfg, "ax4200", true));
    assert!(cfg.wg_proxies[0].enabled, "設定要寫進去");
    assert!(rows_to_start(&cfg, "ax4200").is_empty());
    assert_eq!(
        wg_enabled_steps("ax4200", true, true, false),
        vec![WgEnabledStep::EmitConfigChanged],
        "沒有任何列要跑就不起引擎（§5.2）"
    );
}

/// W6.15 對不存在的連線名：記一行日誌就退，不 panic、不建出幽靈連線
#[test]
fn set_wg_enabled_on_an_unknown_name_is_a_no_op() {
    let mut cfg = cfg_with_wg();
    let before = cfg.clone();
    assert!(!apply_wg_enabled(&mut cfg, "nope", false), "找不到就回 false");
    assert_eq!(cfg, before, "不可以憑空長出一條連線");
    assert!(rows_to_start(&cfg, "nope").is_empty());
}

/// W6.16 `set_wg_enabled(name, false)` 之後：每一條列各推一次
/// `exit-status = stopped`，連線層不推任何新事件（§5.3 的零新事件仍成立）
#[test]
fn every_row_reports_stopped_exactly_once_and_no_new_event_is_added() {
    let cfg = cfg_with_wg();
    let locals = halted_locals(&cfg, "ax4200");
    let mut sorted = locals.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), locals.len(), "每一條列只推一次，不重複");

    let statuses = row_statuses(&locals, status::STOPPED, &[]);
    assert!(statuses.iter().all(|(_, s)| *s == status::STOPPED));

    // 狀態字彙一個都不用新增——這是「零新事件」能成立的關鍵
    for s in statuses.iter().map(|(_, s)| *s) {
        assert!(
            [
                status::STOPPED,
                status::CONNECTING,
                status::CONNECTED,
                status::RECONNECTING,
                status::PORT_BUSY,
                status::ERROR,
            ]
            .contains(&s),
            "冒出了新的狀態字彙：{s}"
        );
    }
}

// ------------------------------------------ MTU 優先序（PM 裁決 2026-08-24）

/// W6.17 `effective_mtu` 的三態優先序：**介面覆寫 ＞ conf 明寫 ＞ 應用層預設**。
///
/// 這一條就是規格本身。實際的動機是使用者那台 ASUS 路由器匯出的 `.conf` 不寫
/// MTU，而他那條線路的路徑 MTU 又小於 1420，大封包靜默黑洞；他必須能在介面上
/// 壓下去，而且**不必去改那份 `.conf`**。
#[test]
fn the_ui_override_beats_the_conf_which_beats_the_app_default() {
    // ① 兩者都有：介面說了算，conf 的值被覆寫掉
    assert_eq!(effective_mtu(Some(1400), Some(1420)), 1400);
    // ② 只有 conf 明寫：照 conf
    assert_eq!(effective_mtu(None, Some(1420)), 1420);
    // ③ 都沒有：落到應用層預設，而不是 wg-quick 那個 1420
    assert_eq!(effective_mtu(None, None), conf::APP_DEFAULT_MTU);
    assert_eq!(effective_mtu(None, None), 1280);
    // ④ 只有介面覆寫（conf 沒寫 MTU，正是使用者那份檔案的樣子）
    assert_eq!(effective_mtu(Some(1400), None), 1400);
}

/// W6.18 覆寫值與 conf 值相同時不是特例：算出來就是那個值，沒有「等於就忽略」
/// 這種暗規則。順手釘住覆寫值可以比 conf 大（線路吃得下就該讓人往上調）。
#[test]
fn an_override_may_equal_or_exceed_the_conf_value() {
    assert_eq!(effective_mtu(Some(1420), Some(1420)), 1420);
    assert_eq!(effective_mtu(Some(1500), Some(1280)), 1500);
}
