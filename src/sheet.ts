/**
 * 全域設定的置中 sheet 覆蓋層。
 *
 * 取代原本的第二個視窗：Host/User/ProxyCommand 按下 Save 才送出（錯誤字串顯示
 * 在面板內），兩個 toggle 則是即時生效，失敗就把畫面翻回去。
 */

import { el } from "./dom";
import { saveGlobal, setAutostart, setCloseToTray } from "./ipc";
import type { Snapshot } from "./types";

const backdrop = () => el<HTMLDivElement>("sheet-backdrop");
const inHost = () => el<HTMLInputElement>("in-host");
const inUser = () => el<HTMLInputElement>("in-user");
const inProxy = () => el<HTMLInputElement>("in-proxy");
const errBox = () => el<HTMLDivElement>("sheet-error");
const tgClose = () => el<HTMLButtonElement>("tg-close");
const tgAutostart = () => el<HTMLButtonElement>("tg-autostart");

let open = false;

function setToggle(node: HTMLElement, on: boolean) {
  node.classList.toggle("on", on);
  node.setAttribute("aria-checked", String(on));
}

function showError(msg: string) {
  const box = errBox();
  box.textContent = msg;
  box.classList.toggle("show", Boolean(msg));
}

export function isSheetOpen() {
  return open;
}

export function openSheet(snap: Snapshot) {
  inHost().value = snap.host;
  inUser().value = snap.user;
  inProxy().value = snap.proxyCommand;
  setToggle(tgClose(), snap.closeToTray);
  setToggle(tgAutostart(), snap.autostart);
  showError("");

  const node = backdrop();
  node.hidden = false;
  open = true;
  requestAnimationFrame(() => node.classList.add("open"));
  window.setTimeout(() => inHost().focus(), 60);
}

export function closeSheet() {
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

/** sheet 開著時後端又推了新設定，就把 toggle 對齊回去（輸入中的文字不動） */
export function syncSheet(snap: Snapshot) {
  if (!open) return;
  setToggle(tgClose(), snap.closeToTray);
  setToggle(tgAutostart(), snap.autostart);
}

function wireToggle(node: HTMLElement, apply: (on: boolean) => Promise<unknown>) {
  node.addEventListener("click", async () => {
    const next = !node.classList.contains("on");
    setToggle(node, next);
    showError("");
    try {
      await apply(next);
    } catch (e) {
      setToggle(node, !next);
      showError(String(e));
    }
  });
}

export function initSheet() {
  backdrop().addEventListener("mousedown", (e) => {
    if (e.target === backdrop()) closeSheet();
  });
  el<HTMLButtonElement>("sheet-close").addEventListener("click", closeSheet);
  el<HTMLButtonElement>("btn-sheet-cancel").addEventListener("click", closeSheet);

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && open) closeSheet();
  });

  wireToggle(tgClose(), setCloseToTray);
  wireToggle(tgAutostart(), setAutostart);

  el<HTMLButtonElement>("btn-sheet-save").addEventListener("click", async () => {
    showError("");
    try {
      const err = await saveGlobal({
        host: inHost().value.trim(),
        user: inUser().value.trim(),
        proxyCommand: inProxy().value.trim(),
      });
      if (err) showError(err);
      else closeSheet();
    } catch (e) {
      showError(String(e));
    }
  });

  // Enter 直接存檔，跟原本設定視窗的手感一致
  for (const input of [inHost(), inUser(), inProxy()]) {
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") el<HTMLButtonElement>("btn-sheet-save").click();
    });
  }
}
