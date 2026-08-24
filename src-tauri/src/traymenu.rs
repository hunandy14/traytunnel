//! 系統匣右鍵選單：先把「狀態快照 → 選單模型」算成純資料（`menu_model`），
//! 再把模型轉成 Tauri 的原生選單物件貼到系統匣上。
//!
//! 分成兩段是為了讓版面規則（攤平與否、狀態行文字、Connect／Disconnect all 的字）
//! 可以單獨測試，不必生出一個真的 app。
//!
//! 更新策略：選單很小，任何影響它的狀態一變就整個重建再換上去，
//! 不做逐項增刪，也不輪詢。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Wry};

use crate::state::{status, ExitView, SourceView, WgProxyView, TRAY_ID};

/// 選單項目的 id，事件端靠這些字串路由
pub const ID_STATUS: &str = "status";
pub const ID_OPEN: &str = "open";
pub const ID_EXIT: &str = "exit";
pub const ID_ALL_TOGGLE: &str = "all-toggle";
pub const ID_RECONNECT_ALL: &str = "reconnect-all";
/// 單一出口的開關，後面接本地埠
pub const EXIT_PREFIX: &str = "exit:";
/// 單一源的重接，後面接源名
pub const SRC_RECONNECT_PREFIX: &str = "src-reconnect:";
/// 單一 wg 連線的重接，後面接**連線名**——wg 連線沒有代表性的埠（§5.2）。
/// 名稱與 ssh 源共用命名空間且已保證不撞名，前綴不同也已足以分流
pub const WG_RECONNECT_PREFIX: &str = "wg-reconnect:";

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
    Item {
        id: String,
        label: String,
    },
    /// 出口開關，勾選＝設定裡的 enabled
    Check {
        id: String,
        label: String,
        checked: bool,
    },
    /// 一個源一個子選單
    Submenu {
        label: String,
        items: Vec<Node>,
    },
}

fn item(id: impl Into<String>, label: impl Into<String>) -> Node {
    Node::Item { id: id.into(), label: label.into() }
}

/// 每一條列，ssh 的與 wg 的攤平成一串。
///
/// 分子分母都要涵蓋兩型連線——使用者看到的 `3/5 Connected` 講的是全部隧道，
/// 少算一半的話 hover 與畫面會對不起來。
fn all_exits<'a>(
    sources: &'a [SourceView],
    wg: &'a [WgProxyView],
) -> impl Iterator<Item = &'a ExitView> {
    sources.iter().flat_map(|s| s.exits.iter()).chain(wg.iter().flat_map(|p| p.exits.iter()))
}

/// 連線數與出口總數，跨源彙總。提示文字與狀態行共用這一組分數，
/// hover 與右鍵才不會給出兩個不一樣的分母。
fn totals(sources: &[SourceView], wg: &[WgProxyView]) -> (usize, usize) {
    let connected = all_exits(sources, wg).filter(|e| e.status == status::CONNECTED).count();
    (connected, all_exits(sources, wg).count())
}

/// 狀態行：連線數／出口總數，跟主視窗的彙總列同一套算法
fn status_line(sources: &[SourceView], wg: &[WgProxyView]) -> String {
    if sources.is_empty() && wg.is_empty() {
        return "No connections".into();
    }
    let (connected, total) = totals(sources, wg);
    if total == 0 {
        return "No tunnels".into();
    }
    format!("{connected}/{total} Connected")
}

/// 系統匣的 hover 提示，分數與狀態行同一份
fn tooltip_text(sources: &[SourceView], wg: &[WgProxyView]) -> String {
    let (connected, total) = totals(sources, wg);
    if total == 0 {
        "Traytunnel - no tunnels".to_string()
    } else {
        format!("Traytunnel - {connected}/{total} connected")
    }
}

/// 「有效意圖」的列：連線也 enabled、列自己也 enabled，兩者 AND 起來才算——
/// 與 `Config::enabled_locals()` 同一套判準（W6.12 起兩型連線都有連線層總開關）。
///
/// `toggle_label` 靠它跟 `lib.rs` 的 `toggle_all`（判向用的正是
/// `enabled_locals().is_empty()`）對齊：只看列的 enabled 會讓標籤與動作方向
/// 反過來——來源關著、列卻還是 enabled=true 時，標籤誤判成「還有東西開著」
/// 顯示 Disconnect all，點下去卻因為 `enabled_locals()` 是空的而往
/// `set_all_enabled(true)` 那個方向走，變成「按下 Disconnect all 卻全部連上」。
fn effectively_enabled_exits<'a>(
    sources: &'a [SourceView],
    wg: &'a [WgProxyView],
) -> impl Iterator<Item = &'a ExitView> {
    sources
        .iter()
        .filter(|s| s.enabled)
        .flat_map(|s| s.exits.iter())
        .chain(wg.iter().filter(|p| p.enabled).flat_map(|p| p.exits.iter()))
}

/// 有任何「有效意圖」的出口就給 Disconnect all，全停（或連線層總開關關著）時給 Connect all
fn toggle_label(sources: &[SourceView], wg: &[WgProxyView]) -> &'static str {
    if effectively_enabled_exits(sources, wg).any(|e| e.enabled) {
        "Disconnect all"
    } else {
        "Connect all"
    }
}

/// 一條列的選單標籤。`socks` 列沒有目的地可寫，改標示它提供的是什麼（§5.6）
fn exit_label(exit: &ExitView) -> String {
    let head = format!("{} ({})", exit.name, exit.local);
    match (exit.kind.as_str(), exit.remote.as_deref()) {
        ("socks", _) => format!("{head}  SOCKS5"),
        (_, Some(remote)) => format!("{head} → {remote}"),
        (_, None) => head,
    }
}

fn exit_node(exit: &ExitView) -> Node {
    Node::Check {
        id: format!("{EXIT_PREFIX}{}", exit.local),
        label: format!("{} ({})", exit.name, exit.local),
        checked: exit.enabled,
    }
}

/// wg 的列標籤多帶目的地／SOCKS5 標示：wg 的列可能是兩種機制，
/// 光看名字與埠分不出這一條到底是自建代理還是轉發
fn wg_exit_node(exit: &ExitView) -> Node {
    Node::Check {
        id: format!("{EXIT_PREFIX}{}", exit.local),
        label: exit_label(exit),
        checked: exit.enabled,
    }
}

/// 一條 wg 連線一個子選單：各列 + 分隔線 + Reconnect。
///
/// 列的順序沿用 `WgProxyView.exits`（`socks` 已置頂，§5.3），系統匣與主視窗
/// 因此排出同一種順序。系統匣**不畫區段標題**——選單太小放不下，
/// 順序本身加上標籤已經足以分辨。
fn wg_node(proxy: &WgProxyView) -> Node {
    let mut items: Vec<Node> = proxy.exits.iter().map(wg_exit_node).collect();
    if !items.is_empty() {
        items.push(Node::Separator);
    }
    items.push(item(format!("{WG_RECONNECT_PREFIX}{}", proxy.name), "Reconnect"));
    Node::Submenu { label: proxy.name.clone(), items }
}

/// 一個源一個子選單：出口 + 分隔線 + Reconnect。
/// 源底下沒出口時不放那條分隔線，免得子選單開頭空一格。
fn source_node(src: &SourceView) -> Node {
    let mut items: Vec<Node> = src.exits.iter().map(exit_node).collect();
    if !items.is_empty() {
        items.push(Node::Separator);
    }
    items.push(item(format!("{SRC_RECONNECT_PREFIX}{}", src.name), "Reconnect"));
    Node::Submenu { label: src.name.clone(), items }
}

/// 單一源、出口不多、**而且一條 wg 連線都沒有**時才攤平。
///
/// 有 wg 連線就一定要有子選單：wg 的 Reconnect 只存在於子選單裡，攤平會讓它
/// 整個消失（§5.6）。
fn flattened(sources: &[SourceView], wg: &[WgProxyView]) -> bool {
    wg.is_empty() && matches!(sources, [only] if only.exits.len() < FLATTEN_LIMIT)
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
pub fn menu_model(sources: &[SourceView], wg: &[WgProxyView]) -> Vec<Node> {
    let mut sections = vec![vec![Node::Status(status_line(sources, wg))]];
    if !sources.is_empty() || !wg.is_empty() {
        sections.push(if flattened(sources, wg) {
            sources[0].exits.iter().map(exit_node).collect()
        } else {
            sources.iter().map(source_node).chain(wg.iter().map(wg_node)).collect()
        });
        sections.push(vec![
            item(ID_ALL_TOGGLE, toggle_label(sources, wg)),
            item(ID_RECONNECT_ALL, "Reconnect all"),
        ]);
    }
    sections.push(vec![item(ID_OPEN, "Open window"), item(ID_EXIT, "Exit")]);
    join(sections)
}

/// 一次要換上系統匣的全部東西。提示與選單出自同一份快照，也一起套用，
/// 免得連線風暴時兩者各跑各的、還各自來回主執行緒一趟。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayView {
    pub tooltip: String,
    pub menu: Vec<Node>,
}

pub fn tray_view(sources: &[SourceView], wg: &[WgProxyView]) -> TrayView {
    TrayView { tooltip: tooltip_text(sources, wg), menu: menu_model(sources, wg) }
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

/// 配一張套用號碼牌。
///
/// 呼叫端必須在**取快照的那把鎖還持著時**配號（見 `AppState::views_with_seq`）：
/// 只要中間放掉鎖，兩條執行緒就能在「取完快照、還沒配號」的空檔交錯，
/// 讓其中一份拿到較大的號碼卻載著較舊的快照，晚到的舊狀態反而蓋掉新的，
/// 系統匣就停在過期的樣子直到下一次狀態變化。
pub fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::SeqCst) + 1
}

/// 重算整份提示與選單並換上系統匣，`seq` 是這份快照的號碼牌。
///
/// 模型在呼叫端的執行緒上就算好（純函式，不碰鎖也不碰 tray），真正碰系統匣的
/// 動作丟到背景執行：選單事件是在主執行緒上處理的，若在事件處理途中同步把選單
/// 換掉，等於在自己的回呼裡抽掉正在用的那一份。
pub fn refresh(app: &AppHandle, sources: &[SourceView], wg: &[WgProxyView], seq: u64) {
    let view = tray_view(sources, wg);
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = APPLY.lock().unwrap_or_else(|e| e.into_inner());
        // 已經有更新的快照排在後面，這一份貼上去只會讓畫面倒退
        if SEQ.load(Ordering::SeqCst) != seq {
            return;
        }
        if let Err(e) = apply(&app, &view) {
            log::warn!("could not refresh the tray: {e}");
        }
    });
}

fn apply(app: &AppHandle, view: &TrayView) -> tauri::Result<()> {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(()); // 系統匣還沒長出來（或已經收掉），沒什麼好更新的
    };
    tray.set_tooltip(Some(&view.tooltip))?;
    tray.set_menu(Some(build(app, &view.menu)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `menu_model`／`tray_view`／`status_line`／`tooltip_text`／`toggle_label`
    /// 的簽名依 §5.6 都多了一個 `&[WgProxyView]`。既有這一組測試講的是「只有
    /// ssh 源時的版面」，**斷言一個字都沒改**，只是由這幾支薄墊片補上空陣列；
    /// wg 那半邊由下面的專屬測試覆蓋。
    fn menu_model(sources: &[SourceView]) -> Vec<Node> {
        super::menu_model(sources, &[])
    }
    fn tray_view(sources: &[SourceView]) -> TrayView {
        super::tray_view(sources, &[])
    }
    fn status_line(sources: &[SourceView]) -> String {
        super::status_line(sources, &[])
    }
    fn tooltip_text(sources: &[SourceView]) -> String {
        super::tooltip_text(sources, &[])
    }
    fn toggle_label(sources: &[SourceView]) -> &'static str {
        super::toggle_label(sources, &[])
    }

    fn exit(name: &str, local: u16, enabled: bool, st: &str) -> ExitView {
        ExitView {
            name: name.into(),
            local,
            remote: Some(format!("127.0.0.1:{local}")),
            kind: "forward".into(),
            probe_proxy: false,
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
            enabled: true,
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
                        item("src-reconnect:hk", "Reconnect"),
                    ],
                },
                Node::Submenu {
                    label: "tw".into(),
                    items: vec![
                        check("exit:1090", "c (1090)", true),
                        Node::Separator,
                        item("src-reconnect:tw", "Reconnect"),
                    ],
                },
                Node::Separator,
                item("all-toggle", "Disconnect all"),
                item("reconnect-all", "Reconnect all"),
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

    /// hover 提示與右鍵狀態行必須是同一個分數，分母都是全部出口（含停用的），
    /// 否則滑過去看到的與按下去看到的會互相矛盾
    #[test]
    fn tooltip_and_status_line_share_one_score() {
        let mut sources = two_sources();
        assert_eq!(tooltip_text(&sources), "Traytunnel - 2/3 connected");
        assert_eq!(status_line(&sources), "2/3 Connected");
        // 停用一個出口不會讓分母縮水
        sources[0].exits[0].enabled = false;
        assert_eq!(tooltip_text(&sources), "Traytunnel - 2/3 connected");
        assert_eq!(status_line(&sources), "2/3 Connected");
        // 兩邊永遠報同一組數字
        for s in sources.iter_mut() {
            for e in s.exits.iter_mut() {
                e.status = status::STOPPED.into();
            }
        }
        assert_eq!(tooltip_text(&sources), "Traytunnel - 0/3 connected");
        assert_eq!(status_line(&sources), "0/3 Connected");
    }

    /// 一條隧道都沒有時不報 0/0
    #[test]
    fn tooltip_says_no_tunnels_when_there_are_none() {
        assert_eq!(tooltip_text(&[]), "Traytunnel - no tunnels");
        assert_eq!(tooltip_text(&[source("hk", vec![])]), "Traytunnel - no tunnels");
    }

    /// 提示與選單出自同一份快照，才能一起套用
    #[test]
    fn tray_view_carries_both_halves() {
        let sources = two_sources();
        let view = tray_view(&sources);
        assert_eq!(view.tooltip, tooltip_text(&sources));
        assert_eq!(view.menu, menu_model(&sources));
    }

    /// 單源且出口不多：出口直接放根層，沒有子選單也沒有 Reconnect
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
                item("all-toggle", "Disconnect all"),
                item("reconnect-all", "Reconnect all"),
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
        assert_eq!(items.len(), FLATTEN_LIMIT + 2); // 出口 + 分隔線 + Reconnect
        assert_eq!(items.last(), Some(&item("src-reconnect:hk", "Reconnect")));
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

    /// 零連線：只有狀態行與視窗動作，連 Connect all／Reconnect all 都不給
    #[test]
    fn no_connections_shows_only_the_status_line_and_window_actions() {
        assert_eq!(
            menu_model(&[]),
            vec![
                Node::Status("No connections".into()),
                Node::Separator,
                item("open", "Open window"),
                item("exit", "Exit"),
            ]
        );
    }

    /// 有連線沒隧道：狀態行講 No tunnels，全域動作仍在（Connect all 會是無事發生但不必藏）
    #[test]
    fn a_connection_without_tunnels_says_no_tunnels() {
        let model = menu_model(&[source("hk", vec![])]);
        assert_eq!(model[0], Node::Status("No tunnels".into()));
        // 不會冒出兩條連在一起的分隔線
        assert_eq!(model[1], Node::Separator);
        assert_eq!(model[2], item("all-toggle", "Connect all"));
    }

    /// 全部停用時要顯示 Connect all，只要還有一個 enabled 就是 Disconnect all
    #[test]
    fn toggle_reads_start_all_only_when_everything_is_disabled() {
        let mut sources = two_sources();
        assert_eq!(toggle_label(&sources), "Disconnect all");
        sources[1].exits[0].enabled = false;
        assert_eq!(toggle_label(&sources), "Disconnect all"); // hk 的 a 還開著
        sources[0].exits[0].enabled = false;
        assert_eq!(toggle_label(&sources), "Connect all");
        assert_eq!(menu_model(&sources)[5], item("all-toggle", "Connect all"));
    }

    /// 阻擋缺陷守衛（PR #44 覆審）：來源被總開關關掉，但底下的列全部還是
    /// enabled=true——標籤要跟 `lib.rs::toggle_all` 判向用的
    /// `enabled_locals().is_empty()` 同一個方向：Connect all。只看列的
    /// enabled（不看來源總開關）會讓標籤說 Disconnect all，點下去卻因為
    /// `enabled_locals()` 是空的而往 `set_all_enabled(true)` 走，方向整個反過來。
    #[test]
    fn a_source_switched_off_reads_as_connect_all_even_with_every_row_enabled() {
        let mut sources = two_sources();
        for s in sources.iter_mut() {
            for e in s.exits.iter_mut() {
                e.enabled = true;
            }
        }
        assert_eq!(toggle_label(&sources), "Disconnect all", "前提：列全開時本來就是 Disconnect all");

        for s in sources.iter_mut() {
            s.enabled = false;
        }
        assert_eq!(
            toggle_label(&sources),
            "Connect all",
            "來源總開關都關著，即使列自己還是 enabled=true，標籤也要跟 \
             enabled_locals() 一樣讀成空的方向"
        );
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
