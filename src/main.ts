import { bootstrap } from "./bootstrap";
import { installContextMenuGuard } from "./context-menu";
import { el, h } from "./dom";
import {
  deleteForward,
  getState,
  onConfigChanged,
  onExitStatus,
  onExitTest,
  onLog,
  restartExit,
  startExit,
  startSource,
  stopExit,
  stopSource,
  testSource,
  windowClose,
  windowMinimize,
} from "./ipc";
import {
  closeTunnelSheet,
  initSettingsPage,
  initSourceSheet,
  initTunnelSheet,
  openSourceSheet,
  openTunnelSheet,
  syncSettingsPage,
} from "./sheet";
import { showErrorToast, showUndoToast, type UndoToast } from "./toast";
import type {
  ExitInfo,
  ExitStatus,
  ExitStatusEvent,
  ExitTestEvent,
  Snapshot,
  SourceInfo,
} from "./types";

// Segoe MDL2 Assets：E71A Stop、E768 Play、E70F 鉛筆、E895 Sync（重新連接）。
// 一律用跳脫寫法，PUA 字元直接貼進原始碼很容易在編輯途中被吃掉。
const GLYPH_STOP = "";
const GLYPH_START = "";
const GLYPH_EDIT = "";
const GLYPH_RESTART = "";

const EMPTY: Snapshot = { closeToTray: true, autostart: false, sources: [], logs: [] };

let snap: Snapshot = EMPTY;

/** 目前選中的源名稱；null 代表還沒有源 */
let selected: string | null = null;

/**
 * 剛送出 upsert_source、還在等 config-changed 把這個名字帶回來。
 *
 * 真後端的事件順序是 invoke 先 resolve、config-changed 才到，所以存檔當下快照裡
 * 還是舊名字。這段期間不能讓 render() 的「選中不存在就回退第一個」把改名後的
 * 選中吃掉，得等快照真的出現這個名字才切過去。
 */
let pendingSelect: string | null = null;

type View = "source" | "log" | "settings";
let view: View = "source";

/** 完整日誌都留在記憶體，切源／切頁時重畫 */
let logLines: string[] = [];
const LOG_CAP = 500;

/** 已按下刪除、但 undo 倒數還沒結束的出口：畫面先當它不存在 */
const pendingDelete = new Set<number>();
/**
 * 還在倒數的 undo toast，整個源被刪掉時要能把底下出口的倒數一起收乾淨。
 * 一併記下當初的源名稱：刪源時快照可能已經被 config-changed 更新過了，
 * 不能靠當下的 snap 反查出口屬於誰。
 */
const undoToasts = new Map<number, { source: string; toast: UndoToast }>();

interface CardRefs {
  root: HTMLElement;
  dot: HTMLElement;
  test: HTMLElement;
  detail: HTMLElement;
  toggle: HTMLButtonElement;
}

const cardRefs = new Map<number, CardRefs>();
/** 側欄每個源 icon 的狀態小點，讓 exit-status 事件不用整列重建就能更新 */
const railStatusRefs = new Map<string, HTMLElement>();

// ---------------------------------------------------------------- 狀態工具

const RUNNING: ExitStatus[] = ["connecting", "connected", "reconnecting", "port_busy", "error"];

const isRunning = (e: ExitInfo) => RUNNING.includes(e.status);
const isBad = (e: ExitInfo) => e.status === "port_busy" || e.status === "error";

type Tone = "grey" | "amber" | "green" | "red";

function statusTone(status: ExitStatus): Tone {
  switch (status) {
    case "connected":
      return "green";
    case "connecting":
    case "reconnecting":
      return "amber";
    case "port_busy":
    case "error":
      return "red";
    default:
      return "grey";
  }
}

/** 源的彙總狀態：全連綠／部分琥珀／全停灰，任一出口出錯就直接紅 */
function sourceTone(src: SourceInfo): Tone {
  const exits = src.exits.filter((e) => !pendingDelete.has(e.local));
  if (exits.length === 0) return "grey";
  if (exits.some(isBad)) return "red";
  if (!exits.some(isRunning)) return "grey";
  return exits.every((e) => e.status === "connected") ? "green" : "amber";
}

const sources = () => snap.sources;

function currentSource(): SourceInfo | null {
  if (!selected) return null;
  return snap.sources.find((s) => s.name === selected) ?? null;
}

const visibleExits = (src: SourceInfo | null) =>
  (src?.exits ?? []).filter((e) => !pendingDelete.has(e.local));

/** local 全域唯一，所以出口一律用埠號跨源找，順便把它所屬的源帶回來 */
function locate(local: number): { exit: ExitInfo; source: SourceInfo } | undefined {
  for (const source of snap.sources) {
    const exit = source.exits.find((e) => e.local === local);
    if (exit) return { exit, source };
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

// ---------------------------------------------------------------- 左側源軌道

/** 名稱 hash → 色相，同一個源在每次啟動都拿到同一個顏色 */
function hashHue(name: string): number {
  let acc = 0;
  for (let i = 0; i < name.length; i++) acc = (acc * 31 + name.charCodeAt(i)) >>> 0;
  return acc % 360;
}

function initial(name: string): string {
  const ch = name.trim().charAt(0);
  return ch ? ch.toUpperCase() : "?";
}

/** 只重畫某個源 icon 的狀態小點，不動整列（避免密集事件下重建 DOM 與丟焦點） */
function paintRailStatus(name: string) {
  const node = railStatusRefs.get(name);
  const src = snap.sources.find((s) => s.name === name);
  if (node && src) node.className = `src-status tone-${sourceTone(src)}`;
}

function renderRail() {
  const list = el<HTMLDivElement>("rail-list");
  list.textContent = "";
  railStatusRefs.clear();

  for (const src of sources()) {
    const hue = hashHue(src.name);
    const btn = h("button", { class: "src-icon", text: initial(src.name) });
    btn.style.setProperty("--src-bg", `hsl(${hue} 34% 34%)`);
    btn.style.setProperty("--src-ink", `hsl(${hue} 70% 86%)`);
    btn.title = `${src.name} — ssh ${src.user}@${src.host}`;
    btn.classList.toggle("active", view === "source" && src.name === selected);
    const status = h("span", { class: `src-status tone-${sourceTone(src)}` });
    railStatusRefs.set(src.name, status);
    btn.appendChild(status);
    btn.addEventListener("click", () => selectSource(src.name));
    list.appendChild(btn);
  }

  const add = h("button", { class: "rail-btn add", text: "+", title: "Add connection" });
  add.addEventListener("click", () => openSourceSheet(null));
  list.appendChild(add);

  el<HTMLButtonElement>("btn-logs").classList.toggle("active", view === "log");
  el<HTMLButtonElement>("btn-settings").classList.toggle("active", view === "settings");
}

// ---------------------------------------------------------------- 視圖切換

function setView(next: View) {
  if (view !== next) closeTunnelSheet();
  closeMenu();
  // 使用者自己動了畫面，等快照的旗標就作廢，免得等不到時永遠卡著
  pendingSelect = null;
  view = next;
  render();
}

function selectSource(name: string) {
  // 切到別條連線時，開著的隧道 sheet 就沒有對象了
  if (selected !== name) closeTunnelSheet();
  closeMenu();
  pendingSelect = null;
  selected = name;
  view = "source";
  render();
}

function applyViewVisibility() {
  const noSources = sources().length === 0;
  const showEmpty = view === "source" && noSources;
  el<HTMLElement>("view-source").hidden = view !== "source" || noSources;
  el<HTMLElement>("view-empty").hidden = !showEmpty;
  el<HTMLElement>("view-log").hidden = view !== "log";
  el<HTMLElement>("view-settings").hidden = view !== "settings";
}

/** 一次把整個畫面對齊到目前的 snap／selected／view */
function render() {
  const before = selected;

  // 存檔後的名字一旦出現在快照裡就切過去，切完才解除等待
  if (pendingSelect !== null && snap.sources.some((s) => s.name === pendingSelect)) {
    selected = pendingSelect;
    pendingSelect = null;
  }

  // 選中的源被刪掉或還沒選過，就落回第一個；
  // 但還在等 config-changed 時不回退，否則改名會被打回舊的源
  if (pendingSelect === null && !currentSource()) selected = sources()[0]?.name ?? null;

  // ⋯ 選單的每一項都以「選中的那條連線」為對象。外部變更（別的視窗改了設定檔、
  // 連線被刪掉、整份清空）可能在選單開著時把它換掉或抽走，這時要收起來——
  // 否則使用者按下去的動作會打在另一條連線上，或打在不存在的東西上。
  if (menuOpen && (selected !== before || !currentSource())) closeMenu();

  applyViewVisibility();
  renderRail();
  if (view === "source") {
    renderSummary();
    renderCards();
  }
  renderLogs();
}

// ---------------------------------------------------------------- 頂部彙總列

function renderSummary() {
  const src = currentSource();
  const exits = visibleExits(src);
  const total = exits.length;
  const connected = exits.filter((e) => e.status === "connected").length;
  const busy = exits.filter((e) => e.status === "connecting" || e.status === "reconnecting").length;
  const bad = exits.filter(isBad).length;
  const running = exits.some(isRunning);

  // 左段：連線名稱當主標，ssh 目標當副標
  const title = el<HTMLDivElement>("summary-title");
  title.textContent = src ? src.name : "No connection";
  title.title = src ? src.name : "";
  el<HTMLDivElement>("summary-sub").textContent = src
    ? `ssh ${src.user}@${src.host}`
    : "no host configured";

  // 中段：大分數＋小字狀態，顏色代表整條連線的健康度
  let score: string;
  let label: string;
  let tone: Tone;
  if (!src || total === 0) {
    score = "—";
    label = "no tunnels";
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
  el<HTMLSpanElement>("menu-toggle-ico").innerHTML = running ? GLYPH_STOP : GLYPH_START;
  el<HTMLSpanElement>("menu-toggle-text").textContent = running ? "Disconnect" : "Connect";
  const toggleItem = el<HTMLButtonElement>("menu-toggle-source");
  toggleItem.classList.toggle("danger", running);
  toggleItem.classList.toggle("go", !running);
}

// ---------------------------------------------------------------- ⋯ 選單

let menuOpen = false;

function setMenuOpen(on: boolean) {
  // 沒有選中的連線時整組動作都沒有對象，乾脆不讓它開
  if (on && !currentSource()) return;
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

  menuItem("menu-add-exit", beginCreate);
  menuItem("menu-toggle-source", () => {
    const src = currentSource();
    if (!src) return;
    if (visibleExits(src).some(isRunning)) void run(() => stopSource(src.name), `stop ${src.name}`);
    else void run(() => startSource(src.name), `start ${src.name}`);
  });
  menuItem("menu-test-source", () => {
    const src = currentSource();
    if (src) void run(() => testSource(src.name), `test ${src.name}`);
  });
  menuItem("menu-activity", () => setView("log"));
  menuItem("menu-edit-source", () => {
    const src = currentSource();
    if (src) openSourceSheet(src);
  });
}

// ---------------------------------------------------------------- 出口卡片

function testLine(exit: ExitInfo): { text: string; tone: string } {
  const t = exit.lastTest;
  if (!t) return { text: "", tone: "muted" };
  if (t.state === "testing") return { text: t.text || "testing…", tone: "muted" };
  if (t.state === "fail") return { text: t.text || "no response", tone: "red" };
  return { text: t.text, tone: "text" };
}

/**
 * 自測成功的字串是後端組好的「ip␠␠city, country」，拆成兩行顯示。
 * 拆不開（格式不如預期）就退回單行，不要硬猜。
 */
function splitTest(text: string): { ip: string; place: string } | null {
  const i = text.indexOf("  ");
  if (i <= 0) return null;
  const ip = text.slice(0, i).trim();
  const place = text.slice(i + 2).trim();
  return ip && place ? { ip, place } : null;
}

/** 後端沒帶 detail 時至少讓紅點有句話可看 */
function defaultDetail(status: ExitStatus): string {
  return status === "port_busy" ? "local port is already in use" : "connection failed";
}

function paintCard(exit: ExitInfo) {
  const refs = cardRefs.get(exit.local);
  if (!refs) return;

  refs.dot.className = `dot tone-${statusTone(exit.status)}`;
  refs.dot.title = exit.status;
  refs.root.dataset.status = exit.status;

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
    refs.test.title = "";
    refs.test.textContent = t.text;
  }

  const detail = isBad(exit) ? (exit.detailText ?? defaultDetail(exit.status)) : "";
  refs.detail.textContent = detail;
  refs.detail.title = detail;
  refs.detail.classList.toggle("show", Boolean(detail));

  const running = isRunning(exit);
  refs.toggle.innerHTML = running ? GLYPH_STOP : GLYPH_START;
  refs.toggle.title = running ? "Disconnect" : "Connect";
  refs.toggle.classList.toggle("danger", running);
  refs.toggle.classList.toggle("go", !running);
}

function buildCard(exit: ExitInfo, source: string): HTMLElement {
  const dot = h("span", { class: "dot" });
  const name = h("div", { class: "card-name", text: exit.name });
  const route = h("div", { class: "card-route", text: `:${exit.local} → ${exit.remote}` });
  const test = h("div", { class: "card-test" });
  const detail = h("div", { class: "card-detail" });

  const toggle = h("button", { class: "iconbtn sm" });
  toggle.addEventListener("click", () => {
    if (isRunning(exit)) void run(() => stopExit(exit.local), `disconnect ${exit.name}`);
    else void run(() => startExit(exit.local), `connect ${exit.name}`);
  });

  const restart = h("button", { class: "iconbtn sm", html: GLYPH_RESTART, title: "Reconnect" });
  restart.addEventListener("click", () =>
    void run(() => restartExit(exit.local), `reconnect ${exit.name}`),
  );

  const edit = h("button", { class: "iconbtn sm", html: GLYPH_EDIT, title: "Edit" });
  edit.addEventListener("click", () => openTunnelSheet(source, exit));

  const main = h("div", { class: "card-main" }, [
    dot,
    h("div", { class: "card-id" }, [name, route]),
    test,
    h("div", { class: "card-actions" }, [toggle, restart, edit]),
  ]);

  const root = h("article", { class: "card" }, [main, detail]);
  root.dataset.local = String(exit.local);

  cardRefs.set(exit.local, { root, dot, test, detail, toggle });
  return root;
}

function renderCards() {
  const box = el<HTMLDivElement>("cards");
  box.textContent = "";
  cardRefs.clear();

  const src = currentSource();
  box.classList.remove("grouped");
  if (!src) return;

  const exits = visibleExits(src);
  // 有隧道列才套群組外框，零隧道時留給虛線引導卡自己的樣子
  box.classList.toggle("grouped", exits.length > 0);
  for (const exit of exits) {
    box.appendChild(buildCard(exit, src.name));
    paintCard(exit);
  }

  if (exits.length === 0) {
    // 大虛線卡只在這條連線零隧道時出現，其餘時候用 ⋯ 選單的 Add tunnel
    const ghost = h("button", { class: "ghost-card" }, [
      h("span", { class: "ghost-plus", text: "+" }),
      h("span", { text: "Add tunnel" }),
    ]);
    ghost.addEventListener("click", beginCreate);
    box.appendChild(ghost);
  }
}

// ---------------------------------------------------------------- 新增隧道

function beginCreate() {
  const src = currentSource();
  if (src) openTunnelSheet(src.name, null);
}

// ---------------------------------------------------------------- 刪除／undo

/** sheet 的 Delete 鍵按下後走到這裡：畫面先移除，5 秒內都還能收回 */
function requestDelete(local: number) {
  const hit = locate(local);
  if (!hit) return;
  const name = hit.exit.name;
  const owner = hit.source.name;

  pendingDelete.add(local);
  render();

  const toast = showUndoToast(
    `Deleted tunnel ${name}`,
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
  undoToasts.set(local, { source: owner, toast });
}

/**
 * 整個源被刪掉時，底下出口還掛著的 undo 倒數就沒有意義了：
 * 讓它到期去 deleteForward 一個已經不存在的埠只會噴錯，
 * pendingDelete 裡的殘留旗標也會一直卡著。這裡一次收乾淨。
 */
function dropPendingDeletesOf(sourceName: string) {
  // 快照可能已經被 config-changed 洗掉這個源了，所以以登記的來源為準，
  // 快照裡還在的話就再併一次，兩邊都不漏
  const locals = new Set<number>();
  for (const [local, entry] of undoToasts) if (entry.source === sourceName) locals.add(local);
  const still = snap.sources.find((s) => s.name === sourceName);
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

/** 日誌只剩下獨立的活動頁一個出口，主區不再掛即時的小視窗 */
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

  snap = {
    ...next,
    sources: (next.sources ?? []).map((s) => ({
      ...s,
      exits: s.exits.map((e) => ({ ...e, detailText: keep.get(e.local) ?? null })),
    })),
  };

  if (replayLogs) logLines = [...(next.logs ?? [])].slice(-LOG_CAP);

  syncSettingsPage(snap);
  render();
}

function applyExitStatus(ev: ExitStatusEvent) {
  const hit = locate(ev.local);
  if (!hit) return;
  const { exit } = hit;
  exit.status = ev.status;
  exit.detailText = ev.detail ?? null;
  if (ev.status === "stopped") exit.lastTest = null;
  paintCard(exit);
  if (view === "source") renderSummary();
  paintRailStatus(hit.source.name);
}

function applyExitTest(ev: ExitTestEvent) {
  const hit = locate(ev.local);
  if (!hit) return;
  hit.exit.lastTest = { state: ev.state, text: ev.text };
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
      if (attempt === 19) appendLog(`ui error: ${String(e)}`);
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
  } catch (e) {
    appendLog(`ui error: ${String(e)}`);
  }
  await loadSnapshot();
}

installContextMenuGuard();

el<HTMLButtonElement>("btn-min").addEventListener("click", () =>
  void run(windowMinimize, "minimize the window"),
);
el<HTMLButtonElement>("btn-close").addEventListener("click", () =>
  void run(windowClose, "close the window"),
);

el<HTMLButtonElement>("btn-logs").addEventListener("click", () => setView("log"));
el<HTMLButtonElement>("btn-settings").addEventListener("click", () => setView("settings"));

initSummaryMenu();
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
    // 整條連線都沒了，底下那條隧道的 sheet 也不該留著
    closeTunnelSheet();
    dropPendingDeletesOf(name);
    // 同樣不等 config-changed，先把它從本地快照拿掉，
    // 免得回退第一個源時又挑回這個剛被刪掉的
    snap = { ...snap, sources: snap.sources.filter((s) => s.name !== name) };
    pendingSelect = null;
    if (selected === name) selected = null;
    view = "source";
    render();
  },
});
initTunnelSheet({ onDelete: requestDelete });
render();
// initSettingsPage() 一開頭就會問 get_config_path，dev-mock 是動態 import、
// 得等 bootstrap 把假後端裝好才問得到；正式版走真的 Tauri runtime，
// 這個順序不影響任何行為（invoke 一開始就能用）。
void bootstrap(init).then(() => initSettingsPage());
