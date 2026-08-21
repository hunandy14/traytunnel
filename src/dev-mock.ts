/**
 * 瀏覽器 UI 開發模式的假後端（IPC 契約 v2）。
 *
 * 用官方的 @tauri-apps/api/mocks：mockIPC 攔下所有 invoke，shouldMockEvents
 * 讓 listen/emit 也走記憶體，前端程式碼完全不用為了 mock 改寫。
 *
 * 這支檔案只會在 `npm run dev` 且偵測不到 Tauri runtime 時被動態載入，
 * 正式打包時整段會被 import.meta.env.DEV 判斷掉，不會進 bundle。
 */

import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import type { ExitInfo, ExitStatus, Snapshot, TestState } from "./types";

const STORE_KEY = "traytunnel-dev-mock-v2";

const DEFAULT_SNAPSHOT: Snapshot = {
  host: "gateway.example.com",
  user: "ubuntu",
  proxyCommand: "cloudflared access ssh --hostname %h",
  closeToTray: true,
  autostart: false,
  exits: [
    { name: "exit-tokyo", local: 1080, remote: "127.0.0.1:1080", enabled: true, status: "stopped", lastTest: null },
    { name: "exit-taipei", local: 1083, remote: "127.0.0.1:1083", enabled: true, status: "stopped", lastTest: null },
    { name: "exit-edge", local: 1084, remote: "127.0.0.1:1084", enabled: true, status: "stopped", lastTest: null },
  ],
  logs: [],
};

/** 假的出口自測結果，照原版「ip  city, country」格式；沒列到的埠就當作測不到 */
const FAKE_TEST: Record<number, string> = {
  1080: "203.0.113.42  Tokyo, JP",
  1083: "198.51.100.17  Taipei, TW",
};

/** 這些埠一啟動就會撞埠，讓 port_busy 這條路徑在瀏覽器也演練得到 */
const BUSY_PORTS = new Set([1084]);

const state: Snapshot = load();
const timers = new Map<number, number>();

function load(): Snapshot {
  try {
    const raw = sessionStorage.getItem(STORE_KEY);
    if (raw) {
      const saved = JSON.parse(raw) as Snapshot;
      // 狀態與日誌不留存，每次重載都從 stopped 開始
      saved.exits = saved.exits.map((e) => ({ ...e, status: "stopped", lastTest: null }));
      saved.logs = [];
      return saved;
    }
  } catch {
    /* 讀不到就用預設值 */
  }
  return structuredClone(DEFAULT_SNAPSHOT);
}

function persist() {
  try {
    sessionStorage.setItem(STORE_KEY, JSON.stringify({ ...state, logs: [] }));
  } catch {
    /* 無痕模式之類的存不進去就算了，記憶體裡還是對的 */
  }
}

function stamp(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function log(msg: string) {
  const line = `${stamp()}  ${msg}`;
  state.logs.push(line);
  if (state.logs.length > 500) state.logs.shift();
  void emit("log", line);
}

function pushConfig() {
  persist();
  void emit("config-changed", structuredClone(state));
}

function find(local: number): ExitInfo | undefined {
  return state.exits.find((e) => e.local === local);
}

function setStatus(exit: ExitInfo, status: ExitStatus, detail?: string) {
  exit.status = status;
  if (status === "stopped") exit.lastTest = null;
  void emit("exit-status", { local: exit.local, status, detail: detail ?? null });
}

function setTest(exit: ExitInfo, testState: TestState, text: string) {
  exit.lastTest = { state: testState, text };
  void emit("exit-test", { local: exit.local, state: testState, text });
}

function later(local: number, ms: number, fn: () => void) {
  const old = timers.get(local);
  if (old) window.clearTimeout(old);
  timers.set(local, window.setTimeout(fn, ms));
}

function runTest(exit: ExitInfo) {
  if (exit.status !== "connected") return;
  setTest(exit, "testing", "testing…");
  window.setTimeout(
    () => {
      if (exit.status !== "connected") return;
      const result = FAKE_TEST[exit.local];
      if (result) {
        setTest(exit, "ok", result);
        log(`port ${exit.local} : ${result}`);
      } else {
        setTest(exit, "fail", "no response");
        log(`port ${exit.local} : no response`);
      }
    },
    700 + Math.random() * 900,
  );
}

function start(exit: ExitInfo) {
  exit.enabled = true;
  setStatus(exit, "connecting");
  log(`${exit.name}: starting tunnel on :${exit.local}`);
  later(exit.local, 1100, () => {
    if (exit.status !== "connecting" && exit.status !== "reconnecting") return;
    if (BUSY_PORTS.has(exit.local)) {
      setStatus(exit, "port_busy", `local port ${exit.local} is already in use (PID 8124)`);
      log(`${exit.name}: bind :${exit.local} failed, port busy`);
      return;
    }
    setStatus(exit, "connected");
    log(`${exit.name}: tunnel up`);
    runTest(exit);
  });
}

function stop(exit: ExitInfo) {
  exit.enabled = false;
  const old = timers.get(exit.local);
  if (old) window.clearTimeout(old);
  setStatus(exit, "stopped");
  log(`${exit.name}: stopped`);
}

/** 與 Rust 端相同的驗證規則；錯誤字串用 `field: message` 開頭讓 UI 能逐欄顯示 */
function validateForward(input: {
  originalLocal: number | null;
  name: string;
  local: number;
  remote: string;
}): string | null {
  if (!input.name.trim()) return "name: name is required";
  if (!Number.isInteger(input.local) || input.local < 1 || input.local > 65535) {
    return "local: must be 1-65535";
  }
  const clash = state.exits.find((e) => e.local === input.local && e.local !== input.originalLocal);
  if (clash) return `local: already used by ${clash.name}`;
  if (!/^[^\s:]+:\d+$/.test(input.remote.trim())) {
    return "remote: expected host:port, for example 127.0.0.1:1080";
  }
  const dupName = state.exits.find(
    (e) => e.name === input.name.trim() && e.local !== input.originalLocal,
  );
  if (dupName) return "name: another exit already uses this name";
  return null;
}

interface Args {
  local?: number;
  on?: boolean;
  host?: string;
  user?: string;
  proxyCommand?: string;
  originalLocal?: number | null;
  name?: string;
  remote?: string;
}

function handle(cmd: string, args: Args): unknown {
  switch (cmd) {
    // 自繪標題列的視窗指令，瀏覽器裡沒有視窗可動，只記一行
    case "window_minimize":
    case "window_close":
    case "exit_app":
      log(`(browser mock) ${cmd}`);
      return null;

    case "get_state":
      return structuredClone(state);

    case "start_exit": {
      const exit = find(args.local as number);
      if (exit) start(exit);
      return null;
    }

    case "stop_exit": {
      const exit = find(args.local as number);
      if (exit) stop(exit);
      return null;
    }

    case "start_all":
      log("starting all exits");
      for (const e of state.exits) if (e.status === "stopped") start(e);
      return null;

    case "stop_all":
      log("stopping all exits");
      for (const e of state.exits) if (e.status !== "stopped") stop(e);
      return null;

    case "test_all":
      log("testing all exits...");
      for (const e of state.exits) runTest(e);
      return null;

    case "save_global": {
      const host = (args.host ?? "").trim();
      const user = (args.user ?? "").trim();
      if (!host) return "Host is required.";
      if (!user) return "User is required.";
      if (/\s/.test(host)) return "Host must not contain spaces.";
      state.host = host;
      state.user = user;
      state.proxyCommand = (args.proxyCommand ?? "").trim();
      pushConfig();
      log(`global settings saved (${user}@${host})`);
      return null;
    }

    case "upsert_forward": {
      const input = {
        originalLocal: args.originalLocal ?? null,
        name: (args.name ?? "").trim(),
        local: Number(args.local),
        remote: (args.remote ?? "").trim(),
      };
      const err = validateForward(input);
      if (err) return err;

      const existing = input.originalLocal === null ? undefined : find(input.originalLocal);
      if (existing) {
        const wasRunning = existing.status !== "stopped";
        if (wasRunning) stop(existing);
        existing.name = input.name;
        existing.local = input.local;
        existing.remote = input.remote;
        pushConfig();
        log(`${input.name}: updated`);
        if (wasRunning) start(existing);
      } else {
        state.exits.push({
          name: input.name,
          local: input.local,
          remote: input.remote,
          enabled: true,
          status: "stopped",
          lastTest: null,
        });
        pushConfig();
        log(`${input.name}: added`);
      }
      return null;
    }

    case "delete_forward": {
      const exit = find(args.local as number);
      if (exit) {
        stop(exit);
        state.exits = state.exits.filter((e) => e.local !== exit.local);
        pushConfig();
        log(`${exit.name}: deleted`);
      }
      return null;
    }

    case "set_close_to_tray":
      state.closeToTray = Boolean(args.on);
      pushConfig();
      log(state.closeToTray ? "close hides to tray" : "close exits app");
      return null;

    case "set_autostart":
      state.autostart = Boolean(args.on);
      pushConfig();
      log(state.autostart ? "autostart enabled" : "autostart disabled");
      return null;

    default:
      log(`(browser mock) unhandled command: ${cmd}`);
      return null;
  }
}

/**
 * 截圖／手動演練用的操控面板，只有 dev 模式才存在。
 * 例：__mock.drop(1080) 演練 reconnecting，__mock.reset() 回到初始狀態。
 */
function installScenarioHooks() {
  const api = {
    snapshot: () => structuredClone(state),
    status(local: number, status: ExitStatus, detail?: string) {
      const exit = find(local);
      if (!exit) return;
      const old = timers.get(local);
      if (old) window.clearTimeout(old);
      setStatus(exit, status, detail);
    },
    test(local: number, testState: TestState, text: string) {
      const exit = find(local);
      if (exit) setTest(exit, testState, text);
    },
    /** 演練斷線重連：connected → reconnecting → connected */
    drop(local: number) {
      const exit = find(local);
      if (!exit) return;
      setStatus(exit, "reconnecting");
      log(`${exit.name}: connection lost, reconnecting...`);
      later(local, 2500, () => {
        if (exit.status !== "reconnecting") return;
        setStatus(exit, "connected");
        log(`${exit.name}: reconnected`);
        runTest(exit);
      });
    },
    reset() {
      sessionStorage.removeItem(STORE_KEY);
      location.reload();
    },
  };
  (window as unknown as { __mock: typeof api }).__mock = api;
}

export function installDevMock() {
  mockIPC((cmd, payload) => handle(cmd, (payload ?? {}) as Args), { shouldMockEvents: true });
  installScenarioHooks();

  // 先鋪一點歷史日誌，讓活動區一載入就有東西看
  log("Traytunnel started");
  log("(browser mock) no Tauri runtime, using fake backend");
  window.setTimeout(() => {
    for (const e of state.exits) if (e.enabled) start(e);
  }, 250);
}
