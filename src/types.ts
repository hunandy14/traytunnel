/**
 * IPC 契約 v3 的型別定義，欄位名稱與 Rust 端逐字對齊（參數平鋪 camelCase）。
 *
 * ExitStatus／TestState 這兩組狀態值是跟 Rust 端手動同步的字面量聯集，
 * 沒有共用的單一來源。新增、改名或刪除狀態值時，以下幾處都要一起改，
 * 漏改不會在編譯期報錯，只會在執行期悄悄壞掉（畫面卡在某個 tone、
 * dev-mock 模擬不出新狀態之類）：
 *
 *   - src-tauri/src/state.rs 的 status 模組常數（STOPPED／CONNECTING／…）
 *   - src/status.ts 的 RUNNING 陣列與 statusTone()（isBad／isRunning
 *     這些衍生判斷也在同一支檔案）
 *   - src/dev-mock.ts 假後端裡任何列舉狀態值的地方（模擬情境、setStatus
 *     的呼叫點）
 */

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
