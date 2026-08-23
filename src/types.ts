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

/**
 * 有新版可用時的資訊。null（或欄位不存在）代表沒有新版，設定頁就不顯示更新列。
 *
 * installed 決定使用者看到的是哪一種動作：安裝版可以就地更新（Restart to
 * update），可攜／單檔版只能開瀏覽器到 Releases 自己換檔案（Download）。
 */
export interface UpdateInfo {
  /** 遠端公告的新版本號，不帶 v */
  version: string;
  installed: boolean;
}

export interface Snapshot {
  closeToTray: boolean;
  autostart: boolean;
  /** 實際生效的值：設定檔沒寫時，一般模式是 true、可攜模式是 false */
  checkForUpdates: boolean;
  sources: SourceInfo[];
  /** 活動日誌的整行（含時間戳與 [源名] 前綴），舊到新 */
  logs: string[];
  /** 背景更新檢查的結果，沒有新版就是 null */
  update: UpdateInfo | null;
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
