import { bootstrap } from "./bootstrap";
import { installContextMenuGuard } from "./context-menu";
import { el, h } from "./dom";
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
  type ConnTarget,
} from "./sheet";
import {
  defaultDetail,
  isBad,
  isRunning,
  sourceTone,
  splitTest,
  statusTone,
  testLine,
  type Tone,
} from "./status";
import { showErrorToast, showUndoToast, type UndoToast } from "./toast";
import type {
  ConnKind,
  ExitInfo,
  ExitStatusEvent,
  ExitTestEvent,
  RowKind,
  Snapshot,
  SourceInfo,
  WgProxyInfo,
} from "./types";

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

/** 統一介面：不管是 ssh 源還是 wg 連線，畫面邏輯只在意名稱、型別與底下的列 */
interface ConnRef {
  kind: ConnKind;
  name: string;
  exits: ExitInfo[];
  ssh?: SourceInfo;
  wg?: WgProxyInfo;
}

function allConns(): ConnRef[] {
  return [
    ...snap.sources.map((s): ConnRef => ({ kind: "ssh", name: s.name, exits: s.exits, ssh: s })),
    ...snap.wgProxies.map((p): ConnRef => ({ kind: "wg", name: p.name, exits: p.exits, wg: p })),
  ];
}

function findConn(name: string): ConnRef | null {
  const src = snap.sources.find((s) => s.name === name);
  if (src) return { kind: "ssh", name: src.name, exits: src.exits, ssh: src };
  const wg = snap.wgProxies.find((p) => p.name === name);
  if (wg) return { kind: "wg", name: wg.name, exits: wg.exits, wg };
  return null;
}

function currentConn(): ConnRef | null {
  return selected ? findConn(selected) : null;
}

const visibleExits = (conn: ConnRef | null) =>
  (conn?.exits ?? []).filter((e) => !pendingDelete.has(e.local));

/** local 全域唯一，所以列一律用埠號跨連線找，順便把它所屬的連線帶回來 */
function locate(local: number): { exit: ExitInfo; connName: string } | undefined {
  for (const source of snap.sources) {
    const exit = source.exits.find((e) => e.local === local);
    if (exit) return { exit, connName: source.name };
  }
  for (const proxy of snap.wgProxies) {
    const exit = proxy.exits.find((e) => e.local === local);
    if (exit) return { exit, connName: proxy.name };
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

function basename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** 只重畫某個連線 icon 的狀態小點，不動整列（避免密集事件下重建 DOM 與丟焦點） */
function paintRailStatus(name: string) {
  const node = railStatusRefs.get(name);
  const conn = findConn(name);
  if (node && conn) node.className = `src-status tone-${sourceTone(visibleExits(conn))}`;
}

function renderRail() {
  const list = el<HTMLDivElement>("rail-list");
  list.textContent = "";
  railStatusRefs.clear();

  for (const conn of allConns()) {
    let btn: HTMLButtonElement;
    if (conn.kind === "ssh") {
      const src = conn.ssh as SourceInfo;
      const hue = hashHue(conn.name);
      btn = h("button", { class: "src-icon", text: initial(conn.name) });
      btn.style.setProperty("--src-bg", `hsl(${hue} 34% 34%)`);
      btn.style.setProperty("--src-ink", `hsl(${hue} 70% 86%)`);
      btn.title = `${conn.name} — ssh ${src.user}@${src.host}`;
    } else {
      const wg = conn.wg as WgProxyInfo;
      // WG 節點固定用 accent 品牌色 + "WG" 兩字，與 ssh 的雜湊色首字並列（Q10）
      btn = h("button", { class: "src-icon type-wg", text: "WG" });
      btn.title = `${conn.name} — wg ${wg.endpoint || basename(wg.confPath)}`;
    }
    btn.classList.toggle("active", view === "source" && conn.name === selected);
    const status = h("span", { class: `src-status tone-${sourceTone(visibleExits(conn))}` });
    railStatusRefs.set(conn.name, status);
    btn.appendChild(status);
    btn.addEventListener("click", () => selectConn(conn.name));
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

function applyViewVisibility() {
  const noConns = allConns().length === 0;
  const showEmpty = view === "source" && noConns;
  el<HTMLElement>("view-source").hidden = view !== "source" || noConns;
  el<HTMLElement>("view-empty").hidden = !showEmpty;
  el<HTMLElement>("view-log").hidden = view !== "log";
  el<HTMLElement>("view-settings").hidden = view !== "settings";
}

/** 一次把整個畫面對齊到目前的 snap／selected／view */
function render() {
  const before = selected;

  // 存檔後的名字一旦出現在快照裡就切過去，切完才解除等待
  if (pendingSelect !== null && allConns().some((c) => c.name === pendingSelect)) {
    selected = pendingSelect;
    pendingSelect = null;
  }

  // 選中的連線被刪掉或還沒選過，就落回第一個；
  // 但還在等 config-changed 時不回退，否則改名會被打回舊的連線
  if (pendingSelect === null && !currentConn()) selected = allConns()[0]?.name ?? null;

  // ⋯ 選單的每一項都以「選中的那條連線」為對象。外部變更（別的視窗改了設定檔、
  // 連線被刪掉、整份清空）可能在選單開著時把它換掉或抽走，這時要收起來——
  // 否則使用者按下去的動作會打在另一條連線上，或打在不存在的東西上。
  if (menuOpen && (selected !== before || !currentConn())) closeMenu();

  applyViewVisibility();
  renderRail();
  if (view === "source") {
    renderSummary();
    renderCards();
  }
  renderLogs();
}

// ---------------------------------------------------------------- 頂部彙總列

/** wg 連線副標顯示 endpoint（U4）；ssh 照舊 "ssh user@host" */
function summarySubText(conn: ConnRef): string {
  if (conn.kind === "ssh") {
    const src = conn.ssh as SourceInfo;
    return `ssh ${src.user}@${src.host}`;
  }
  const wg = conn.wg as WgProxyInfo;
  return wg.endpoint ? `wg ${wg.endpoint}` : "wg";
}

function renderSummary() {
  const conn = currentConn();
  const exits = visibleExits(conn);
  const total = exits.length;
  const connected = exits.filter((e) => e.status === "connected").length;
  const busy = exits.filter((e) => e.status === "connecting" || e.status === "reconnecting").length;
  const bad = exits.filter(isBad).length;
  const running = exits.some(isRunning);

  // 左段：連線名稱當主標，身分摘要當副標；WG 專屬的引擎狀態點跟標題並排
  const title = el<HTMLDivElement>("summary-title");
  title.textContent = conn ? conn.name : "No connection";
  title.title = conn ? conn.name : "";
  el<HTMLDivElement>("summary-sub").textContent = conn ? summarySubText(conn) : "no host configured";

  const engineDot = el<HTMLSpanElement>("summary-engine-dot");
  if (conn?.kind === "wg") {
    // 連線層的狀態不另外推事件（wg-design.md §5.2）：由底下各列狀態彙總而來，
    // 跟卡片彙總分數用同一套 sourceTone，兩者天然一致。
    engineDot.hidden = false;
    const tone = sourceTone(exits);
    engineDot.className = `dot tone-${tone}`;
    engineDot.title = tone === "grey" ? "stopped" : tone === "green" ? "connected" : tone;
  } else {
    engineDot.hidden = true;
  }

  const masterToggle = el<HTMLButtonElement>("summary-master-toggle");
  if (conn?.kind === "wg") {
    masterToggle.hidden = false;
    masterToggle.classList.toggle("on", Boolean(conn.wg?.enabled));
  } else {
    masterToggle.hidden = true;
  }

  // 中段：大分數＋小字狀態，顏色代表整條連線的健康度
  let score: string;
  let label: string;
  let tone: Tone;
  if (!conn || total === 0) {
    score = "—";
    label = "no rows";
    tone = "grey";
  } else if (!running) {
    score = `0/${total}`;
    label = "stopped";
    tone = "grey";
  } else {
    score = `${connected}/${total}`;
    label = "connected";
    tone = bad > 0 ? "red" : busy > 0 ? "amber" : connected > 0 ? "green" : "grey";
  }
  const num = el<HTMLDivElement>("summary-score");
  num.textContent = score;
  num.className = `summary-score-num tone-${tone}`;
  el<HTMLDivElement>("summary-score-label").textContent = label;

  // 右段：⋯ 選單裡的連／斷那一項跟著整條連線的狀態換字
  setIcon(el<HTMLSpanElement>("menu-toggle-ico"), running ? "square" : "play", 14);
  el<HTMLSpanElement>("menu-toggle-text").textContent = running ? "Disconnect" : "Connect";
  const toggleItem = el<HTMLButtonElement>("menu-toggle-source");
  toggleItem.classList.toggle("danger", running);
  toggleItem.classList.toggle("go", !running);

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
 * WG 連線的總開關／⋯ 選單的連斷動作：沒有專門的連線層級指令（後端契約只
 * 定義了逐列的 start_exit／stop_exit，wg-design.md §5.5），所以比照既有的
 * 「重新連線」那樣，對底下每一條列各自送一次指令——行為與 SSH 的
 * start_source／stop_source 殊途同歸，只是走逐列迴圈而不是單一指令。
 */
function toggleConnRows(conn: ConnRef) {
  const exits = visibleExits(conn);
  const running = exits.some(isRunning);
  for (const exit of exits) {
    if (running) void run(() => stopExit(exit.local), `disconnect ${exit.name}`);
    else void run(() => startExit(exit.local), `connect ${exit.name}`);
  }
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
    if (conn.kind === "ssh") {
      if (visibleExits(conn).some(isRunning)) void run(() => stopSource(conn.name), `stop ${conn.name}`);
      else void run(() => startSource(conn.name), `start ${conn.name}`);
    } else {
      toggleConnRows(conn);
    }
  });
  menuItem("menu-reconnect-source", () => {
    const conn = currentConn();
    if (!conn) return;
    // 只重接目前連線中的列，停用中的維持停用，不拉起來
    for (const exit of visibleExits(conn).filter(isRunning)) {
      void run(() => restartExit(exit.local), `reconnect ${exit.name}`);
    }
  });
  menuItem("menu-activity", () => setView("log"));
  menuItem("menu-edit-source", () => {
    const conn = currentConn();
    if (!conn) return;
    const target: ConnTarget =
      conn.kind === "ssh" ? { kind: "ssh", data: conn.ssh as SourceInfo } : { kind: "wg", data: conn.wg as WgProxyInfo };
    openSourceSheet(target);
  });
}

// ---------------------------------------------------------------- 列卡片

function routeText(exit: ExitInfo): string {
  return exit.remote ? `:${exit.local} → ${exit.remote}` : `:${exit.local}`;
}

/** 這一列要不要顯示協定徽章＋出口檢測：kind=socks 恆真，forward 列看 probeProxy */
function showsProbe(exit: ExitInfo): boolean {
  return exit.kind === "socks" || exit.probeProxy;
}

function paintCard(exit: ExitInfo) {
  const refs = cardRefs.get(exit.local);
  if (!refs) return;

  refs.dot.className = `dot tone-${statusTone(exit.status)}`;
  refs.dot.title = exit.status;

  if (refs.badge) {
    if (exit.kind === "socks") {
      refs.badge.textContent = "SOCKS5";
      refs.badge.className = "type-badge wg";
      refs.badge.removeAttribute("title");
    } else {
      const protocol = exit.lastTest?.protocol;
      refs.badge.textContent = protocol ? protocol.toUpperCase() : "PROXY?";
      refs.badge.className = protocol ? "type-badge wg" : "type-badge ssh";
      if (!protocol) refs.badge.title = "Confirm the destination is a proxy, or turn the flag off";
      else refs.badge.removeAttribute("title");
    }
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
      refs.test.className = `card-test tone-text-${t.tone}`;
      refs.test.title =
        exit.kind === "forward" && exit.probeProxy && !exit.lastTest?.protocol && t.tone === "red"
          ? "Confirm the destination is a proxy, or turn the flag off"
          : "";
      refs.test.textContent = t.text;
    }
  }

  const detail = isBad(exit) ? (exit.detailText ?? defaultDetail(exit.status)) : "";
  refs.detail.textContent = detail;
  refs.detail.title = detail;
  refs.detail.classList.toggle("show", Boolean(detail));

  // switch 表達「意圖」（exit.enabled，跟系統匣勾選同一個依據），
  // 不是連線的即時狀態——那是上面的狀態點在管，兩者刻意分開顯示。
  const on = exit.enabled;
  refs.toggle.classList.toggle("on", on);
  refs.toggle.setAttribute("aria-checked", String(on));
  refs.toggle.title = on ? "Disconnect" : "Connect";
}

function buildCard(exit: ExitInfo, conn: ConnRef, dimmed: boolean): HTMLElement {
  const dot = h("span", { class: "dot" });

  const showBadge = showsProbe(exit);
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
  const test = showBadge ? h("div", { class: "card-test" }) : null;
  const detail = h("div", { class: "card-detail" });

  // 沿用設定頁既有的 .toggle 開關樣式；綁的是 exit.enabled（意圖），
  // 跟 stopExit／startExit 既有的 IPC 與系統匣勾選同一套邏輯，行為對齊。
  const toggle = h("button", { class: "toggle", attrs: { role: "switch", type: "button" } });
  if (dimmed) toggle.disabled = true;
  toggle.addEventListener("click", () => {
    if (exit.enabled) void run(() => stopExit(exit.local), `disconnect ${exit.name}`);
    else void run(() => startExit(exit.local), `connect ${exit.name}`);
  });

  const edit = h("button", { class: "iconbtn sm", title: "Edit" }, [icon("pencil", 15)]);
  edit.addEventListener("click", () => {
    if (exit.kind === "socks") openSocksSheet(conn.name, exit);
    else openTunnelSheet(conn.name, conn.kind, exit);
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

function renderCards() {
  const proxiesHead = el<HTMLDivElement>("proxies-list-head");
  const proxiesBox = el<HTMLDivElement>("proxies-cards");
  const forwardsHead = el<HTMLDivElement>("forwards-list-head");
  const forwardsBox = el<HTMLDivElement>("forwards-cards");

  proxiesBox.textContent = "";
  forwardsBox.textContent = "";
  proxiesBox.classList.remove("grouped");
  forwardsBox.classList.remove("grouped");
  cardRefs.clear();

  const conn = currentConn();
  if (!conn) {
    proxiesHead.hidden = true;
    forwardsHead.hidden = true;
    return;
  }

  // 分段依機制而非語意（wg-design.md §1.4）：SOCKS5 只放 kind=socks
  // （只有 wg 連線會有），PORT FORWARDS 放全部 kind=forward，含 probeProxy=true
  // 的列——就地顯示徽章＋出口 IP，不搬去別的分組。空區段整段不畫，含標題。
  const rows = visibleExits(conn);
  const socksItems = rows.filter((e): e is ExitInfo & { kind: "socks" } => e.kind === "socks");
  const forwardItems = rows.filter((e) => e.kind !== "socks");
  const dimmed = conn.kind === "wg" && !conn.wg?.enabled;

  proxiesHead.hidden = socksItems.length === 0;
  proxiesBox.classList.toggle("grouped", socksItems.length > 0);
  for (const item of socksItems) {
    proxiesBox.appendChild(buildCard(item, conn, dimmed));
    paintCard(item);
  }

  forwardsHead.hidden = forwardItems.length === 0;
  forwardsBox.classList.toggle("grouped", forwardItems.length > 0);
  for (const item of forwardItems) {
    forwardsBox.appendChild(buildCard(item, conn, dimmed));
    paintCard(item);
  }

  if (socksItems.length === 0 && forwardItems.length === 0) {
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
  const owner = hit.connName;

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
  // detail 只在事件裡出現，快照重整時要保住已知的 detail 文字
  const keep = new Map<number, string | null | undefined>();
  for (const s of snap.sources) for (const e of s.exits) keep.set(e.local, e.detailText);
  for (const p of snap.wgProxies) for (const e of p.exits) keep.set(e.local, e.detailText);

  snap = {
    ...next,
    sources: (next.sources ?? []).map((s) => ({
      ...s,
      exits: s.exits.map((e) => ({ ...e, detailText: keep.get(e.local) ?? null })),
    })),
    wgProxies: (next.wgProxies ?? []).map((p) => ({
      ...p,
      exits: p.exits.map((e) => ({ ...e, detailText: keep.get(e.local) ?? null })),
    })),
  };

  if (replayLogs) logLines = [...(next.logs ?? [])].slice(-LOG_CAP);

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
  const { exit } = hit;
  exit.status = ev.status;
  exit.detailText = ev.detail ?? null;
  if (ev.status !== "connected") exit.lastTest = null;
  paintCard(exit);
  if (view === "source") renderSummary();
  paintRailStatus(hit.connName);
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
  hit.exit.lastTest =
    ev.text && ev.state
      ? ev.protocol
        ? { state: ev.state, text: ev.text, protocol: ev.protocol }
        : { state: ev.state, text: ev.text }
      : null;
  paintCard(hit.exit);
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
  if (conn && conn.kind === "wg") toggleConnRows(conn);
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
