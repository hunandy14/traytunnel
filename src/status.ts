/**
 * 出口／源狀態的純函式工具與卡片文字格式化，不碰 DOM。
 *
 * 這一層只依當下的出口狀態做判斷，不持有任何畫面簿記：pendingDelete
 * （哪些出口正在等 undo 倒數、畫面上先當它不存在）屬於 main.ts，
 * 因此 sourceTone 吃的是「已經濾好的出口清單」，過濾動作留在呼叫端
 * （main.ts 的 visibleExits）。
 */

import type { ConnRef, ExitInfo, ExitStatus } from "./types";

/** 這些狀態都算「連線中」，涵蓋成功與各種過渡／異常狀態 */
export const RUNNING: ExitStatus[] = [
  "connecting",
  "connected",
  "reconnecting",
  "port_busy",
  "error",
];

export const isRunning = (e: ExitInfo) => RUNNING.includes(e.status);
export const isBad = (e: ExitInfo) => e.status === "port_busy" || e.status === "error";

/**
 * 這一列要不要被探測（wg-design.md §5.4 的 should_probe）：`socks` 列是引擎
 * 自建的代理，恆真；`forward` 列看使用者勾的 probeProxy 旗標。
 *
 * 畫面（要不要留協定徽章與出口檢測那一格）與 dev-mock 的假自測排程共用這一份
 * 判斷，兩邊分別寫一份的話規則一改就會不同步。
 *
 * **過渡期的相容分支**：kind／probeProxy 是本輪才加進 ExitView 的欄位，引擎車道
 * 還沒落地，包裝版跑起來時後端送來的出口兩個欄位都不存在——那時 `kind` 是
 * undefined、`probeProxy` 是 undefined，一路 falsy 下去會讓**現有 SSH 出口的
 * 出口 IP 檢測行整排消失**（PR 之前是無條件顯示的）。所以舊形狀一律回 true，
 * 維持既有行為。後端補上這兩個欄位之後這條分支就可以拿掉。
 */
export const shouldProbe = (e: ExitInfo) =>
  e.kind === undefined || e.kind === "socks" || e.probeProxy;

export type Tone = "grey" | "amber" | "green" | "red";

export function statusTone(status: ExitStatus): Tone {
  switch (status) {
    case "connected":
      return "green";
    case "connecting":
    case "reconnecting":
      return "amber";
    case "port_busy":
    case "error":
      return "red";
    default:
      return "grey";
  }
}

/**
 * 源的彙總狀態：全連綠／部分琥珀／全停灰，任一出口出錯就直接紅。
 * exits 要是呼叫端先濾掉 pendingDelete 之後的清單。
 */
export function sourceTone(exits: ExitInfo[]): Tone {
  if (exits.length === 0) return "grey";
  if (exits.some(isBad)) return "red";
  if (!exits.some(isRunning)) return "grey";
  return exits.every((e) => e.status === "connected") ? "green" : "amber";
}

/**
 * 一條連線的健康度色調——左軌的小點、summary 的引擎點、中段統計分數三處
 * **共用這一支**，不要各自再兜一次條件。之前三個地方各算各的，結果同一條
 * 連線可以同時在左軌顯示灰、在 summary 顯示紅。
 *
 * 融合三層資訊，由重到輕：
 *
 *   1. `.conf` 壞掉（confError）→ 紅。那是連線本身的問題，不是列的彙總看得
 *      出來的，這種連線根本起不來。
 *   2. WG 引擎關著 → 灰。列的意圖還在，但引擎沒開它們不可能跑。
 *   3. 其餘照列的彙總（sourceTone）。引擎開著卻沒有任何列在跑時**刻意不給灰**
 *      ——灰配上一顆寫著 Disconnect 的開關是自相矛盾的畫面，改用琥珀表示
 *      「引擎起來了，只是還沒有東西在上面跑」。
 *
 * exits 要是呼叫端先濾掉 pendingDelete 之後的清單。
 */
export function connTone(conn: ConnRef, exits: ExitInfo[]): Tone {
  if (conn.kind === "wg") {
    if (conn.data.confError) return "red";
    if (!conn.data.enabled) return "grey";
    if (exits.some(isBad)) return "red";
    if (!exits.some(isRunning)) return "amber";
  }
  return sourceTone(exits);
}

/**
 * 同一份判斷的文字版，給 tooltip 用。
 *
 * **一律回狀態詞，絕不回 tone 的色票名**：使用者滑過去看到的應該是
 * 「reconnecting」而不是「amber」——後者是我們內部的實作字彙，對他毫無意義。
 */
export function connStatusText(conn: ConnRef, exits: ExitInfo[]): string {
  if (conn.kind === "wg") {
    if (conn.data.confError) return conn.data.confError;
    if (!conn.data.enabled) return "stopped";
  }
  if (exits.some((e) => e.status === "port_busy")) return "port busy";
  if (exits.some((e) => e.status === "error")) return "error";
  if (!exits.some(isRunning)) {
    // 引擎開著、卻沒有列在跑（含一條列都還沒建）：不是「停止」，是閒置
    if (conn.kind === "wg" && conn.data.enabled) return exits.length === 0 ? "no rows" : "idle";
    return exits.length === 0 ? "no rows" : "stopped";
  }
  if (exits.every((e) => e.status === "connected")) return "connected";
  if (exits.some((e) => e.status === "reconnecting")) return "reconnecting";
  return "connecting";
}

export function testLine(exit: ExitInfo): { text: string; tone: string } {
  const t = exit.lastTest;
  if (!t) return { text: "", tone: "muted" };
  if (t.state === "testing") return { text: t.text || "testing…", tone: "muted" };
  if (t.state === "fail") return { text: t.text || "no response", tone: "red" };
  return { text: t.text, tone: "text" };
}

/**
 * 自測成功的字串是後端組好的「ip␠␠city, country」，拆成兩行顯示。
 * 拆不開（格式不如預期）就退回單行，不要硬猜。
 */
export function splitTest(text: string): { ip: string; place: string } | null {
  const i = text.indexOf("  ");
  if (i <= 0) return null;
  const ip = text.slice(0, i).trim();
  const place = text.slice(i + 2).trim();
  return ip && place ? { ip, place } : null;
}

/** 後端沒帶 detail 時至少讓紅點有句話可看 */
export function defaultDetail(status: ExitStatus): string {
  return status === "port_busy" ? "local port is already in use" : "connection failed";
}
