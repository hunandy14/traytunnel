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
