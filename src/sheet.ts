/**
 * 兩個 sheet dialog 與一個設定頁：
 *
 * 1. 連線的新增／編輯 sheet —— 置中覆蓋層。按下 Save 才送出，後端的錯誤字串用
 *    `field: message` 前綴逐欄顯示；刪除連線用一次確認（就地把頁腳換成確認列），
 *    不走 undo。
 * 2. 隧道的新增／編輯 sheet —— 同一套 .sheet 元件、同一套逐欄錯誤規則，操作手感
 *    與連線編輯一致。刪除走 undo toast（畫面先移除、5 秒內可收回），實際的倒數
 *    與復原由 main.ts 的 onDelete 接手。
 * 3. 主區的全域設定頁 —— 兩個 toggle 即時生效，失敗就把畫面翻回去。
 */

import { el } from "./dom";
import {
  deleteSource,
  getConfigPath,
  openConfigDir,
  setAutostart,
  setCloseToTray,
  testConnection,
  upsertForward,
  upsertSource,
} from "./ipc";
import type { ExitInfo, Snapshot, SourceInfo } from "./types";
import { loadAppVersion } from "./version";

// ---------------------------------------------------------------- sheet 共用

/**
 * 逐欄錯誤：每個 .field-<key> 裡固定有一個 .field-error 與一個 .input，
 * 訊息掛在前者、紅框掛在後者。空字串就是把錯誤清掉。
 */
function setFieldError(root: HTMLElement, key: string, msg: string) {
  const box = root.querySelector(`.field-${key}`) as HTMLElement;
  const err = box.querySelector(".field-error") as HTMLElement;
  err.textContent = msg;
  err.classList.toggle("show", Boolean(msg));
  (box.querySelector(".input") as HTMLElement).classList.toggle("invalid", Boolean(msg));
}

/** 認不出欄位前綴的錯誤放這裡，位置在按鈕上方 */
function setGeneralError(node: HTMLElement, msg: string) {
  node.textContent = msg;
  node.classList.toggle("show", Boolean(msg));
}

/** Test 按鈕的就地結果：成功綠字、失敗紅字，空字串就整行藏起來 */
function setTestResult(node: HTMLElement, msg: string, ok: boolean) {
  node.textContent = msg;
  node.classList.toggle("show", Boolean(msg));
  node.classList.toggle("ok", ok);
  node.classList.toggle("fail", !ok);
}

function showSheet(node: HTMLElement, focus: HTMLInputElement) {
  node.hidden = false;
  // 先掛上再加 class，進場的 opacity／scale 過渡才有起始幀
  requestAnimationFrame(() => node.classList.add("open"));
  window.setTimeout(() => focus.focus(), 60);
}

/** stillClosed 是為了防「關到一半又被打開」時把新開的那次藏掉 */
function hideSheet(node: HTMLElement, stillClosed: () => boolean) {
  node.classList.remove("open");
  const finish = () => {
    if (stillClosed()) node.hidden = true;
  };
  node.addEventListener("transitionend", finish, { once: true });
  window.setTimeout(finish, 400);
}

// ---------------------------------------------------------------- 連線 sheet

type SourceField = "name" | "host" | "user" | "proxyCommand";

const FIELDS: SourceField[] = ["name", "host", "user", "proxyCommand"];

const INPUT_ID: Record<SourceField, string> = {
  name: "src-name",
  host: "src-host",
  user: "src-user",
  proxyCommand: "src-proxy",
};

const backdrop = () => el<HTMLDivElement>("src-backdrop");
const input = (f: SourceField) => el<HTMLInputElement>(INPUT_ID[f]);

interface Handlers {
  /** 存檔成功，帶回最終的連線名稱（改名後是新名字） */
  onSaved: (name: string) => void;
  onDeleted: (name: string) => void;
}

let handlers: Handlers = { onSaved: () => {}, onDeleted: () => {} };
let open = false;
let busy = false;
/** null 代表這是「新增」 */
let originalName: string | null = null;

let testBusy = false;
/**
 * 每次開關 sheet 都遞增，測試中途關掉 sheet 就讓當初送出的那次結果作廢，
 * 不必真的取消後端探測——反正它自己有 15 秒兜底逾時，函式結束就收乾淨。
 */
let testGeneration = 0;

function clearErrors() {
  for (const f of FIELDS) setFieldError(backdrop(), f, "");
  setGeneralError(el<HTMLDivElement>("src-error"), "");
}

function clearTestResult() {
  setTestResult(el<HTMLDivElement>("src-test-result"), "", true);
}

function setTestBusy(next: boolean) {
  const btn = el<HTMLButtonElement>("src-test");
  testBusy = next;
  btn.disabled = next;
  btn.classList.toggle("loading", next);
}

/** 後端回傳的錯誤字串約定用 `field: message` 開頭，認不出前綴就當成整體錯誤 */
function assignError(msg: string) {
  const m = /^\s*(name|host|user|proxycommand)\s*:\s*([\s\S]+)$/i.exec(msg);
  if (!m) {
    setGeneralError(el<HTMLDivElement>("src-error"), msg);
    return;
  }
  // 欄位鍵是 camelCase，比對過的前綴要轉回來
  const lower = m[1].toLowerCase();
  const key: SourceField = lower === "proxycommand" ? "proxyCommand" : (lower as SourceField);
  setFieldError(backdrop(), key, m[2].trim());
}

/** 送出前先做一輪本地檢查，訊息與後端用同一套欄位前綴 */
function localValidate(): Partial<Record<SourceField, string>> {
  const errors: Partial<Record<SourceField, string>> = {};
  const name = input("name").value.trim();
  const host = input("host").value.trim();
  const user = input("user").value.trim();
  if (!name) errors.name = "name is required";
  if (!host) errors.host = "host is required";
  else if (/\s/.test(host)) errors.host = "must not contain spaces";
  if (!user) errors.user = "user is required";
  return errors;
}

function showFoot(mode: "edit" | "confirm") {
  el<HTMLElement>("src-foot").hidden = mode !== "edit";
  el<HTMLElement>("src-confirm").hidden = mode !== "confirm";
}

export function openSourceSheet(src: SourceInfo | null) {
  originalName = src ? src.name : null;
  busy = false;

  input("name").value = src?.name ?? "";
  input("host").value = src?.host ?? "";
  input("user").value = src?.user ?? "";
  input("proxyCommand").value = src?.proxyCommand ?? "";

  el<HTMLSpanElement>("src-title").textContent = src ? "Edit connection" : "Add connection";
  el<HTMLButtonElement>("src-save").textContent = src ? "Save" : "Add";
  el<HTMLButtonElement>("src-delete").hidden = !src;
  el<HTMLSpanElement>("src-confirm-text").textContent = src
    ? `Delete ${src.name} and its ${src.exits.length} tunnel${src.exits.length === 1 ? "" : "s"}?`
    : "Delete this connection?";

  clearErrors();
  clearTestResult();
  setTestBusy(false);
  testGeneration++;
  showFoot("edit");
  open = true;
  showSheet(backdrop(), input("name"));
}

function closeSourceSheet() {
  if (!open) return;
  open = false;
  // 讓還在飛的測試結果作廢，reopen 之後 gen 對不上就不會被顯示出來
  testGeneration++;
  hideSheet(backdrop(), () => !open);
}

async function save() {
  if (busy) return;
  clearErrors();

  const errors = localValidate();
  const keys = Object.keys(errors) as SourceField[];
  if (keys.length > 0) {
    for (const k of keys) setFieldError(backdrop(), k, errors[k] as string);
    return;
  }

  const name = input("name").value.trim();
  busy = true;
  try {
    const err = await upsertSource({
      originalName,
      name,
      host: input("host").value.trim(),
      user: input("user").value.trim(),
      proxyCommand: input("proxyCommand").value.trim(),
    });
    busy = false;
    if (err) {
      assignError(err);
      return;
    }
    closeSourceSheet();
    handlers.onSaved(name);
  } catch (e) {
    busy = false;
    setGeneralError(el<HTMLDivElement>("src-error"), String(e));
  }
}

/**
 * 存檔前的連線測試：拿表單「當下」填的值探測，不必先存檔。
 * 前置驗證只看 host／user（跟 localValidate 用同一套訊息），空白就地顯示
 * 錯誤、不 spawn；name 是否合法與這裡無關。
 */
async function testConnectionNow() {
  if (testBusy) return;
  clearTestResult();

  const errors = localValidate();
  const relevant: Partial<Record<SourceField, string>> = {};
  if (errors.host) relevant.host = errors.host;
  if (errors.user) relevant.user = errors.user;
  const keys = Object.keys(relevant) as SourceField[];
  if (keys.length > 0) {
    for (const k of keys) setFieldError(backdrop(), k, relevant[k] as string);
    return;
  }

  // gen 對不上就代表 sheet 中途被關掉或重開過（testGeneration 已經前進），
  // 這時連按鈕狀態都不去動——它早被 open/close 那一刻重置過，不能讓晚到的
  // 這次回應蓋掉正在跑的下一輪測試。
  const gen = testGeneration;
  setTestBusy(true);
  try {
    const result = await testConnection({
      host: input("host").value.trim(),
      user: input("user").value.trim(),
      proxyCommand: input("proxyCommand").value.trim(),
    });
    if (gen !== testGeneration) return;
    setTestBusy(false);
    setTestResult(el<HTMLDivElement>("src-test-result"), result.message, result.ok);
  } catch (e) {
    if (gen !== testGeneration) return;
    setTestBusy(false);
    setTestResult(el<HTMLDivElement>("src-test-result"), String(e), false);
  }
}

async function commitDelete() {
  const target = originalName;
  if (!target || busy) return;
  busy = true;
  try {
    await deleteSource(target);
    busy = false;
    closeSourceSheet();
    handlers.onDeleted(target);
  } catch (e) {
    busy = false;
    showFoot("edit");
    setGeneralError(el<HTMLDivElement>("src-error"), String(e));
  }
}

export function initSourceSheet(h: Handlers) {
  handlers = h;

  backdrop().addEventListener("mousedown", (e) => {
    if (e.target === backdrop()) closeSourceSheet();
  });
  el<HTMLButtonElement>("src-close").addEventListener("click", closeSourceSheet);
  el<HTMLButtonElement>("src-cancel").addEventListener("click", closeSourceSheet);
  el<HTMLButtonElement>("src-save").addEventListener("click", () => void save());
  el<HTMLButtonElement>("src-test").addEventListener("click", () => void testConnectionNow());

  el<HTMLButtonElement>("src-delete").addEventListener("click", () => showFoot("confirm"));
  el<HTMLButtonElement>("src-confirm-no").addEventListener("click", () => showFoot("edit"));
  el<HTMLButtonElement>("src-confirm-yes").addEventListener("click", () => void commitDelete());

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && open) closeSourceSheet();
  });

  for (const f of FIELDS) {
    const node = input(f);
    node.addEventListener("input", () => {
      setFieldError(backdrop(), f, "");
      // 欄位改過了，舊的測試結果已經不對這一份表單負責，藏起來避免誤導
      clearTestResult();
    });
    node.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void save();
    });
  }
}

// ---------------------------------------------------------------- 隧道 sheet

type ForwardField = "name" | "local" | "remote";

const FWD_FIELDS: ForwardField[] = ["name", "local", "remote"];

const FWD_INPUT_ID: Record<ForwardField, string> = {
  name: "fwd-name",
  local: "fwd-local",
  remote: "fwd-remote",
};

const fwdBackdrop = () => el<HTMLDivElement>("fwd-backdrop");
const fwdInput = (f: ForwardField) => el<HTMLInputElement>(FWD_INPUT_ID[f]);

interface TunnelHandlers {
  /** 刪除鍵：sheet 先關掉，undo toast 的倒數與復原交給 main.ts */
  onDelete: (local: number) => void;
}

let fwdHandlers: TunnelHandlers = { onDelete: () => {} };
let fwdOpen = false;
let fwdBusy = false;
/** 這條隧道掛在哪條連線底下 */
let fwdSource = "";
/** null 代表這是「新增」 */
let fwdOriginalLocal: number | null = null;

function fwdClearErrors() {
  for (const f of FWD_FIELDS) setFieldError(fwdBackdrop(), f, "");
  setGeneralError(el<HTMLDivElement>("fwd-error"), "");
}

function fwdAssignError(msg: string) {
  const m = /^\s*(name|local|remote)\s*:\s*([\s\S]+)$/i.exec(msg);
  if (m) setFieldError(fwdBackdrop(), m[1].toLowerCase(), m[2].trim());
  else setGeneralError(el<HTMLDivElement>("fwd-error"), msg);
}

const isPort = (v: string) => /^\d+$/.test(v) && Number(v) >= 1 && Number(v) <= 65535;

/**
 * 送出前先做一輪本地檢查，訊息與後端用同一套欄位前綴。
 * remote 允許只填埠號（那就是伺服器本機的那個埠），正規化成 host:port 由後端做。
 */
function fwdLocalValidate(): Partial<Record<ForwardField, string>> {
  const errors: Partial<Record<ForwardField, string>> = {};
  if (!fwdInput("name").value.trim()) errors.name = "name is required";
  if (!isPort(fwdInput("local").value.trim())) errors.local = "must be 1-65535";

  const remote = fwdInput("remote").value.trim();
  if (!remote) errors.remote = "remote is required";
  else if (/^\d+$/.test(remote)) {
    if (!isPort(remote)) errors.remote = "must be 1-65535";
  } else if (!/^[^\s:]+:\d+$/.test(remote)) {
    errors.remote = "expected a port or host:port";
  }
  return errors;
}

export function openTunnelSheet(source: string, exit: ExitInfo | null) {
  fwdSource = source;
  fwdOriginalLocal = exit ? exit.local : null;
  fwdBusy = false;

  fwdInput("name").value = exit?.name ?? "";
  fwdInput("local").value = exit ? String(exit.local) : "";
  fwdInput("remote").value = exit?.remote ?? "";

  el<HTMLSpanElement>("fwd-title").textContent = exit ? "Edit tunnel" : "Add tunnel";
  el<HTMLButtonElement>("fwd-save").textContent = exit ? "Save" : "Add";
  el<HTMLButtonElement>("fwd-delete").hidden = !exit;

  fwdClearErrors();
  fwdOpen = true;
  showSheet(fwdBackdrop(), fwdInput("name"));
}

export function closeTunnelSheet() {
  if (!fwdOpen) return;
  fwdOpen = false;
  hideSheet(fwdBackdrop(), () => !fwdOpen);
}

async function fwdSave() {
  if (fwdBusy) return;
  fwdClearErrors();

  const errors = fwdLocalValidate();
  const keys = Object.keys(errors) as ForwardField[];
  if (keys.length > 0) {
    for (const k of keys) setFieldError(fwdBackdrop(), k, errors[k] as string);
    return;
  }

  fwdBusy = true;
  try {
    const err = await upsertForward({
      source: fwdSource,
      originalLocal: fwdOriginalLocal,
      name: fwdInput("name").value.trim(),
      local: Number(fwdInput("local").value.trim()),
      remote: fwdInput("remote").value.trim(),
    });
    fwdBusy = false;
    if (err) {
      fwdAssignError(err);
      return;
    }
    closeTunnelSheet();
  } catch (e) {
    fwdBusy = false;
    setGeneralError(el<HTMLDivElement>("fwd-error"), String(e));
  }
}

export function initTunnelSheet(h: TunnelHandlers) {
  fwdHandlers = h;

  fwdBackdrop().addEventListener("mousedown", (e) => {
    if (e.target === fwdBackdrop()) closeTunnelSheet();
  });
  el<HTMLButtonElement>("fwd-close").addEventListener("click", closeTunnelSheet);
  el<HTMLButtonElement>("fwd-cancel").addEventListener("click", closeTunnelSheet);
  el<HTMLButtonElement>("fwd-save").addEventListener("click", () => void fwdSave());

  el<HTMLButtonElement>("fwd-delete").addEventListener("click", () => {
    const local = fwdOriginalLocal;
    if (local === null) return;
    closeTunnelSheet();
    fwdHandlers.onDelete(local);
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && fwdOpen) closeTunnelSheet();
  });

  for (const f of FWD_FIELDS) {
    const node = fwdInput(f);
    node.addEventListener("input", () => setFieldError(fwdBackdrop(), f, ""));
    node.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void fwdSave();
    });
  }
}

// ---------------------------------------------------------------- 設定頁

const tgClose = () => el<HTMLButtonElement>("tg-close");
const tgAutostart = () => el<HTMLButtonElement>("tg-autostart");

function setToggle(node: HTMLElement, on: boolean) {
  node.classList.toggle("on", on);
  node.setAttribute("aria-checked", String(on));
}

function settingsError(msg: string) {
  setGeneralError(el<HTMLDivElement>("settings-error"), msg);
}

/** 後端推了新設定就把 toggle 對齊回去 */
export function syncSettingsPage(snap: Snapshot) {
  setToggle(tgClose(), snap.closeToTray);
  setToggle(tgAutostart(), snap.autostart);
}

function wireToggle(node: HTMLElement, apply: (on: boolean) => Promise<unknown>) {
  node.addEventListener("click", async () => {
    const next = !node.classList.contains("on");
    setToggle(node, next);
    settingsError("");
    try {
      await apply(next);
    } catch (e) {
      setToggle(node, !next);
      settingsError(String(e));
    }
  });
}

/**
 * About 的「Config file」一列：本身是非互動列，只顯示實際生效的完整路徑；
 * 開檔案總管的動作收進右側獨立的圖示按鈕，路徑問不到之前先停用它。
 * 路徑問不到時（後端還沒起來之類）留一個破折號，不讓這一列開天窗。
 * dev-mock 模式的假路徑與 no-op 由 mockIPC 那邊給。
 */
function initConfigPathRow() {
  const label = el<HTMLDivElement>("config-path");
  const openBtn = el<HTMLButtonElement>("btn-open-config-dir");

  void getConfigPath()
    .then((p) => {
      label.textContent = p;
      // 省略號會吃掉路徑尾巴，滑過去至少看得到全文
      label.title = p;
      openBtn.disabled = false;
    })
    .catch(() => {
      label.textContent = "—";
    });

  openBtn.addEventListener("click", () => {
    void openConfigDir().catch((e) => settingsError(String(e)));
  });
}

export function initSettingsPage() {
  wireToggle(tgClose(), setCloseToTray);
  wireToggle(tgAutostart(), setAutostart);
  initConfigPathRow();
  void loadAppVersion().then((v) => {
    el<HTMLSpanElement>("app-version").textContent = v;
  });
}
