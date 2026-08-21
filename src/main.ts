import { bootstrap } from "./bootstrap";
import { el, h } from "./dom";
import {
  deleteForward,
  getState,
  onConfigChanged,
  onExitStatus,
  onExitTest,
  onLog,
  startAll,
  startExit,
  stopAll,
  stopExit,
  testAll,
  upsertForward,
  windowClose,
  windowMinimize,
} from "./ipc";
import { closeSheet, initSheet, isSheetOpen, openSheet, syncSheet } from "./sheet";
import { showErrorToast, showUndoToast } from "./toast";
import type { ExitInfo, ExitStatus, ExitStatusEvent, ExitTestEvent, Snapshot } from "./types";

// Segoe MDL2 Assets 的字元：E71A Stop、E768 Play、E713 齒輪、E72C 重新整理、E70F 鉛筆
const GLYPH_STOP = "";
const GLYPH_START = "";
const GLYPH_EDIT = "";

const EMPTY: Snapshot = {
  host: "",
  user: "",
  proxyCommand: "",
  closeToTray: true,
  autostart: false,
  exits: [],
  logs: [],
};

let snap: Snapshot = EMPTY;

/** 已按下刪除、但 undo 倒數還沒結束的出口：畫面先當它不存在 */
const pendingDelete = new Set<number>();

interface Draft {
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

/** 這一輪 renderCards 裡要展開的那張卡 */
let openNode: HTMLElement | null = null;
/** 只有「剛按下編輯／新增」那次才播展開動畫，重繪（例如顯示驗證錯誤）不重播 */
let animateOpen = false;

// ---------------------------------------------------------------- 狀態彙總

const RUNNING: ExitStatus[] = ["connecting", "connected", "reconnecting", "port_busy", "error"];

const isRunning = (e: ExitInfo) => RUNNING.includes(e.status);

function statusTone(status: ExitStatus): "grey" | "amber" | "green" | "red" {
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

function renderSummary() {
  const exits = visibleExits();
  const total = exits.length;
  const connected = exits.filter((e) => e.status === "connected").length;
  const busy = exits.filter((e) => e.status === "connecting" || e.status === "reconnecting").length;
  const bad = exits.filter((e) => e.status === "port_busy" || e.status === "error").length;
  const running = exits.some(isRunning);

  let text: string;
  let tone: "grey" | "amber" | "green" | "red";
  if (total === 0) {
    text = "No exits";
    tone = "grey";
  } else if (!running) {
    text = "Stopped";
    tone = "grey";
  } else {
    text = `${connected}/${total} Connected`;
    tone = bad > 0 ? "red" : busy > 0 ? "amber" : connected > 0 ? "green" : "grey";
  }

  el<HTMLDivElement>("summary-title").textContent = text;
  const dot = el<HTMLSpanElement>("summary-dot");
  dot.className = `dot tone-${tone}`;

  const sub = snap.host && snap.user ? `ssh ${snap.user}@${snap.host}` : "no host configured";
  el<HTMLDivElement>("summary-sub").textContent = sub;

  const toggle = el<HTMLButtonElement>("btn-toggle-all");
  toggle.textContent = running ? GLYPH_STOP : GLYPH_START;
  toggle.title = running ? "Stop all" : "Start all";
  toggle.classList.toggle("danger", running);
  toggle.classList.toggle("go", !running);
}

// ---------------------------------------------------------------- 出口卡片

const visibleExits = () => snap.exits.filter((e) => !pendingDelete.has(e.local));

function testLine(exit: ExitInfo): { text: string; tone: string } {
  const t = exit.lastTest;
  if (!t) return { text: "", tone: "muted" };
  if (t.state === "testing") return { text: t.text || "testing…", tone: "muted" };
  if (t.state === "fail") return { text: t.text || "no response", tone: "red" };
  return { text: t.text, tone: "text" };
}

function paintCard(exit: ExitInfo) {
  const refs = cardRefs.get(exit.local);
  if (!refs) return;

  refs.dot.className = `dot tone-${statusTone(exit.status)}`;
  refs.dot.title = exit.status;
  refs.root.dataset.status = exit.status;

  const t = testLine(exit);
  refs.test.textContent = t.text;
  refs.test.className = `card-test tone-text-${t.tone}`;

  const bad = exit.status === "port_busy" || exit.status === "error";
  const detail = bad ? (exit.detailText ?? defaultDetail(exit.status)) : "";
  refs.detail.textContent = detail;
  refs.detail.title = detail;
  refs.detail.classList.toggle("show", Boolean(detail));

  const running = isRunning(exit);
  refs.toggle.innerHTML = running ? GLYPH_STOP : GLYPH_START;
  refs.toggle.title = running ? "Disconnect" : "Connect";
  refs.toggle.classList.toggle("danger", running);
  refs.toggle.classList.toggle("go", !running);
}

/** 後端沒帶 detail 時至少讓紅點有句話可看 */
function defaultDetail(status: ExitStatus): string {
  return status === "port_busy" ? "local port is already in use" : "connection failed";
}

function buildCard(exit: ExitInfo): HTMLElement {
  const dot = h("span", { class: "dot" });
  const name = h("div", { class: "card-name", text: exit.name });
  const route = h("div", { class: "card-route", text: `:${exit.local} → ${exit.remote}` });
  const test = h("div", { class: "card-test" });
  const detail = h("div", { class: "card-detail" });

  const toggle = h("button", { class: "iconbtn sm" });
  toggle.addEventListener("click", () => {
    if (isRunning(exit)) void stopExit(exit.local);
    else void startExit(exit.local);
  });

  const edit = h("button", { class: "iconbtn sm", html: GLYPH_EDIT, title: "Edit" });
  edit.addEventListener("click", () => beginEdit(exit));

  const main = h("div", { class: "card-main" }, [
    dot,
    h("div", { class: "card-id" }, [name, route]),
    test,
    h("div", { class: "card-actions" }, [toggle, edit]),
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

// ---------------------------------------------------------------- 就地編輯

function beginEdit(exit: ExitInfo) {
  draft = {
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
  draft = { originalLocal: null, name: "", local: "", remote: "", errors: {}, busy: false };
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

  const save = h("button", { class: "btn primary", text: d.originalLocal === null ? "Add" : "Save" });
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
        h("div", { class: "edit-actions" }, [
          ...actions,
          h("div", { class: "spacer" }),
          cancel,
          save,
        ]),
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
  const exit = snap.exits.find((e) => e.local === local);
  if (!exit) return;
  const name = exit.name;

  collapseEditor(() => {
    draft = null;
    pendingDelete.add(local);
    renderCards();

    showUndoToast(
      `Deleted ${name}`,
      async () => {
        try {
          await deleteForward(local);
          // 刪成功才收掉暫存旗標，之後靠 config-changed 把卡片真的移除
          pendingDelete.delete(local);
        } catch (e) {
          // 後端拒絕就把卡片放回來，不要無聲復活
          pendingDelete.delete(local);
          renderCards();
          showErrorToast(`Could not delete ${name}: ${String(e)}`);
        }
      },
      () => {
        pendingDelete.delete(local);
        renderCards();
      },
    );
  });
}

// ---------------------------------------------------------------- 清單

function renderCards() {
  const box = el<HTMLDivElement>("cards");
  box.textContent = "";
  cardRefs.clear();
  openNode = null;

  for (const exit of visibleExits()) {
    box.appendChild(buildCard(exit));
    paintCard(exit);
  }

  if (draft && draft.originalLocal === null) {
    const card = h("article", { class: "card new" }, [buildEditor()]);
    box.appendChild(card);
    openNode = card;
  } else {
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

  renderSummary();
}

// ---------------------------------------------------------------- 事件套用

/**
 * replayLogs 只在最初載入時開：先掛 listen 再取 snapshot，中間漏接或重複的
 * log 事件就靠「清空後整份回放」收斂。config-changed 帶的 logs 不重播，
 * 否則每次改設定活動區都會被洗掉。
 */
function applySnapshot(next: Snapshot, replayLogs = false) {
  // detail 只在事件裡出現，快照重整時要保住已知的 detail 文字
  const keep = new Map(snap.exits.map((e) => [e.local, e.detailText]));
  snap = { ...next, exits: next.exits.map((e) => ({ ...e, detailText: keep.get(e.local) ?? null })) };
  if (replayLogs) {
    const box = el<HTMLDivElement>("log-box");
    box.textContent = "";
    for (const line of next.logs ?? []) appendLog(line);
  }
  syncSheet(snap);
  renderCards();
}

function applyExitStatus(ev: ExitStatusEvent) {
  const exit = snap.exits.find((e) => e.local === ev.local);
  if (!exit) return;
  exit.status = ev.status;
  exit.detailText = ev.detail ?? null;
  if (ev.status === "stopped") exit.lastTest = null;
  paintCard(exit);
  renderSummary();
}

function applyExitTest(ev: ExitTestEvent) {
  const exit = snap.exits.find((e) => e.local === ev.local);
  if (!exit) return;
  exit.lastTest = { state: ev.state, text: ev.text };
  paintCard(exit);
}

function appendLog(line: string) {
  const box = el<HTMLDivElement>("log-box");
  const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 4;
  box.appendChild(h("div", { text: line }));
  while (box.childElementCount > 500) box.removeChild(box.firstChild as ChildNode);
  if (atBottom) box.scrollTop = box.scrollHeight;
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
    await onConfigChanged(applySnapshot);
  } catch (e) {
    appendLog(`ui error: ${String(e)}`);
  }
  await loadSnapshot();
}

el<HTMLButtonElement>("btn-min").addEventListener("click", () => void windowMinimize());
el<HTMLButtonElement>("btn-close").addEventListener("click", () => void windowClose());
el<HTMLButtonElement>("btn-settings").addEventListener("click", () => {
  if (isSheetOpen()) closeSheet();
  else openSheet(snap);
});
el<HTMLButtonElement>("btn-retest").addEventListener("click", () => void testAll());
el<HTMLButtonElement>("btn-toggle-all").addEventListener("click", () => {
  if (visibleExits().some(isRunning)) void stopAll();
  else void startAll();
});

initSheet();
renderCards();
bootstrap(init);
