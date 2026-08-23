/**
 * 瀏覽器 UI 開發模式的假後端（IPC 契約 v3）。
 *
 * 用官方的 @tauri-apps/api/mocks：mockIPC 攔下所有 invoke，shouldMockEvents
 * 讓 listen/emit 也走記憶體，前端程式碼完全不用為了 mock 改寫。
 *
 * 這支檔案只會在 `npm run dev` 且偵測不到 Tauri runtime 時被動態載入，
 * 正式打包時整段會被 import.meta.env.DEV 判斷掉，不會進 bundle。
 */

import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import type {
  ExitInfo,
  ExitStatus,
  Snapshot,
  SourceInfo,
  TestConnectionResult,
  TestState,
} from "./types";

const STORE_KEY = "traytunnel-dev-mock-v3";

const DEFAULT_SNAPSHOT: Snapshot = {
  closeToTray: true,
  autostart: false,
  checkForUpdates: true,
  sources: [
    {
      name: "tokyo",
      host: "gateway-jp.example.com",
      user: "ubuntu",
      proxyCommand: "cloudflared access ssh --hostname %h",
      exits: [
        { name: "socks-jp", local: 1080, remote: "127.0.0.1:1080", enabled: true, status: "stopped", lastTest: null },
        { name: "pg-jp", local: 1081, remote: "10.0.4.12:5432", enabled: true, status: "stopped", lastTest: null },
      ],
    },
    {
      name: "taipei",
      host: "gw-tw.example.com",
      user: "ec2-user",
      proxyCommand: "cloudflared access ssh --hostname %h",
      exits: [
        { name: "socks-tw", local: 1083, remote: "127.0.0.1:1083", enabled: true, status: "stopped", lastTest: null },
        { name: "edge-tw", local: 1084, remote: "127.0.0.1:1084", enabled: true, status: "stopped", lastTest: null },
      ],
    },
    {
      name: "lab",
      host: "lab.internal",
      user: "root",
      proxyCommand: "",
      exits: [],
    },
  ],
  logs: [],
  // 預設沒有新版，更新列不出現；三種更新情境由 __mock 那邊演（見 installScenarioHooks）
  update: null,
};

/** 假的出口自測結果，照原版「ip  city, country」格式；沒列到的埠就當作測不到 */
const FAKE_TEST: Record<number, string> = {
  1080: "203.0.113.42  Tokyo, JP",
  1081: "203.0.113.42  Tokyo, JP",
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
      for (const s of saved.sources) {
        s.exits = s.exits.map((e) => ({ ...e, status: "stopped", lastTest: null }));
      }
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

/** source 給 null 就是 app 級訊息，不帶 [源名] 前綴，任何源都看得到 */
function log(source: string | null, msg: string) {
  const line = source ? `${stamp()}  [${source}] ${msg}` : `${stamp()}  ${msg}`;
  state.logs.push(line);
  if (state.logs.length > 500) state.logs.shift();
  void emit("log", line);
}

/**
 * config-changed 的送達延遲，預設 0。
 *
 * 真後端是 invoke 先 resolve、config-changed 才到，兩者之間有真實的時間差；
 * 把延遲拉大就能在瀏覽器裡演練那個順序（例如驗證改名後的選中不會被吃掉）。
 */
let configDelay = 0;

function pushConfig() {
  persist();
  // 先照相：真後端也是序列化當下的狀態再送出，之後的變動不該回頭改到這一份
  const payload = structuredClone(state);
  if (configDelay > 0) window.setTimeout(() => void emit("config-changed", payload), configDelay);
  else void emit("config-changed", payload);
}

/**
 * 下一次 install_update 要不要演成失敗，null 代表照常（永不 resolve）。
 * 由 __mock.updateFails() 開關。
 */
let installFailure: string | null = null;

/** 更新資訊改變時的唯一出口，與真後端一樣：存進狀態並推 update-available */
function setUpdate(info: Snapshot["update"]) {
  state.update = info;
  persist();
  void emit("update-available", info);
}

function findSource(name: string): SourceInfo | undefined {
  return state.sources.find((s) => s.name === name);
}

/** local 全域唯一，所以出口一律用埠號跨源找 */
function find(local: number): { exit: ExitInfo; source: SourceInfo } | undefined {
  for (const source of state.sources) {
    const exit = source.exits.find((e) => e.local === local);
    if (exit) return { exit, source };
  }
  return undefined;
}

function ownerOf(local: number): string {
  return find(local)?.source.name ?? "";
}

/**
 * 已知與真後端的落差：Rust 端的 state.set_exit_status 在狀態與 detail 都
 * 沒變時會直接 return、不重推 exit-status 事件（見 src-tauri/src/state.rs），
 * 這裡為了讓假後端的邏輯簡單直接，一律無條件 emit。實際差異只在「同狀態
 * 又被設一次」這種邊界情況會多送一次事件，UI 收到重複事件本來就是冪等的
 * （paintCard／paintRailStatus 都是整格重畫），不影響畫面表現，故不修行為，
 * 僅在此註記。
 */
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

function runTest(exit: ExitInfo, source: string) {
  if (exit.status !== "connected") return;
  setTest(exit, "testing", "testing…");
  window.setTimeout(
    () => {
      if (exit.status !== "connected") return;
      const result = FAKE_TEST[exit.local];
      if (result) {
        setTest(exit, "ok", result);
        log(source, `port ${exit.local} : ${result}`);
      } else {
        setTest(exit, "fail", "no response");
        log(source, `port ${exit.local} : no response`);
      }
    },
    700 + Math.random() * 900,
  );
}

function start(exit: ExitInfo, source: string) {
  exit.enabled = true;
  setStatus(exit, "connecting");
  log(source, `${exit.name}: starting tunnel on :${exit.local}`);
  later(exit.local, 1100, () => {
    if (exit.status !== "connecting" && exit.status !== "reconnecting") return;
    if (BUSY_PORTS.has(exit.local)) {
      setStatus(exit, "port_busy", `local port ${exit.local} is already in use (PID 8124)`);
      log(source, `${exit.name}: bind :${exit.local} failed, port busy`);
      return;
    }
    setStatus(exit, "connected");
    log(source, `${exit.name}: tunnel up`);
    runTest(exit, source);
  });
}

function stop(exit: ExitInfo, source: string) {
  exit.enabled = false;
  const old = timers.get(exit.local);
  if (old) window.clearTimeout(old);
  setStatus(exit, "stopped");
  log(source, `${exit.name}: stopped`);
}

// ---------------------------------------------------------------- 驗證

/**
 * 與 Rust 端相同的驗證規則；錯誤字串用 `field: message` 開頭讓 UI 能逐欄顯示。
 * local 是跨源全域唯一的，撞到別的源也要擋下來並指出是誰佔走的。
 */
function validateForward(input: {
  source: string;
  originalLocal: number | null;
  name: string;
  local: number;
  remote: string;
}): string | null {
  if (!findSource(input.source)) return `source ${input.source} not found`;
  if (!input.name) return "name: name is required";
  if (!Number.isInteger(input.local) || input.local < 1 || input.local > 65535) {
    return "local: must be 1-65535";
  }

  const clash = find(input.local);
  if (clash && clash.exit.local !== input.originalLocal) {
    const where =
      clash.source.name === input.source
        ? `already used by ${clash.exit.name}`
        : `already used by ${clash.exit.name} in ${clash.source.name}`;
    return `local: ${where}`;
  }

  // remote 只填埠號是合法的（代表伺服器本機的那個埠），正規化留到寫進狀態時才做
  if (/^\d+$/.test(input.remote)) {
    const port = Number(input.remote);
    if (port < 1 || port > 65535) return "remote: must be 1-65535";
  } else if (!/^[^\s:]+:\d+$/.test(input.remote)) {
    return "remote: expected a port or host:port, for example 1080 or 127.0.0.1:1080";
  }

  const owner = findSource(input.source) as SourceInfo;
  const dupName = owner.exits.find((e) => e.name === input.name && e.local !== input.originalLocal);
  if (dupName) return "name: another exit in this source already uses this name";
  return null;
}

function validateSource(input: {
  originalName: string | null;
  name: string;
  host: string;
  user: string;
}): string | null {
  if (!input.name) return "name: name is required";
  if (/\s/.test(input.name)) return "name: must not contain spaces";
  // 照 Rust 端 valid_source_name：不可含中括號，日誌行前綴 `[源名]` 才切得出來
  if (/[[\]]/.test(input.name)) return "name: must not contain brackets";
  const dup = state.sources.find((s) => s.name === input.name && s.name !== input.originalName);
  if (dup) return "name: another source already uses this name";
  if (!input.host) return "host: host is required";
  if (/\s/.test(input.host)) return "host: must not contain spaces";
  if (!input.user) return "user: user is required";
  return null;
}

// ---------------------------------------------------------------- 連線測試

/** 演一下真後端的延遲感：spawn ssh、等它跑完，總要花一點時間 */
const TEST_CONNECTION_DELAY_MS = 1500;

/**
 * host 是假資料裡既有的主機名就演成功，其他一律演失敗，訊息照 ssh 真實會印的
 * 那種「無法解析主機名」樣式，讓瀏覽器模式也能演示兩種結果。
 */
function fakeTestConnection(host: string): TestConnectionResult {
  const known = state.sources.some((s) => s.host === host);
  if (known) return { ok: true, message: "Connected" };
  return {
    ok: false,
    message: `ssh: Could not resolve hostname ${host}: Name or service not known`,
  };
}

// ---------------------------------------------------------------- 指令

interface Args {
  local?: number;
  on?: boolean;
  name?: string;
  host?: string;
  user?: string;
  proxyCommand?: string;
  source?: string;
  originalName?: string | null;
  originalLocal?: number | null;
  remote?: string;
}

function handle(cmd: string, args: Args): unknown {
  switch (cmd) {
    // 自繪標題列的視窗指令，瀏覽器裡沒有視窗可動，只記一行
    case "window_minimize":
    case "window_close":
    case "exit_app":
      log(null, `(browser mock) ${cmd}`);
      return null;

    case "get_state":
      return structuredClone(state);

    // ---------------------------------------------------------- 出口層級

    case "start_exit": {
      const hit = find(args.local as number);
      if (hit) start(hit.exit, hit.source.name);
      return null;
    }

    case "stop_exit": {
      const hit = find(args.local as number);
      if (hit) stop(hit.exit, hit.source.name);
      return null;
    }

    case "restart_exit": {
      const hit = find(args.local as number);
      if (!hit) return null;
      log(hit.source.name, `${hit.exit.name}: reconnecting`);
      stop(hit.exit, hit.source.name);
      window.setTimeout(() => start(hit.exit, hit.source.name), 250);
      return null;
    }

    // ---------------------------------------------------------- 源層級

    case "start_source": {
      const src = findSource(args.name as string);
      if (!src) return null;
      log(src.name, "starting all exits");
      for (const e of src.exits) if (e.status === "stopped") start(e, src.name);
      return null;
    }

    case "stop_source": {
      const src = findSource(args.name as string);
      if (!src) return null;
      log(src.name, "stopping all exits");
      for (const e of src.exits) if (e.status !== "stopped") stop(e, src.name);
      return null;
    }

    case "upsert_source": {
      const input = {
        originalName: args.originalName ?? null,
        name: (args.name ?? "").trim(),
        host: (args.host ?? "").trim(),
        user: (args.user ?? "").trim(),
        proxyCommand: (args.proxyCommand ?? "").trim(),
      };
      const err = validateSource(input);
      if (err) return err;

      const existing = input.originalName === null ? undefined : findSource(input.originalName);
      if (existing) {
        existing.name = input.name;
        existing.host = input.host;
        existing.user = input.user;
        existing.proxyCommand = input.proxyCommand;
        pushConfig();
        log(input.name, `source updated (${input.user}@${input.host})`);
      } else {
        state.sources.push({ ...input, exits: [] });
        pushConfig();
        log(input.name, `source added (${input.user}@${input.host})`);
      }
      return null;
    }

    case "test_connection": {
      const host = (args.host ?? "").trim();
      return new Promise<TestConnectionResult>((resolve) => {
        window.setTimeout(() => resolve(fakeTestConnection(host)), TEST_CONNECTION_DELAY_MS);
      });
    }

    case "delete_source": {
      const src = findSource(args.name as string);
      if (!src) return null;
      for (const e of src.exits) if (e.status !== "stopped") stop(e, src.name);
      state.sources = state.sources.filter((s) => s.name !== src.name);
      pushConfig();
      log(null, `source ${src.name} deleted`);
      return null;
    }

    // ---------------------------------------------------------- 轉發設定

    case "upsert_forward": {
      const input = {
        source: (args.source ?? "").trim(),
        originalLocal: args.originalLocal ?? null,
        name: (args.name ?? "").trim(),
        local: Number(args.local),
        remote: (args.remote ?? "").trim(),
      };
      const err = validateForward(input);
      if (err) return err;

      // 純埠號補成伺服器本機的 host:port，比照真後端會做的正規化
      if (/^\d+$/.test(input.remote)) input.remote = `127.0.0.1:${input.remote}`;

      const owner = findSource(input.source) as SourceInfo;
      const existing = input.originalLocal === null ? undefined : find(input.originalLocal);
      if (existing) {
        const wasRunning = existing.exit.status !== "stopped";
        if (wasRunning) stop(existing.exit, existing.source.name);
        existing.exit.name = input.name;
        existing.exit.local = input.local;
        existing.exit.remote = input.remote;
        pushConfig();
        log(owner.name, `${input.name}: updated`);
        if (wasRunning) start(existing.exit, owner.name);
      } else {
        owner.exits.push({
          name: input.name,
          local: input.local,
          remote: input.remote,
          enabled: true,
          status: "stopped",
          lastTest: null,
        });
        pushConfig();
        log(owner.name, `${input.name}: added`);
      }
      return null;
    }

    case "delete_forward": {
      const hit = find(args.local as number);
      if (hit) {
        stop(hit.exit, hit.source.name);
        hit.source.exits = hit.source.exits.filter((e) => e.local !== hit.exit.local);
        pushConfig();
        log(hit.source.name, `${hit.exit.name}: deleted`);
      }
      return null;
    }

    // ---------------------------------------------------------- 全域設定

    case "set_close_to_tray":
      state.closeToTray = Boolean(args.on);
      pushConfig();
      log(null, state.closeToTray ? "close hides to tray" : "close exits app");
      return null;

    case "set_autostart":
      state.autostart = Boolean(args.on);
      pushConfig();
      log(null, state.autostart ? "autostart enabled" : "autostart disabled");
      return null;

    // 真後端關掉時會順手把已經找到的那一版從畫面上收掉，這裡照做
    case "set_check_for_updates":
      state.checkForUpdates = Boolean(args.on);
      if (!state.checkForUpdates) state.update = null;
      pushConfig();
      log(null, state.checkForUpdates ? "update checks enabled" : "update checks disabled");
      return null;

    // 瀏覽器裡沒有真的設定檔，給一條看得出樣子的假路徑（夠長，順便驗省略號）
    case "get_config_path":
      return "C:\\Users\\browser-mock\\.traytunnel.toml";

    // 也沒有檔案總管可開，只記一行
    case "open_config_dir":
    // 可攜版的 Download：真後端會 ShellExecuteW 開系統瀏覽器，這裡只記一行
    // （瀏覽器裡自己開新分頁多半會被彈出視窗攔截，反而演不出東西）
    case "open_releases_page":
      log(null, `(browser mock) ${cmd}`);
      return null;

    /**
     * 安裝版的 Restart to update。真後端成功時**永遠不會 resolve**——安裝程式
     * 一接手，程式就 exit 了，所以按鈕會一直停用著、畫面停在原樣。這裡刻意
     * 回一個不會 settle 的 promise 把那個行為演到位；要看失敗那條路
     * （按鈕彈回來、錯誤列顯示原因）請用 __mock.updateFails()。
     */
    case "install_update":
      log(null, "downloading update…");
      log(null, "(browser mock) the real app would exit and hand over to the installer here");
      if (installFailure) return Promise.reject(installFailure);
      return new Promise<never>(() => {});

    default:
      log(null, `(browser mock) unhandled command: ${cmd}`);
      return null;
  }
}

/**
 * 截圖／手動演練用的操控面板，只有 dev 模式才存在。
 * 例：__mock.drop(1080) 演練 reconnecting、__mock.wipe() 演練零源空狀態、
 * __mock.reset() 回到初始狀態。
 */
function installScenarioHooks() {
  const api = {
    snapshot: () => structuredClone(state),
    status(local: number, status: ExitStatus, detail?: string) {
      const hit = find(local);
      if (!hit) return;
      const old = timers.get(local);
      if (old) window.clearTimeout(old);
      setStatus(hit.exit, status, detail);
    },
    test(local: number, testState: TestState, text: string) {
      const hit = find(local);
      if (hit) setTest(hit.exit, testState, text);
    },
    /** 演練斷線重連：connected → reconnecting → connected */
    drop(local: number) {
      const hit = find(local);
      if (!hit) return;
      const source = hit.source.name;
      setStatus(hit.exit, "reconnecting");
      log(source, `${hit.exit.name}: connection lost, reconnecting...`);
      later(local, 2500, () => {
        if (hit.exit.status !== "reconnecting") return;
        setStatus(hit.exit, "connected");
        log(source, `${hit.exit.name}: reconnected`);
        runTest(hit.exit, source);
      });
    },
    /** 把所有源清掉，用來看零源的引導空狀態 */
    wipe() {
      for (const s of state.sources) for (const e of s.exits) stop(e, s.name);
      state.sources = [];
      pushConfig();
      log(null, "all sources removed");
    },
    /**
     * 演練背景更新檢查的三種結果，兩種車道的 UI 都看得到：
     *
     *   __mock.update("installed")  安裝版發現新版 → 按鈕是 Restart to update
     *   __mock.update("portable")   可攜／單檔版發現新版 → 按鈕是 Download
     *   __mock.update("none")       已是最新 → 更新列整列收起來
     *   __mock.update("fail")       檢查失敗 → 畫面完全不動，只在活動日誌留一行
     *
     * 對應真後端的行為：查到新版才推 update-available，查不到／失敗都不動畫面
     * （失敗只 log 一行），所以「已最新」與「檢查失敗」在畫面上都是沒有更新列。
     */
    update(kind: "installed" | "portable" | "none" | "fail", version = "9.9.9") {
      if (kind === "fail") {
        log(null, "update check failed: (browser mock) simulated network error");
        return;
      }
      if (kind === "none") {
        log(null, "(browser mock) update check: already up to date");
        setUpdate(null);
        return;
      }
      setUpdate({ version, installed: kind === "installed" });
      log(null, `(browser mock) update available: v${version}`);
    },
    /**
     * 讓下一次（與之後每一次）按 Restart to update 都失敗，用來看錯誤那條路：
     * 按鈕彈回可按、原因寫在設定頁的錯誤列。傳 null 關掉。
     */
    updateFails(message: string | null = "Failed to download the update") {
      installFailure = message;
      return installFailure;
    },
    owner: ownerOf,
    /** 演練「config-changed 晚於 invoke resolve」的事件順序，0 是預設的即時送達 */
    configDelay(ms: number) {
      configDelay = Math.max(0, ms);
      return configDelay;
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
  log(null, "Traytunnel started");
  log(null, "(browser mock) no Tauri runtime, using fake backend");
  window.setTimeout(() => {
    for (const s of state.sources) for (const e of s.exits) if (e.enabled) start(e, s.name);
  }, 250);
}
