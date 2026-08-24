/**
 * 全 UI 的圖示出口（Lucide）。
 *
 * Lucide 的每顆圖示就是一份純資料的 IconNode 陣列，各自住在自己的模組裡，
 * 所以只有下面 ICONS 點名的那幾顆會進 bundle，其餘三千多顆被 tree-shake 掉。
 * 一律走 npm 套件內嵌，不打任何 CDN。
 *
 * 顏色刻意不設：createElement 的預設屬性是 stroke="currentColor"，
 * 既有的 .tone-* / .danger / .go 那套用 color 著色的機制就原樣生效。
 */

import {
  Check,
  ChevronDown,
  createElement,
  Download,
  Ellipsis,
  ExternalLink,
  FileText,
  FolderOpen,
  History,
  LoaderCircle,
  Minus,
  PanelBottom,
  Pencil,
  Plus,
  Power,
  RefreshCw,
  Settings,
  TriangleAlert,
  X,
  type IconNode,
} from "lucide";

/** 名字沿用 Lucide 官方的 kebab-case，方便對著 lucide.dev 查 */
const ICONS = {
  check: Check,
  "chevron-down": ChevronDown,
  download: Download,
  ellipsis: Ellipsis,
  "external-link": ExternalLink,
  "file-text": FileText,
  "folder-open": FolderOpen,
  history: History,
  "loader-circle": LoaderCircle,
  minus: Minus,
  "panel-bottom": PanelBottom,
  pencil: Pencil,
  plus: Plus,
  power: Power,
  "refresh-cw": RefreshCw,
  settings: Settings,
  "triangle-alert": TriangleAlert,
  x: X,
} satisfies Record<string, IconNode>;

export type IconName = keyof typeof ICONS;

/** 深色底上 1.75 比官方預設的 2 清爽，又不會像 1.5 那樣在 16px 糊掉 */
const STROKE = 1.75;
const SIZE = 16;

/** 產一顆 SVG。size 是像素邊長，圖示本身永遠是正方形的 24 格 viewBox。 */
export function icon(name: IconName, size: number = SIZE): SVGElement {
  return createElement(ICONS[name], {
    width: size,
    height: size,
    "stroke-width": STROKE,
    // 純裝飾：意義都由按鈕的 title／文字承擔，不要讓螢幕閱讀器多念一次
    "aria-hidden": "true",
    focusable: "false",
    class: "icon",
  });
}

/** 把節點原本的內容整個換成一顆圖示（狀態切換時重畫用） */
export function setIcon(node: Element, name: IconName, size: number = SIZE): void {
  node.replaceChildren(icon(name, size));
}

/**
 * 把 HTML 裡宣告式的圖示佔位填起來：
 * `<button data-icon="minus">` → 塞進 minus 的 SVG，`data-icon-size` 可覆寫邊長。
 *
 * 版面寫在 index.html、圖示只在這裡集中出圖，HTML 就不用夾任何字元實體。
 */
export function hydrateIcons(root: ParentNode = document): void {
  for (const node of root.querySelectorAll<HTMLElement>("[data-icon]")) {
    const name = node.dataset.icon as IconName;
    if (!(name in ICONS)) continue;
    const size = Number(node.dataset.iconSize) || SIZE;
    setIcon(node, name, size);
  }
}
