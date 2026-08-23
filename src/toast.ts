/**
 * 刪除用的 undo toast。
 *
 * 刪除不跳確認框：畫面上先把卡片拿掉，倒數結束才真的呼叫 delete_forward，
 * 期間按下 Undo 就整個取消。倒數條是純 CSS 動畫，時間與 timer 用同一個常數。
 */

import { afterTransition, el, h } from "./dom";

const UNDO_MS = 5000;

export interface UndoToast {
  /**
   * 立刻結束倒數並執行 commit（例如視窗要關掉前）。
   * 回傳 commit 完成的 Promise，讓呼叫端（目前是關窗前的 flushPendingDeletes）
   * 有機會等它真的做完再繼續，不必假設同一輪 tick 就能派送出去。
   */
  flush: () => Promise<void>;
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
  /** 回傳 fn() 的 Promise，讓 flush() 能等 commit 真的做完 */
  const finish = (fn: () => void | Promise<void>): Promise<void> => {
    if (done) return Promise.resolve();
    done = true;
    window.clearTimeout(timer);
    toast.classList.remove("in");
    afterTransition(toast, () => toast.remove());
    return Promise.resolve(fn());
  };

  const timer = window.setTimeout(() => void finish(onCommit), UNDO_MS);
  undoBtn.addEventListener("click", () => void finish(onUndo));

  return {
    flush: () => finish(onCommit),
    dismiss: () => void finish(() => {}),
  };
}
