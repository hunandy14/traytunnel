/**
 * 刪除用的 undo toast。
 *
 * 刪除不跳確認框：畫面上先把卡片拿掉，倒數結束才真的呼叫 delete_forward，
 * 期間按下 Undo 就整個取消。倒數條是純 CSS 動畫，時間與 timer 用同一個常數。
 */

import { el, h } from "./dom";

const UNDO_MS = 5000;

export interface UndoToast {
  /** 立刻結束倒數並執行 commit（例如視窗要關掉前） */
  flush: () => void;
  /**
   * 直接收掉這個 toast，commit 與 undo 都不執行。
   * 用在「要刪的東西已經連根被刪掉了」這種情境，例如整個源被刪掉時，
   * 底下出口還掛著的 undo 倒數就沒有意義了。
   */
  dismiss: () => void;
}

/** 沒有 Undo 的一般提示，主要拿來報「刪除失敗」這種後端錯誤 */
export function showErrorToast(text: string) {
  const stack = el<HTMLDivElement>("toasts");
  const toast = h("div", { class: "toast error" }, [
    h("span", { class: "toast-text", text }),
  ]);
  stack.appendChild(toast);
  requestAnimationFrame(() => toast.classList.add("in"));
  window.setTimeout(() => {
    toast.classList.remove("in");
    window.setTimeout(() => toast.remove(), 400);
  }, UNDO_MS);
}

export function showUndoToast(
  text: string,
  /** 倒數結束才執行；失敗處理由呼叫端在這個 callback 內自己做完 */
  onCommit: () => void | Promise<void>,
  onUndo: () => void,
): UndoToast {
  const stack = el<HTMLDivElement>("toasts");

  const bar = h("div", { class: "toast-bar" });
  const undoBtn = h("button", { class: "btn ghost sm", text: "Undo" });
  const toast = h("div", { class: "toast" }, [
    h("span", { class: "toast-text", text }),
    undoBtn,
    bar,
  ]);
  stack.appendChild(toast);
  // 讓進場動畫有機會跑（先掛上再加 class）
  requestAnimationFrame(() => toast.classList.add("in"));

  let done = false;
  const finish = (fn: () => void | Promise<void>) => {
    if (done) return;
    done = true;
    window.clearTimeout(timer);
    toast.classList.remove("in");
    toast.addEventListener("transitionend", () => toast.remove(), { once: true });
    window.setTimeout(() => toast.remove(), 400);
    void fn();
  };

  const timer = window.setTimeout(() => finish(onCommit), UNDO_MS);
  undoBtn.addEventListener("click", () => finish(onUndo));

  return {
    flush: () => finish(onCommit),
    dismiss: () => finish(() => {}),
  };
}
