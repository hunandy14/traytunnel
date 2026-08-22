/**
 * 關掉 WebView2 預設的右鍵選單（View source／重新載入那一套），
 * 整個應用一律無選單——除了可編輯欄位，那裡還是要留右鍵貼上。
 */

const EDITABLE_SELECTOR = "input, textarea, [contenteditable]";

function isEditableTarget(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(EDITABLE_SELECTOR) !== null;
}

export function installContextMenuGuard() {
  document.addEventListener("contextmenu", (e) => {
    if (isEditableTarget(e.target)) return;
    e.preventDefault();
  });
}
