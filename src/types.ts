/** IPC 契約 v3 的型別定義，欄位名稱與 Rust 端逐字對齊（參數平鋪 camelCase）。 */

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

/** 一個 ssh 來源（一組 user@host + ProxyCommand），底下掛自己的出口 */
export interface SourceInfo {
  name: string;
  host: string;
  user: string;
  proxyCommand: string;
  exits: ExitInfo[];
}

export interface Snapshot {
  closeToTray: boolean;
  autostart: boolean;
  sources: SourceInfo[];
  /** 活動日誌的整行（含時間戳與 [源名] 前綴），舊到新 */
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

/** upsert_source 的輸入；originalName 為 null 代表新增 */
export interface SourceInput {
  originalName: string | null;
  name: string;
  host: string;
  user: string;
  proxyCommand: string;
}

/** upsert_forward 的輸入；originalLocal 為 null 代表新增 */
export interface ForwardInput {
  /** 這個出口屬於哪個源（用源名稱指定） */
  source: string;
  originalLocal: number | null;
  name: string;
  local: number;
  remote: string;
}

/** test_connection 的輸入：拿表單當下的值測，不必先存檔 */
export interface TestConnectionInput {
  host: string;
  user: string;
  proxyCommand: string;
}

/** test_connection 的回傳：ok 為 false 時 message 是 ssh 失敗原因的最後一行 */
export interface TestConnectionResult {
  ok: boolean;
  message: string;
}
