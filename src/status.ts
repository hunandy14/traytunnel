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
 * 舊後端沒有這兩個欄位的情況**不在這裡處理**——ipc.ts 的 normalizeSnapshot
 * 已經在 IPC 邊界把形狀補齊了，這裡照宣告的型別信任資料就好。
 */
export const shouldProbe = (e: ExitInfo) => e.kind === "socks" || e.probeProxy;

/**
 * 健康度只看「使用者要它跑」的列。
 *
 * 使用者刻意停掉的那一條不該拖累整條連線的顏色——三條列停了一條、其餘都連上，
 * 那是「照他的意思在跑」，不是「有東西不對」。沒有這道過濾的話，`sourceTone`
 * 的「全連才綠」會讓這種連線永久卡在琥珀，使用者看著一個永遠好不了的警示色。
 */
const activeExits = (exits: ExitInfo[]) => exits.filter((e) => e.enabled);

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
 * exits 要是呼叫端先濾掉 pendingDelete 之後的清單；停用中的列在這裡才過濾，
 * 三個呼叫點（connTone／connStatusText／統計分數）因此拿到同一套判準。
 */
export function sourceTone(exits: ExitInfo[]): Tone {
  const active = activeExits(exits);
  if (active.length === 0) return "grey";
  if (active.some(isBad)) return "red";
  if (!active.some(isRunning)) return "grey";
  return active.every((e) => e.status === "connected") ? "green" : "amber";
}

/**
 * 一條連線的健康度色調——左軌的小點、summary 的引擎點、中段統計分數三處
 * **共用這一支**，不要各自再兜一次條件。之前三個地方各算各的，結果同一條
 * 連線可以同時在左軌顯示灰、在 summary 顯示紅。
 *
 * 融合三層資訊，由重到輕：
 *
 *   1. `.conf` 壞掉（confError，只有 WG 有這個欄位）→ 紅。那是連線本身的問題，
 *      不是列的彙總看得出來的，這種連線根本起不來。
 *   2. 連線總開關關著（SSH／WG 皆有 `conn.data.enabled`，主卡總開關的行為
 *      自 SSH 對齊 WG 起兩者同一套語意）→ 灰。列的意圖還在，但連線沒開它們
 *      不可能跑。
 *   3. 其餘照列的彙總（sourceTone），但把它的灰改判成琥珀：連線開著卻沒有
 *      任何列在跑時**刻意不給灰**——灰配上一顆寫著 Disconnect 的開關是自相
 *      矛盾的畫面，改用琥珀表示「連線起來了，只是還沒有東西在上面跑」。
 *      sourceTone 本身回灰的兩種情況（exits 是空的、或沒有一個在跑）在這裡
 *      是同一個「連線開著、沒東西在跑」的意思，不必在這裡另外重複一次
 *      isBad／isRunning 那組階梯。
 *
 * exits 要是呼叫端先濾掉 pendingDelete 之後的清單。
 */
export function connTone(conn: ConnRef, exits: ExitInfo[]): Tone {
  if (conn.kind === "wg" && conn.data.confError) return "red";
  if (!conn.data.enabled) return "grey";
  const tone = sourceTone(exits);
  return tone === "grey" ? "amber" : tone;
}

/**
 * 同一份判斷的文字版。狀態點的 tooltip 與統計分數底下那行小字都用它，
 * 兩處因此不會出現「分數說 stopped、引擎點說 idle」這種各說各話。
 *
 * **一律回狀態詞，絕不回 tone 的色票名**：使用者滑過去看到的應該是
 * 「reconnecting」而不是「amber」——後者是我們內部的實作字彙，對他毫無意義。
 *
 * 也**不回 confError 的原文**：這個回傳值要能當一行小標籤用，錯誤訊息可以很長。
 * 完整訊息由呼叫端自己貼在需要的地方（summary 副標與引擎點的 tooltip）。
 */
export function connStatusText(conn: ConnRef, exits: ExitInfo[]): string {
  if (conn.kind === "wg" && conn.data.confError) return "config error";
  if (!conn.data.enabled) return "stopped";
  if (exits.length === 0) return "no rows";
  const active = activeExits(exits);
  if (active.some((e) => e.status === "port_busy")) return "port busy";
  if (active.some((e) => e.status === "error")) return "error";
  // 連線總開關開著、卻沒有列在跑（列全被停用，或一條都還沒建）：不是「停止」，是閒置。
  // SSH 與 WG 自總開關對齊起同一套判斷，都以 conn.data.enabled 為準（上面已經
  // 提前擋掉關著的情況，這裡能走到就代表開著）
  if (!active.some(isRunning)) return "idle";
  if (active.every((e) => e.status === "connected")) return "connected";
  if (active.some((e) => e.status === "reconnecting")) return "reconnecting";
  return "connecting";
}

export function testLine(exit: ExitInfo): { text: string; tone: string } {
  const t = exit.lastTest;
  if (!t) return { text: "", tone: "muted" };
  if (t.state === "testing") return { text: t.text || "Connecting…", tone: "muted" };
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
