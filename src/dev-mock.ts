/**
 * 瀏覽器 UI 開發模式的假後端（IPC 契約 v4：SSH＋WireGuard）。
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
  ProxyProtocol,
  RowKind,
  Snapshot,
  SourceInfo,
  TestConnectionResult,
  TestState,
  WgProxyInfo,
} from "./types";

const STORE_KEY = "traytunnel-dev-mock-v4";

/** local 全域唯一，出口一律用埠號跨連線找 —— 這裡把 ssh 源與 wg 連線的所屬統一成一種形狀 */
type Owner = { kind: "ssh"; source: SourceInfo } | { kind: "wg"; proxy: WgProxyInfo };

function ownerName(owner: Owner): string {
  return owner.kind === "ssh" ? owner.source.name : owner.proxy.name;
}

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
        // ② forward + probeProxy=true：後端是代理服務，會做出口檢測並識別協定
        {
          name: "socks-jp",
          local: 1080,
          remote: "127.0.0.1:1080",
          kind: "forward",
          probeProxy: true,
          enabled: true,
          status: "stopped",
          lastTest: null,
        },
        // ① forward + probeProxy=false：純轉發，只有狀態點
        {
          name: "pg-jp",
          local: 1081,
          remote: "10.0.4.12:5432",
          kind: "forward",
          probeProxy: false,
          enabled: true,
          status: "stopped",
          lastTest: null,
        },
      ],
    },
    {
      name: "taipei",
      host: "gw-tw.example.com",
      user: "ec2-user",
      proxyCommand: "cloudflared access ssh --hostname %h",
      exits: [
        {
          name: "socks-tw",
          local: 1083,
          remote: "127.0.0.1:1083",
          kind: "forward",
          probeProxy: true,
          enabled: true,
          status: "stopped",
          lastTest: null,
        },
        {
          name: "edge-tw",
          local: 1084,
          remote: "127.0.0.1:1084",
          kind: "forward",
          probeProxy: false,
          enabled: true,
          status: "stopped",
          lastTest: null,
        },
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
  wgProxies: [
    {
      name: "home-relay",
      confPath: "C:\\Users\\browser-mock\\wg\\home-relay.conf",
      enabled: true,
      confError: null,
      endpoint: "vpn.example.com:51820",
      addresses: ["10.9.0.2/32"],
      dns: ["10.9.0.1"],
      allowedIps: ["0.0.0.0/0", "::/0"],
      exits: [
        // ⑤ socks 列：引擎自建 SOCKS5，恆測、協定已知，排在最前（SOCKS5 區段）
        {
          name: "proxy",
          local: 1085,
          remote: null,
          kind: "socks",
          probeProxy: true,
          enabled: true,
          status: "stopped",
          lastTest: null,
        },
        // ④ forward + probeProxy=true：接隧道對面已經在跑的代理服務
        {
          name: "corp",
          local: 1086,
          remote: "10.0.0.9:1080",
          kind: "forward",
          probeProxy: true,
          enabled: true,
          status: "stopped",
          lastTest: null,
        },
        // ③ forward：本地埠 → 隧道內固定目的地，語意等同 ssh 的 -L
        {
          name: "nas-ssh",
          local: 2222,
          remote: "10.0.0.5:22",
          kind: "forward",
          probeProxy: false,
          enabled: true,
          status: "stopped",
          lastTest: null,
        },
      ],
    },
  ],
  logs: [],
  // 預設沒有新版，更新列不出現；三種更新情境由 __mock 那邊演練（見 installScenarioHooks）
  update: null,
};

/** 假的出口自測結果，格式與真後端一致（`ip  city, country`）；沒列到的埠就當作測不到 */
const FAKE_TEST: Record<number, { text: string; protocol?: ProxyProtocol }> = {
  1080: { text: "203.0.113.42  Tokyo, JP", protocol: "socks5" },
  1083: { text: "198.51.100.17  Taipei, TW", protocol: "socks5" },
  1085: { text: "45.32.99.10  Amsterdam, NL", protocol: "socks5" },
  1086: { text: "45.32.99.10  Amsterdam, NL", protocol: "http" },
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
      for (const p of saved.wgProxies ?? []) {
        p.exits = p.exits.map((e) => ({ ...e, status: "stopped", lastTest: null }));
      }
      saved.wgProxies = saved.wgProxies ?? [];
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
    /* 無痕模式之類的存不進去就略過，記憶體裡的狀態仍然正確 */
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

/** 手動檢查（主鈕與下拉的 Check now）下一次要演出哪一種結果，由 __mock.updateNext() 換 */
type CheckKind = "installed" | "portable" | "none" | "fail";
let nextCheck: { kind: CheckKind; version: string } = { kind: "none", version: "9.9.9" };

/** 演一下真的連外要花的時間，不然 Checking… 那顆 spinner 一幀都看不到 */
const CHECK_DELAY_MS = 1200;

/** 更新資訊改變時的唯一出口，與真後端一樣：存進狀態並推 update-available */
function setUpdate(info: Snapshot["update"]) {
  state.update = info;
  persist();
  void emit("update-available", info);
}

function findSource(name: string): SourceInfo | undefined {
  return state.sources.find((s) => s.name === name);
}

function findWgProxy(name: string): WgProxyInfo | undefined {
  return state.wgProxies.find((p) => p.name === name);
}

/** 兩種連線共用同一個命名空間，日誌前綴 `[名字]` 才不會撞 */
function findConn(name: string): Owner | undefined {
  const src = findSource(name);
  if (src) return { kind: "ssh", source: src };
  const wg = findWgProxy(name);
  if (wg) return { kind: "wg", proxy: wg };
  return undefined;
}

/** local 全域唯一，所以出口一律用埠號跨連線找，順便把它所屬的連線帶回來 */
function find(local: number): { exit: ExitInfo; owner: Owner } | undefined {
  for (const source of state.sources) {
    const exit = source.exits.find((e) => e.local === local);
    if (exit) return { exit, owner: { kind: "ssh", source } };
  }
  for (const proxy of state.wgProxies) {
    const exit = proxy.exits.find((e) => e.local === local);
    if (exit) return { exit, owner: { kind: "wg", proxy } };
  }
  return undefined;
}

function ownerOf(local: number): string {
  const hit = find(local);
  return hit ? ownerName(hit.owner) : "";
}

/** 這一列要不要被探測：kind=socks 恆真，其餘看 probeProxy（wg-design.md §5.4 的 should_probe） */
function shouldProbe(exit: ExitInfo): boolean {
  return exit.kind === "socks" || exit.probeProxy;
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
  // 比照 main.ts 的 applyExitStatus：非 connected 一律清舊的自測結果，
  // 不只 stopped——斷線重連期間舊的「測試成功」字樣沒有理由繼續掛著。
  // 用 clearTest 推清除形狀的 exit-test 事件，讓瀏覽器模式也演練得到
  // applyExitTest 的清除分支，而不只是靠本地直接清 state.exits。
  if (status !== "connected") clearTest(exit);
  void emit("exit-status", { local: exit.local, status, detail: detail ?? null });
}

function setTest(exit: ExitInfo, testState: TestState, text: string, protocol?: ProxyProtocol) {
  exit.lastTest = protocol ? { state: testState, text, protocol } : { state: testState, text };
  void emit("exit-test", { local: exit.local, state: testState, text, protocol });
}

/**
 * 清除某出口的自測結果，payload 只帶 `{ local }`——對齊真後端
 * clear_exit_test 推的清除事件形狀（state／text 整個不存在，不是空字串），
 * 見 types.ts 的 ExitTestEvent 與 main.ts 的 applyExitTest。
 */
function clearTest(exit: ExitInfo) {
  exit.lastTest = null;
  void emit("exit-test", { local: exit.local });
}

function later(local: number, ms: number, fn: () => void) {
  const old = timers.get(local);
  if (old) window.clearTimeout(old);
  timers.set(local, window.setTimeout(fn, ms));
}

/** 只有要被探測的列（kind=socks 或 probeProxy=true）才跑自測，純轉發列連排程都不進（wg-design.md §5.4） */
function runTest(exit: ExitInfo, source: string) {
  if (exit.status !== "connected") return;
  if (!shouldProbe(exit)) return;
  setTest(exit, "testing", "testing…");
  window.setTimeout(
    () => {
      if (exit.status !== "connected") return;
      const result = FAKE_TEST[exit.local];
      if (result) {
        setTest(exit, "ok", result.text, result.protocol);
        log(source, `port ${exit.local} : ${result.text}`);
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
 * local 是跨連線全域唯一的，撞到別的連線也要擋下來並指出是誰佔走的。
 *
 * kind 建立後不可變（U1）：編輯既有列時若 input.kind 跟現況不符，直接回錯誤，
 * 不動任何欄位。connectionKind 與連線的實際型別不符（例如拿 ssh 源名去掛
 * wg 的 forward 列）也一併擋下（W3.37）。
 */
function validateForward(input: {
  connection: string;
  connectionKind: "ssh" | "wg";
  originalLocal: number | null;
  name: string;
  local: number;
  remote: string;
  kind: RowKind;
}): string | null {
  const owner = findConn(input.connection);
  if (!owner) return `connection ${input.connection} not found`;
  if (owner.kind !== input.connectionKind) return "kind: connection type mismatch";
  if (input.kind === "socks" && owner.kind !== "wg") {
    return "kind: socks rows are only allowed under a WireGuard connection";
  }
  if (!input.name) return "name: name is required";
  if (!Number.isInteger(input.local) || input.local < 1 || input.local > 65535) {
    return "local: must be 1-65535";
  }

  const clash = find(input.local);
  if (clash && clash.exit.local !== input.originalLocal) {
    const clashOwner = ownerName(clash.owner);
    const where =
      clashOwner === input.connection
        ? `already used by ${clash.exit.name}`
        : `already used by ${clash.exit.name} in ${clashOwner}`;
    return `local: ${where}`;
  }

  const existing = input.originalLocal === null ? undefined : find(input.originalLocal);
  if (existing && existing.exit.kind !== input.kind) {
    return "kind: 列的種類建立後不可變更，請刪除後重新新增";
  }

  // remote 只填埠號是合法的（代表伺服器本機的那個埠），正規化留到寫進狀態時才做。
  // host:port 分支也要把埠號抽出來驗上限，不能只驗格式——999999 這種位數
  // 符合 \d+ 但早已超過埠號範圍，跟 sheet.ts 的前端檢查與 Rust 端對稱。
  if (/^\d+$/.test(input.remote)) {
    const port = Number(input.remote);
    if (port < 1 || port > 65535) return "remote: must be 1-65535";
  } else {
    const m = /^([^\s:]+):(\d+)$/.exec(input.remote);
    if (!m) return "remote: expected a port or host:port, for example 1080 or 127.0.0.1:1080";
    const port = Number(m[2]);
    if (port < 1 || port > 65535) return "remote: must be 1-65535";
  }

  const rows = owner.kind === "ssh" ? owner.source.exits : owner.proxy.exits;
  const dupName = rows.find((e) => e.name === input.name && e.local !== input.originalLocal);
  if (dupName) return "name: another exit in this connection already uses this name";
  return null;
}

/** WG 專屬的 socks 列驗證：沒有 remote，connection 必須是 wg 連線 */
function validateWgSocks(input: {
  connection: string;
  originalLocal: number | null;
  name: string;
  local: number;
}): string | null {
  const owner = findConn(input.connection);
  if (!owner) return `connection ${input.connection} not found`;
  if (owner.kind !== "wg") return "connection: socks rows are only allowed under a WireGuard connection";
  if (!input.name) return "name: name is required";
  if (!Number.isInteger(input.local) || input.local < 1 || input.local > 65535) {
    return "local: must be 1-65535";
  }

  const clash = find(input.local);
  if (clash && clash.exit.local !== input.originalLocal) {
    const clashOwner = ownerName(clash.owner);
    const where =
      clashOwner === input.connection
        ? `already used by ${clash.exit.name}`
        : `already used by ${clash.exit.name} in ${clashOwner}`;
    return `local: ${where}`;
  }

  const existing = input.originalLocal === null ? undefined : find(input.originalLocal);
  if (existing && existing.exit.kind !== "socks") {
    return "kind: 列的種類建立後不可變更，請刪除後重新新增";
  }

  const dupName = owner.proxy.exits.find((e) => e.name === input.name && e.local !== input.originalLocal);
  if (dupName) return "name: another exit in this connection already uses this name";
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
  const dup = findConn(input.name);
  if (dup && input.name !== input.originalName) return "name: another connection already uses this name";
  if (!input.host) return "host: host is required";
  if (/\s/.test(input.host)) return "host: must not contain spaces";
  if (!input.user) return "user: user is required";
  return null;
}

/** WG 連線的驗證：名稱與 ssh 源共用同一個命名空間；U1——不可把 ssh 源改成 wg 連線 */
function validateWgProxy(input: {
  originalName: string | null;
  name: string;
  confPath: string;
}): string | null {
  if (!input.name) return "name: name is required";
  if (/\s/.test(input.name)) return "name: must not contain spaces";
  if (/[[\]]/.test(input.name)) return "name: must not contain brackets";

  if (input.originalName !== null) {
    const original = findConn(input.originalName);
    if (original && original.kind !== "wg") {
      return "name: connection type is immutable, delete and re-add instead";
    }
  }

  const dup = findConn(input.name);
  if (dup && input.name !== input.originalName) return "name: another connection already uses this name";
  if (!input.confPath.trim()) return "confPath: path is required";
  return null;
}

// ---------------------------------------------------------------- 連線測試

/** 模擬真後端的延遲：spawn ssh、等它跑完，總要花一點時間 */
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

/** .conf 路徑打中假資料裡任何 wg 連線目前的路徑，或看起來像 .conf 就演成功 */
function fakeTestWgConf(confPath: string): TestConnectionResult {
  const trimmed = confPath.trim();
  if (!trimmed) return { ok: false, message: "no .conf file selected" };
  if (!/\.conf$/i.test(trimmed)) {
    return { ok: false, message: "not a valid WireGuard config: missing [Interface] section" };
  }
  return { ok: true, message: "Handshake succeeded (128ms)" };
}

/** 假的檔案選擇器結果，循環吐出幾個看起來合理的路徑，示範成功與尚未使用兩種情境 */
const FAKE_CONF_PICKS = [
  "C:\\Users\\browser-mock\\wg\\jp-node.conf",
  "C:\\Users\\browser-mock\\Documents\\wireguard\\office.conf",
];
let fakeConfPickIdx = -1;

// ---------------------------------------------------------------- 指令

interface Args {
  local?: number;
  on?: boolean;
  name?: string;
  host?: string;
  user?: string;
  proxyCommand?: string;
  source?: string;
  connection?: string;
  connectionKind?: "ssh" | "wg";
  originalName?: string | null;
  originalLocal?: number | null;
  remote?: string;
  probeProxy?: boolean;
  confPath?: string;
  version?: string | null;
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

    // ---------------------------------------------------------- 出口層級（ssh／wg 共用同一個埠鍵空間）

    case "start_exit": {
      const hit = find(args.local as number);
      if (hit) start(hit.exit, ownerName(hit.owner));
      return null;
    }

    case "stop_exit": {
      const hit = find(args.local as number);
      if (hit) stop(hit.exit, ownerName(hit.owner));
      return null;
    }

    case "restart_exit": {
      const hit = find(args.local as number);
      if (!hit) return null;
      const owner = ownerName(hit.owner);
      log(owner, `${hit.exit.name}: reconnecting`);
      stop(hit.exit, owner);
      window.setTimeout(() => start(hit.exit, owner), 250);
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

    // ---------------------------------------------------------- WireGuard 連線層

    case "upsert_wg_proxy": {
      const input = {
        originalName: args.originalName ?? null,
        name: (args.name ?? "").trim(),
        confPath: (args.confPath ?? "").trim(),
      };
      const err = validateWgProxy(input);
      if (err) return err;

      const existing = input.originalName === null ? undefined : findWgProxy(input.originalName);
      if (existing) {
        // 編輯不重接：conf 變更要重接由使用者透過「重新連線」動作觸發，這裡只改欄位
        existing.name = input.name;
        existing.confPath = input.confPath;
        pushConfig();
        log(input.name, `connection updated (${input.confPath})`);
      } else {
        state.wgProxies.push({
          name: input.name,
          confPath: input.confPath,
          enabled: false,
          confError: null,
          endpoint: "",
          addresses: [],
          dns: [],
          allowedIps: [],
          exits: [],
        });
        pushConfig();
        log(input.name, `WireGuard connection added (${input.confPath})`);
      }
      return null;
    }

    case "delete_wg_proxy": {
      const proxy = findWgProxy(args.name as string);
      if (!proxy) return null;
      for (const e of proxy.exits) if (e.status !== "stopped") stop(e, proxy.name);
      state.wgProxies = state.wgProxies.filter((p) => p.name !== proxy.name);
      pushConfig();
      log(null, `WireGuard connection ${proxy.name} deleted`);
      return null;
    }

    case "test_wg_conf": {
      const confPath = (args.confPath ?? "").trim();
      return new Promise<TestConnectionResult>((resolve) => {
        window.setTimeout(() => resolve(fakeTestWgConf(confPath)), TEST_CONNECTION_DELAY_MS);
      });
    }

    case "pick_wg_conf": {
      fakeConfPickIdx = (fakeConfPickIdx + 1) % FAKE_CONF_PICKS.length;
      const path = FAKE_CONF_PICKS[fakeConfPickIdx];
      log(null, `(browser mock) picked ${path}`);
      return path;
    }

    // ---------------------------------------------------------- 轉發設定（forward 列，ssh／wg 共用）

    case "upsert_forward": {
      const input = {
        connection: (args.connection ?? "").trim(),
        connectionKind: (args.connectionKind ?? "ssh") as "ssh" | "wg",
        originalLocal: args.originalLocal ?? null,
        name: (args.name ?? "").trim(),
        local: Number(args.local),
        remote: (args.remote ?? "").trim(),
        kind: "forward" as RowKind,
        probeProxy: Boolean(args.probeProxy),
      };
      const err = validateForward(input);
      if (err) return err;

      // 純埠號補成伺服器本機的 host:port，比照真後端會做的正規化
      if (/^\d+$/.test(input.remote)) input.remote = `127.0.0.1:${input.remote}`;

      const owner = findConn(input.connection) as Owner;
      const rows = owner.kind === "ssh" ? owner.source.exits : owner.proxy.exits;
      const existing = input.originalLocal === null ? undefined : find(input.originalLocal);
      if (existing) {
        const wasRunning = existing.exit.status !== "stopped";
        if (wasRunning) stop(existing.exit, ownerName(owner));
        existing.exit.name = input.name;
        existing.exit.local = input.local;
        existing.exit.remote = input.remote;
        existing.exit.probeProxy = input.probeProxy;
        pushConfig();
        log(ownerName(owner), `${input.name}: updated`);
        if (wasRunning) start(existing.exit, ownerName(owner));
      } else {
        rows.push({
          name: input.name,
          local: input.local,
          remote: input.remote,
          kind: "forward",
          probeProxy: input.probeProxy,
          enabled: true,
          status: "stopped",
          lastTest: null,
        });
        pushConfig();
        log(ownerName(owner), `${input.name}: added`);
      }
      return null;
    }

    // ---------------------------------------------------------- SOCKS5 代理列（WG 專屬）

    case "upsert_wg_socks": {
      const input = {
        connection: (args.connection ?? "").trim(),
        originalLocal: args.originalLocal ?? null,
        name: (args.name ?? "").trim(),
        local: Number(args.local),
      };
      const err = validateWgSocks(input);
      if (err) return err;

      const owner = findConn(input.connection) as Owner & { kind: "wg" };
      const existing = input.originalLocal === null ? undefined : find(input.originalLocal);
      if (existing) {
        const wasRunning = existing.exit.status !== "stopped";
        if (wasRunning) stop(existing.exit, owner.proxy.name);
        existing.exit.name = input.name;
        existing.exit.local = input.local;
        pushConfig();
        log(owner.proxy.name, `${input.name}: updated`);
        if (wasRunning) start(existing.exit, owner.proxy.name);
      } else {
        owner.proxy.exits.unshift({
          name: input.name,
          local: input.local,
          remote: null,
          kind: "socks",
          probeProxy: true,
          enabled: true,
          status: "stopped",
          lastTest: null,
        });
        pushConfig();
        log(owner.proxy.name, `${input.name}: added`);
      }
      return null;
    }

    case "delete_forward": {
      const hit = find(args.local as number);
      if (hit) {
        const name = ownerName(hit.owner);
        stop(hit.exit, name);
        if (hit.owner.kind === "ssh") {
          hit.owner.source.exits = hit.owner.source.exits.filter((e) => e.local !== hit.exit.local);
        } else {
          hit.owner.proxy.exits = hit.owner.proxy.exits.filter((e) => e.local !== hit.exit.local);
        }
        pushConfig();
        log(name, `${hit.exit.name}: deleted`);
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

    /**
     * 手動檢查更新。三種結果都演得到，由 __mock.updateNext() 先選好：
     * 有新版（兩條車道各一）、已是最新、檢查失敗。
     *
     * 跟真後端一樣**不看 state.checkForUpdates**——那個開關管的是背景自動連外，
     * 使用者親手按下按鈕是另一回事，關著也照樣要查得動。
     *
     * 這裡先 setUpdate 再 resolve，刻意重現真後端的事件／回傳競態：
     * update-available 事件會比 invoke 的 resolve 早到，前端在 checking 狀態下
     * 必須忍住不讓那個事件插隊改按鈕（見 sheet.ts 的 applyUpdateInfo）。
     */
    case "check_for_updates_now":
      log(null, "checking for updates…");
      return new Promise<Snapshot["update"]>((resolve, reject) => {
        window.setTimeout(() => {
          const { kind, version } = nextCheck;
          if (kind === "fail") {
            log(null, "update check failed: (browser mock) simulated network error");
            reject("(browser mock) simulated network error");
            return;
          }
          if (kind === "none") {
            setUpdate(null);
            log(null, "update check: already up to date");
            resolve(null);
            return;
          }
          const info = { version, installed: kind === "installed" };
          setUpdate(info);
          log(null, `update check: v${version} is available`);
          resolve(info);
        }, CHECK_DELAY_MS);
      });

    // 單一版本的 release 頁：真後端會 ShellExecuteW 開系統瀏覽器，這裡把要開的
    // 版本記進日誌，至少看得出可攜版的 Get 與下拉的 View release notes 各自帶了什麼
    case "open_release_page":
      log(null, `(browser mock) open_release_page → ${args.version ?? "releases/latest"}`);
      return null;

    // 也沒有檔案總管可開，只記一行
    case "open_config_dir":
    // Releases 列表頁：同樣只記一行（瀏覽器裡自己開新分頁多半會被彈出視窗攔截，
    // 反而演不出東西）
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
    test(local: number, testState: TestState, text: string, protocol?: ProxyProtocol) {
      const hit = find(local);
      if (hit) setTest(hit.exit, testState, text, protocol);
    },
    /** 演練斷線重連：connected → reconnecting → connected */
    drop(local: number) {
      const hit = find(local);
      if (!hit) return;
      const source = ownerName(hit.owner);
      setStatus(hit.exit, "reconnecting");
      log(source, `${hit.exit.name}: connection lost, reconnecting...`);
      later(local, 2500, () => {
        if (hit.exit.status !== "reconnecting") return;
        setStatus(hit.exit, "connected");
        log(source, `${hit.exit.name}: reconnected`);
        runTest(hit.exit, source);
      });
    },
    /** 把所有源與 wg 連線清掉，用來看零連線的引導空狀態 */
    wipe() {
      for (const s of state.sources) for (const e of s.exits) stop(e, s.name);
      for (const p of state.wgProxies) for (const e of p.exits) stop(e, p.name);
      state.sources = [];
      state.wgProxies = [];
      pushConfig();
      log(null, "all connections removed");
    },
    /**
     * 演練**背景**更新檢查的結果，也就是不經使用者操作、直接推 update-available
     * 那條路。版本列的標題與 split button 會當場跟著變：
     *
     *   __mock.update("installed")  安裝版發現新版 → 綠鈕 Update to vX.Y.Z
     *   __mock.update("portable")   可攜／單檔版發現新版 → 綠鈕 Get vX.Y.Z
     *   __mock.update("none")       已是最新 → 標題退回 Version、鈕退回 Check for updates
     *   __mock.update("fail")       檢查失敗 → 畫面完全不動，只在活動日誌留一行
     *
     * 對應真後端的行為：背景車道查不到／失敗都不動畫面（失敗只 log 一行），
     * 所以那兩種結果在背景這條路上是看不出來的。想看 Up to date 與 Check failed
     * 那兩個瞬態，要走手動檢查——用 updateNext() 選好結果再按按鈕。
     */
    update(kind: CheckKind, version = "9.9.9") {
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
     * 選好**手動**檢查（主鈕的 Check for updates 或下拉的 Check now）下一次要
     * 演出什麼結果，然後去按那顆鈕，就能把整條狀態機走一遍：
     *
     *   __mock.updateNext("none")       按下去 → Checking… → Up to date → 兩秒後退回
     *   __mock.updateNext("fail")       按下去 → Checking… → Check failed → 兩秒後退回
     *   __mock.updateNext("installed")  按下去 → Checking… → 綠鈕 Update to vX.Y.Z
     *   __mock.updateNext("portable")   按下去 → Checking… → 綠鈕 Get vX.Y.Z
     *
     * 選好之後每一次手動檢查都是同一個結果，要換再叫一次。
     */
    updateNext(kind: CheckKind, version = "9.9.9") {
      nextCheck = { kind, version };
      return nextCheck;
    },
    /**
     * 讓下一次（與之後每一次）按綠色的 Update to vX.Y.Z 都失敗，用來看錯誤那條路：
     * 鈕從 Updating… 彈回 Update to vX.Y.Z、原因寫在設定頁的錯誤列。傳 null 關掉。
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
    for (const p of state.wgProxies) if (p.enabled) for (const e of p.exits) if (e.enabled) start(e, p.name);
  }, 250);
}
