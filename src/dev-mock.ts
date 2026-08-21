/**
 * 瀏覽器 UI 開發模式的假後端。
 *
 * 用官方的 @tauri-apps/api/mocks：mockIPC 攔下所有 invoke，shouldMockEvents
 * 讓 listen/emit 也走記憶體，前端程式碼完全不用為了 mock 改寫。
 *
 * 這支檔案只會在 `npm run dev` 且偵測不到 Tauri runtime 時被動態載入，
 * 正式打包時整段會被 import.meta.env.DEV 判斷掉，不會進 bundle。
 */

import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import type { Config, ExitPayload, Forward, Snapshot, StatusPayload } from "./types";

const STORE_KEY = "traytunnel-dev-mock-config";

const DEFAULT_CONFIG: Config = {
  host: "your-host.example.com",
  user: "your-user",
  proxyCommand: "cloudflared access ssh --hostname %h",
  closeToTray: true,
  forwards: [
    { name: "exit-a", local: 1080, remote: "127.0.0.1:1080" },
    { name: "exit-b", local: 1083, remote: "127.0.0.1:1083" },
  ],
};

/** 假的出口自測結果，照原版「ip  city, country」格式 */
const FAKE_EXITS: Record<number, string> = {
  1080: "203.0.113.42  Taipei, TW",
  1083: "198.51.100.17  Tokyo, JP",
};

interface MockState {
  config: Config;
  status: StatusPayload;
  wantRun: boolean;
  connected: boolean;
  logs: string[];
  exits: Map<number, ExitPayload>;
  autostart: boolean;
}

const state: MockState = {
  config: loadConfig(),
  status: { text: "Connecting...", kind: "amber" },
  wantRun: true,
  connected: false,
  logs: [],
  exits: new Map(),
  autostart: false,
};

/** 設定存在 sessionStorage，這樣在主視窗與設定頁之間切換不會掉 */
function loadConfig(): Config {
  try {
    const raw = sessionStorage.getItem(STORE_KEY);
    if (raw) return JSON.parse(raw) as Config;
  } catch {
    /* 讀不到就用預設值 */
  }
  return structuredClone(DEFAULT_CONFIG);
}

function saveConfig(cfg: Config) {
  state.config = cfg;
  try {
    sessionStorage.setItem(STORE_KEY, JSON.stringify(cfg));
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

function setStatus(text: string, kind: string) {
  state.status = { text, kind };
  void emit("status", state.status);
}

function setExit(port: number, exitState: string, text: string) {
  const payload: ExitPayload = { port, state: exitState, text };
  state.exits.set(port, payload);
  void emit("exit", payload);
}

function resetExits() {
  state.exits.clear();
  for (const f of state.config.forwards) setExit(f.local, "idle", "-");
}

function runExitTests() {
  log("testing exits...");
  for (const f of state.config.forwards) {
    setExit(f.local, "testing", "testing...");
    const result = FAKE_EXITS[f.local];
    window.setTimeout(
      () => {
        if (result) {
          setExit(f.local, "ok", result);
          log(`port ${f.local} : ${result}`);
        } else {
          setExit(f.local, "fail", "no response");
          log(`port ${f.local} : no response`);
        }
      },
      900 + Math.random() * 1200,
    );
  }
}

/** 假的連線流程：Connecting → Connected → 跑出口自測 */
function connect() {
  state.connected = false;
  setStatus("Connecting...", "amber");
  log(`tunnel starting (pid ${1000 + Math.floor(Math.random() * 60000)})`);
  window.setTimeout(() => {
    if (!state.wantRun) return;
    state.connected = true;
    setStatus("Connected", "accent");
    log("tunnel up");
    runExitTests();
  }, 1600);
}

/** 與 Rust 端 parse_forward_lines 相同的驗證規則，讓錯誤路徑也能在瀏覽器演練 */
function parseForwardLines(text: string): Forward[] {
  const out: Forward[] = [];
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line) continue;
    const parts = line.split(/\s+/);
    const okLocal = parts.length === 3 && /^\d+$/.test(parts[1]);
    const okRemote = okLocal && /^[^:\s]+:\d+$/.test(parts[2]);
    if (!okRemote) throw new Error(line);
    out.push({ name: parts[0], local: Number(parts[1]), remote: parts[2] });
  }
  return out;
}

function snapshot(): Snapshot {
  return {
    config: state.config,
    status: state.status,
    wantRun: state.wantRun,
    connected: state.connected,
    logs: [...state.logs],
    exits: [...state.exits.values()],
    autostart: state.autostart,
  };
}

interface SaveArgs {
  input: { host: string; user: string; proxyCommand: string; forwards: string };
}

function handle(cmd: string, payload?: Record<string, unknown>): unknown {
  switch (cmd) {
    case "get_state":
      return snapshot();

    case "toggle_run":
      if (state.wantRun) {
        state.wantRun = false;
        state.connected = false;
        void emit("run-state", false);
        log("tunnel stopped");
        setStatus("Stopped", "muted");
        resetExits();
      } else {
        state.wantRun = true;
        void emit("run-state", true);
        connect();
      }
      return null;

    case "retest":
      if (state.connected) runExitTests();
      else log("not connected, cannot test");
      return null;

    case "save_config": {
      const input = (payload as unknown as SaveArgs).input;
      let forwards: Forward[];
      try {
        forwards = parseForwardLines(input.forwards);
      } catch (e) {
        throw `Invalid forward line:\n${(e as Error).message}\n\nExpected:  name  localPort  remoteHost:remotePort`;
      }
      if (!input.host.trim() || !input.user.trim() || forwards.length === 0) {
        throw "Host, User and at least one forward are required.";
      }
      saveConfig({
        host: input.host.trim(),
        user: input.user.trim(),
        proxyCommand: input.proxyCommand.trim(),
        closeToTray: state.config.closeToTray,
        forwards,
      });
      void emit("config", state.config);
      resetExits();
      log("config saved, restarting tunnel");
      connect();
      return null;
    }

    case "set_close_to_tray":
      saveConfig({ ...state.config, closeToTray: Boolean(payload?.on) });
      void emit("config", state.config);
      log(payload?.on ? "close hides to tray" : "close exits app");
      return null;

    case "set_autostart":
      state.autostart = Boolean(payload?.on);
      log(state.autostart ? "autostart enabled" : "autostart disabled");
      return null;

    // 瀏覽器裡沒有多視窗，就用換頁來演示兩個畫面
    case "open_settings":
      window.location.href = "/settings.html";
      return null;

    case "close_settings":
      window.location.href = "/index.html";
      return null;

    case "window_minimize":
    case "window_close":
    case "exit_app":
      log(`(browser mock) ${cmd}`);
      return null;

    default:
      log(`(browser mock) unhandled command: ${cmd}`);
      return null;
  }
}

export function installDevMock() {
  mockIPC((cmd, payload) => handle(cmd, payload as Record<string, unknown>), {
    shouldMockEvents: true,
  });

  // 先鋪一點歷史日誌，讓 ACTIVITY 區一載入就有東西看
  log("Traytunnel started");
  log("(browser mock) no Tauri runtime, using fake backend");
  resetExits();
  connect();
}
