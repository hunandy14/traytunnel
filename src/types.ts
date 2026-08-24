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

/**
 * 列的機制（wg-design.md §1.2）：`forward` 是本地埠→固定目的地的原樣搬運，
 * SSH／WG 皆有；`socks` 是引擎在本地埠自建一個 SOCKS5 伺服器，僅 WG 連線
 * 底下才會出現。
 */
export type RowKind = "forward" | "socks";

/** 識別出的代理協定，給列的徽章用；識別不出來時整個 protocol 欄位不存在 */
export type ProxyProtocol = "socks5" | "http";

export interface ExitTest {
  state: TestState;
  text: string;
  /**
   * 識別出的代理協定。只有「要被探測」的列（kind=socks 或 probeProxy=true）
   * 才可能帶這個欄位；識別失敗或還沒識別出來時整個欄位不存在，不可以用
   * 空字串代表「沒有」（見 wg-design.md §5.4 的 TestView）。
   */
  protocol?: ProxyProtocol;
}

export interface ExitInfo {
  name: string;
  local: number;
  /** `socks` 列沒有目的地（引擎自建監聽器），`forward` 列必填 */
  remote: string | null;
  /** 列的機制（wg-design.md §1.2）。建立後不可變 */
  kind: RowKind;
  /**
   * 這條列的後端是不是一個代理服務——true 時會做出口檢測並自動識別協定。
   * 只對 `kind === "forward"` 有意義；`kind === "socks"` 的列恆為代理
   * （引擎自建），語意上等同 true，但欄位本身可以隨時改（不像 kind 不可變）。
   */
  probeProxy: boolean;
  enabled: boolean;
  status: ExitStatus;
  /** 最近一次自測結果，沒測過就是 null */
  lastTest: ExitTest | null;
  /**
   * 只在前端用的暫存欄位：port_busy / error 的說明文字由 "exit-status" 事件帶進來，
   * 快照本身沒有這個欄位。
   */
  detailText?: string | null;
  /**
   * 只在前端用的暫存欄位：這條列**曾經**被識別出來的代理協定。
   *
   * lastTest.protocol 會隨著斷線／重測被清掉（自測結果只在 connected 時有效），
   * 但「這個目的地是不是代理」這件事不會因為連線斷一下就變了。徽章若直接跟著
   * lastTest 走，列每停一次就會從 SOCKS5 閃成「PROXY?」外加一句指責使用者設錯
   * 的 tooltip。所以識別結果在這裡黏著記憶，跨 status／test 轉換保留。
   *
   * 失效時機由 main.ts 的 applySnapshot 落實：**換了 remote 或關掉 probeProxy
   * 就不再搬過去**——那時記憶指的已經是另一個目的地，留著就是說謊。
   */
  knownProtocol?: ProxyProtocol | null;
  /**
   * 只在前端用的暫存欄位：這一筆是不是**舊後端**送來的形狀（沒有 kind／
   * probeProxy 兩個欄位）。由 ipc.ts 的 normalizeSnapshot 在 IPC 邊界標記，
   * 讓下游可以照常信任型別、不必到處寫 `kind === undefined`。
   *
   * 目前唯一的用途是「不要替舊資料臆測代理徽章」：那兩個欄位不存在時，
   * normalize 會把 probeProxy 補成 true 以保住出口 IP 檢測行（PR 之前是無條件
   * 顯示的），但那是為了相容而假設的值，不足以拿來宣稱「這條列是代理」。
   *
   * **引擎後端補上 kind／probeProxy 之後，這個欄位連同 normalize 的補值一起移除。**
   */
  legacy?: boolean;
}

/** 一個 ssh 來源（一組 user@host + ProxyCommand），底下掛自己的出口 */
export interface SourceInfo {
  name: string;
  host: string;
  user: string;
  proxyCommand: string;
  /**
   * 連線總開關：與 `WgProxyInfo.enabled` 同一套語意（主卡總開關讀的是它，
   * 不是底下列的 enabled）。只影響連線本身，底下每一條列各自的 enabled
   * 意圖不受它牽動——關閉時原封不動留著，重新打開時只有原本 enabled 的
   * 那幾條會被拉起來。
   */
  enabled: boolean;
  exits: ExitInfo[];
}

/**
 * 一條 WireGuard 連線（wg-design.md §5.3 的 WgProxyView）：行程內的使用者態
 * WireGuard 隧道，底下掛 0..N 條列（`exits`，`socks` 列排在 `forward` 列
 * 之前，後端保證順序，見 §5.3）。分段成「SOCKS5」／「PORT FORWARDS」兩個
 * 視覺區塊是純前端的事，這裡的形狀跟 SourceInfo 一樣是一份 IPC 快照。
 */
export interface WgProxyInfo {
  name: string;
  confPath: string;
  enabled: boolean;
  /**
   * 隧道 MTU 的覆寫值，null 代表「照 .conf」（.conf 也沒寫就用應用層預設 1280）。
   * 編輯面板要靠它把現值帶回欄位裡。
   */
  mtu: number | null;
  /** .conf 讀不到／解析不過時的訊息，讀得到就是 null */
  confError: string | null;
  /** 以下四項來自 .conf，唯讀顯示用；金鑰永遠不在其中 */
  endpoint: string;
  addresses: string[];
  dns: string[];
  allowedIps: string[];
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
  /** WireGuard 連線，欄位名稱與 Rust 端的 wgProxies 對齊 */
  wgProxies: WgProxyInfo[];
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

/**
 * "exit-test" 事件。
 *
 * state／text 是可選的：後端清除自測結果時（斷線／停止，見 state.rs 的
 * clear_exit_test）payload 只有 `{ local }`，state／text 兩個欄位整個不
 * 存在，不是「空字串」。前端收到 state／text 缺席（連帶容忍空字串，保守
 * 起見一併當清除訊號）就要把 lastTest 記成 null，不能照樣畫成一筆結果
 * （見 main.ts 的 applyExitTest；dev-mock.ts 的 clearTest 對齊同一形狀）。
 *
 * protocol 是新增的可選欄位（wg-design.md §5.3）：識別出的代理協定，
 * 沒識別出來就整個欄位不存在。
 */
export interface ExitTestEvent {
  local: number;
  state?: TestState;
  text?: string;
  protocol?: ProxyProtocol;
}

/** upsert_source 的輸入；originalName 為 null 代表新增 */
export interface SourceInput {
  originalName: string | null;
  name: string;
  host: string;
  user: string;
  proxyCommand: string;
}

/** 連線的型別：決定 upsertForward 要把列掛進 sources 還是 wgProxies */
export type ConnKind = "ssh" | "wg";

/**
 * 一條連線的判別聯集：`kind` 決定 `data` 是哪一種快照，型別收窄之後就不必
 * 用 `as` 硬轉、也不會有「wg 連線的 wg 欄位理論上可能不存在」這種假可空。
 *
 * main.ts 的 ConnRef（多帶 name／exits 兩個便利欄位）建立在同一個形狀上，
 * 因此可以直接餵給吃 ConnTarget 的 openSourceSheet，不必再轉一手。
 */
export type ConnTarget = { kind: "ssh"; data: SourceInfo } | { kind: "wg"; data: WgProxyInfo };

/**
 * ConnTarget 再帶上兩個便利欄位，是畫面邏輯眼中「一條連線」的形狀：不管底下
 * 是 ssh 源還是 wg 引擎，要排列、要算彙總狀態都只需要 name 與 exits。
 *
 * 放在這裡而不是 main.ts，是因為 status.ts 的 connTone／connStatusText 也要
 * 吃它——連線層的健康度計算是純函式，不該為了型別位置被迫留在 main.ts 裡。
 */
export type ConnRef = ConnTarget & { name: string; exits: ExitInfo[] };

/**
 * upsert_forward 的輸入；originalLocal 為 null 代表新增。
 *
 * SSH 與 WG 的 `forward` 列共用同一支指令（wg-design.md §5.5）；`kind`
 * 建立後不可變，這裡不需要帶——後端會比對既有列的 kind，不符就回 Err。
 */
export interface ForwardInput {
  /** 這個列屬於哪個連線（ssh 源名或 wg 連線名） */
  connection: string;
  connectionKind: ConnKind;
  originalLocal: number | null;
  name: string;
  local: number;
  remote: string;
  /** REMOTE 欄位下方那顆「目的地是代理」switch，隨時可改 */
  probeProxy: boolean;
}

/** upsert_wg_socks 的輸入；originalLocal 為 null 代表新增。WG 專屬 */
export interface WgSocksInput {
  connection: string;
  originalLocal: number | null;
  name: string;
  local: number;
}

/** upsert_wg_proxy 的輸入；originalName 為 null 代表新增 */
export interface WgProxyInput {
  originalName: string | null;
  name: string;
  confPath: string;
  /** 選填的 MTU 覆寫；null＝不覆寫，後端會把設定檔裡的 mtu 鍵拿掉 */
  mtu: number | null;
}

/** test_connection 的輸入：拿表單當下的值測，不必先存檔 */
export interface TestConnectionInput {
  host: string;
  user: string;
  proxyCommand: string;
}

/** test_connection／test_wg_conf 的回傳：ok 為 false 時 message 是失敗原因的最後一行 */
export interface TestConnectionResult {
  ok: boolean;
  message: string;
}
