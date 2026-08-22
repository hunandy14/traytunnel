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
