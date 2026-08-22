/**
 * 兩個「全域級」的表單：
 *
 * 1. 源的新增／編輯 sheet —— 情境式編輯，維持置中覆蓋層。按下 Save 才送出，
 *    後端的錯誤字串用 `field: message` 前綴逐欄顯示；刪除源用一次確認
 *    （就地把頁腳換成確認列），不走 undo。
 * 2. 主區的全域設定頁 —— 兩個 toggle 即時生效，失敗就把畫面翻回去。
 */

import { el } from "./dom";
import {
  deleteSource,
  getConfigPath,
  openConfigDir,
  setAutostart,
  setCloseToTray,
  upsertSource,
} from "./ipc";
import type { Snapshot, SourceInfo } from "./types";
import { loadAppVersion } from "./version";

// ---------------------------------------------------------------- 源 sheet

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
  /** 存檔成功，帶回最終的源名稱（改名後是新名字） */
  onSaved: (name: string) => void;
  onDeleted: (name: string) => void;
}

let handlers: Handlers = { onSaved: () => {}, onDeleted: () => {} };
let open = false;
let busy = false;
/** null 代表這是「新增」 */
let originalName: string | null = null;

function fieldBox(f: SourceField): HTMLElement {
  return backdrop().querySelector(`.field-${f}`) as HTMLElement;
}

function setFieldError(f: SourceField, msg: string) {
  const box = fieldBox(f);
  const err = box.querySelector(".field-error") as HTMLElement;
  err.textContent = msg;
  err.classList.toggle("show", Boolean(msg));
  (box.querySelector(".input") as HTMLElement).classList.toggle("invalid", Boolean(msg));
}

function clearErrors() {
  for (const f of FIELDS) setFieldError(f, "");
  const general = el<HTMLDivElement>("src-error");
  general.textContent = "";
  general.classList.remove("show");
}

function showGeneral(msg: string) {
  const general = el<HTMLDivElement>("src-error");
  general.textContent = msg;
  general.classList.toggle("show", Boolean(msg));
}

/** 後端回傳的錯誤字串約定用 `field: message` 開頭，認不出前綴就當成整體錯誤 */
function assignError(msg: string) {
  const m = /^\s*(name|host|user|proxycommand)\s*:\s*([\s\S]+)$/i.exec(msg);
  if (!m) {
    showGeneral(msg);
    return;
  }
  // 欄位鍵是 camelCase，比對過的前綴要轉回來
  const lower = m[1].toLowerCase();
  const key: SourceField = lower === "proxycommand" ? "proxyCommand" : (lower as SourceField);
  setFieldError(key, m[2].trim());
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

export const isSourceSheetOpen = () => open;

export function openSourceSheet(src: SourceInfo | null) {
  originalName = src ? src.name : null;
  busy = false;

  input("name").value = src?.name ?? "";
  input("host").value = src?.host ?? "";
  input("user").value = src?.user ?? "";
  input("proxyCommand").value = src?.proxyCommand ?? "";

  el<HTMLSpanElement>("src-title").textContent = src ? `Edit ${src.name}` : "Add source";
  el<HTMLButtonElement>("src-save").textContent = src ? "Save" : "Add";
  el<HTMLButtonElement>("src-delete").hidden = !src;
  el<HTMLSpanElement>("src-confirm-text").textContent = src
    ? `Delete ${src.name} and its ${src.exits.length} exit${src.exits.length === 1 ? "" : "s"}?`
    : "Delete this source?";

  clearErrors();
  showFoot("edit");

  const node = backdrop();
  node.hidden = false;
  open = true;
  requestAnimationFrame(() => node.classList.add("open"));
  window.setTimeout(() => input("name").focus(), 60);
}

export function closeSourceSheet() {
  if (!open) return;
  open = false;
  const node = backdrop();
  node.classList.remove("open");
  node.addEventListener("transitionend", () => {
    if (!open) node.hidden = true;
  }, { once: true });
  window.setTimeout(() => {
    if (!open) node.hidden = true;
  }, 400);
}

async function save() {
  if (busy) return;
  clearErrors();

  const errors = localValidate();
  const keys = Object.keys(errors) as SourceField[];
  if (keys.length > 0) {
    for (const k of keys) setFieldError(k, errors[k] as string);
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
    showGeneral(String(e));
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
    showGeneral(String(e));
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

  el<HTMLButtonElement>("src-delete").addEventListener("click", () => showFoot("confirm"));
  el<HTMLButtonElement>("src-confirm-no").addEventListener("click", () => showFoot("edit"));
  el<HTMLButtonElement>("src-confirm-yes").addEventListener("click", () => void commitDelete());

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && open) closeSourceSheet();
  });

  for (const f of FIELDS) {
    const node = input(f);
    node.addEventListener("input", () => setFieldError(f, ""));
    node.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void save();
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
  const box = el<HTMLDivElement>("settings-error");
  box.textContent = msg;
  box.classList.toggle("show", Boolean(msg));
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
 * About 的「Config file」一列：副標顯示實際生效的完整路徑，整列點下去
 * 就開檔案總管並選中它。路徑問不到時（後端還沒起來之類）留一個破折號，
 * 不讓這一列開天窗。dev-mock 模式的假路徑與 no-op 由 mockIPC 那邊給。
 */
function initConfigPathRow() {
  const label = el<HTMLDivElement>("config-path");
  void getConfigPath()
    .then((p) => {
      label.textContent = p;
      // 省略號會吃掉路徑尾巴，滑過去至少看得到全文
      label.title = p;
    })
    .catch(() => {
      label.textContent = "—";
    });

  el<HTMLButtonElement>("row-config-path").addEventListener("click", () => {
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
