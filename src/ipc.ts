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

/** 回傳錯誤字串代表驗證失敗，null 代表成功 */
export const upsertForward = (input: ForwardInput) =>
  invoke<string | null>("upsert_forward", { ...input });

export const deleteForward = (local: number) => invoke<void>("delete_forward", { local });

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

/** 可攜／單檔版的更新：開系統瀏覽器到 Releases 頁，不下載也不改寫自己 */
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
