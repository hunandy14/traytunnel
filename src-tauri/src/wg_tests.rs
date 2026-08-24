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

/// 這一段的三態拿來當 `plan_mtu` 的斷言值，讀起來比 `mtu::Plan::Fixed(..)` 短
use crate::wg::mtu::Plan::{Fixed, Probe as ProbePlan};

/// W6.17 `plan_mtu` 的優先序：**介面覆寫 ＞ conf 明寫 ＞ 自動探測**。
///
/// 這一條就是規格本身。實際的動機是使用者那台 ASUS 路由器匯出的 `.conf` 不寫
/// MTU，而他那條線路的路徑 MTU 又小於 1420，大封包靜默黑洞；他必須能在介面上
/// 壓下去，而且**不必去改那份 `.conf`**。
///
/// 覆審打回 2026-08-24：原本 `effective_mtu`（算值）與 `should_probe_mtu`
/// （算要不要探）是兩支，各說一半；併成一個回傳 enum 的決策點之後，
/// 「要探測、卻同時又指定了 1400」那種組合連表達都表達不出來。
#[test]
fn the_ui_override_beats_the_conf_which_beats_the_probe() {
    // ① 兩者都有：介面說了算，conf 的值被覆寫掉
    assert_eq!(plan_mtu(Some(1400), Some(1420)), Fixed(1400));
    // ② 只有 conf 明寫：照 conf
    assert_eq!(plan_mtu(None, Some(1420)), Fixed(1420));
    // ③ 只有介面覆寫（conf 沒寫 MTU，正是使用者那份檔案的樣子）
    assert_eq!(plan_mtu(Some(1400), None), Fixed(1400));
    // ④ 都沒有：才輪到自動探測。探不出來時它自己會落回應用層預設
    assert_eq!(plan_mtu(None, None), ProbePlan);
    assert_eq!(mtu::SAFE_MTU, conf::APP_DEFAULT_MTU);
    assert_eq!(mtu::SAFE_MTU, 1280);
    assert_eq!(mtu::HIGH_MTU, 1420, "探得過才升到這個值");
}

/// W6.18 覆寫值與 conf 值相同時不是特例：算出來就是那個值，沒有「等於就忽略」
/// 這種暗規則。順手釘住覆寫值可以比 conf 大（線路吃得下就該讓人往上調）。
#[test]
fn an_override_may_equal_or_exceed_the_conf_value() {
    assert_eq!(plan_mtu(Some(1420), Some(1420)), Fixed(1420));
    assert_eq!(plan_mtu(Some(1500), Some(1280)), Fixed(1500));
}

// ------------------------------ 握手韌性（PM 裁決 2026-08-24，
//                                 覆審打回後重做：typed 事件＋先解析後決定）

fn engine_event(health: engine::EngineHealth) -> engine::EngineEvent {
    engine::EngineEvent::Engine(health, None)
}

/// W6.19 首次握手的寬限期是**獨立的一個值**，而且遠小於 `REJECT_AFTER`。
///
/// 這一條就是規格：以前「從來沒握上」借用 180 秒當耐心值，使用者要盯著三分鐘
/// 的 connecting 才看得到 reconnecting，底下所有靠 reconnecting 觸發的自癒
/// 也一起被押到三分鐘之後。
#[test]
fn the_first_handshake_grace_is_its_own_much_shorter_value() {
    use std::time::Duration;
    assert_eq!(device::FIRST_HANDSHAKE_GRACE, Duration::from_secs(15));
    assert!(device::FIRST_HANDSHAKE_GRACE < device::REJECT_AFTER);
    // 既有 session 的門檻一個字都沒動
    assert_eq!(device::REJECT_AFTER, Duration::from_secs(180));
}

/// W6.20 狀態日誌：握手成功記**真的耗時**、進入 reconnecting 記一行，
/// 兩者都只在變化時記一次。
///
/// 覆審打回 2026-08-24 兩件事：吃的是 typed 的 `EngineEvent`（不是 UI 字串），
/// 而且計時錨在引擎啟動那一刻——舊版把 watch 建在 `engine::spawn` 之後，
/// 探測路徑上事件早就在佇列裡等著，量出來恆為 0ms。
#[test]
fn the_watch_logs_each_handshake_transition_exactly_once() {
    use engine::EngineHealth::{Connected, Failed, Reconnecting};
    use std::time::Duration;
    let t0 = Instant::now();
    let mut watch = HandshakeWatch::new(t0, RECONNECT_REBUILD_AFTER);

    // 握上了：耗時從引擎啟動起算
    let line = watch.on_event(&engine_event(Connected), t0 + Duration::from_millis(420));
    assert_eq!(line.as_deref(), Some("handshake ok in 420ms"));
    // 同一個狀態再來一次不重複刷屏
    assert!(watch.on_event(&engine_event(Connected), t0 + Duration::from_millis(500)).is_none());

    // 掉線：以前這條路徑是完全靜默的
    let line = watch.on_event(&engine_event(Reconnecting), t0 + Duration::from_secs(10));
    assert_eq!(line.as_deref(), Some(HANDSHAKE_RETRY_LOG));
    assert!(watch.on_event(&engine_event(Reconnecting), t0 + Duration::from_secs(12)).is_none());

    // 再握上：耗時從**掉線那一刻**起算，不是從引擎啟動起算
    let line = watch.on_event(&engine_event(Connected), t0 + Duration::from_secs(13));
    assert_eq!(line.as_deref(), Some("handshake ok in 3000ms"));

    // 其他事件不歸這顆狀態機管
    assert!(watch.on_event(&engine_event(Failed), t0 + Duration::from_secs(14)).is_none());
    let row = engine::EngineEvent::Row(1085, status::PORT_BUSY, None);
    assert!(watch.on_event(&row, t0 + Duration::from_secs(14)).is_none());
    let probed = engine::EngineEvent::Mtu(mtu::Probe::Ok);
    assert!(watch.on_event(&probed, t0 + Duration::from_secs(14)).is_none());
}

/// W6.21 卡太久就該**去複查端點**（不是直接重建）。
#[test]
fn a_stuck_reconnect_eventually_asks_for_an_endpoint_recheck() {
    use engine::EngineHealth::{Connected, Reconnecting};
    use std::time::Duration;
    let t0 = Instant::now();
    // 門檻可注入，正式路徑上傳的是 RECONNECT_REBUILD_AFTER
    let mut watch = HandshakeWatch::new(t0, Duration::from_secs(30));

    // 還沒掉線過：再久都不動它（隧道好好的，不可以自己去拆）
    assert!(!watch.overdue(t0 + Duration::from_secs(3600)));
    watch.on_event(&engine_event(Connected), t0);
    assert!(!watch.overdue(t0 + Duration::from_secs(3600)));

    watch.on_event(&engine_event(Reconnecting), t0 + Duration::from_secs(10));
    assert!(!watch.overdue(t0 + Duration::from_secs(39)), "只是抖一下就去動它太吵");
    assert!(watch.overdue(t0 + Duration::from_secs(40)), "門檻一到就該複查");

    // 複查完、位址沒變：重新計時，而且同一段掉線只記一行
    let after = watch.note_endpoint_unchanged(t0 + Duration::from_secs(40));
    assert_eq!(after, AfterRecheck::KeepWaiting(Some(ENDPOINT_UNCHANGED_LOG.to_string())));
    assert!(!watch.overdue(t0 + Duration::from_secs(69)), "複查過就重新計時");
    assert!(watch.overdue(t0 + Duration::from_secs(70)), "下一次複查在一個門檻之後");
    assert_eq!(
        watch.note_endpoint_unchanged(t0 + Duration::from_secs(70)),
        AfterRecheck::KeepWaiting(None),
        "同一段掉線不重複刷屏——這正是離線端點每 80 秒洗一次日誌的來源"
    );

    // 中途自己好了就歸零
    watch.on_event(&engine_event(Connected), t0 + Duration::from_secs(71));
    assert!(!watch.overdue(t0 + Duration::from_secs(3600)));

    // 正式常數：60 秒，比重連間隔（5 秒）大一個數量級
    assert_eq!(RECONNECT_REBUILD_AFTER, Duration::from_secs(60));
    assert!(RECONNECT_REBUILD_AFTER > RETRY);
}

/// W6.22 DDNS 自癒的裁決：**位址真的變了才重建**。
///
/// 覆審打回 2026-08-24：舊版是「卡了 60 秒就無條件重建」，在端點根本沒搬家的
/// 情況下那是一個無限重建迴圈——每 60 秒把一條只是暫時連不上的隧道整個拆掉
/// 重蓋，期間所有列都被打回 connecting，而它八成只是對端還沒開機。
#[test]
fn only_a_changed_endpoint_address_justifies_a_rebuild() {
    let current: std::net::SocketAddr = "203.0.113.7:51820".parse().unwrap();
    let moved: std::net::SocketAddr = "203.0.113.9:51820".parse().unwrap();

    assert_eq!(stuck_action(current, Ok(moved)), StuckAction::Rebuild, "IP 漂走了才重建");
    assert_eq!(
        stuck_action(current, Ok(current)),
        StuckAction::KeepWaiting,
        "位址沒變就別拆——重建一次也連不上，只是把畫面洗一遍"
    );
    // 連埠變了也算搬家
    let other_port: std::net::SocketAddr = "203.0.113.7:51821".parse().unwrap();
    assert_eq!(stuck_action(current, Ok(other_port)), StuckAction::Rebuild);
    // 這一刻解析不出來（多半是本機網路整個斷了）：等，不重建
    assert_eq!(stuck_action(current, Err("nope".into())), StuckAction::KeepWaiting);
}

/// W6.25 保險絲：位址從頭到尾沒變，但連續複查 `STUCK_RECHECK_FUSE` 次隧道
/// 都不會好——還是重建一次。
///
/// PM 裁決 2026-08-24（採納重做後的自我提示）：`stuck_action` 只認得「端點
/// 搬家了」這一種故障；**端點沒搬家、但引擎自身卡死**那一類未知故障若完全
/// 沒有重建的機會，等於把自癒能力押在「我們已經想到所有故障模式」上。
/// 5 次 × 60 秒 ≈ 5 分鐘，比舊版的每 60 秒無條件重建安靜一個級距。
#[test]
fn five_unchanged_rechecks_still_blow_the_fuse() {
    use engine::EngineHealth::{Connected, Reconnecting};
    use std::time::Duration;
    let t0 = Instant::now();
    let step = Duration::from_secs(60);
    let mut watch = HandshakeWatch::new(t0, step);
    watch.on_event(&engine_event(Reconnecting), t0);

    // 前四次都只是等：不可以每 60 秒就把隧道拆掉重蓋
    for n in 1..STUCK_RECHECK_FUSE {
        let after = watch.note_endpoint_unchanged(t0 + step * n);
        assert!(
            matches!(after, AfterRecheck::KeepWaiting(_)),
            "第 {n} 次複查位址沒變，這時候還不該重建"
        );
    }
    // 第五次：保險絲燒斷
    assert_eq!(
        watch.note_endpoint_unchanged(t0 + step * STUCK_RECHECK_FUSE),
        AfterRecheck::BlowFuse,
        "連續 {STUCK_RECHECK_FUSE} 次都沒好，未知故障也該有一次自癒的機會"
    );
    // 日誌措辭要與「IP 變了」那一行分得開，否則使用者會被導去查 DNS
    assert_ne!(rebuild_fuse_log(), REBUILD_LOG);
    assert!(rebuild_fuse_log().contains("still stuck"));
    assert_eq!(STUCK_RECHECK_FUSE, 5);

    // 中途自己好了：計數歸零，下一段掉線重新數五次
    let mut watch = HandshakeWatch::new(t0, step);
    watch.on_event(&engine_event(Reconnecting), t0);
    for n in 1..STUCK_RECHECK_FUSE {
        watch.note_endpoint_unchanged(t0 + step * n);
    }
    watch.on_event(&engine_event(Connected), t0 + step * 5);
    watch.on_event(&engine_event(Reconnecting), t0 + step * 6);
    assert!(
        matches!(watch.note_endpoint_unchanged(t0 + step * 7), AfterRecheck::KeepWaiting(_)),
        "復原過就歸零：不可以在下一段掉線的第一次複查就燒保險絲"
    );
}

/// W6.24 **復原的事件優先於「卡太久」的判定**。
///
/// 覆審打回 2026-08-24 揪出的競態：一顆剛送達、還沒被處理的 `connected`
/// 若被 overdue 搶先，一條剛剛自己復原的隧道就會被當成卡死的拆掉重建。
#[test]
fn a_pending_event_is_always_handled_before_the_stuck_check() {
    use engine::EngineHealth::{Connected, Reconnecting};
    use std::time::Duration;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<engine::EngineEvent>(4);
    let t0 = Instant::now();
    let mut watch = HandshakeWatch::new(t0, Duration::from_secs(30));
    watch.on_event(&engine_event(Reconnecting), t0);
    let late = t0 + Duration::from_secs(60);
    assert!(watch.overdue(late), "前提：這一刻已經卡過門檻了");

    // 隧道其實剛剛就好了，那顆 connected 還排在佇列裡
    tx.try_send(engine_event(Connected)).unwrap();
    assert_eq!(
        next_step(&mut rx, &watch, late),
        Next::Event(engine_event(Connected)),
        "佇列裡還有東西時不可以先去複查端點，那會把剛復原的隧道拆掉"
    );

    // 吃乾淨了才輪到複查
    assert_eq!(next_step(&mut rx, &watch, late), Next::Recheck);
    // 還沒到期就只是去等下一顆
    assert_eq!(next_step(&mut rx, &watch, t0 + Duration::from_secs(1)), Next::Wait);
    // 引擎那棵任務樹沒了
    drop(tx);
    assert_eq!(next_step(&mut rx, &watch, late), Next::Gone);
}

/// W6.23 探測快取：**上一輪探過就沿用，這一輪不再探**。
///
/// 覆審打回 2026-08-24：沒有快取的話，一個離線的端點會每 80 秒刷一次探測
/// 日誌，而且每一輪都白等一次握手逾時。
#[test]
fn the_probe_result_is_reused_across_rebuilds() {
    // 沒探過：照原計劃探
    assert_eq!(mtu_for_round(ProbePlan, None), ProbePlan);
    // 上一輪探到了 1420：直接用，不再探
    assert_eq!(mtu_for_round(ProbePlan, Some(&mtu::Probe::Ok)), Fixed(mtu::HIGH_MTU));
    // 上一輪探不過：也記住，不要每 80 秒再警告一次
    assert_eq!(mtu_for_round(ProbePlan, Some(&mtu::Probe::Failed)), Fixed(mtu::SAFE_MTU));
    // 上一輪連握手都沒等到：一樣沿用，重建輪不再白等 8 秒
    let skipped = mtu::Probe::Skipped(mtu::NO_HANDSHAKE);
    assert_eq!(mtu_for_round(ProbePlan, Some(&skipped)), Fixed(mtu::SAFE_MTU));
    // 有人明寫過 MTU 的話，快取一點影響力都沒有
    assert_eq!(mtu_for_round(Fixed(1400), Some(&mtu::Probe::Ok)), Fixed(1400));
}
