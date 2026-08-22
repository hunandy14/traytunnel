//! 系統匣右鍵選單：先把「狀態快照 → 選單模型」算成純資料（`menu_model`），
//! 再把模型轉成 Tauri 的原生選單物件貼到系統匣上。
//!
//! 分成兩段是為了讓版面規則（攤平與否、狀態行文字、Start／Stop all 的字）
//! 可以單獨測試，不必生出一個真的 app。
//!
//! 更新策略：選單很小，任何影響它的狀態一變就整個重建再換上去，
//! 不做逐項增刪，也不輪詢。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Wry};

use crate::state::{status, ExitView, SourceView, TRAY_ID};

/// 選單項目的 id，事件端靠這些字串路由
pub const ID_STATUS: &str = "status";
pub const ID_OPEN: &str = "open";
pub const ID_EXIT: &str = "exit";
pub const ID_ALL_TOGGLE: &str = "all-toggle";
pub const ID_TEST_ALL: &str = "test-all";
/// 單一出口的開關，後面接本地埠
pub const EXIT_PREFIX: &str = "exit:";
/// 單一源的重測，後面接源名
pub const SRC_TEST_PREFIX: &str = "src-test:";

/// 出口少於這個數量而且只有一個源時，出口直接攤在根層；
/// 再多就巢狀進子選單，免得整份選單長到蓋住半個螢幕。
const FLATTEN_LIMIT: usize = 5;

/// 選單模型的一個節點，純資料
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// 不可點的狀態行
    Status(String),
    Separator,
    /// 一般可點項目
    Item { id: String, label: String },
    /// 出口開關，勾選＝設定裡的 enabled
    Check { id: String, label: String, checked: bool },
    /// 一個源一個子選單
    Submenu { label: String, items: Vec<Node> },
}

fn item(id: impl Into<String>, label: impl Into<String>) -> Node {
    Node::Item { id: id.into(), label: label.into() }
}

/// 所有源底下的出口，攤平成一串
fn all_exits(sources: &[SourceView]) -> impl Iterator<Item = &ExitView> {
    sources.iter().flat_map(|s| s.exits.iter())
}

/// 狀態行：連線數／出口總數，跟主視窗的彙總列同一套算法
fn status_line(sources: &[SourceView]) -> String {
    if sources.is_empty() {
        return "No sources".into();
    }
    let total = all_exits(sources).count();
    if total == 0 {
        return "No exits".into();
    }
    let connected = all_exits(sources).filter(|e| e.status == status::CONNECTED).count();
    format!("{connected}/{total} Connected")
}

/// 有任何出口 enabled 就給 Stop all，全停時給 Start all
fn toggle_label(sources: &[SourceView]) -> &'static str {
    if all_exits(sources).any(|e| e.enabled) {
        "Stop all"
    } else {
        "Start all"
    }
}

fn exit_node(exit: &ExitView) -> Node {
    Node::Check {
        id: format!("{EXIT_PREFIX}{}", exit.local),
        label: format!("{} ({})", exit.name, exit.local),
        checked: exit.enabled,
    }
}

/// 一個源一個子選單：出口 + 分隔線 + Retest source。
/// 源底下沒出口時不放那條分隔線，免得子選單開頭空一格。
fn source_node(src: &SourceView) -> Node {
    let mut items: Vec<Node> = src.exits.iter().map(exit_node).collect();
    if !items.is_empty() {
        items.push(Node::Separator);
    }
    items.push(item(format!("{SRC_TEST_PREFIX}{}", src.name), "Retest source"));
    Node::Submenu { label: src.name.clone(), items }
}

/// 單一源而且出口不多時才攤平
fn flattened(sources: &[SourceView]) -> bool {
    matches!(sources, [only] if only.exits.len() < FLATTEN_LIMIT)
}

/// 把非空的區段用分隔線串起來，開頭結尾都不會多出分隔線
fn join(sections: Vec<Vec<Node>>) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::new();
    for section in sections.into_iter().filter(|s| !s.is_empty()) {
        if !out.is_empty() {
            out.push(Node::Separator);
        }
        out.extend(section);
    }
    out
}

/// 狀態快照 → 選單模型。這是版面規則的唯一出處。
pub fn menu_model(sources: &[SourceView]) -> Vec<Node> {
    let mut sections = vec![vec![Node::Status(status_line(sources))]];
    if !sources.is_empty() {
        sections.push(if flattened(sources) {
            sources[0].exits.iter().map(exit_node).collect()
        } else {
            sources.iter().map(source_node).collect()
        });
        sections.push(vec![
            item(ID_ALL_TOGGLE, toggle_label(sources)),
            item(ID_TEST_ALL, "Retest all"),
        ]);
    }
    sections.push(vec![item(ID_OPEN, "Open window"), item(ID_EXIT, "Exit")]);
    join(sections)
}

// ---------------------------------------------------------------- 貼到系統匣

type Items = Vec<Box<dyn IsMenuItem<Wry>>>;

fn build_items(app: &AppHandle, nodes: &[Node]) -> tauri::Result<Items> {
    let mut out: Items = Vec::with_capacity(nodes.len());
    for node in nodes {
        let built: Box<dyn IsMenuItem<Wry>> = match node {
            Node::Status(text) => {
                Box::new(MenuItem::with_id(app, ID_STATUS, text, false, None::<&str>)?)
            }
            Node::Separator => Box::new(PredefinedMenuItem::separator(app)?),
            Node::Item { id, label } => {
                Box::new(MenuItem::with_id(app, id.as_str(), label, true, None::<&str>)?)
            }
            Node::Check { id, label, checked } => Box::new(CheckMenuItem::with_id(
                app,
                id.as_str(),
                label,
                true,
                *checked,
                None::<&str>,
            )?),
            Node::Submenu { label, items } => {
                let kids = build_items(app, items)?;
                Box::new(Submenu::with_items(app, label, true, &borrow(&kids))?)
            }
        };
        out.push(built);
    }
    Ok(out)
}

fn borrow(items: &Items) -> Vec<&dyn IsMenuItem<Wry>> {
    items.iter().map(|i| i.as_ref()).collect()
}

/// 依模型長出一份新的原生選單
pub fn build(app: &AppHandle, nodes: &[Node]) -> tauri::Result<Menu<Wry>> {
    let items = build_items(app, nodes)?;
    Menu::with_items(app, &borrow(&items))
}

/// 換上去的那一刻要照順序來：後算出來的模型才是新的
static SEQ: AtomicU64 = AtomicU64::new(0);
static APPLY: Mutex<()> = Mutex::new(());

/// 重建整份選單並換上系統匣。
///
/// 模型在呼叫端的執行緒上就算好（純函式，不碰鎖也不碰 tray），真正生選單與
/// `set_menu` 丟到背景執行：選單事件是在主執行緒上處理的，若在事件處理途中同步
/// 把選單換掉，等於在自己的回呼裡抽掉正在用的那一份。
pub fn refresh(app: &AppHandle, sources: &[SourceView]) {
    let nodes = menu_model(sources);
    let seq = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY.lock().unwrap_or_else(|e| e.into_inner());
        // 已經有更新的模型排在後面，這一份貼上去只會讓畫面倒退
        if SEQ.load(Ordering::SeqCst) != seq {
            return;
        }
        if let Err(e) = apply(&app, &nodes) {
            log::warn!("could not refresh the tray menu: {e}");
        }
    });
}

fn apply(app: &AppHandle, nodes: &[Node]) -> tauri::Result<()> {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(()); // 系統匣還沒長出來（或已經收掉），沒什麼好更新的
    };
    tray.set_menu(Some(build(app, nodes)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit(name: &str, local: u16, enabled: bool, st: &str) -> ExitView {
        ExitView {
            name: name.into(),
            local,
            remote: format!("127.0.0.1:{local}"),
            enabled,
            status: st.into(),
            last_test: None,
        }
    }

    fn source(name: &str, exits: Vec<ExitView>) -> SourceView {
        SourceView {
            name: name.into(),
            host: "h.example.com".into(),
            user: "bob".into(),
            proxy_command: String::new(),
            exits,
        }
    }

    fn check(id: &str, label: &str, checked: bool) -> Node {
        Node::Check { id: id.into(), label: label.into(), checked }
    }

    /// 兩個源、三個出口，其中兩個連上
    fn two_sources() -> Vec<SourceView> {
        vec![
            source(
                "hk",
                vec![
                    exit("a", 1080, true, status::CONNECTED),
                    exit("b", 1083, false, status::STOPPED),
                ],
            ),
            source("tw", vec![exit("c", 1090, true, status::CONNECTED)]),
        ]
    }

    /// 多源：狀態行、每源一個子選單、全域動作、視窗動作，各段之間一條分隔線
    #[test]
    fn multi_source_puts_every_source_in_its_own_submenu() {
        assert_eq!(
            menu_model(&two_sources()),
            vec![
                Node::Status("2/3 Connected".into()),
                Node::Separator,
                Node::Submenu {
                    label: "hk".into(),
                    items: vec![
                        check("exit:1080", "a (1080)", true),
                        check("exit:1083", "b (1083)", false),
                        Node::Separator,
                        item("src-test:hk", "Retest source"),
                    ],
                },
                Node::Submenu {
                    label: "tw".into(),
                    items: vec![
                        check("exit:1090", "c (1090)", true),
                        Node::Separator,
                        item("src-test:tw", "Retest source"),
                    ],
                },
                Node::Separator,
                item("all-toggle", "Stop all"),
                item("test-all", "Retest all"),
                Node::Separator,
                item("open", "Open window"),
                item("exit", "Exit"),
            ]
        );
    }

    /// 狀態行的分母是全部出口（含停用的），分子是連上的，與主視窗彙總一致
    #[test]
    fn status_line_counts_connected_over_all_exits() {
        let mut sources = two_sources();
        assert_eq!(status_line(&sources), "2/3 Connected");
        sources[1].exits[0].status = status::RECONNECTING.into();
        assert_eq!(status_line(&sources), "1/3 Connected");
        for s in sources.iter_mut() {
            for e in s.exits.iter_mut() {
                e.status = status::STOPPED.into();
            }
        }
        assert_eq!(status_line(&sources), "0/3 Connected");
    }

    /// 單源且出口不多：出口直接放根層，沒有子選單也沒有 Retest source
    #[test]
    fn single_source_flattens_its_exits_to_the_root() {
        let sources = vec![source(
            "hk",
            vec![exit("a", 1080, true, status::CONNECTED), exit("b", 1083, true, status::STOPPED)],
        )];
        assert_eq!(
            menu_model(&sources),
            vec![
                Node::Status("1/2 Connected".into()),
                Node::Separator,
                check("exit:1080", "a (1080)", true),
                check("exit:1083", "b (1083)", true),
                Node::Separator,
                item("all-toggle", "Stop all"),
                item("test-all", "Retest all"),
                Node::Separator,
                item("open", "Open window"),
                item("exit", "Exit"),
            ]
        );
    }

    /// 單源但出口多（=FLATTEN_LIMIT）就不攤平，仍舊收進子選單
    #[test]
    fn single_source_with_five_exits_keeps_the_submenu() {
        let exits: Vec<ExitView> = (0..FLATTEN_LIMIT)
            .map(|i| exit(&format!("e{i}"), 1080 + i as u16, true, status::STOPPED))
            .collect();
        let model = menu_model(&[source("hk", exits)]);
        let Some(Node::Submenu { label, items }) = model.get(2) else {
            panic!("第三項應該是子選單，實際是 {:?}", model.get(2));
        };
        assert_eq!(label, "hk");
        assert_eq!(items.len(), FLATTEN_LIMIT + 2); // 出口 + 分隔線 + Retest source
        assert_eq!(items.last(), Some(&item("src-test:hk", "Retest source")));
        // 根層不該直接出現出口
        assert!(!model.iter().any(|n| matches!(n, Node::Check { .. })));
    }

    /// 少一個出口就回到攤平
    #[test]
    fn one_exit_below_the_limit_flattens_again() {
        let exits: Vec<ExitView> = (0..FLATTEN_LIMIT - 1)
            .map(|i| exit(&format!("e{i}"), 1080 + i as u16, true, status::STOPPED))
            .collect();
        let model = menu_model(&[source("hk", exits)]);
        assert!(!model.iter().any(|n| matches!(n, Node::Submenu { .. })));
        assert_eq!(model.iter().filter(|n| matches!(n, Node::Check { .. })).count(), 4);
    }

    /// 零源：只有狀態行與視窗動作，連 Start all／Retest all 都不給
    #[test]
    fn no_sources_shows_only_the_status_line_and_window_actions() {
        assert_eq!(
            menu_model(&[]),
            vec![
                Node::Status("No sources".into()),
                Node::Separator,
                item("open", "Open window"),
                item("exit", "Exit"),
            ]
        );
    }

    /// 有源沒出口：狀態行講 No exits，全域動作仍在（Start all 會是無事發生但不必藏）
    #[test]
    fn a_source_without_exits_says_no_exits() {
        let model = menu_model(&[source("hk", vec![])]);
        assert_eq!(model[0], Node::Status("No exits".into()));
        // 不會冒出兩條連在一起的分隔線
        assert_eq!(model[1], Node::Separator);
        assert_eq!(model[2], item("all-toggle", "Start all"));
    }

    /// 全部停用時要顯示 Start all，只要還有一個 enabled 就是 Stop all
    #[test]
    fn toggle_reads_start_all_only_when_everything_is_disabled() {
        let mut sources = two_sources();
        assert_eq!(toggle_label(&sources), "Stop all");
        sources[1].exits[0].enabled = false;
        assert_eq!(toggle_label(&sources), "Stop all"); // hk 的 a 還開著
        sources[0].exits[0].enabled = false;
        assert_eq!(toggle_label(&sources), "Start all");
        assert_eq!(menu_model(&sources)[5], item("all-toggle", "Start all"));
    }

    /// 勾選狀態看的是設定裡的 enabled，不是連線狀態
    #[test]
    fn checks_follow_enabled_not_status() {
        let sources = vec![source(
            "hk",
            vec![
                exit("a", 1080, true, status::RECONNECTING),
                exit("b", 1083, false, status::CONNECTED),
            ],
        )];
        let model = menu_model(&sources);
        let checks: Vec<Node> =
            model.iter().filter(|n| matches!(n, Node::Check { .. })).cloned().collect();
        assert_eq!(
            checks,
            vec![check("exit:1080", "a (1080)", true), check("exit:1083", "b (1083)", false)]
        );
    }

    /// 出口的 id 帶得回本地埠，事件端才路由得到
    #[test]
    fn exit_ids_carry_the_local_port() {
        let model = menu_model(&two_sources());
        let Some(Node::Submenu { items, .. }) = model.get(2) else { panic!("要有子選單") };
        let Node::Check { id, .. } = &items[0] else { panic!("第一項要是出口") };
        assert_eq!(id.strip_prefix(EXIT_PREFIX).and_then(|p| p.parse::<u16>().ok()), Some(1080));
    }

    /// 版面不變量：不會有連在一起的分隔線，也不會開頭或結尾就是分隔線
    #[test]
    fn separators_never_bunch_up() {
        let cases = vec![
            menu_model(&[]),
            menu_model(&[source("hk", vec![])]),
            menu_model(&[source("hk", vec![exit("a", 1080, true, status::STOPPED)])]),
            menu_model(&two_sources()),
        ];
        for model in cases {
            assert_ne!(model.first(), Some(&Node::Separator));
            assert_ne!(model.last(), Some(&Node::Separator));
            for pair in model.windows(2) {
                assert!(
                    !(pair[0] == Node::Separator && pair[1] == Node::Separator),
                    "分隔線黏在一起：{model:?}"
                );
            }
        }
    }
}
