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
} from "./types";

export const getState = () => invoke<Snapshot>("get_state");

// ------------------------------------------------------------ 出口層級

export const startExit = (local: number) => invoke<void>("start_exit", { local });
export const stopExit = (local: number) => invoke<void>("stop_exit", { local });
export const restartExit = (local: number) => invoke<void>("restart_exit", { local });

// ------------------------------------------------------------ 源層級

export const startSource = (name: string) => invoke<void>("start_source", { name });
export const stopSource = (name: string) => invoke<void>("stop_source", { name });
export const testSource = (name: string) => invoke<void>("test_source", { name });

/** 回傳錯誤字串代表驗證失敗，null 代表成功 */
export const upsertSource = (input: SourceInput) =>
  invoke<string | null>("upsert_source", { ...input });

export const deleteSource = (name: string) => invoke<void>("delete_source", { name });

// ------------------------------------------------------------ 轉發設定

/** 回傳錯誤字串代表驗證失敗，null 代表成功 */
export const upsertForward = (input: ForwardInput) =>
  invoke<string | null>("upsert_forward", { ...input });

export const deleteForward = (local: number) => invoke<void>("delete_forward", { local });

// ------------------------------------------------------------ 全域設定

export const setCloseToTray = (on: boolean) => invoke<void>("set_close_to_tray", { on });
export const setAutostart = (on: boolean) => invoke<void>("set_autostart", { on });

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
