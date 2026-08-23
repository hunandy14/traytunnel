/**
 * 出口／源狀態的純函式工具與卡片文字格式化，不碰 DOM。
 *
 * 這一層只依當下的出口狀態做判斷，不持有任何畫面簿記：pendingDelete
 * （哪些出口正在等 undo 倒數、畫面上先當它不存在）屬於 main.ts，
 * 因此 sourceTone 吃的是「已經濾好的出口清單」，過濾動作留在呼叫端
 * （main.ts 的 visibleExits）。
 */

import type { ExitInfo, ExitStatus } from "./types";

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
