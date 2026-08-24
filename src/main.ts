import { bootstrap } from "./bootstrap";
import { installContextMenuGuard } from "./context-menu";
import { el, h, setToggle } from "./dom";
import { hydrateIcons, icon, setIcon } from "./icons";
import {
  deleteForward,
  getState,
  onConfigChanged,
  onExitStatus,
  onExitTest,
  onLog,
  onUpdateAvailable,
  restartExit,
  setWgEnabled,
  startExit,
  startSource,
  stopExit,
  stopSource,
  windowClose,
  windowMinimize,
} from "./ipc";
import {
  closeSocksSheet,
  closeTunnelSheet,
  initSettingsPage,
  initSocksSheet,
  initSourceSheet,
  initTunnelSheet,
  openSocksSheet,
  openSourceSheet,
  openTunnelSheet,
  syncSettingsPage,
} from "./sheet";
import {
  connStatusText,
  connTone,
  defaultDetail,
  isBad,
  isRunning,
  shouldProbe,
  splitTest,
  statusTone,
  testLine,
  type Tone,
} from "./status";
import { showErrorToast, showUndoToast, type UndoToast } from "./toast";
import type {
  ConnRef,
  ExitInfo,
  ExitStatusEvent,
  ExitTestEvent,
  RowKind,
  Snapshot,
  SourceInfo,
  WgProxyInfo,
} from "./types";
import { basename } from "./util";

const EMPTY: Snapshot = {
  closeToTray: true,
  autostart: false,
  checkForUpdates: true,
  sources: [],
  wgProxies: [],
  logs: [],
  update: null,
};

let snap: Snapshot = EMPTY;

/** 目前選中的連線名稱（ssh 源或 wg 連線，兩者共用同一個命名空間）；null 代表還沒有連線 */
let selected: string | null = null;

/**
 * 剛送出 upsert_source／upsert_wg_proxy、還在等 config-changed 把這個名字帶回來。
 *
 * 真後端的事件順序是 invoke 先 resolve、config-changed 才到，所以存檔當下快照裡
 * 還是舊名字。這段期間不能讓 render() 的「選中不存在就回退第一個」把改名後的
 * 選中吃掉，得等快照真的出現這個名字才切過去。
 */
let pendingSelect: string | null = null;

type View = "source" | "log" | "settings";
let view: View = "source";

/** 完整日誌都留在記憶體，切連線／切頁時重畫 */
let logLines: string[] = [];
const LOG_CAP = 500;

/** 已按下刪除、但 undo 倒數還沒結束的列：畫面先當它不存在 */
const pendingDelete = new Set<number>();
/**
 * 還在倒數的 undo toast，整條連線被刪掉時要能把底下列的倒數一起收乾淨。
 * 一併記下當初的連線名稱：刪連線時快照可能已經被 config-changed 更新過了，
 * 不能靠當下的 snap 反查列屬於誰。
 */
const undoToasts = new Map<number, { connection: string; toast: UndoToast }>();

interface CardRefs {
  root: HTMLElement;
  dot: HTMLElement;
  badge: HTMLElement | null;
  test: HTMLElement | null;
  detail: HTMLElement;
  toggle: HTMLButtonElement;
}

const cardRefs = new Map<number, CardRefs>();
/** 側欄每個連線 icon 的狀態小點，讓 exit-status 事件不用整列重建就能更新 */
const railStatusRefs = new Map<string, HTMLElement>();

// ---------------------------------------------------------------- 連線抽象（SSH／WG 共用）

/**
 * Snapshot → ConnRef 的映射只在這兩支裡發生（型別本體見 types.ts）：
 * 兩種連線的併排順序與欄位對應寫一次，其餘地方一律拿現成的 ConnRef。
 */
const sshRef = (s: SourceInfo): ConnRef => ({ kind: "ssh", name: s.name, exits: s.exits, data: s });
const wgRef = (p: WgProxyInfo): ConnRef => ({ kind: "wg", name: p.name, exits: p.exits, data: p });

function allConns(): ConnRef[] {
  return [...snap.sources.map(sshRef), ...snap.wgProxies.map(wgRef)];
}

/**
 * 單筆查找**不要**先 materialize 整個 allConns() 陣列再 find——那等於為了拿
 * 一條連線把每一條都包裝一次。直接掃原始快照，只包裝命中的那一筆。
 * 這支被 paintRailStatus 之類的密集路徑呼叫，白做的包裝會實際累積起來。
 */
function findConn(name: string): ConnRef | null {
  const src = snap.sources.find((s) => s.name === name);
  if (src) return sshRef(src);
  const wg = snap.wgProxies.find((p) => p.name === name);
  return wg ? wgRef(wg) : null;
}

function currentConn(): ConnRef | null {
  return selected ? findConn(selected) : null;
}

const visibleExits = (conn: ConnRef | null) =>
  (conn?.exits ?? []).filter((e) => !pendingDelete.has(e.local));

/**
 * local 全域唯一，所以列一律用埠號跨連線找，順便把它所屬的連線帶回來。
 *
 * 回傳的是整個 ConnRef 而不只是名字：呼叫端（applyExitStatus／applyExitTest）
 * 拿到之後多半還要算連線層的彙總或重畫左軌小點，只給名字的話它們得再用那個
 * 名字回頭查一次、甚至兩次，而連線這時候明明就在手上。
 */
function locate(local: number): { exit: ExitInfo; conn: ConnRef } | undefined {
  for (const source of snap.sources) {
    const exit = source.exits.find((e) => e.local === local);
    if (exit) return { exit, conn: sshRef(source) };
  }
  for (const proxy of snap.wgProxies) {
    const exit = proxy.exits.find((e) => e.local === local);
    if (exit) return { exit, conn: wgRef(proxy) };
  }
  return undefined;
}

/** 指令送出後統一的失敗處理：後端拒絕就跳錯誤 toast，不要無聲吞掉 */
async function run(action: () => Promise<unknown>, what: string) {
  try {
    await action();
  } catch (e) {
    showErrorToast(`Could not ${what}: ${String(e)}`);
  }
}

// ---------------------------------------------------------------- 左側連線軌道

/** 名稱 hash → 色相，同一個 ssh 源在每次啟動都拿到同一個顏色 */
function hashHue(name: string): number {
  let acc = 0;
  for (let i = 0; i < name.length; i++) acc = (acc * 31 + name.charCodeAt(i)) >>> 0;
  return acc % 360;
}

function initial(name: string): string {
  const ch = name.trim().charAt(0);
  return ch ? ch.toUpperCase() : "?";
}

/**
 * 只重畫某個連線 icon 的狀態小點，不動整列（避免密集事件下重建 DOM 與丟焦點）。
 * 吃現成的 ConnRef：呼叫端手上都已經有了，再拿名字回查一次就白費了這條
 * 「便宜路徑」的用意。
 */
function paintRailStatus(conn: ConnRef) {
  const node = railStatusRefs.get(conn.name);
  if (!node) return;
  const exits = visibleExits(conn);
  node.className = `src-status tone-${connTone(conn, exits)}`;
  node.title = connStatusText(conn, exits);
}

function renderRail(conns: ConnRef[]) {
  const list = el<HTMLDivElement>("rail-list");
  list.textContent = "";
  railStatusRefs.clear();

  for (const conn of conns) {
    const name = conn.name;
    let btn: HTMLButtonElement;
    if (conn.kind === "ssh") {
      const hue = hashHue(name);
      btn = h("button", { class: "src-icon", text: initial(name) });
      btn.style.setProperty("--src-bg", `hsl(${hue} 34% 34%)`);
      btn.style.setProperty("--src-ink", `hsl(${hue} 70% 86%)`);
    } else {
      // WG 節點固定用 accent 品牌色 + "WG" 兩字，與 ssh 的雜湊色首字並列（Q10）
      btn = h("button", { class: "src-icon type-wg", text: "WG" });
    }
    // tooltip 與 summary 副標同源，兩處對「這條連線是什麼」不會給出不同答案
    btn.title = `${name} — ${summarySubText(conn)}`;
    btn.classList.toggle("active", view === "source" && name === selected);
    const status = h("span", { class: "src-status" });
    railStatusRefs.set(name, status);
    btn.appendChild(status);
    paintRailStatus(conn);
    // 閉包只抓名字，不抓整個 ConnRef（軌道按鈕會活到下一次 renderRail）
    btn.addEventListener("click", () => selectConn(name));
    list.appendChild(btn);
  }

  const add = h("button", { class: "rail-btn add", title: "Add connection" }, [icon("plus", 18)]);
  add.addEventListener("click", () => openSourceSheet(null));
  list.appendChild(add);

  el<HTMLButtonElement>("btn-logs").classList.toggle("active", view === "log");
  el<HTMLButtonElement>("btn-settings").classList.toggle("active", view === "settings");
}

// ---------------------------------------------------------------- 視圖切換

function setView(next: View) {
  if (view !== next) {
    closeTunnelSheet();
    closeSocksSheet();
  }
  closeMenu();
  // 使用者自己動了畫面，等快照的旗標就作廢，免得等不到時永遠卡著
  pendingSelect = null;
  view = next;
  render();
}

function selectConn(name: string) {
  // 切到別條連線時，開著的列 sheet 就沒有對象了
  if (selected !== name) {
    closeTunnelSheet();
    closeSocksSheet();
  }
  closeMenu();
  pendingSelect = null;
  selected = name;
  view = "source";
  render();
}

function applyViewVisibility(conns: ConnRef[]) {
  const noConns = conns.length === 0;
  const showEmpty = view === "source" && noConns;
  el<HTMLElement>("view-source").hidden = view !== "source" || noConns;
  el<HTMLElement>("view-empty").hidden = !showEmpty;
  el<HTMLElement>("view-log").hidden = view !== "log";
  el<HTMLElement>("view-settings").hidden = view !== "settings";
}

/**
 * 一次把整個畫面對齊到目前的 snap／selected／view。
 *
 * 連線清單在這裡算一次就往下傳：allConns() 每次呼叫都會重建包裝陣列，
 * 一輪 render 裡問四次等於白做三次，而且中途 snap 不會變，四份結果必然相同。
 */
function render() {
  const before = selected;
  const conns = allConns();

  // 存檔後的名字一旦出現在快照裡就切過去，切完才解除等待
  if (pendingSelect !== null && conns.some((c) => c.name === pendingSelect)) {
    selected = pendingSelect;
    pendingSelect = null;
  }

  // 選中的連線被刪掉或還沒選過，就落回第一個；
  // 但還在等 config-changed 時不回退，否則改名會被打回舊的連線
  let conn = conns.find((c) => c.name === selected) ?? null;
  if (pendingSelect === null && !conn) {
    conn = conns[0] ?? null;
    selected = conn?.name ?? null;
  }

  // ⋯ 選單的每一項都以「選中的那條連線」為對象。外部變更（別的視窗改了設定檔、
  // 連線被刪掉、整份清空）可能在選單開著時把它換掉或抽走，這時要收起來——
  // 否則使用者按下去的動作會打在另一條連線上，或打在不存在的東西上。
  if (menuOpen && (selected !== before || !conn)) closeMenu();

  applyViewVisibility(conns);
  renderRail(conns);
  if (view === "source") {
    renderSummary(conn);
    renderCards(conn);
  }
  renderLogs();
}

// ---------------------------------------------------------------- 頂部彙總列

/**
 * 連線的身分摘要，summary 副標與左軌 tooltip 共用。
 * wg 顯示 endpoint（U4）；.conf 還沒解析出 endpoint 時退回檔名，
 * 至少讓人分得出是哪一份設定。ssh 照舊 "ssh user@host"。
 */
function summarySubText(conn: ConnRef): string {
  if (conn.kind === "ssh") return `ssh ${conn.data.user}@${conn.data.host}`;
  const detail = conn.data.endpoint || basename(conn.data.confPath);
  return detail ? `wg ${detail}` : "wg";
}

function renderSummary(conn: ConnRef | null) {
  const exits = visibleExits(conn);
  const total = exits.length;
  const connected = exits.filter((e) => e.status === "connected").length;
  // 色調與狀態詞都由 status.ts 那一組純函式算，這裡各算一次共用給三個消費點
  const tone: Tone = conn ? connTone(conn, exits) : "grey";
  const statusText = conn ? connStatusText(conn, exits) : "no rows";

  // 左段：連線名稱當主標，身分摘要當副標；WG 專屬的引擎狀態點跟標題並排
  const title = el<HTMLDivElement>("summary-title");
  title.textContent = conn ? conn.name : "No connection";
  title.title = conn ? conn.name : "";

  // .conf 讀不到／解析不過時（WgProxyInfo.confError），副標讓位給錯誤訊息：
  // 這種連線根本起不來，先講清楚為什麼，endpoint 那類摘要這時候也是空的。
  const confError = conn?.kind === "wg" ? conn.data.confError : null;
  const sub = el<HTMLDivElement>("summary-sub");
  sub.textContent = confError ?? (conn ? summarySubText(conn) : "no host configured");
  sub.title = confError ?? "";
  sub.classList.toggle("tone-text-red", Boolean(confError));

  // 連線層的狀態不另外推事件（wg-design.md §5.2）：由 connTone 融合 confError、
  // 引擎旗標與底下各列的彙總算出來，左軌小點與下面的統計分數用的是同一支，
  // 三處不會再各說各話。
  const engineDot = el<HTMLSpanElement>("summary-engine-dot");
  engineDot.hidden = conn?.kind !== "wg";
  engineDot.className = `dot tone-${tone}`;
  // 短狀態詞當常態，conf 壞掉時滑過去要看得到完整原因（副標會被省略號截斷）
  engineDot.title = confError ?? statusText;

  // 總開關綁的是引擎旗標（conn.data.enabled），跟列開關綁 exit.enabled 同一個
  // 語意層級：兩者都是「意圖」，不是即時狀態——後者由狀態點負責。
  const masterToggle = el<HTMLButtonElement>("summary-master-toggle");
  masterToggle.hidden = conn?.kind !== "wg";
  setToggle(masterToggle, conn?.kind === "wg" && conn.data.enabled, TOGGLE_TITLES);

  // 中段：大分數＋小字狀態。分母是看得到的列數（跟畫面上的卡片張數對得起來），
  // 顏色與底下那行小字則跟兩顆狀態點同源——之前小字自己算一套，會出現
  // 分數說「stopped」、引擎點說「idle」互相打架的畫面。
  const num = el<HTMLDivElement>("summary-score");
  num.textContent = !conn || total === 0 ? "—" : `${connected}/${total}`;
  num.className = `summary-score-num tone-${tone}`;
  el<HTMLDivElement>("summary-score-label").textContent = conn ? statusText : "no rows";

  // 右段：⋯ 選單裡的連／斷那一項跟著整條連線的狀態換字。WG 跟的是引擎旗標而不是
  // 列的執行狀態——它按下去呼叫的就是 setWgEnabled，跟旁邊的總開關同一件事，
  // 兩個入口的字面與行為必須一致（引擎開著但列全部停用時，那一項要說 Disconnect）。
  const toggleOn =
    conn?.kind === "wg" ? conn.data.enabled : exits.some(isRunning);
  setIcon(el<HTMLSpanElement>("menu-toggle-ico"), toggleOn ? "square" : "play", 14);
  el<HTMLSpanElement>("menu-toggle-text").textContent = toggleOn ? "Disconnect" : "Connect";
  const toggleItem = el<HTMLButtonElement>("menu-toggle-source");
  toggleItem.classList.toggle("danger", toggleOn);
  toggleItem.classList.toggle("go", !toggleOn);

  // WG 專屬的「新增代理」選單項，SSH 連線不顯示
  el<HTMLButtonElement>("menu-add-socks").hidden = conn?.kind !== "wg";
}

// ---------------------------------------------------------------- ⋯ 選單

let menuOpen = false;

function setMenuOpen(on: boolean) {
  // 沒有選中的連線時整組動作都沒有對象，乾脆不讓它開
  if (on && !currentConn()) return;
  menuOpen = on;
  el<HTMLDivElement>("summary-menu").hidden = !on;
  el<HTMLButtonElement>("btn-more").setAttribute("aria-expanded", String(on));
}

function closeMenu() {
  if (menuOpen) setMenuOpen(false);
}

/** 選單項一律「先收選單、再執行動作」，免得動作換頁後選單還飄在上面 */
function menuItem(id: string, action: () => void) {
  el<HTMLButtonElement>(id).addEventListener("click", () => {
    closeMenu();
    action();
  });
}

/**
 * WG 連線的總開關與 ⋯ 選單的連斷動作，兩個入口共用這一支。
 *
 * 走連線層級的 set_wg_enabled（wg-design.md §5.5 第 3 支），**不是**對底下每
 * 一條列各送一次 start_exit／stop_exit：後者不會寫到 wgProxies[name].enabled，
 * 而畫面上的總開關與「卡片變暗」讀的正是那個旗標——新建的 WG 連線
 * （enabled = false）會因此永遠變暗、列開關永遠停用、總開關永遠推不動。
 *
 * 語意也不同：逐列迴圈會連帶輾平每一條列自己的 enabled 意圖，而引擎總開關
 * 關掉時列的意圖要原封不動，重新打開時只起原本就啟用的那幾條。
 */
/**
 * 已經送出 set_wg_enabled、還在等回應的連線。
 *
 * 那顆開關按下去到 config-changed 回來之間，畫面上的旗標還是舊值——連按兩下
 * 就會依同一個舊值算出同一個 next，送出兩次一模一樣的指令；更糟的是「開→關」
 * 這種一心二意的連點，最後生效的是哪一個完全看回應順序碰運氣。
 */
const wgEnginePending = new Set<string>();

function toggleWgEngine(conn: ConnRef & { kind: "wg" }) {
  if (wgEnginePending.has(conn.name)) return;
  const next = !conn.data.enabled;
  // 樂觀更新：先把開關撥過去，使用者才不會覺得按了沒反應。
  setToggle(el<HTMLButtonElement>("summary-master-toggle"), next, TOGGLE_TITLES);
  wgEnginePending.add(conn.name);
  void run(
    () => setWgEnabled(conn.name, next),
    `${next ? "connect" : "disconnect"} ${conn.name}`,
  ).finally(() => {
    wgEnginePending.delete(conn.name);
    // 收尾時照快照重畫一次，把上面那個樂觀的猜測校正回事實。
    // 多數情況 config-changed 早就到了、這一次 render 只是冪等的重畫；
    // 但後端**可以合法地拒絕**（例如 .conf 解析不過的連線根本起不來，
    // 那時它不改旗標也不推 config-changed），少了這一手，開關就會停在
    // 一個永遠不會被修正的錯誤位置。
    render();
  });
}

/**
 * WG 連線的「重新連線」：關掉引擎再打開，而不是對底下每一條列送 restart_exit。
 *
 * 逐列 restart 有兩個問題：引擎關著的時候一條列都不在跑，整個動作會靜靜地什麼
 * 都不做；而且 restart_exit 重啟的是列的監聽器，**引擎自己從頭到尾沒有動過**，
 * 使用者改了 .conf 之後按重新連線，那份 conf 永遠不會被重讀。走引擎級的
 * off→on 才是真的重啟（順帶重讀 conf），也才對得起選單上那個字。
 */
function reconnectWgEngine(conn: ConnRef & { kind: "wg" }) {
  if (wgEnginePending.has(conn.name)) return;
  wgEnginePending.add(conn.name);
  void run(async () => {
    await setWgEnabled(conn.name, false);
    await setWgEnabled(conn.name, true);
  }, `reconnect ${conn.name}`).finally(() => wgEnginePending.delete(conn.name));
}

function initSummaryMenu() {
  el<HTMLButtonElement>("btn-more").addEventListener("click", (e) => {
    e.stopPropagation();
    setMenuOpen(!menuOpen);
  });

  // 點到選單以外的任何地方就關；用 mousedown 才不會被按鈕自己的 click 蓋掉
  document.addEventListener("mousedown", (e) => {
    if (!menuOpen) return;
    const target = e.target;
    if (target instanceof Element && target.closest(".summary-more-wrap")) return;
    closeMenu();
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && menuOpen) {
      e.stopPropagation();
      closeMenu();
    }
  });

  menuItem("menu-add-exit", beginCreateForward);
  menuItem("menu-add-socks", beginCreateSocks);
  menuItem("menu-toggle-source", () => {
    const conn = currentConn();
    if (!conn) return;
    if (conn.kind === "wg") {
      toggleWgEngine(conn);
      return;
    }
    // SSH 沒有「連線」這個執行實體，連斷只能靠 start_source／stop_source
    // 逐條掃過去（那一層在後端，前端不重複做一次迴圈）
    if (visibleExits(conn).some(isRunning)) void run(() => stopSource(conn.name), `stop ${conn.name}`);
    else void run(() => startSource(conn.name), `start ${conn.name}`);
  });
  menuItem("menu-reconnect-source", () => {
    const conn = currentConn();
    if (!conn) return;
    // WG 有一顆真的引擎可以重啟，走引擎級的 off→on（順帶重讀 conf）。
    // 引擎關著就什麼都不做——「重新連線」不該把一個使用者刻意停掉的連線
    // 擅自開起來，這一點與下面 SSH 分支「只重接執行中的列」是同一個原則。
    if (conn.kind === "wg") {
      if (conn.data.enabled) reconnectWgEngine(conn);
      return;
    }
    // SSH 一個出口就是一條 ssh 程序，只能逐條重接；
    // 只重接目前連線中的列，停用中的維持停用，不拉起來
    for (const exit of visibleExits(conn).filter(isRunning)) {
      void run(() => restartExit(exit.local), `reconnect ${exit.name}`);
    }
  });
  menuItem("menu-activity", () => setView("log"));
  menuItem("menu-edit-source", () => {
    // ConnRef 就是 ConnTarget 多帶兩個便利欄位，直接餵進去即可
    const conn = currentConn();
    if (conn) openSourceSheet(conn);
  });
}

// ---------------------------------------------------------------- 列卡片

function routeText(exit: ExitInfo): string {
  return exit.remote ? `:${exit.local} → ${exit.remote}` : `:${exit.local}`;
}

/**
 * 勾了「目的地是代理」卻**確定**識別不出協定時的說明，徽章與檢測字兩處共用同
 * 一句：兩邊講的是同一件事，字面沒有理由分兩份維護。
 *
 * 「確定」兩個字是重點——見 badgeLook 對三種情境的區分。
 */
const NOT_A_PROXY_HINT = "Confirm the destination is a proxy, or turn the flag off";

/** 列開關與連線總開關共用的 tooltip 字面（[開著時，關著時]） */
const TOGGLE_TITLES = ["Disconnect", "Connect"] as const;

/**
 * 協定識別記憶的**唯一**寫入點，刻意放在 paintCard 的第一行、早退之前。
 *
 * 自測結果（lastTest）只在 connected 時有效，一斷線就整筆清掉；但「這個目的地
 * 是不是代理、是哪一種」不會因為連線斷一下就改變。沒有這層記憶的話，徽章會
 * 跟著每一次停止／重測在「SOCKS5」與「PROXY?」之間來回閃，而 PROXY? 還掛著
 * 一句指責使用者設定錯誤的 tooltip——那條列明明剛剛才成功識別過。
 *
 * 為什麼一個寫入點就夠：帶著 protocol 的自測結果一定先經過 applyExitTest，
 * 而它結尾必定呼叫 paintCard；等到之後 exit-status 把 lastTest 清掉時，
 * 記憶早就存下來了。放在早退之前是關鍵——沒被選中的連線畫不出卡片，
 * 但它底下的列同樣會收到自測事件，記憶不能因為「現在沒在看」就漏掉。
 * （開機第一份快照沒有事件可收，由 applySnapshot 的 carry 直接播種。）
 */
function rememberProtocol(exit: ExitInfo) {
  const seen = exit.lastTest?.protocol;
  if (seen) exit.knownProtocol = seen;
}

/**
 * 徽章的三態，關鍵在於把「還不知道」與「確定不是」分開：
 *
 *   已知協定（含記憶）  SOCKS5／HTTP，accent 樣式，沒有 tooltip
 *   確定探測失敗        PROXY?，淡樣式，掛 NOT_A_PROXY_HINT——只有在真的測過
 *                       且測出「不像代理」之後才說這句話
 *   還沒測／測試中      PROXY，淡樣式，不掛 tooltip。中性陳述「這條列被標成
 *                       代理」，不對還沒發生的事下結論、更不指責使用者
 *
 * `hint` 是「該不該說那句話」的**唯一**判準，徽章與底下的檢測行都引用它，
 * 不各自再算一次——兩處講的是同一件事，判準分兩份遲早會分岔。
 */
function badgeLook(exit: ExitInfo): { text: string; accent: boolean; hint: boolean } {
  if (exit.kind === "socks") return { text: "SOCKS5", accent: true, hint: false };
  const known = exit.knownProtocol;
  if (known) return { text: known.toUpperCase(), accent: true, hint: false };
  if (exit.lastTest?.state === "fail") return { text: "PROXY?", accent: false, hint: true };
  return { text: "PROXY", accent: false, hint: false };
}

function paintCard(exit: ExitInfo) {
  // 早退之前：這是協定記憶的同步點，跟卡片畫不畫得出來無關
  rememberProtocol(exit);

  const refs = cardRefs.get(exit.local);
  if (!refs) return;
  const look = badgeLook(exit);

  refs.dot.className = `dot tone-${statusTone(exit.status)}`;
  refs.dot.title = exit.status;

  if (refs.badge) {
    refs.badge.textContent = look.text;
    refs.badge.className = `type-badge ${look.accent ? "wg" : "ssh"}`;
    if (look.hint) refs.badge.title = NOT_A_PROXY_HINT;
    else refs.badge.removeAttribute("title");
  }

  if (refs.test) {
    const t = testLine(exit);
    const two = t.tone === "text" ? splitTest(t.text) : null;
    refs.test.textContent = "";
    if (two) {
      refs.test.className = "card-test two-line";
      refs.test.title = t.text;
      refs.test.appendChild(h("div", { class: "card-test-place", text: two.place }));
      refs.test.appendChild(h("div", { class: "card-test-ip mono", text: two.ip }));
    } else {
      // 直接引用徽章算好的 hint，不自己再判一次：曾經識別出協定的列偶爾測
      // 失敗一次，那是連線問題而不是使用者把旗標設錯了，兩處必須同一個答案
      refs.test.className = `card-test tone-text-${t.tone}`;
      refs.test.title = look.hint ? NOT_A_PROXY_HINT : "";
      refs.test.textContent = t.text;
    }
  }

  const detail = isBad(exit) ? (exit.detailText ?? defaultDetail(exit.status)) : "";
  refs.detail.textContent = detail;
  refs.detail.title = detail;
  refs.detail.classList.toggle("show", Boolean(detail));

  // switch 表達「意圖」（exit.enabled，跟系統匣勾選同一個依據），
  // 不是連線的即時狀態——那是上面的狀態點在管，兩者刻意分開顯示。
  setToggle(refs.toggle, exit.enabled, TOGGLE_TITLES);
}

function buildCard(exit: ExitInfo, conn: ConnRef, dimmed: boolean): HTMLElement {
  const dot = h("span", { class: "dot" });

  // 檢測行：舊後端的列被 normalizeSnapshot 補成 probeProxy=true，維持 PR 之前
  // 無條件顯示出口 IP 的行為
  const showTest = shouldProbe(exit);
  // 徽章：舊後端那個 true 是為了相容假設出來的，不足以拿來宣稱「這條列是代理」。
  // PR 之前這個位置本來就沒有徽章，憑空長出一排 PROXY／PROXY? 只會讓人以為設定
  // 被改過。後端補上 kind／probeProxy 後 legacy 不再成立，徽章自然回來。
  const showBadge = !exit.legacy && showTest;
  let badge: HTMLElement | null = null;
  let nameEl: HTMLElement;
  if (showBadge) {
    badge = h("span", { class: "type-badge" });
    nameEl = h("div", { class: "card-name-row" }, [
      h("span", { class: "card-name", text: exit.name }),
      badge,
    ]);
  } else {
    nameEl = h("div", { class: "card-name", text: exit.name });
  }
  const route = h("div", { class: "card-route", text: routeText(exit) });
  const id = h("div", { class: "card-id" }, [nameEl, route]);

  // 沒勾「目的地是代理」的轉發列不建立檢測區塊——那是代理探測，對任意 TCP
  // 目的地必失敗，沒有意義；旗標一改就整個 renderCards 重建，不留殘影。
  const test = showTest ? h("div", { class: "card-test" }) : null;
  const detail = h("div", { class: "card-detail" });

  // 沿用設定頁既有的 .toggle 開關樣式；綁的是 exit.enabled（意圖），
  // 跟 stopExit／startExit 既有的 IPC 與系統匣勾選同一套邏輯，行為對齊。
  const toggle = h("button", { class: "toggle", attrs: { role: "switch", type: "button" } });
  if (dimmed) toggle.disabled = true;
  toggle.addEventListener("click", () => {
    if (exit.enabled) void run(() => stopExit(exit.local), `disconnect ${exit.name}`);
    else void run(() => startExit(exit.local), `connect ${exit.name}`);
  });

  // 閉包只抓連線的名字與型別兩個原始值，不抓整個 ConnRef：卡片會活到下一次
  // renderCards 為止，抓住 ConnRef 等於連著它指到的那一份快照一起留下來，
  // 事件密集時容易讓兩代快照同時滯留在記憶體裡。
  const connName = conn.name;
  const connKind = conn.kind;
  const edit = h("button", { class: "iconbtn sm", title: "Edit" }, [icon("pencil", 15)]);
  edit.addEventListener("click", () => {
    if (exit.kind === "socks") openSocksSheet(connName, exit);
    else openTunnelSheet(connName, connKind, exit);
  });

  const main = h("div", { class: "card-main" }, [
    dot,
    id,
    test,
    h("div", { class: "card-actions" }, [toggle, edit]),
  ]);

  const root = h("article", { class: dimmed ? "card disabled" : "card" }, [main, detail]);
  root.dataset.local = String(exit.local);

  cardRefs.set(exit.local, { root, dot, badge, test, detail, toggle });
  return root;
}

/**
 * 一個區段（標題＋卡片容器）：空的就整段不畫，含標題。
 * conn 給 null 就只是把區段清成空的——「沒有選中連線」與「這個區段沒有列」
 * 對畫面來說是同一件事，不必為前者另寫一組四行的重設。
 */
function renderGroup(
  head: HTMLElement,
  box: HTMLElement,
  items: ExitInfo[],
  conn: ConnRef | null,
  dimmed: boolean,
) {
  head.hidden = items.length === 0;
  box.classList.toggle("grouped", items.length > 0);
  if (!conn) return;
  for (const item of items) {
    box.appendChild(buildCard(item, conn, dimmed));
    paintCard(item);
  }
}

function renderCards(conn: ConnRef | null) {
  const proxiesBox = el<HTMLDivElement>("proxies-cards");
  const forwardsBox = el<HTMLDivElement>("forwards-cards");

  // grouped 由下面的 renderGroup 依內容重設，這裡不必先手動清一次
  proxiesBox.textContent = "";
  forwardsBox.textContent = "";
  cardRefs.clear();

  // 分段依機制而非語意（wg-design.md §1.4）：SOCKS5 只放 kind=socks
  // （只有 wg 連線會有），PORT FORWARDS 放全部 kind=forward，含 probeProxy=true
  // 的列——就地顯示徽章＋出口 IP，不搬去別的分組。空區段整段不畫，含標題。
  const rows = conn ? visibleExits(conn) : [];
  const socksItems = rows.filter((e) => e.kind === "socks");
  const forwardItems = rows.filter((e) => e.kind !== "socks");
  // 引擎沒開就整片變暗、列開關停用：那些列的意圖還在，但引擎關著它們不可能跑
  const dimmed = conn?.kind === "wg" && !conn.data.enabled;

  renderGroup(el<HTMLDivElement>("proxies-list-head"), proxiesBox, socksItems, conn, dimmed);
  renderGroup(el<HTMLDivElement>("forwards-list-head"), forwardsBox, forwardItems, conn, dimmed);

  if (conn && socksItems.length === 0 && forwardItems.length === 0) {
    const ghost = h("button", { class: "ghost-card" }, [
      h("span", { class: "ghost-plus" }, [icon("plus", 18)]),
      h("span", { text: "Add forward" }),
    ]);
    ghost.addEventListener("click", beginCreateForward);
    forwardsBox.appendChild(ghost);
  }
}

// ---------------------------------------------------------------- 新增列

function beginCreateForward() {
  const conn = currentConn();
  if (conn) openTunnelSheet(conn.name, conn.kind, null);
}

/** WG 專屬：引擎自建 SOCKS5，選單只在 wg 連線時顯示這一項 */
function beginCreateSocks() {
  const conn = currentConn();
  if (conn && conn.kind === "wg") openSocksSheet(conn.name, null);
}

// ---------------------------------------------------------------- 刪除／undo

/** sheet 的 Delete 鍵按下後走到這裡：畫面先移除，5 秒內都還能收回 */
function requestDelete(local: number) {
  const hit = locate(local);
  if (!hit) return;
  const name = hit.exit.name;
  const owner = hit.conn.name;

  pendingDelete.add(local);
  render();

  const kindLabel: RowKind = hit.exit.kind;
  const toast = showUndoToast(
    `Deleted ${kindLabel === "socks" ? "proxy" : "forward"} ${name}`,
    async () => {
      undoToasts.delete(local);
      try {
        await deleteForward(local);
        // 刪成功才收掉暫存旗標，之後靠 config-changed 把卡片真的移除
        pendingDelete.delete(local);
      } catch (e) {
        // 後端拒絕就把卡片放回來，不要無聲復活
        pendingDelete.delete(local);
        render();
        showErrorToast(`Could not delete ${name}: ${String(e)}`);
      }
    },
    () => {
      undoToasts.delete(local);
      pendingDelete.delete(local);
      render();
    },
  );
  undoToasts.set(local, { connection: owner, toast });
}

/**
 * 關窗前把所有還在倒數的刪除 undo toast 立刻補提交，不要讓倒數被視窗關閉打斷。
 * 只覆蓋前端自己攔得到的關窗路徑（標題列的 Close 按鈕）；系統匣選單的 Exit
 * 與 Alt+F4 都是不經過這顆按鈕的關窗路徑（前者是 Rust 端直接處理，後者是
 * 視窗系統直接關閉），前端這裡一律攔不到，是已知限制。
 *
 * 回傳 Promise.allSettled，讓呼叫端能等所有 commit 真的送出去再繼續往下
 * 呼叫 windowClose——這是裁決採納的廉價保險：目前驗證過同一 tick 內派送
 * 就足夠安全，這裡加一手只是防未來有人在 flush 與 windowClose 之間插進
 * 一個 await，讓派送被視窗關閉截斷。allSettled 而非 all：任何一個 commit
 * 失敗都不該擋住其餘的送出或擋住關窗本身。
 */
function flushPendingDeletes(): Promise<unknown> {
  return Promise.allSettled([...undoToasts.values()].map(({ toast }) => toast.flush()));
}

/**
 * 整條連線被刪掉時，底下列還掛著的 undo 倒數就沒有意義了：
 * 讓它到期去 deleteForward 一個已經不存在的埠只會噴錯，
 * pendingDelete 裡的殘留旗標也會一直卡著。這裡一次收乾淨。
 */
function dropPendingDeletesOf(connectionName: string) {
  // 快照可能已經被 config-changed 洗掉這條連線了，所以以登記的來源為準，
  // 快照裡還在的話就再併一次，兩邊都不漏
  const locals = new Set<number>();
  for (const [local, entry] of undoToasts) if (entry.connection === connectionName) locals.add(local);
  const still = findConn(connectionName);
  if (still) for (const e of still.exits) locals.add(e.local);

  for (const local of locals) {
    undoToasts.get(local)?.toast.dismiss();
    undoToasts.delete(local);
    pendingDelete.delete(local);
  }
}

// ---------------------------------------------------------------- 日誌

function fill(box: HTMLElement, lines: string[], emptyText: string) {
  // 比照 appendLine：使用者自己往上捲去看舊訊息時就不要硬把他拉回底部
  const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 4;
  box.textContent = "";
  if (lines.length === 0) {
    box.appendChild(h("div", { class: "log-empty", text: emptyText }));
    return;
  }
  for (const line of lines) box.appendChild(h("div", { text: line }));
  if (atBottom) box.scrollTop = box.scrollHeight;
}

/** 日誌只有獨立的活動頁一個出口，主區不掛即時視窗 */
function renderLogs() {
  if (view === "log") fill(el<HTMLDivElement>("full-log"), logLines, "No activity yet");
}

function appendLine(box: HTMLElement, line: string) {
  const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 4;
  box.querySelector(".log-empty")?.remove();
  box.appendChild(h("div", { text: line }));
  while (box.childElementCount > LOG_CAP) box.removeChild(box.firstChild as ChildNode);
  if (atBottom) box.scrollTop = box.scrollHeight;
}

function appendLog(line: string) {
  logLines.push(line);
  if (logLines.length > LOG_CAP) logLines.shift();
  if (view === "log") appendLine(el<HTMLDivElement>("full-log"), line);
}

// ---------------------------------------------------------------- 事件套用

/**
 * replayLogs 只在最初載入時開：先掛 listen 再取 snapshot，中間漏接或重複的
 * log 事件就靠「清空後整份回放」收斂。config-changed 帶的 logs 不重播，
 * 否則每次改設定活動區都會被洗掉。
 */
function applySnapshot(next: Snapshot, replayLogs = false) {
  // detailText 與 knownProtocol 都只活在前端（前者由 exit-status 事件帶進來、
  // 後者是協定識別的黏著記憶），快照裡沒有這兩個欄位，重整時要自己保住。
  // 直接掃原始陣列，不繞 allConns()——這裡只要每一條列，不需要連線的包裝。
  const keep = new Map<number, ExitInfo>();
  for (const s of snap.sources) for (const e of s.exits) keep.set(e.local, e);
  for (const p of snap.wgProxies) for (const e of p.exits) keep.set(e.local, e);

  /**
   * 沿用同一個 local 的列就把前端暫存欄位接過去。
   *
   * knownProtocol 的搬運條件比 detailText 嚴格：**目的地換了、或代理旗標被關掉，
   * 記憶就作廢**。埠號相同不代表是同一條列——使用者可以把 :1080 從一台代理改指
   * 到另一台完全不同的機器，這時還宣稱它是 SOCKS5 就是在說謊。快照自己帶著
   * protocol 的話（開機第一份快照沒有事件可收）則直接播種。
   */
  const carry = (e: ExitInfo): ExitInfo => {
    const prev = keep.get(e.local);
    const sameTarget = prev !== undefined && prev.remote === e.remote && shouldProbe(e);
    return {
      ...e,
      detailText: prev?.detailText ?? null,
      knownProtocol: e.lastTest?.protocol ?? (sameTarget ? (prev.knownProtocol ?? null) : null),
    };
  };

  // 形狀正規化（wgProxies 缺席之類）已經在 ipc.ts 的 normalizeSnapshot 做掉了，
  // 這裡照宣告的型別信任資料
  snap = {
    ...next,
    sources: next.sources.map((s) => ({ ...s, exits: s.exits.map(carry) })),
    wgProxies: next.wgProxies.map((p) => ({ ...p, exits: p.exits.map(carry) })),
  };

  if (replayLogs) logLines = next.logs.slice(-LOG_CAP);

  syncSettingsPage(snap);
  render();
}

/**
 * 只要不是 connected 就把舊的自測結果清乾淨，不只 stopped：斷線重連期間
 * （connecting／reconnecting／port_busy／error）舊的「測試成功」字樣沒有
 * 理由繼續掛著，那是上一輪連線的結果，跟現在這輪連線狀態對不上。
 *
 * 這條規則自己就夠用，不依賴後端另外推事件——即使後端車道之後補上專門的
 * clear 事件或在 exit-test 帶空 result 過來（見 applyExitTest），這裡仍然
 * 是第一道、最快生效的防線。
 */
function applyExitStatus(ev: ExitStatusEvent) {
  const hit = locate(ev.local);
  if (!hit) return;
  const { exit, conn } = hit;
  exit.status = ev.status;
  exit.detailText = ev.detail ?? null;
  // 清的是「這一輪連線的自測結果」，不是「這個目的地是不是代理」——
  // knownProtocol 刻意不動（那筆識別結果早在它抵達時就由 paintCard 記下了），
  // 否則徽章會跟著每一次斷線閃回未識別的樣子
  if (ev.status !== "connected") exit.lastTest = null;
  paintCard(exit);
  // 連線就在手上，不必再拿名字回頭查一次（renderSummary 只在它就是選中的那條時才畫）
  if (view === "source" && conn.name === selected) renderSummary(conn);
  paintRailStatus(conn);
}

/**
 * state／text 缺席（清除事件的 payload 只有 `{ local }`，見 types.ts 的
 * ExitTestEvent）代表後端要清掉這個列的自測結果，不是「剛好測出一筆
 * 空內容的結果」；比照 applyExitStatus 一樣改記成 null，不要把空殼結果
 * 畫到卡片上。`ev.text && ev.state` 同時當 discriminant：兩者必須一起
 * 出現才組得成一筆真正的結果。protocol 是可選的識別結果（wg-design.md
 * §5.3），沒識別出來就整個欄位不存在，一併帶進 lastTest。
 */
function applyExitTest(ev: ExitTestEvent) {
  const hit = locate(ev.local);
  if (!hit) return;
  const { exit } = hit;
  // 識別結果不在這裡另外抄一份：下面的 paintCard 開頭就是記憶的同步點
  exit.lastTest =
    ev.text && ev.state
      ? ev.protocol
        ? { state: ev.state, text: ev.text, protocol: ev.protocol }
        : { state: ev.state, text: ev.text }
      : null;
  paintCard(exit);
}

// ---------------------------------------------------------------- 啟動

/**
 * 視窗比 Rust 端 manage 狀態更早出現是有可能的，第一次取狀態失敗就重試幾輪，
 * 真的拿不到才把錯誤寫進活動區。
 */
async function loadSnapshot() {
  for (let attempt = 0; attempt < 20; attempt++) {
    try {
      applySnapshot(await getState(), true);
      return;
    } catch (e) {
      if (attempt === 19) {
        // 最後一輪已經不會再重試，寫錯誤就直接結束，不用再空等 250ms
        appendLog(`ui error: ${String(e)}`);
        break;
      }
      await new Promise((r) => setTimeout(r, 250));
    }
  }
}

async function init() {
  try {
    await onLog(appendLog);
    await onExitStatus(applyExitStatus);
    await onExitTest(applyExitTest);
    await onConfigChanged((s) => applySnapshot(s));
    // 更新檢查是背景跑的，結果晚於啟動快照才到，靠這個事件補進來
    await onUpdateAvailable((info) => {
      snap = { ...snap, update: info };
      syncSettingsPage(snap);
    });
  } catch (e) {
    appendLog(`ui error: ${String(e)}`);
  }
  await loadSnapshot();
}

installContextMenuGuard();

// index.html 只用 data-icon 宣告位置，真正的 SVG 在這裡一次補齊
hydrateIcons();

el<HTMLButtonElement>("btn-min").addEventListener("click", () =>
  void run(windowMinimize, "minimize the window"),
);
/**
 * 關窗前等 flush 的逾時上限。deleteForward 若因為後端卡住（例如 ssh 行程
 * 掛住）遲遲不 resolve，Promise.allSettled 也會跟著吊住，關窗鈕會看起來像
 * 當掉——這裡拿「使用者能不能按得動關窗鈕」換「最壞情況下這一筆刪除的
 * flush 保險失效」：2.5 秒後直接放行，正常情況（flush 遠快於這個上限）
 * 完全不受影響，只在真的卡住時才會退化回「花式版」的舊行為，讓視窗照樣
 * 關得掉。
 */
const CLOSE_FLUSH_TIMEOUT_MS = 2500;

el<HTMLButtonElement>("btn-close").addEventListener("click", () => {
  void (async () => {
    // 關窗前先把還在倒數的刪除 undo 補提交，免得倒數被視窗關閉打斷、刪除靜靜消失；
    // 等 flush 完成再關窗，見 flushPendingDeletes 的說明；逾時放行見上方常數註解
    await Promise.race([
      flushPendingDeletes(),
      new Promise((resolve) => window.setTimeout(resolve, CLOSE_FLUSH_TIMEOUT_MS)),
    ]);
    await run(windowClose, "close the window");
  })();
});

el<HTMLButtonElement>("btn-logs").addEventListener("click", () => setView("log"));
el<HTMLButtonElement>("btn-settings").addEventListener("click", () => setView("settings"));

initSummaryMenu();
el<HTMLButtonElement>("summary-master-toggle").addEventListener("click", () => {
  const conn = currentConn();
  if (conn?.kind === "wg") toggleWgEngine(conn);
});
el<HTMLButtonElement>("btn-first-source").addEventListener("click", () => openSourceSheet(null));

initSourceSheet({
  onSaved: (name) => {
    // 快照這時候還是舊的（config-changed 比 invoke 的 resolve 晚），
    // 先記下要選誰，等 render() 在快照裡看到這個名字再真的切過去
    pendingSelect = name;
    view = "source";
    render();
  },
  onDeleted: (name) => {
    // 整條連線都沒了，底下那些列的 sheet 也不該留著
    closeTunnelSheet();
    closeSocksSheet();
    dropPendingDeletesOf(name);
    // 同樣不等 config-changed，先把它從本地快照拿掉，
    // 免得回退第一個連線時又挑回這個剛被刪掉的
    snap = {
      ...snap,
      sources: snap.sources.filter((s) => s.name !== name),
      wgProxies: snap.wgProxies.filter((p) => p.name !== name),
    };
    pendingSelect = null;
    if (selected === name) selected = null;
    view = "source";
    render();
  },
});
initTunnelSheet({ onDelete: requestDelete });
initSocksSheet({ onDelete: requestDelete });
render();
/**
 * initSettingsPage() 一開頭就會問 get_config_path，dev-mock 是動態 import、
 * 得等 bootstrap 把假後端裝好才問得到；正式版走真的 Tauri runtime，
 * invoke 一開始就能用，不用等。
 *
 * 但 bootstrap／init 這條鏈可能慢（loadSnapshot 最多重試 5 秒）也可能失敗
 * （bootstrap 動態 import dev-mock 出錯就是 rejected promise）：慢啟動時
 * 設定頁的 toggle／開資料夾按鈕在這條鏈跑完前都還沒接上事件，點了沒反應；
 * 若整條鏈 reject，沒有 catch 就是沒人接的 unhandled rejection，
 * initSettingsPage() 也永遠不會被呼叫、設定頁從此死掉。用 finally 保證
 * initSettingsPage 一定會跑到，用 catch 把失敗寫進活動區。
 */
void bootstrap(init)
  .catch((e) => appendLog(`ui error: ${String(e)}`))
  .finally(() => initSettingsPage());
