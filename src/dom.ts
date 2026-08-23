/** 極小的 DOM 建構工具，省掉整份檔案裡重複的 createElement 樣板。 */

export const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

interface Options {
  class?: string;
  text?: string;
  title?: string;
  attrs?: Record<string, string>;
}

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  opts: Options = {},
  children: (Node | null | undefined)[] = [],
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (opts.class) node.className = opts.class;
  if (opts.text !== undefined) node.textContent = opts.text;
  if (opts.title) node.title = opts.title;
  for (const [k, v] of Object.entries(opts.attrs ?? {})) node.setAttribute(k, v);
  for (const c of children) if (c) node.appendChild(c);
  return node;
}

/**
 * 畫一顆 `.toggle` 開關：視覺的 `on` class 與無障礙的 aria-checked 必須一起
 * 動，缺了後者螢幕閱讀器讀到的永遠是同一個狀態。整份程式的開關（設定頁三顆、
 * 列開關、連線總開關、表單裡的「目的地是代理」）都走這一支，不要再各自手寫
 * ——之前四處拷貝裡就有一處漏掉了 aria-checked。
 *
 * titles 給了就順便把 tooltip 換成對應的動作詞（[開著時, 關著時]）。
 */
export function setToggle(
  node: HTMLElement,
  on: boolean,
  titles?: readonly [onTitle: string, offTitle: string],
): void {
  node.classList.toggle("on", on);
  node.setAttribute("aria-checked", String(on));
  if (titles) node.title = on ? titles[0] : titles[1];
}

/** 開關現在是開著的嗎——讀的是同一個 `on` class，跟 setToggle 對稱 */
export const isToggleOn = (node: HTMLElement) => node.classList.contains("on");

/**
 * 過渡動畫結束後跑 fn，transitionend 與 setTimeout 雙保險——transitionend
 * 可能因為元素中途被隱藏、沒有實際變化的屬性、瀏覽器怪癖等原因不觸發，
 * 保底的 timeout 用同樣的時長確保 fn 一定會被叫到一次。
 *
 * 兩條路徑都可能各自呼叫一次 fn，呼叫端要自己保證 fn 重複執行是安全的
 * （目前兩處用法都是：一次是移除節點，另一次是判斷條件後才生效）。
 */
export function afterTransition(node: HTMLElement, fn: () => void, timeoutMs = 400): void {
  node.addEventListener("transitionend", fn, { once: true });
  window.setTimeout(fn, timeoutMs);
}
