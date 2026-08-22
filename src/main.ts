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
  upsertForward,
  windowClose,
  windowMinimize,
} from "./ipc";
import {
  initSettingsPage,
  initSourceSheet,
  openSourceSheet,
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

interface Draft {
  /** 這張草稿屬於哪個源 */
  source: string;
  /** null 代表這是「新增」中的草稿 */
  originalLocal: number | null;
  name: string;
  local: string;
  remote: string;
  errors: Partial<Record<"name" | "local" | "remote" | "general", string>>;
  busy: boolean;
}

let draft: Draft | null = null;

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

/** 這一輪 renderCards 裡要展開的那張卡 */
let openNode: HTMLElement | null = null;
/** 只有「剛按下編輯／新增」那次才播展開動畫，重繪（例如顯示驗證錯誤）不重播 */
let animateOpen = false;

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

  const add = h("button", { class: "rail-btn add", text: "+", title: "Add source" });
  add.addEventListener("click", () => openSourceSheet(null));
  list.appendChild(add);

  el<HTMLButtonElement>("btn-logs").classList.toggle("active", view === "log");
  el<HTMLButtonElement>("btn-settings").classList.toggle("active", view === "settings");
}

// ---------------------------------------------------------------- 視圖切換

function setView(next: View) {
  if (view !== next && draft) cancelEditNow();
  closeMenu();
  // 使用者自己動了畫面，等快照的旗標就作廢，免得等不到時永遠卡著
  pendingSelect = null;
  view = next;
  render();
}

function selectSource(name: string) {
  if (selected !== name && draft) cancelEditNow();
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
  // 存檔後的名字一旦出現在快照裡就切過去，切完才解除等待
  if (pendingSelect !== null && snap.sources.some((s) => s.name === pendingSelect)) {
    selected = pendingSelect;
    pendingSelect = null;
  }

  // 選中的源被刪掉或還沒選過，就落回第一個；
  // 但還在等 config-changed 時不回退，否則改名會被打回舊的源
  if (pendingSelect === null && !currentSource()) selected = sources()[0]?.name ?? null;

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
  edit.addEventListener("click", () => beginEdit(exit, source));

  const main = h("div", { class: "card-main" }, [
    dot,
    h("div", { class: "card-id" }, [name, route]),
    test,
    h("div", { class: "card-actions" }, [toggle, restart, edit]),
  ]);

  const root = h("article", { class: "card" }, [main, detail]);
  root.dataset.local = String(exit.local);

  cardRefs.set(exit.local, { root, dot, test, detail, toggle });

  if (draft && draft.originalLocal === exit.local) {
    root.appendChild(buildEditor());
    openNode = root;
  }
  return root;
}

function renderCards() {
  const box = el<HTMLDivElement>("cards");
  box.textContent = "";
  cardRefs.clear();
  openNode = null;

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

  if (draft && draft.originalLocal === null) {
    const card = h("article", { class: "card new" }, [buildEditor()]);
    box.appendChild(card);
    openNode = card;
  } else if (exits.length === 0) {
    // 大虛線卡只在該源零出口時出現，其餘時候用彙總列的 ＋ 新增
    const ghost = h("button", { class: "ghost-card" }, [
      h("span", { class: "ghost-plus", text: "+" }),
      h("span", { text: "Add exit" }),
    ]);
    ghost.addEventListener("click", beginCreate);
    box.appendChild(ghost);
  }

  if (openNode) {
    const node = openNode;
    if (animateOpen) {
      animateOpen = false;
      // 節點剛進 DOM，要先讓瀏覽器把 0fr 的起始狀態畫出來，下一幀再切 1fr，
      // 否則兩個值同一幀套上去，transition 不會有中間幀
      requestAnimationFrame(() => requestAnimationFrame(() => node.classList.add("editing")));
    } else {
      node.classList.add("editing");
    }
  }
}

// ---------------------------------------------------------------- 就地編輯

function beginEdit(exit: ExitInfo, source: string) {
  draft = {
    source,
    originalLocal: exit.local,
    name: exit.name,
    local: String(exit.local),
    remote: exit.remote,
    errors: {},
    busy: false,
  };
  animateOpen = true;
  renderCards();
}

function beginCreate() {
  const src = currentSource();
  if (!src) return;
  draft = {
    source: src.name,
    originalLocal: null,
    name: "",
    local: "",
    remote: "",
    errors: {},
    busy: false,
  };
  animateOpen = true;
  renderCards();
}

/**
 * 收合要等動畫跑完才能把節點抽掉，否則 renderCards 一重繪就是瞬間消失。
 * transitionend 沒來（例如頁面在背景）就靠逾時保底。
 */
function collapseEditor(after: () => void) {
  const node = document.querySelector<HTMLElement>("#cards .card.editing");
  if (!node) {
    after();
    return;
  }
  let done = false;
  const finish = () => {
    if (done) return;
    done = true;
    after();
  };
  node.classList.remove("editing");
  node.querySelector(".card-edit")?.addEventListener("transitionend", finish, { once: true });
  window.setTimeout(finish, 320);
}

function cancelEdit() {
  collapseEditor(() => {
    draft = null;
    renderCards();
  });
}

/** 切源／切頁時不播收合動畫，直接把草稿丟掉 */
function cancelEditNow() {
  draft = null;
}

function field(
  key: "name" | "local" | "remote",
  label: string,
  placeholder: string,
  mono = false,
): HTMLElement {
  const d = draft as Draft;
  const input = h("input", { class: mono ? "mono" : "" }) as HTMLInputElement;
  input.value = d[key];
  input.placeholder = placeholder;
  input.spellcheck = false;
  input.addEventListener("input", () => {
    d[key] = input.value;
    delete d.errors[key];
    err.textContent = "";
    err.classList.remove("show");
    wrap.classList.remove("invalid");
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") void commitEdit();
    if (e.key === "Escape") {
      e.stopPropagation();
      cancelEdit();
    }
  });

  const wrap = h("div", { class: "input" }, [input]);
  const err = h("div", { class: "field-error" });
  if (d.errors[key]) {
    err.textContent = d.errors[key] as string;
    err.classList.add("show");
    wrap.classList.add("invalid");
  }
  return h("label", { class: `edit-field field-${key}` }, [
    h("span", { class: "edit-label", text: label }),
    wrap,
    err,
  ]);
}

function buildEditor(): HTMLElement {
  const d = draft as Draft;

  const save = h("button", {
    class: "btn primary",
    text: d.originalLocal === null ? "Add" : "Save",
  });
  save.addEventListener("click", () => void commitEdit());

  const cancel = h("button", { class: "btn ghost", text: "Cancel" });
  cancel.addEventListener("click", cancelEdit);

  const actions: HTMLElement[] = [];
  if (d.originalLocal !== null) {
    const del = h("button", { class: "btn danger-ghost", text: "Delete" });
    del.addEventListener("click", () => requestDelete(d.originalLocal as number));
    actions.push(del);
  }

  const general = h("div", { class: "field-error general" });
  if (d.errors.general) {
    general.textContent = d.errors.general;
    general.classList.add("show");
  }

  // card-edit（grid 0fr→1fr）> inner（overflow hidden）> body（分隔線與內距，收合時一起被裁掉）
  return h("div", { class: "card-edit" }, [
    h("div", { class: "card-edit-inner" }, [
      h("div", { class: "card-edit-body" }, [
        h("div", { class: "edit-grid" }, [
          field("name", "Name", "exit-a"),
          field("local", "Local port", "1080", true),
          field("remote", "Remote", "127.0.0.1:1080", true),
        ]),
        general,
        h("div", { class: "edit-actions" }, [...actions, h("div", { class: "spacer" }), cancel, save]),
      ]),
    ]),
  ]);
}

/** 送出前先做一輪本地檢查，錯誤訊息與後端用同一套欄位前綴 */
function localValidate(d: Draft): Partial<Record<"name" | "local" | "remote", string>> {
  const errors: Partial<Record<"name" | "local" | "remote", string>> = {};
  if (!d.name.trim()) errors.name = "name is required";
  const port = Number(d.local.trim());
  if (!/^\d+$/.test(d.local.trim()) || port < 1 || port > 65535) {
    errors.local = "must be 1-65535";
  }
  if (!/^[^\s:]+:\d+$/.test(d.remote.trim())) errors.remote = "expected host:port";
  return errors;
}

/**
 * 後端回傳的錯誤字串約定用 `field: message` 開頭（name / local / remote），
 * 這樣才能逐欄顯示；認不出前綴就當成整體錯誤放在按鈕上方。
 */
function assignError(d: Draft, msg: string) {
  const m = /^\s*(name|local|remote)\s*:\s*([\s\S]+)$/i.exec(msg);
  if (m) d.errors[m[1].toLowerCase() as "name" | "local" | "remote"] = m[2].trim();
  else d.errors.general = msg;
}

async function commitEdit() {
  const d = draft;
  if (!d || d.busy) return;

  d.errors = {};
  const errors = localValidate(d);
  if (Object.keys(errors).length > 0) {
    d.errors = errors;
    renderCards();
    return;
  }

  d.busy = true;
  try {
    const err = await upsertForward({
      source: d.source,
      originalLocal: d.originalLocal,
      name: d.name.trim(),
      local: Number(d.local.trim()),
      remote: d.remote.trim(),
    });
    d.busy = false;
    if (err) {
      assignError(d, err);
      renderCards();
      return;
    }
    collapseEditor(() => {
      draft = null;
      renderCards();
    });
  } catch (e) {
    d.busy = false;
    assignError(d, String(e));
    renderCards();
  }
}

// ---------------------------------------------------------------- 刪除／undo

function requestDelete(local: number) {
  const hit = locate(local);
  if (!hit) return;
  const name = hit.exit.name;
  const owner = hit.source.name;

  collapseEditor(() => {
    draft = null;
    pendingDelete.add(local);
    render();

    const toast = showUndoToast(
      `Deleted ${name}`,
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
  });
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

/**
 * 後端在每行日誌前面就放好了 [源名]，前端只負責過濾：
 * 認得出前綴的行只在對應的源顯示，沒有前綴的 app 級訊息則永遠顯示。
 */
const PREFIX_RE = /^(?:\s*\d{1,2}:\d{2}(?::\d{2})?(?:\.\d+)?\s+)?\[([^\]]+)\]/;

function logSourceOf(line: string): string | null {
  const m = PREFIX_RE.exec(line);
  return m ? m[1].trim() : null;
}

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

function renderLogs() {
  if (view === "log") {
    fill(el<HTMLDivElement>("full-log"), logLines, "No activity yet");
    return;
  }
  if (view !== "source") return;

  const scope = el<HTMLSpanElement>("mini-log-scope");
  scope.textContent = selected ? selected : "";
  fill(
    el<HTMLDivElement>("mini-log"),
    logLines.filter((l) => {
      const s = logSourceOf(l);
      return s === null || s === selected;
    }),
    "No activity yet",
  );
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

  if (view === "log") {
    appendLine(el<HTMLDivElement>("full-log"), line);
    return;
  }
  if (view !== "source") return;
  const s = logSourceOf(line);
  if (s === null || s === selected) appendLine(el<HTMLDivElement>("mini-log"), line);
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
initSettingsPage();
render();
bootstrap(init);
