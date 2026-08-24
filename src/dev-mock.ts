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
import { shouldProbe } from "./status";
import type {
  ConnKind,
  ConnTarget,
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
import { validateConnName } from "./util";

const STORE_KEY = "traytunnel-dev-mock-v5";

/**
 * local 全域唯一，出口一律用埠號跨連線找。「一條連線」的形狀直接用前端那邊的
 * ConnTarget（判別聯集，payload 一律叫 `data`）——假後端與 UI 對此的認知因此
 * 完全一致，不必再各留一份，連別名都不取，讀的人一眼就知道是同一個型別。
 */
const ownerName = (owner: ConnTarget): string => owner.data.name;

/** 這條連線底下的列。兩種連線的欄位名現在一樣，收窄之後直接取 */
const ownerRows = (owner: ConnTarget): ExitInfo[] => owner.data.exits;

const DEFAULT_SNAPSHOT: Snapshot = {
  closeToTray: true,
  autostart: false,
  automaticUpdates: true,
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
      // 一條有覆寫、一條沒有：編輯面板「帶回現值」與「留空顯示 Auto」兩條
      // 路徑在瀏覽器模式下都調得到
      mtu: 1400,
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
    // ⑥ .conf 讀不到／解析不過的連線：副標讓位給紅字錯誤、引擎點恆紅，
    //    引擎旗標也是關的（起不來），用來驗 confError 那條顯示路徑
    {
      name: "office-wg",
      confPath: "C:\\Users\\browser-mock\\wg\\office.conf",
      enabled: false,
      mtu: null,
      confError: "office.conf: missing PrivateKey in [Interface] (line 3)",
      endpoint: "",
      addresses: [],
      dns: [],
      allowedIps: [],
      exits: [
        {
          name: "intranet",
          local: 1087,
          remote: "10.1.0.20:443",
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
  // 預設沒有新版，更新列不出現；各種更新情境由 __mock 那邊演練（見 installScenarioHooks）
  update: null,
  pendingUpdate: null,
  updateStalled: false,
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
 * 下一次 apply_update 要不要演成失敗，null 代表照常（永不 resolve）。
 * 由 __mock.updateFails() 開關。
 */
let installFailure: string | null = null;

/** 更新的兩條車道：安裝版會自己下載好，可攜版只能開瀏覽器 */
type UpdateKind = "installed" | "portable";

/** 演一下背景下載要花的時間，不然 Downloading… 那顆 spinner 一幀都看不到 */
const DOWNLOAD_DELAY_MS = 1800;

/**
 * 那顆假下載的計時器。
 *
 * 一定要能取消：連叫兩次 `__mock.update()`（或中途 `updateNone()`）時，
 * 先前那顆計時器還在路上，時間到了照樣會把它那一版的 `pendingUpdate` 寫進去
 * ——畫面就會莫名其妙跳回一個已經被換掉、甚至已經被清掉的版本。
 */
let downloadTimer: number | null = null;

function cancelDownload() {
  if (downloadTimer !== null) {
    window.clearTimeout(downloadTimer);
    downloadTimer = null;
  }
}

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
function findConn(name: string): ConnTarget | undefined {
  const src = findSource(name);
  if (src) return { kind: "ssh", data: src };
  const wg = findWgProxy(name);
  if (wg) return { kind: "wg", data: wg };
  return undefined;
}

/** local 全域唯一，所以出口一律用埠號跨連線找，順便把它所屬的連線帶回來 */
function find(local: number): { exit: ExitInfo; owner: ConnTarget } | undefined {
  for (const source of state.sources) {
    const exit = source.exits.find((e) => e.local === local);
    if (exit) return { exit, owner: { kind: "ssh", data: source } };
  }
  for (const proxy of state.wgProxies) {
    const exit = proxy.exits.find((e) => e.local === local);
    if (exit) return { exit, owner: { kind: "wg", data: proxy } };
  }
  return undefined;
}

function ownerOf(local: number): string {
  const hit = find(local);
  return hit ? ownerName(hit.owner) : "";
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
  setTest(exit, "testing", "Connecting…");
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

/**
 * 起一條列。
 *
 * `setIntent` 是給引擎總開關用的逃生口：set_wg_enabled 起引擎時要「只啟動
 * enabled = true 的列」，**不能反過來把列的 enabled 寫成 true**——那樣使用者
 * 原本刻意停用的列會在連線重新打開時全部復活（wg-design.md §5.5 的對照表）。
 * 使用者親手按列開關或 start_exit 走的才是預設的 true（那本來就是在表達意圖）。
 */
function start(exit: ExitInfo, source: string, setIntent = true) {
  if (setIntent) exit.enabled = true;
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

/** 停一條列；`setIntent` 的用意與 start 相同，見上方說明 */
function stop(exit: ExitInfo, source: string, setIntent = true) {
  if (setIntent) exit.enabled = false;
  const old = timers.get(exit.local);
  if (old) window.clearTimeout(old);
  setStatus(exit, "stopped");
  log(source, `${exit.name}: stopped`);
}

/**
 * 編輯一條既有的列：停→改→起→pushConfig。
 *
 * 這個順序是不變式，兩支 upsert 共用同一份實作以免其中一支被改壞：
 *
 *   - 執行中的列要先停再改（埠號可能就是被改的那一個）。
 *   - pushConfig **一定放在最後**。中間那個瞬間 enabled 是 false、status 是
 *     stopped，在那時照相等於送出一份「已經不成立」的快照；前端會照單全收，
 *     之後 status 還有 exit-status 事件補得回來，**enabled 沒有任何事件可以補**
 *     ——開關就這樣卡在 OFF，旁邊卻是一顆綠色的狀態點。真後端是落檔完成才推
 *     config-changed，這裡對齊它。
 */
function editRow(exit: ExitInfo, source: string, mutate: () => void) {
  const wasRunning = exit.status !== "stopped";
  if (wasRunning) stop(exit, source);
  mutate();
  if (wasRunning) start(exit, source);
  pushConfig();
}

// ---------------------------------------------------------------- 驗證

/**
 * 兩種列共通的那一段驗證：名稱必填、埠號範圍、跨連線撞埠、kind 不可變、
 * 同一條連線內不可重名。forward 與 socks 只差在 forward 多一個 remote，
 * 這一段照抄兩份的話，改任何一條規則都得記得改兩個地方。
 *
 * local 是跨連線全域唯一的，撞到別的連線也要擋下來並指出是誰佔走的。
 * kind 建立後不可變（U1）：編輯既有列時若目標 kind 跟現況不符，直接回錯誤，
 * 不動任何欄位。
 */
function validateRowCommon(
  input: { connection: string; originalLocal: number | null; name: string; local: number },
  owner: ConnTarget,
  kind: RowKind,
): string | null {
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
  if (existing && existing.exit.kind !== kind) {
    return "kind: 列的種類建立後不可變更，請刪除後重新新增";
  }

  const dupName = ownerRows(owner).find((e) => e.name === input.name && e.local !== input.originalLocal);
  if (dupName) return "name: another exit in this connection already uses this name";
  return null;
}

/**
 * 與 Rust 端相同的驗證規則；錯誤字串用 `field: message` 開頭讓 UI 能逐欄顯示。
 *
 * connectionKind 與連線的實際型別不符（例如拿 ssh 源名去掛 wg 的 forward 列）
 * 要擋下（W3.37）。forward 列的 kind 恆為 "forward"（兩支 upsert 各自帶入固定的
 * kind，見 wg-design.md §5.5），所以這裡沒有「socks 列掛到 ssh 源底下」這種分支
 * ——那條路走不到，留著只會讓人以為 input.kind 是可變的。
 */
function validateForward(input: {
  connection: string;
  connectionKind: "ssh" | "wg";
  originalLocal: number | null;
  name: string;
  local: number;
  remote: string;
}): string | null {
  const owner = findConn(input.connection);
  if (!owner) return `connection ${input.connection} not found`;
  if (owner.kind !== input.connectionKind) return "kind: connection type mismatch";

  const common = validateRowCommon(input, owner, "forward");
  if (common) return common;

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
  return validateRowCommon(input, owner, "socks");
}

/**
 * 兩種連線層共通的三條規則：名稱本身合法、型別建立後不可變（U1）、名稱不撞。
 *
 * ssh 與 wg 共用同一個命名空間，所以「不可變」這條要兩邊對稱地擋——只擋一邊
 * 等於沒擋：拿一個指向 WG 連線的 originalName 呼叫 upsert_source，findSource
 * 會找不到而落進「新增」分支，憑空生出一條同名的 ssh 源，直接撞車。
 */
function validateConnCommon(
  input: { originalName: string | null; name: string },
  kind: ConnKind,
): string | null {
  // 名稱的三條規則與表單共用 util.ts 的 validateConnName，兩邊訊息保證一致
  const nameErr = validateConnName(input.name);
  if (nameErr) return `name: ${nameErr}`;

  if (input.originalName !== null) {
    const original = findConn(input.originalName);
    if (original && original.kind !== kind) {
      return "name: connection type is immutable, delete and re-add instead";
    }
  }

  const dup = findConn(input.name);
  if (dup && input.name !== input.originalName) return "name: another connection already uses this name";
  return null;
}

function validateSource(input: {
  originalName: string | null;
  name: string;
  host: string;
  user: string;
}): string | null {
  const common = validateConnCommon(input, "ssh");
  if (common) return common;
  if (!input.host) return "host: host is required";
  if (/\s/.test(input.host)) return "host: must not contain spaces";
  if (!input.user) return "user: user is required";
  return null;
}

/**
 * MTU 覆寫的合法範圍與訊息，與 Rust 的 `wg::conf::MTU_RANGE`／
 * `config::mtu_range_error()` 以及 sheet.ts 的本地檢查逐字相同——三份實作講的
 * 必須是同一句話，否則瀏覽器模式演出來的錯誤跟真後端不一樣就沒有參考價值。
 */
const MTU_MIN = 576;
const MTU_MAX = 9000;
const MTU_ERROR = `mtu: must be a whole number between ${MTU_MIN} and ${MTU_MAX}`;

/** WG 連線的驗證：名稱與 ssh 源共用同一個命名空間；U1——不可把 ssh 源改成 wg 連線 */
function validateWgProxy(input: {
  originalName: string | null;
  name: string;
  confPath: string;
  mtu: number | null;
}): string | null {
  const common = validateConnCommon(input, "wg");
  if (common) return common;
  if (!input.confPath.trim()) return "confPath: path is required";
  // null＝不覆寫＝合法（真後端的 validate_wg_proxy 也是這樣：只有真的填了值
  // 才檢查範圍）
  if (input.mtu !== null) {
    if (!Number.isInteger(input.mtu) || input.mtu < MTU_MIN || input.mtu > MTU_MAX) {
      return MTU_ERROR;
    }
  }
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
  mtu?: number | null;
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

    /**
     * 這兩支都會改寫 exit.enabled（那是「使用者要不要它跑」的意圖，見 start／stop
     * 的 setIntent），所以**一定要推 config-changed**——真後端的 set_exit_enabled
     * 就是這樣（PR #35 驗證過），旁邊的 set_wg_enabled 也是。
     *
     * 少了這一行，前端永遠學不到 enabled 變了：exit-status 事件只帶得回 status，
     * 列開關綁的卻是 enabled，於是開關的視覺卡死在舊值、關掉之後再也開不回來。
     *
     * pushConfig 擺在最後，照「最終狀態確定後再推」的既有裁決：start() 是同步
     * 把狀態帶到 connecting 才回來的，這時照相拿到的才是成立的那一份。
     */
    case "start_exit": {
      const hit = find(args.local as number);
      if (!hit) return null;
      start(hit.exit, ownerName(hit.owner));
      pushConfig();
      return null;
    }

    case "stop_exit": {
      const hit = find(args.local as number);
      if (!hit) return null;
      stop(hit.exit, ownerName(hit.owner));
      pushConfig();
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
        mtu: args.mtu ?? null,
      };
      const err = validateWgProxy(input);
      if (err) return err;

      const existing = input.originalName === null ? undefined : findWgProxy(input.originalName);
      if (existing) {
        // 編輯不重接：conf 變更要重接由使用者透過「重新連線」動作觸發，這裡只改欄位。
        // 但換了檔案就要把舊的解析錯誤清掉——否則使用者照著紅字把 .conf 修好、
        // 重新選檔存起來之後，那行紅字還是永遠掛在那裡，整條復原動線根本走不完。
        // （真後端是重新解析新檔後才知道結果；mock 沒有真的解析器，換檔就假設它可解析。）
        if (existing.confPath !== input.confPath) existing.confError = null;
        existing.name = input.name;
        existing.confPath = input.confPath;
        // 清空欄位＝把覆寫拿掉，所以無條件指派（真後端 upsert_wg_proxy 同一條規則）
        existing.mtu = input.mtu;
        pushConfig();
        log(input.name, `connection updated (${input.confPath})`);
      } else {
        state.wgProxies.push({
          name: input.name,
          confPath: input.confPath,
          enabled: false,
          mtu: input.mtu,
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

    /**
     * 引擎總開關（wg-design.md §5.5 第 3 支）。與 ssh 的 stop_source 刻意不對稱：
     *
     *   on = false：停引擎、收掉所有列的監聽器，**各列自身的 enabled 意圖不動**
     *   on = true ：起引擎，只啟動 enabled = true 的列（尊重逐列的意圖）
     *
     * 所以這裡的 start／stop 都帶 setIntent = false。先動狀態再 pushConfig，
     * 快照才會帶著剛剛那批 connecting／stopped 出去，而不是一份過期的 stopped。
     */
    case "set_wg_enabled": {
      const proxy = findWgProxy(args.name as string);
      if (!proxy) return null;
      const on = Boolean(args.on);
      // conf 解析不過的連線起不來——引擎沒有東西可以拿去建隧道。放它通過的話
      // 畫面會出現一個規格上不存在的狀態：引擎點是紅的（confError），底下的列
      // 卻一條條變綠。旗標維持 false，只在活動日誌留一行原因。
      if (on && proxy.confError) {
        log(proxy.name, `cannot start: ${proxy.confError}`);
        return null;
      }
      if (proxy.enabled === on) return null;
      proxy.enabled = on;
      if (on) {
        for (const e of proxy.exits) if (e.enabled && e.status === "stopped") start(e, proxy.name, false);
      } else {
        for (const e of proxy.exits) if (e.status !== "stopped") stop(e, proxy.name, false);
      }
      pushConfig();
      log(proxy.name, on ? "engine started" : "engine stopped");
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

      const owner = findConn(input.connection) as ConnTarget;
      const rows = ownerRows(owner);
      const existing = input.originalLocal === null ? undefined : find(input.originalLocal);
      if (existing) {
        editRow(existing.exit, ownerName(owner), () => {
          existing.exit.name = input.name;
          existing.exit.local = input.local;
          existing.exit.remote = input.remote;
          existing.exit.probeProxy = input.probeProxy;
        });
        log(ownerName(owner), `${input.name}: updated`);
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

      const owner = findConn(input.connection) as ConnTarget & { kind: "wg" };
      const existing = input.originalLocal === null ? undefined : find(input.originalLocal);
      if (existing) {
        editRow(existing.exit, owner.data.name, () => {
          existing.exit.name = input.name;
          existing.exit.local = input.local;
        });
        log(owner.data.name, `${input.name}: updated`);
      } else {
        owner.data.exits.unshift({
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
        log(owner.data.name, `${input.name}: added`);
      }
      return null;
    }

    case "delete_forward": {
      const hit = find(args.local as number);
      if (hit) {
        const name = ownerName(hit.owner);
        stop(hit.exit, name);
        hit.owner.data.exits = hit.owner.data.exits.filter((e) => e.local !== hit.exit.local);
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

    // 真後端關掉時會把已經找到的那一版與已經下載好等著裝的那一份一起收掉
    // （套用那條路跑在設定載入之前，看不到這個開關），這裡照做
    case "set_automatic_updates":
      state.automaticUpdates = Boolean(args.on);
      if (!state.automaticUpdates) {
        cancelDownload();
        state.update = null;
        state.pendingUpdate = null;
        state.updateStalled = false;
      }
      pushConfig();
      log(null, state.automaticUpdates ? "automatic updates enabled" : "automatic updates disabled");
      return null;

    // 瀏覽器裡沒有真的設定檔，給一條看得出樣子的假路徑（夠長，順便驗省略號）
    case "get_config_path":
      return "C:\\Users\\browser-mock\\.traytunnel.toml";

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
     * Restart to update。真後端成功時**永遠不會 resolve**——安裝程式一接手，
     * 程式就 exit 了，所以按鈕會一直停在 Restarting…、畫面停在原樣。這裡刻意
     * 回一個不會 settle 的 promise 把那個行為演到位；要看失敗那條路
     * （按鈕彈回來、錯誤列顯示原因）請用 __mock.updateFails()。
     */
    case "apply_update":
      log(null, `restarting to install v${state.pendingUpdate ?? "?"}`);
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
     * 演練整條自動更新：背景查到新版 → 靜默下載 → 就緒等重啟。
     *
     *   __mock.update("installed")  安裝版：綠鈕先轉 Downloading v9.9.9…，
     *                               約兩秒後變成 Restart to update (v9.9.9)
     *   __mock.update("portable")   可攜／單檔版：綠鈕 Get v9.9.9（沒有下載這一段）
     *
     * 真後端的背景車道查不到新版或檢查失敗時都完全不動畫面（失敗只 log 一行），
     * 所以那兩種結果沒有東西好演；要清掉現在這一份請用 __mock.updateNone()。
     */
    update(kind: UpdateKind, version = "9.9.9") {
      // 上一輪的假下載還在路上就先取消，免得它時間到了把舊版本寫回來
      cancelDownload();
      state.pendingUpdate = null;
      state.updateStalled = false;
      setUpdate({ version, installed: kind === "installed" });
      log(null, `(browser mock) update available: v${version}`);
      if (kind !== "installed") return;
      log(null, `downloading update v${version} in the background`);
      downloadTimer = window.setTimeout(() => {
        downloadTimer = null;
        state.pendingUpdate = version;
        pushConfig();
        log(
          null,
          `update v${version} downloaded, it will be installed the next time Traytunnel starts`,
        );
      }, DOWNLOAD_DELAY_MS);
    },
    /**
     * 演練「查到新版、但下載失敗正在退避等重試」：主鈕變成琥珀色的
     * `Download failed — will retry`，**不轉圈**。先 update("installed") 再叫它。
     */
    updateStalls() {
      cancelDownload();
      state.pendingUpdate = null;
      state.updateStalled = true;
      pushConfig();
      log(null, "update download failed: (browser mock) simulated network error");
      return state.updateStalled;
    },
    /** 回到「沒有任何更新」：標題退回 Version，主鈕整顆消失 */
    updateNone() {
      cancelDownload();
      state.pendingUpdate = null;
      state.updateStalled = false;
      setUpdate(null);
      pushConfig();
      log(null, "(browser mock) no update available");
    },
    /**
     * 讓下一次（與之後每一次）按 Restart to update 都失敗，用來看錯誤那條路：
     * 鈕從 Restarting… 彈回 Restart to update、原因寫在設定頁的錯誤列。傳 null 關掉。
     */
    updateFails(message: string | null = "Failed to start the installer") {
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
