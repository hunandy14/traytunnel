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
  ExitStatusEvent,
  ExitTestEvent,
  ForwardInput,
  Snapshot,
  SourceInput,
  TestConnectionInput,
  TestConnectionResult,
  UpdateInfo,
  WgProxyInput,
  WgSocksInput,
} from "./types";

export const getState = () => invoke<Snapshot>("get_state");

// ------------------------------------------------------------ 出口層級

export const startExit = (local: number) => invoke<void>("start_exit", { local });
export const stopExit = (local: number) => invoke<void>("stop_exit", { local });
export const restartExit = (local: number) => invoke<void>("restart_exit", { local });

// ------------------------------------------------------------ 源層級

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

/** 回傳錯誤字串代表驗證失敗，null 代表成功。SSH 與 WG 的 forward 列共用 */
export const upsertForward = (input: ForwardInput) =>
  invoke<string | null>("upsert_forward", { ...input });

/** local 是全域唯一鍵，刪任何一種列（forward／socks）都走這一支 */
export const deleteForward = (local: number) => invoke<void>("delete_forward", { local });

// ------------------------------------------------------------ WireGuard 連線

/** 回傳錯誤字串代表驗證失敗，null 代表成功；originalName 為 null 代表新增 */
export const upsertWgProxy = (input: WgProxyInput) =>
  invoke<string | null>("upsert_wg_proxy", { ...input });

/** 刪 WG 連線，底下所有列一併刪掉，運行中的先停 */
export const deleteWgProxy = (name: string) => invoke<void>("delete_wg_proxy", { name });

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

/** 背景檢查更新的開關；關掉之後完全不連外 */
export const setCheckForUpdates = (on: boolean) =>
  invoke<void>("set_check_for_updates", { on });

/** 這次執行實際生效的設定檔完整路徑（可攜模式與家目錄模式會不一樣） */
export const getConfigPath = () => invoke<string>("get_config_path");

/** 在檔案總管開啟設定檔所在資料夾並選中它 */
export const openConfigDir = () => invoke<void>("open_config_dir");

// ------------------------------------------------------------ 更新

/**
 * 安裝版的就地更新：下載新版安裝檔並交棒給它。
 *
 * 正常情況下這個 promise **不會 resolve**——安裝程式一起來，程式本身就退出了。
 * 會 reject 才代表更新沒能開始（沒網路、簽章驗不過之類）。
 */
export const installUpdate = () => invoke<void>("install_update");

/**
 * 使用者主動按下的檢查更新。
 *
 * **不受背景檢查開關管**：那個開關管的是自動連外，親手按下就是明示同意這一次。
 * resolve 成 null 代表已經是最新，reject 代表這次檢查失敗（原因後端會寫進活動日誌）。
 */
export const checkForUpdatesNow = () => invoke<UpdateInfo | null>("check_for_updates_now");

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
  listen<Snapshot>("config-changed", (e) => fn(e.payload));

/** 背景檢查發現新版時推一次；payload 為 null 代表回到「沒有新版」 */
export const onUpdateAvailable = (fn: (info: UpdateInfo | null) => void) =>
  listen<UpdateInfo | null>("update-available", (e) => fn(e.payload));
