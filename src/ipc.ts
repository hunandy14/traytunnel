/**
 * IPC 契約 v3 的唯一出入口。
 *
 * 前端其他檔案一律不直接呼叫 invoke，指令名稱與參數形狀只在這裡出現一次，
 * 之後後端要改欄位也只需要動這支。
 *
 * local 埠號是全域唯一的，所以出口層級的指令都不帶 source 參數；
 * 只有 upsert_forward 需要知道新出口要掛在哪個源底下。
 */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  ExitInfo,
  ExitStatusEvent,
  ExitTestEvent,
  ForwardInput,
  Snapshot,
  SourceInfo,
  SourceInput,
  TestConnectionInput,
  TestConnectionResult,
  UpdateInfo,
  WgProxyInfo,
  WgProxyInput,
  WgSocksInput,
} from "./types";

// ------------------------------------------------------------ 過渡期的形狀正規化
//
// 引擎車道（WireGuard 支援的 Rust 那一半）還沒落地，包裝版裡跑的後端送來的
// Snapshot 比 types.ts 宣告的形狀少了好幾樣東西。這些差異全部**只在這裡**補齊，
// 下游（main.ts／status.ts／sheet.ts）一律照宣告的型別信任資料，不必到處寫
// `?? []` 或 `kind === undefined` ——那種散點防護漏一處就是執行期爆掉，而且
// 沒有人說得清哪些是真的在防什麼。
//
// **引擎後端落地後，整段 normalize 連同 ExitInfo.legacy 一起移除。**
// 同一批過渡墊片還有一處：下面 upsertForward 的 `source` 雙鍵。

/** 舊後端的 ExitView 沒有 kind／probeProxy 兩個欄位，這裡補成等價的舊行為 */
function normalizeExit(raw: ExitInfo): ExitInfo {
  // 兩個欄位一起缺席才算舊形狀（新後端一定兩個都送）
  const legacy = raw.kind === undefined || raw.probeProxy === undefined;
  if (!legacy) return raw;
  return {
    ...raw,
    kind: raw.kind ?? "forward",
    // 補 true 是為了保住出口 IP 檢測行——PR 之前那一行是無條件顯示的，
    // 補 false 會讓所有既有 SSH 出口的檢測行整排消失。
    probeProxy: raw.probeProxy ?? true,
    legacy: true,
  };
}

function normalizeSnapshot(raw: Snapshot): Snapshot {
  const sources: SourceInfo[] = (raw.sources ?? []).map((s) => ({
    ...s,
    // 舊後端還沒送這個欄位時，沒有鍵就等於「還沒關過」，跟 Rust 端的
    // serde 預設（default_true）與 wgProxies.mtu 的 ?? 補值走同一套慣例
    enabled: s.enabled ?? true,
    exits: (s.exits ?? []).map(normalizeExit),
  }));
  // wgProxies 在舊後端根本不存在，直接 .map 會讓 UI 在第一次取狀態就整個掛掉
  const wgProxies: WgProxyInfo[] = (raw.wgProxies ?? []).map((p) => ({
    ...p,
    // 沒有覆寫時後端連鍵都不送，undefined 會讓編輯面板把 value 印成 "undefined"
    mtu: p.mtu ?? null,
    exits: (p.exits ?? []).map(normalizeExit),
  }));
  return {
    ...raw,
    sources,
    wgProxies,
    logs: raw.logs ?? [],
    update: raw.update ?? null,
    pendingUpdate: raw.pendingUpdate ?? null,
    updateStalled: raw.updateStalled ?? false,
  };
}

export const getState = () => invoke<Snapshot>("get_state").then(normalizeSnapshot);

// ------------------------------------------------------------ 出口層級

export const startExit = (local: number) => invoke<void>("start_exit", { local });
export const stopExit = (local: number) => invoke<void>("stop_exit", { local });
export const restartExit = (local: number) => invoke<void>("restart_exit", { local });

// ------------------------------------------------------------ 源層級

/**
 * SSH 主卡的連線總開關，與 `setWgEnabled` 同一套語意（見上方說明）：只改寫
 * `SourceInfo.enabled`，底下各列的 enabled 意圖一個都不碰。
 *
 *   stopSource ：把底下所有列停掉，各列的意圖原封不動
 *   startSource：只啟動列自己也 enabled = true 的那些
 */
export const startSource = (name: string) => invoke<void>("start_source", { name });
export const stopSource = (name: string) => invoke<void>("stop_source", { name });

/** 回傳錯誤字串代表驗證失敗，null 代表成功 */
export const upsertSource = (input: SourceInput) =>
  invoke<string | null>("upsert_source", { ...input });

export const deleteSource = (name: string) => invoke<void>("delete_source", { name });

/** 存檔前的手動連線測試，拿表單當下的值 spawn 一次性 ssh，不必先存檔 */
export const testConnection = (input: TestConnectionInput) =>
  invoke<TestConnectionResult>("test_connection", { ...input });

// ------------------------------------------------------------ 轉發設定

/**
 * 回傳錯誤字串代表驗證失敗，null 代表成功。SSH 與 WG 的 forward 列共用。
 *
 * **過渡墊片**：新契約把「這條列掛在誰底下」的參數從 `source` 改名成
 * `connection`（因為它現在可能是 wg 連線，不只是 ssh 源），但引擎車道還沒落地
 * ——目前包裝版裡跑的 Rust `upsert_forward` 仍然只認得 `source`，只送新鍵的話
 * 存轉發會直接失敗。所以兩個鍵一起送，值相同：舊後端讀 `source`，新後端讀
 * `connection`，Tauri 對認不得的鍵是直接忽略，兩邊都不會出事。
 *
 * **引擎後端落地（改吃 connection／connectionKind）之後，移除 `source` 這一行。**
 * 與上面 normalizeSnapshot 屬於同一批過渡墊片，要拆一起拆。
 *
 * 已知的過渡副作用（暫不處理，見 PR 描述）：舊後端不認得 `probeProxy`，
 * 這個旗標在它落地之前存不進設定檔。
 */
export const upsertForward = (input: ForwardInput) =>
  invoke<string | null>("upsert_forward", { source: input.connection, ...input });

/** local 是全域唯一鍵，刪任何一種列（forward／socks）都走這一支 */
export const deleteForward = (local: number) => invoke<void>("delete_forward", { local });

// ------------------------------------------------------------ WireGuard 連線

/** 回傳錯誤字串代表驗證失敗，null 代表成功；originalName 為 null 代表新增 */
export const upsertWgProxy = (input: WgProxyInput) =>
  invoke<string | null>("upsert_wg_proxy", { ...input });

/** 刪 WG 連線，底下所有列一併刪掉，運行中的先停 */
export const deleteWgProxy = (name: string) => invoke<void>("delete_wg_proxy", { name });

/**
 * 連線層的引擎總開關（wg-design.md §5.5 第 3 支）。前端的連線總開關與 ⋯ 選單的
 * Connect／Disconnect 都走這一支，不要退回「逐列迴圈呼叫 start_exit／stop_exit」。
 *
 * 自 SSH 主卡總開關上線起與 `startSource`／`stopSource` 是同一套語意：只改寫
 * 連線自己的 enabled，**底下各列的 enabled 意圖一個都不碰**——
 *
 *   on = false：停引擎、收掉所有列的監聽器，各列的意圖原封不動
 *   on = true ：起引擎，並且只啟動 enabled = true 的列
 *
 * 使用者重新打開連線時，原本刻意停用的那幾條列才不會被一起打開。
 */
export const setWgEnabled = (name: string, on: boolean) =>
  invoke<void>("set_wg_enabled", { name, on });

/** 存檔前的 .conf 測試：解析＋真握手，15 秒上限，回傳形狀與 test_connection 一致 */
export const testWgConf = (confPath: string) =>
  invoke<TestConnectionResult>("test_wg_conf", { confPath });

/** 原生檔案選擇器，選 .conf；取消時回 null */
export const pickWgConf = () => invoke<string | null>("pick_wg_conf");

/** WG 專屬：新增／編輯引擎自建 SOCKS5 代理列，originalLocal 為 null 代表新增 */
export const upsertWgSocks = (input: WgSocksInput) =>
  invoke<string | null>("upsert_wg_socks", { ...input });

// ------------------------------------------------------------ 全域設定

export const setCloseToTray = (on: boolean) => invoke<void>("set_close_to_tray", { on });
export const setAutostart = (on: boolean) => invoke<void>("set_autostart", { on });

/**
 * 自動更新的總開關（背景查版本、靜默下載、下次啟動安裝一整條）；
 * 關掉之後完全不連外
 */
export const setAutomaticUpdates = (on: boolean) =>
  invoke<void>("set_automatic_updates", { on });

/** 這次執行實際生效的設定檔完整路徑（可攜模式與家目錄模式會不一樣） */
export const getConfigPath = () => invoke<string>("get_config_path");

/** 在檔案總管開啟設定檔所在資料夾並選中它 */
export const openConfigDir = () => invoke<void>("open_config_dir");

// ------------------------------------------------------------ 更新

/**
 * 「Restart to update」：把已經下載好的那一版現在就裝上去。
 *
 * 正常情況下這個 promise **不會 resolve**——安裝程式一起來，程式本身就退出了。
 * 會 reject 才代表更新沒能開始（暫存檔驗不過、安裝程式起不來之類）。
 */
export const applyUpdate = () => invoke<void>("apply_update");

/**
 * 開某一版的 release 頁：發佈說明與那一版的下載資產都在同一頁上，所以可攜版的
 * 「Get vX.Y.Z」與下拉的「View release notes」共用它。version 給 null 就是
 * releases/latest。
 */
export const openReleasePage = (version: string | null) =>
  invoke<void>("open_release_page", { version });

/** Releases 列表頁，讓使用者自己挑版本；不下載也不改寫自己 */
export const openReleasesPage = () => invoke<void>("open_releases_page");

/** 自繪標題列用的視窗指令，close 的行為（縮到匣或結束）由 Rust 端決定 */
export const windowMinimize = () => invoke<void>("window_minimize");
export const windowClose = () => invoke<void>("window_close");

// ------------------------------------------------------------ 事件

export const onExitStatus = (fn: (e: ExitStatusEvent) => void) =>
  listen<ExitStatusEvent>("exit-status", (e) => fn(e.payload));

export const onExitTest = (fn: (e: ExitTestEvent) => void) =>
  listen<ExitTestEvent>("exit-test", (e) => fn(e.payload));

export const onLog = (fn: (line: string) => void) =>
  listen<string>("log", (e) => fn(e.payload));

export const onConfigChanged = (fn: (snap: Snapshot) => void) =>
  listen<Snapshot>("config-changed", (e) => fn(normalizeSnapshot(e.payload)));

/** 背景檢查發現新版時推一次；payload 為 null 代表回到「沒有新版」 */
export const onUpdateAvailable = (fn: (info: UpdateInfo | null) => void) =>
  listen<UpdateInfo | null>("update-available", (e) => fn(e.payload));
