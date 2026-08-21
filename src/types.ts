/** IPC 契約 v2 的型別定義，欄位名稱與 Rust 端逐字對齊。 */

/** 出口的連線狀態（六態） */
export type ExitStatus =
  | "stopped"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "port_busy"
  | "error";

/** 出口自測的狀態 */
export type TestState = "testing" | "ok" | "fail";

export interface ExitTest {
  state: TestState;
  text: string;
}

export interface ExitInfo {
  name: string;
  local: number;
  remote: string;
  enabled: boolean;
  status: ExitStatus;
  /** 最近一次自測結果，沒測過就是 null */
  lastTest: ExitTest | null;
  /**
   * 只在前端用的暫存欄位：port_busy / error 的說明文字由 "exit-status" 事件帶進來，
   * 快照本身沒有這個欄位。
   */
  detailText?: string | null;
}

export interface Snapshot {
  host: string;
  user: string;
  proxyCommand: string;
  closeToTray: boolean;
  autostart: boolean;
  exits: ExitInfo[];
  /** 活動日誌的整行（含時間戳），舊到新 */
  logs: string[];
}

/** "exit-status" 事件 */
export interface ExitStatusEvent {
  local: number;
  status: ExitStatus;
  /** port_busy / error 時的補充說明 */
  detail?: string | null;
}

/** "exit-test" 事件 */
export interface ExitTestEvent {
  local: number;
  state: TestState;
  text: string;
}

/** save_global 的輸入 */
export interface GlobalInput {
  host: string;
  user: string;
  proxyCommand: string;
}

/** upsert_forward 的輸入；originalLocal 為 null 代表新增 */
export interface ForwardInput {
  originalLocal: number | null;
  name: string;
  local: number;
  remote: string;
}
