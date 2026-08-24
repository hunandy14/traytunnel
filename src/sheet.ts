/**
 * 四個 sheet dialog 與一個設定頁：
 *
 * 1. 連線的新增／編輯 sheet —— 置中覆蓋層，SSH／WireGuard 共用同一張 sheet，
 *    靠類型分頁（僅新增時可切換，wg-design.md U1：連線類型建立後不可變）分出
 *    兩組欄位。按下 Save 才送出，後端的錯誤字串用 `field: message` 前綴逐欄
 *    顯示；刪除連線用一次確認（就地把頁腳換成確認列），不走 undo。
 * 2. 轉發（forward 列）的新增／編輯 sheet —— SSH 與 WG 共用同一套表單，
 *    REMOTE 欄位下方多一顆「Destination is a proxy」switch（probeProxy，
 *    wg-design.md §1.6），隨時可改。刪除走 undo toast（畫面先移除、5 秒內
 *    可收回），實際的倒數與復原由 main.ts 的 onDelete 接手。
 * 3. SOCKS5 代理列（kind=socks）的新增／編輯 sheet —— WG 專屬，只有名稱／
 *    本地埠兩欄，跟轉發表單完全分開（沒有目的地、沒有 probeProxy 開關）。
 *    刪除同樣走 undo toast，與轉發列共用同一個 onDelete 回呼（local 是
 *    全域唯一鍵，main.ts 那邊不必知道是哪一種列）。
 * 4. 主區的全域設定頁 —— 兩個 toggle 即時生效，失敗就把畫面翻回去。
 */

import { afterTransition, el, isToggleOn, setToggle } from "./dom";
import { setIcon, type IconName } from "./icons";
import {
  checkForUpdatesNow,
  deleteSource,
  deleteWgProxy,
  getConfigPath,
  installUpdate,
  openConfigDir,
  openReleasePage,
  openReleasesPage,
  pickWgConf,
  setAutostart,
  setCheckForUpdates,
  setCloseToTray,
  testConnection,
  testWgConf,
  upsertForward,
  upsertSource,
  upsertWgProxy,
  upsertWgSocks,
} from "./ipc";
import type { ConnKind, ConnTarget, ExitInfo, Snapshot } from "./types";
import { basename, validateConnName } from "./util";
import { loadAppVersion } from "./version";

// ---------------------------------------------------------------- sheet 共用

/**
 * 逐欄錯誤：每個 .field-<key> 裡固定有一個 .field-error 與一個 .input，
 * 訊息掛在前者、紅框掛在後者。空字串就是把錯誤清掉。
 */
function setFieldError(root: HTMLElement, key: string, msg: string) {
  const box = root.querySelector(`.field-${key}`) as HTMLElement;
  const err = box.querySelector(".field-error") as HTMLElement;
  err.textContent = msg;
  err.classList.toggle("show", Boolean(msg));
  (box.querySelector(".input") as HTMLElement).classList.toggle("invalid", Boolean(msg));
}

/** 認不出欄位前綴的錯誤放這裡，位置在按鈕上方 */
function setGeneralError(node: HTMLElement, msg: string) {
  node.textContent = msg;
  node.classList.toggle("show", Boolean(msg));
}

/**
 * 後端錯誤字串（約定 `field: message`）的統一路由，四張 sheet 共用。
 *
 * `keyMap` 明列**這張 sheet 認得的前綴**與它對應的欄位鍵。認不得的前綴一律
 * 原文進 general error——這一點是刻意的，兩個方向的錯誤都出過事：
 *
 *   - 只用正規表示式抓前綴、再直接拿去當欄位鍵：ssh 分頁收到 `confPath:`
 *     （wg 才有的欄位）時會去找不存在的 `.field-confPath`，querySelector 回
 *     null，接著在它身上取 `.field-error` 就是一個 TypeError——真正的錯誤訊息
 *     反而被這個例外蓋掉，使用者什麼都看不到。
 *   - 反過來，用「不是 A 就當成 B」的二分法兜底：wg 分頁會把任何沒見過的前綴
 *     全部押到 Name 欄，等於指著一個沒問題的欄位說它錯了。
 *
 * 原文而不是去掉前綴：general error 是給人讀的最後一道，寧可多一個前綴也不要
 * 誤砍掉訊息本身的一部分（後端也會回 `ssh: Could not resolve hostname …`
 * 這種**本身就以冒號開頭**、前綴並不是欄位名的句子）。
 */
function applyFieldError(
  root: HTMLElement,
  msg: string,
  keyMap: Record<string, string>,
  generalNode: HTMLElement,
) {
  const m = /^\s*([A-Za-z]+)\s*:\s*([\s\S]+)$/.exec(msg);
  const key = m ? keyMap[m[1].toLowerCase()] : undefined;
  if (!m || !key) {
    setGeneralError(generalNode, msg);
    return;
  }
  setFieldError(root, key, m[2].trim());
}

/** Test 按鈕的就地結果：成功綠字、失敗紅字，空字串就整行藏起來 */
function setTestResult(node: HTMLElement, msg: string, ok: boolean) {
  node.textContent = msg;
  node.classList.toggle("show", Boolean(msg));
  node.classList.toggle("ok", ok);
  node.classList.toggle("fail", !ok);
}

function showSheet(node: HTMLElement, focus: HTMLInputElement) {
  node.hidden = false;
  // 先掛上再加 class，進場的 opacity／scale 過渡才有起始幀
  requestAnimationFrame(() => node.classList.add("open"));
  window.setTimeout(() => focus.focus(), 60);
}

/** stillClosed 是為了防「關到一半又被打開」時把新開的那次藏掉 */
function hideSheet(node: HTMLElement, stillClosed: () => boolean) {
  node.classList.remove("open");
  afterTransition(node, () => {
    if (stillClosed()) node.hidden = true;
  });
}

// ---------------------------------------------------------------- 連線 sheet（SSH／WireGuard 共用）

type SshField = "name" | "host" | "user" | "proxyCommand";
type WgField = "wgName" | "conf";

const SSH_FIELDS: SshField[] = ["name", "host", "user", "proxyCommand"];
const WG_FIELDS: WgField[] = ["wgName", "conf"];

const SSH_INPUT_ID: Record<SshField, string> = {
  name: "src-name",
  host: "src-host",
  user: "src-user",
  proxyCommand: "src-proxy",
};

const WG_INPUT_ID: Record<WgField, string> = {
  wgName: "wg-name",
  conf: "wg-conf",
};

const backdrop = () => el<HTMLDivElement>("src-backdrop");
const sshInput = (f: SshField) => el<HTMLInputElement>(SSH_INPUT_ID[f]);
const wgInput = (f: WgField) => el<HTMLInputElement>(WG_INPUT_ID[f]);

/** 目前 sheet 裡選的是哪個分頁；編輯模式下鎖定為對象的既有型別 */
type Tab = "ssh" | "wg";

interface Handlers {
  /** 存檔成功，帶回最終的連線名稱（改名後是新名字） */
  onSaved: (name: string) => void;
  onDeleted: (name: string) => void;
}

let handlers: Handlers = { onSaved: () => {}, onDeleted: () => {} };
let open = false;
let busy = false;

/**
 * 送出／刪除進行中的鎖。
 *
 * 一律走這一支而不是直接指派 `busy`：除了旗標本身，`.conf` 路徑輸入框也要跟著
 * 唯讀。那個欄位現在可以手打（不再是純唯讀的選檔結果），而表單的值在按下 Save
 * 的當下就已經被讀走了——這時還能改路徑、還能連帶觸發自動帶名，畫面就會跟送出去
 * 的內容對不起來。瀏覽鈕靠 pickConf 開頭的 `if (busy) return` 擋，兩者同一道 gate。
 */
function setBusy(next: boolean) {
  busy = next;
  wgInput("conf").readOnly = next;
}

/** null 代表這是「新增」 */
let originalName: string | null = null;
let originalKind: ConnKind | null = null;
let activeTab: Tab = "ssh";

let testBusy = false;
/**
 * 開關 sheet 或改動任一欄位都遞增，讓當初送出的那次探測結果作廢——
 * 不必真的取消後端探測，後端探測本身有 15 秒逾時上限，函式結束時就會收乾淨。
 * 沒有這一手的話，測試在途時把欄位改掉，晚到的舊回應會照樣畫到已經改過的
 * 表單上，讓人誤以為新內容也測過了。
 */
let testGeneration = 0;

/**
 * 清掉整張 sheet 的欄位錯誤與整體錯誤（兩張分頁一起）。
 *
 * 曾經拆成「只清目前分頁」與「兩張都清」兩支，省下的是六次 DOM 屬性寫入，
 * 換來的是一個看得見的坑：清哪些欄位取決於呼叫當下的 activeTab，於是
 * 「先切分頁再清」與「先清再切分頁」會有不同結果。這種狀態依賴不值那六次寫入。
 * backdrop() 仍然提出迴圈外，不必每個欄位都重查一次 DOM。
 */
function clearErrors() {
  const root = backdrop();
  for (const f of SSH_FIELDS) setFieldError(root, f, "");
  for (const f of WG_FIELDS) setFieldError(root, f, "");
  setGeneralError(el<HTMLDivElement>("src-error"), "");
}

function clearTestResult() {
  setTestResult(el<HTMLDivElement>("src-test-result"), "", true);
}

function setTestBusy(next: boolean) {
  const btn = el<HTMLButtonElement>("src-test");
  testBusy = next;
  btn.disabled = next;
  btn.classList.toggle("loading", next);
}

/**
 * 「這一刻起，先前那次測試的結果都不算數了」——清畫面上的結果、讓在途的回應
 * 作廢（gen 對不上就不會被畫出來）、把按鈕解鎖。
 *
 * 改欄位、選檔、切分頁、開關 sheet 這四種情境要的都是同一件事，之前各自手寫
 * 三行，漏掉其中一行（例如切分頁只清了結果沒動 gen）就是一條競態。
 */
function invalidateTest() {
  clearTestResult();
  testGeneration++;
  if (testBusy) setTestBusy(false);
}

/**
 * 兩張分頁各自認得的前綴。刻意分開列：ssh 分頁上沒有 confPath 這個欄位，
 * wg 分頁上也沒有 host／user，把不屬於自己的前綴列進來只會製造一個指不到
 * 任何 DOM 的欄位鍵。
 */
const SSH_ERROR_KEYS: Record<string, SshField> = {
  name: "name",
  host: "host",
  user: "user",
  proxycommand: "proxyCommand",
};

const WG_ERROR_KEYS: Record<string, WgField> = {
  name: "wgName",
  confpath: "conf",
};

/**
 * 後端回傳的錯誤字串約定用 `field: message` 開頭，認不出前綴就當成整體錯誤。
 *
 * `tab` 一定要由呼叫端把「送出那一刻」的分頁傳進來，不能在這裡讀 activeTab：
 * upsert 在途時使用者切了分頁的話，回應到達時的 activeTab 已經是另一個分頁，
 * 錯誤會被掛到完全無關的欄位上。saveSsh／saveWg 各自送的是自己那一組欄位，
 * 錯誤歸屬在呼叫的當下就已經確定了。
 */
function assignError(msg: string, tab: Tab) {
  const keys = tab === "wg" ? WG_ERROR_KEYS : SSH_ERROR_KEYS;
  applyFieldError(backdrop(), msg, keys, el<HTMLDivElement>("src-error"));
}

/**
 * 送出前先做一輪本地檢查，訊息與後端用同一套欄位前綴。
 * name 的三條規則（不可空白／不可含空白／不可含中括號）跟 dev-mock 與 Rust 端
 * 的 valid_source_name 共用 util.ts 的 validateConnName，不各寫一份。
 */
function localValidateSsh(): Partial<Record<SshField, string>> {
  const errors: Partial<Record<SshField, string>> = {};
  const nameErr = validateConnName(sshInput("name").value.trim());
  const host = sshInput("host").value.trim();
  if (nameErr) errors.name = nameErr;
  if (!host) errors.host = "host is required";
  else if (/\s/.test(host)) errors.host = "must not contain spaces";
  if (!sshInput("user").value.trim()) errors.user = "user is required";
  return errors;
}

function localValidateWg(): Partial<Record<WgField, string>> {
  const errors: Partial<Record<WgField, string>> = {};
  const nameErr = validateConnName(wgInput("wgName").value.trim());
  if (nameErr) errors.wgName = nameErr;
  if (!wgInput("conf").value.trim()) errors.conf = ".conf path is required";
  return errors;
}

function showFoot(mode: "edit" | "confirm") {
  el<HTMLElement>("src-foot").hidden = mode !== "edit";
  el<HTMLElement>("src-confirm").hidden = mode !== "confirm";
}

/**
 * 切分頁。兩張分頁共用同一條測試列，所以切過去之前一定要把上一張的測試結果
 * 收乾淨、並讓在途的那次探測作廢（testGeneration++）——否則 SSH 那邊的
 * 「Connected」會殘留在 WG 分頁底下，或是晚到的舊回應直接畫到新分頁上，
 * 兩者都會讓使用者以為現在這張表單已經測過了。按鈕狀態比照欄位 input handler
 * 一併重置，不然作廢掉的那次探測沒有人會再去解鎖它。
 *
 * 整體錯誤列（src-error）同理：它也是兩張分頁共用的一條，上一張留下的
 * 「host is required」掛在 WG 分頁底下毫無道理。逐欄的錯誤不必動——那些
 * 節點各自屬於自己的分頁，跟著分頁一起被 hidden。
 */
function setTab(tab: Tab) {
  activeTab = tab;
  el<HTMLButtonElement>("src-tab-ssh").classList.toggle("active", tab === "ssh");
  el<HTMLButtonElement>("src-tab-wg").classList.toggle("active", tab === "wg");
  el<HTMLElement>("src-fields-ssh").hidden = tab !== "ssh";
  el<HTMLElement>("src-fields-wg").hidden = tab !== "wg";
  setGeneralError(el<HTMLDivElement>("src-error"), "");
  invalidateTest();
}

/** 傳給 openSourceSheet 的編輯對象：null 代表新增。型別本體在 types.ts */
export type { ConnTarget };

/**
 * 刪除確認那一行的文案。ssh 底下掛的一律是隧道，wg 底下混著 socks 與 forward
 * 兩種列所以只能統稱 row；單複數再各自變化。三層巢狀三元運算子讀起來要數
 * 括號才知道哪個分支配哪個，拆成一個名詞變數就一目了然。
 */
function deleteConfirmText(target: ConnTarget | null): string {
  if (!target) return "Delete this connection?";
  const count = target.data.exits.length;
  const noun = target.kind === "ssh" ? "tunnel" : "row";
  return `Delete ${target.data.name} and its ${count} ${noun}${count === 1 ? "" : "s"}?`;
}

export function openSourceSheet(target: ConnTarget | null) {
  originalName = target ? target.data.name : null;
  originalKind = target ? target.kind : null;
  setBusy(false);

  const tabs = el<HTMLElement>("src-type-tabs");
  const badge = el<HTMLSpanElement>("src-type-badge");
  if (target) {
    // U1：連線型別建立後不可變，編輯時不給切分頁，改用唯讀徽章
    tabs.hidden = true;
    badge.hidden = false;
    badge.className = `type-badge ${target.kind}`;
    badge.textContent = target.kind === "ssh" ? "SSH" : "WG";
  } else {
    tabs.hidden = false;
    badge.hidden = true;
  }

  if (target && target.kind === "ssh") {
    setTab("ssh");
    sshInput("name").value = target.data.name;
    sshInput("host").value = target.data.host;
    sshInput("user").value = target.data.user;
    sshInput("proxyCommand").value = target.data.proxyCommand;
  } else if (target && target.kind === "wg") {
    setTab("wg");
    wgInput("wgName").value = target.data.name;
    wgInput("conf").value = target.data.confPath;
  } else {
    setTab("ssh");
    for (const f of SSH_FIELDS) sshInput(f).value = "";
    for (const f of WG_FIELDS) wgInput(f).value = "";
  }

  el<HTMLSpanElement>("src-title").textContent = target ? "Edit connection" : "Add connection";
  el<HTMLButtonElement>("src-save").textContent = target ? "Save" : "Add";
  el<HTMLButtonElement>("src-delete").hidden = !target;
  el<HTMLSpanElement>("src-confirm-text").textContent = deleteConfirmText(target);

  // 上面的 setTab 已經 invalidateTest 過了，這裡只補欄位錯誤的歸零
  clearErrors();
  showFoot("edit");
  open = true;
  showSheet(backdrop(), activeTab === "ssh" ? sshInput("name") : wgInput("wgName"));
}

function closeSourceSheet() {
  if (!open) return;
  open = false;
  // 讓仍在進行中的測試結果作廢，reopen 之後 gen 對不上就不會被顯示出來
  invalidateTest();
  hideSheet(backdrop(), () => !open);
}

async function saveSsh() {
  const errors = localValidateSsh();
  const keys = Object.keys(errors) as SshField[];
  if (keys.length > 0) {
    for (const k of keys) setFieldError(backdrop(), k, errors[k] as string);
    return;
  }

  const name = sshInput("name").value.trim();
  setBusy(true);
  try {
    const err = await upsertSource({
      originalName: originalKind === "ssh" ? originalName : null,
      name,
      host: sshInput("host").value.trim(),
      user: sshInput("user").value.trim(),
      proxyCommand: sshInput("proxyCommand").value.trim(),
    });
    setBusy(false);
    if (err) {
      // 送出的是 ssh 那組欄位，錯誤就一定歸 ssh 分頁——不看回應到達時的 activeTab
      assignError(err, "ssh");
      return;
    }
    closeSourceSheet();
    handlers.onSaved(name);
  } catch (e) {
    setBusy(false);
    setGeneralError(el<HTMLDivElement>("src-error"), String(e));
  }
}

async function saveWg() {
  // 在路徑欄裡貼上路徑後直接按 Enter 的話 blur 不會發生，這裡補帶一次名字，
  // 免得使用者拿到一句「name is required」卻不知道本來會自動帶
  suggestNameFromConf();

  const errors = localValidateWg();
  const keys = Object.keys(errors) as WgField[];
  if (keys.length > 0) {
    for (const k of keys) setFieldError(backdrop(), k, errors[k] as string);
    return;
  }

  const name = wgInput("wgName").value.trim();
  setBusy(true);
  try {
    const err = await upsertWgProxy({
      originalName: originalKind === "wg" ? originalName : null,
      name,
      confPath: wgInput("conf").value.trim(),
    });
    setBusy(false);
    if (err) {
      assignError(err, "wg");
      return;
    }
    closeSourceSheet();
    handlers.onSaved(name);
  } catch (e) {
    setBusy(false);
    setGeneralError(el<HTMLDivElement>("src-error"), String(e));
  }
}

async function save() {
  if (busy) return;
  clearErrors();
  if (activeTab === "ssh") await saveSsh();
  else await saveWg();
}

/**
 * 存檔前的連線測試：拿表單「當下」填的值探測，不必先存檔。
 * SSH 分頁測 host／user（跟 localValidateSsh 用同一套訊息），WG 分頁測
 * .conf 路徑（真握手，15 秒上限，回傳形狀與 ssh 那邊一致，見 ipc.ts 的
 * testWgConf）。空白就地顯示錯誤、不送出。
 */
async function testNow() {
  if (testBusy) return;
  clearTestResult();

  if (activeTab === "ssh") {
    const errors = localValidateSsh();
    const relevant: Partial<Record<SshField, string>> = {};
    if (errors.host) relevant.host = errors.host;
    if (errors.user) relevant.user = errors.user;
    const keys = Object.keys(relevant) as SshField[];
    if (keys.length > 0) {
      for (const k of keys) setFieldError(backdrop(), k, relevant[k] as string);
      return;
    }
  } else {
    const conf = wgInput("conf").value.trim();
    if (!conf) {
      setFieldError(backdrop(), "conf", ".conf path is required");
      return;
    }
  }

  // gen 對不上就代表 sheet 中途被關掉／重開過，或欄位在探測進行中被改掉了
  // （testGeneration 已經前進），這時連按鈕狀態都不去動——它早在那一刻
  // 被重置過（reopen 或欄位 input handler），不能讓晚到的這次回應蓋掉
  // 正在跑的下一輪測試。
  const gen = testGeneration;
  const tab = activeTab;
  setTestBusy(true);
  try {
    const result =
      tab === "ssh"
        ? await testConnection({
            host: sshInput("host").value.trim(),
            user: sshInput("user").value.trim(),
            proxyCommand: sshInput("proxyCommand").value.trim(),
          })
        : await testWgConf(wgInput("conf").value.trim());
    if (gen !== testGeneration) return;
    setTestBusy(false);
    setTestResult(el<HTMLDivElement>("src-test-result"), result.message, result.ok);
  } catch (e) {
    if (gen !== testGeneration) return;
    setTestBusy(false);
    setTestResult(el<HTMLDivElement>("src-test-result"), String(e), false);
  }
}

/**
 * Name 還空著的話，拿 .conf 的檔名（去掉副檔名）順手帶一個進去。
 *
 * 兩條入口共用：按瀏覽鈕選檔，以及直接在路徑欄手打／貼上。只在空的時候帶，
 * 使用者自己填過就不動他的——這跟 remote 的正規化一樣，是預填而不是強制。
 *
 * 預填完**立刻驗一次**：檔名不受連線名的規則管，「My VPN.conf」這種帶空格的
 * 名字在 Windows 上再正常不過，直接塞進去等於埋一顆到按下 Save 才爆的雷
 * ——而且那時錯誤指著的是一個使用者根本沒動過的欄位。當場標紅，他馬上就知道
 * 要改。路徑是空的就什麼都不做（清空路徑不該憑空生出一個名字）。
 */
function suggestNameFromConf() {
  const nameField = wgInput("wgName");
  if (nameField.value.trim()) return;
  const path = wgInput("conf").value.trim();
  if (!path) return;
  const suggested = basename(path).replace(/\.conf$/i, "");
  if (!suggested) return;
  nameField.value = suggested;
  setFieldError(backdrop(), "wgName", validateConnName(suggested) ?? "");
}

/** .conf 路徑輸入框內嵌的 folder icon 鈕：叫原生檔案選擇器，取消時 pickWgConf 回 null */
async function pickConf() {
  // Save 在途時表單的值已經被讀走了，這時換掉 .conf 路徑（還可能連帶改 Name）
  // 只會讓畫面與送出去的內容對不起來——跟 Save／Delete 兩顆鈕受同一道鎖管
  if (busy) return;
  try {
    const path = await pickWgConf();
    if (path === null) return;
    wgInput("conf").value = path;
    setFieldError(backdrop(), "conf", "");
    invalidateTest();
    suggestNameFromConf();
  } catch (e) {
    setFieldError(backdrop(), "conf", String(e));
  }
}

async function commitDelete() {
  const target = originalName;
  const kind = originalKind;
  if (!target || !kind || busy) return;
  setBusy(true);
  try {
    if (kind === "ssh") await deleteSource(target);
    else await deleteWgProxy(target);
    setBusy(false);
    closeSourceSheet();
    handlers.onDeleted(target);
  } catch (e) {
    setBusy(false);
    showFoot("edit");
    setGeneralError(el<HTMLDivElement>("src-error"), String(e));
  }
}

export function initSourceSheet(h: Handlers) {
  handlers = h;

  backdrop().addEventListener("mousedown", (e) => {
    if (e.target === backdrop()) closeSourceSheet();
  });
  el<HTMLButtonElement>("src-close").addEventListener("click", closeSourceSheet);
  el<HTMLButtonElement>("src-cancel").addEventListener("click", closeSourceSheet);
  el<HTMLButtonElement>("src-save").addEventListener("click", () => void save());
  el<HTMLButtonElement>("src-test").addEventListener("click", () => void testNow());

  // Save 在途時不准切分頁：送出的是哪一組欄位在按下的當下就定了，中途切過去
  // 只會讓使用者對著另一張表單等一個不屬於它的結果（錯誤路由已經按送出當下的
  // 分頁走，但畫面本身不該這樣飄）
  el<HTMLButtonElement>("src-tab-ssh").addEventListener("click", () => {
    if (!busy) setTab("ssh");
  });
  el<HTMLButtonElement>("src-tab-wg").addEventListener("click", () => {
    if (!busy) setTab("wg");
  });

  el<HTMLButtonElement>("wg-pick").addEventListener("click", () => void pickConf());

  el<HTMLButtonElement>("src-delete").addEventListener("click", () => showFoot("confirm"));
  el<HTMLButtonElement>("src-confirm-no").addEventListener("click", () => showFoot("edit"));
  el<HTMLButtonElement>("src-confirm-yes").addEventListener("click", () => void commitDelete());

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && open) closeSourceSheet();
  });

  // 兩張分頁的欄位佈線完全一樣（改了就清該欄的錯、作廢在途的測試、Enter 送出），
  // 差別只在欄位鍵屬於哪一組，合成同一個迴圈跑
  const allFields: { key: string; node: HTMLInputElement }[] = [
    ...SSH_FIELDS.map((f) => ({ key: f as string, node: sshInput(f) })),
    ...WG_FIELDS.map((f) => ({ key: f as string, node: wgInput(f) })),
  ];
  for (const { key, node } of allFields) {
    node.addEventListener("input", () => {
      setFieldError(backdrop(), key, "");
      invalidateTest();
    });
    node.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void save();
    });
  }

  /**
   * 路徑欄可以手打／貼上，所以自動帶名要綁在「輸入完」而不是每一次按鍵：
   * 綁 input 的話第一個字母敲下去就會把 Name 填成那個字母，之後 Name 不再是空的、
   * 就再也不會更新，使用者最後拿到的是一個殘缺的名字。blur 才是「這欄我填完了」。
   *
   * 走 Enter 直接送出的路徑不會經過 blur，saveWg 開頭因此也補了一次（同一支函式，
   * 只在 Name 為空時作用，重複呼叫是安全的）。
   */
  wgInput("conf").addEventListener("blur", suggestNameFromConf);
}

// ---------------------------------------------------------------- 轉發（forward 列）sheet

type ForwardField = "name" | "local" | "remote";

const FWD_FIELDS: ForwardField[] = ["name", "local", "remote"];

const FWD_INPUT_ID: Record<ForwardField, string> = {
  name: "fwd-name",
  local: "fwd-local",
  remote: "fwd-remote",
};

const fwdBackdrop = () => el<HTMLDivElement>("fwd-backdrop");
const fwdInput = (f: ForwardField) => el<HTMLInputElement>(FWD_INPUT_ID[f]);
const fwdProxyToggle = () => el<HTMLButtonElement>("fwd-is-proxy");

interface TunnelHandlers {
  /** 刪除鍵：sheet 先關掉，undo toast 的倒數與復原交給 main.ts */
  onDelete: (local: number) => void;
}

let fwdHandlers: TunnelHandlers = { onDelete: () => {} };
let fwdOpen = false;
let fwdBusy = false;
/** 這條轉發掛在哪條連線底下，以及那條連線的型別（決定 upsertForward 的 connectionKind） */
let fwdConnection = "";
let fwdConnectionKind: ConnKind = "ssh";
/** null 代表這是「新增」 */
let fwdOriginalLocal: number | null = null;

/**
 * Save 送出期間鎖住 Save／Delete 兩顆鈕，比照連線 sheet 的 setTestBusy：
 * 沒有這道鎖，Save 在途時按 Delete 會把 fwdOriginalLocal 指到的那條（可能剛
 * 存檔改過名字／埠號的）隧道刪掉，而 Save 那個 pending 的 invoke 還在跑，
 * 兩個請求疊在一起，畫面最後留下的東西完全看後端回應順序碰運氣。
 */
function setFwdBusy(next: boolean) {
  fwdBusy = next;
  el<HTMLButtonElement>("fwd-save").disabled = next;
  el<HTMLButtonElement>("fwd-delete").disabled = next;
}

function fwdClearErrors() {
  for (const f of FWD_FIELDS) setFieldError(fwdBackdrop(), f, "");
  setGeneralError(el<HTMLDivElement>("fwd-error"), "");
}

const FWD_ERROR_KEYS: Record<string, ForwardField> = {
  name: "name",
  local: "local",
  remote: "remote",
};

function fwdAssignError(msg: string) {
  applyFieldError(fwdBackdrop(), msg, FWD_ERROR_KEYS, el<HTMLDivElement>("fwd-error"));
}

const isPort = (v: string) => /^\d+$/.test(v) && Number(v) >= 1 && Number(v) <= 65535;

/**
 * 送出前先做一輪本地檢查，訊息與後端用同一套欄位前綴。
 * remote 允許只填埠號（那就是伺服器本機的那個埠），正規化成 host:port 由後端做。
 */
function fwdLocalValidate(): Partial<Record<ForwardField, string>> {
  const errors: Partial<Record<ForwardField, string>> = {};
  if (!fwdInput("name").value.trim()) errors.name = "name is required";
  if (!isPort(fwdInput("local").value.trim())) errors.local = "must be 1-65535";

  const remote = fwdInput("remote").value.trim();
  if (!remote) errors.remote = "remote is required";
  else if (/^\d+$/.test(remote)) {
    if (!isPort(remote)) errors.remote = "must be 1-65535";
  } else {
    // host:port 分支也要把埠號抽出來過 isPort，不能只驗格式：
    // 999999 這種位數符合 \d+ 但早已超過埠號上限，跟 Rust 端的邊界檢查對稱
    const m = /^([^\s:]+):(\d+)$/.exec(remote);
    if (!m) errors.remote = "expected a port or host:port";
    else if (!isPort(m[2])) errors.remote = "must be 1-65535";
  }
  return errors;
}

export function openTunnelSheet(connection: string, connectionKind: ConnKind, exit: ExitInfo | null) {
  fwdConnection = connection;
  fwdConnectionKind = connectionKind;
  fwdOriginalLocal = exit ? exit.local : null;
  setFwdBusy(false);

  fwdInput("name").value = exit?.name ?? "";
  fwdInput("local").value = exit ? String(exit.local) : "";
  fwdInput("remote").value = exit?.remote ?? "";
  setToggle(fwdProxyToggle(), Boolean(exit?.probeProxy));

  el<HTMLSpanElement>("fwd-title").textContent = exit ? "Edit forward" : "Add forward";
  el<HTMLButtonElement>("fwd-save").textContent = exit ? "Save" : "Add";
  el<HTMLButtonElement>("fwd-delete").hidden = !exit;

  fwdClearErrors();
  fwdOpen = true;
  showSheet(fwdBackdrop(), fwdInput("name"));
}

export function closeTunnelSheet() {
  if (!fwdOpen) return;
  fwdOpen = false;
  hideSheet(fwdBackdrop(), () => !fwdOpen);
}

async function fwdSave() {
  if (fwdBusy) return;
  fwdClearErrors();

  const errors = fwdLocalValidate();
  const keys = Object.keys(errors) as ForwardField[];
  if (keys.length > 0) {
    for (const k of keys) setFieldError(fwdBackdrop(), k, errors[k] as string);
    return;
  }

  setFwdBusy(true);
  try {
    const err = await upsertForward({
      connection: fwdConnection,
      connectionKind: fwdConnectionKind,
      originalLocal: fwdOriginalLocal,
      name: fwdInput("name").value.trim(),
      local: Number(fwdInput("local").value.trim()),
      remote: fwdInput("remote").value.trim(),
      probeProxy: isToggleOn(fwdProxyToggle()),
    });
    setFwdBusy(false);
    if (err) {
      fwdAssignError(err);
      return;
    }
    closeTunnelSheet();
  } catch (e) {
    setFwdBusy(false);
    setGeneralError(el<HTMLDivElement>("fwd-error"), String(e));
  }
}

export function initTunnelSheet(h: TunnelHandlers) {
  fwdHandlers = h;

  fwdBackdrop().addEventListener("mousedown", (e) => {
    if (e.target === fwdBackdrop()) closeTunnelSheet();
  });
  el<HTMLButtonElement>("fwd-close").addEventListener("click", closeTunnelSheet);
  el<HTMLButtonElement>("fwd-cancel").addEventListener("click", closeTunnelSheet);
  el<HTMLButtonElement>("fwd-save").addEventListener("click", () => void fwdSave());
  fwdProxyToggle().addEventListener("click", () => {
    // Save 送出期間整張表單的值都已經被讀走了，這時再改開關只會讓畫面與
    // 送出去的內容對不起來——跟 Save／Delete 兩顆鈕受同一道鎖管
    if (fwdBusy) return;
    const node = fwdProxyToggle();
    setToggle(node, !isToggleOn(node));
  });

  el<HTMLButtonElement>("fwd-delete").addEventListener("click", () => {
    // Save 送出、還沒等到回應之前不能按刪除：見 setFwdBusy 的說明
    if (fwdBusy) return;
    const local = fwdOriginalLocal;
    if (local === null) return;
    closeTunnelSheet();
    fwdHandlers.onDelete(local);
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && fwdOpen) closeTunnelSheet();
  });

  for (const f of FWD_FIELDS) {
    const node = fwdInput(f);
    node.addEventListener("input", () => setFieldError(fwdBackdrop(), f, ""));
    node.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void fwdSave();
    });
  }
}

// ---------------------------------------------------------------- SOCKS5 代理列 sheet（WG 專屬）

type SocksField = "name" | "local";

const SOCKS_FIELDS: SocksField[] = ["name", "local"];

const SOCKS_INPUT_ID: Record<SocksField, string> = {
  name: "socks-name",
  local: "socks-local",
};

const socksBackdrop = () => el<HTMLDivElement>("socks-backdrop");
const socksInput = (f: SocksField) => el<HTMLInputElement>(SOCKS_INPUT_ID[f]);

interface SocksHandlers {
  onDelete: (local: number) => void;
}

let socksHandlers: SocksHandlers = { onDelete: () => {} };
let socksOpen = false;
let socksBusy = false;
let socksConnection = "";
let socksOriginalLocal: number | null = null;

function setSocksBusy(next: boolean) {
  socksBusy = next;
  el<HTMLButtonElement>("socks-save").disabled = next;
  el<HTMLButtonElement>("socks-delete").disabled = next;
}

function socksClearErrors() {
  for (const f of SOCKS_FIELDS) setFieldError(socksBackdrop(), f, "");
  setGeneralError(el<HTMLDivElement>("socks-error"), "");
}

/**
 * 這張 sheet 只有 name／local 兩欄，後端卻也會回 `connection:` 開頭的錯誤
 * （socks 列只能掛在 wg 連線底下）。它不在 keyMap 裡，所以會落到整體錯誤列，
 * 而不是被硬塞進某一個欄位——那正是 applyFieldError 兜底規則要處理的情況。
 */
const SOCKS_ERROR_KEYS: Record<string, SocksField> = { name: "name", local: "local" };

function socksAssignError(msg: string) {
  applyFieldError(socksBackdrop(), msg, SOCKS_ERROR_KEYS, el<HTMLDivElement>("socks-error"));
}

function socksLocalValidate(): Partial<Record<SocksField, string>> {
  const errors: Partial<Record<SocksField, string>> = {};
  if (!socksInput("name").value.trim()) errors.name = "name is required";
  if (!isPort(socksInput("local").value.trim())) errors.local = "must be 1-65535";
  return errors;
}

export function openSocksSheet(connection: string, exit: ExitInfo | null) {
  socksConnection = connection;
  socksOriginalLocal = exit ? exit.local : null;
  setSocksBusy(false);

  socksInput("name").value = exit?.name ?? "";
  socksInput("local").value = exit ? String(exit.local) : "";

  el<HTMLSpanElement>("socks-title").textContent = exit ? "Edit SOCKS5 proxy" : "Add SOCKS5 proxy";
  el<HTMLButtonElement>("socks-save").textContent = exit ? "Save" : "Add";
  el<HTMLButtonElement>("socks-delete").hidden = !exit;

  socksClearErrors();
  socksOpen = true;
  showSheet(socksBackdrop(), socksInput("name"));
}

export function closeSocksSheet() {
  if (!socksOpen) return;
  socksOpen = false;
  hideSheet(socksBackdrop(), () => !socksOpen);
}

async function socksSave() {
  if (socksBusy) return;
  socksClearErrors();

  const errors = socksLocalValidate();
  const keys = Object.keys(errors) as SocksField[];
  if (keys.length > 0) {
    for (const k of keys) setFieldError(socksBackdrop(), k, errors[k] as string);
    return;
  }

  setSocksBusy(true);
  try {
    const err = await upsertWgSocks({
      connection: socksConnection,
      originalLocal: socksOriginalLocal,
      name: socksInput("name").value.trim(),
      local: Number(socksInput("local").value.trim()),
    });
    setSocksBusy(false);
    if (err) {
      socksAssignError(err);
      return;
    }
    closeSocksSheet();
  } catch (e) {
    setSocksBusy(false);
    setGeneralError(el<HTMLDivElement>("socks-error"), String(e));
  }
}

export function initSocksSheet(h: SocksHandlers) {
  socksHandlers = h;

  socksBackdrop().addEventListener("mousedown", (e) => {
    if (e.target === socksBackdrop()) closeSocksSheet();
  });
  el<HTMLButtonElement>("socks-close").addEventListener("click", closeSocksSheet);
  el<HTMLButtonElement>("socks-cancel").addEventListener("click", closeSocksSheet);
  el<HTMLButtonElement>("socks-save").addEventListener("click", () => void socksSave());

  el<HTMLButtonElement>("socks-delete").addEventListener("click", () => {
    if (socksBusy) return;
    const local = socksOriginalLocal;
    if (local === null) return;
    closeSocksSheet();
    socksHandlers.onDelete(local);
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && socksOpen) closeSocksSheet();
  });

  for (const f of SOCKS_FIELDS) {
    const node = socksInput(f);
    node.addEventListener("input", () => setFieldError(socksBackdrop(), f, ""));
    node.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void socksSave();
    });
  }
}

// ---------------------------------------------------------------- 設定頁

const tgClose = () => el<HTMLButtonElement>("tg-close");
const tgAutostart = () => el<HTMLButtonElement>("tg-autostart");
const tgUpdates = () => el<HTMLButtonElement>("tg-updates");

function settingsError(msg: string) {
  setGeneralError(el<HTMLDivElement>("settings-error"), msg);
}

// ---------------------------------------------------------- About 版本列的更新鈕

/**
 * 更新鈕的六個狀態。版本列的標題與右側 split button 都由它決定：
 *
 *   idle      沒有新版（或還沒查過）——「Check for updates」，可按
 *   checking  查詢進行中——spinner，不可按
 *   uptodate  剛查完、已是最新——兩秒後自己退回 idle
 *   failed    剛查完、查不到——同樣兩秒後退回 idle
 *   available 有新版——綠色主要鈕，文字是要更新到哪一版
 *   busy      安裝版按下更新後、交棒給安裝程式之前的那段
 */
type UpdateState = "idle" | "checking" | "uptodate" | "failed" | "available" | "busy";

/** Up to date／Check failed 這兩個瞬態停留多久才退回常態 */
const TRANSIENT_MS = 2000;

/**
 * 目前已知的新版。按鈕的行為分兩條車道，按下去的當下要知道自己是哪一條，
 * 所以留一份在這裡，而不是每次都回頭去問快照。
 */
let updateInfo: Snapshot["update"] = null;
let updateState: UpdateState = "idle";
let transientTimer: number | null = null;

const updateSplit = () => el<HTMLDivElement>("update-split");
const updateMain = () => el<HTMLButtonElement>("btn-update");
const updateChevron = () => el<HTMLButtonElement>("btn-update-more");
const updateMenu = () => el<HTMLDivElement>("update-menu");

/**
 * 主鈕在按下去會發生什麼事的那一版文字。
 *
 * 兩條車道的動作完全不同，所以連動詞都不一樣：安裝版是就地更新（下載安裝檔並
 * 交棒給它，程式自己退出、裝完重啟），可攜／單檔版沒有安裝程式可以交棒、也不該
 * 自己改寫自己，只能把使用者送到那一版的 release 頁自己把檔案換掉。
 */
function updateActionLabel(info: NonNullable<Snapshot["update"]>): string {
  return info.installed ? `Update to v${info.version}` : `Get v${info.version}`;
}

/** 每個狀態下主鈕長什麼樣：圖示、文字、能不能按、圖示要不要轉 */
function mainLook(): { icon: IconName; label: string; disabled: boolean; spin: boolean } {
  switch (updateState) {
    case "checking":
      return { icon: "loader-circle", label: "Checking…", disabled: true, spin: true };
    case "uptodate":
      return { icon: "check", label: "Up to date", disabled: true, spin: false };
    case "failed":
      return { icon: "triangle-alert", label: "Check failed", disabled: true, spin: false };
    case "available":
      return {
        icon: "download",
        // available 一定伴隨 updateInfo，這個退路只是為了不讓型別上的 null 變成畫面上的 undefined
        label: updateInfo ? updateActionLabel(updateInfo) : "Update available",
        disabled: false,
        spin: false,
      };
    case "busy":
      return { icon: "loader-circle", label: "Updating…", disabled: true, spin: true };
    default:
      return { icon: "refresh-cw", label: "Check for updates", disabled: false, spin: false };
  }
}

/**
 * 把狀態畫到畫面上。
 *
 * 標題跟的是 updateInfo 而不是按鈕狀態：那一列的標題回答的是「有沒有新版」，
 * 按鈕回答的是「現在可以做什麼」。所以重新檢查（checking）時標題不該從
 * Update available 跳回 Version——已經查到的那一版並沒有因為你再查一次就消失。
 */
function paintUpdateRow() {
  el<HTMLDivElement>("version-title").textContent = updateInfo ? "Update available" : "Version";
  const look = mainLook();
  updateSplit().dataset.state = updateState;
  updateMain().disabled = look.disabled;
  el<HTMLSpanElement>("update-label").textContent = look.label;
  const iconBox = el<HTMLSpanElement>("update-icon");
  setIcon(iconBox, look.icon, 14);
  iconBox.classList.toggle("spin", look.spin);
  // 連外進行中就不要再讓人從下拉裡開第二件事；這兩個狀態也不該被選單蓋住
  const locked = updateState === "checking" || updateState === "busy";
  updateChevron().disabled = locked;
  if (locked) closeUpdateMenu();
}

function setUpdateState(next: UpdateState) {
  if (transientTimer !== null) {
    window.clearTimeout(transientTimer);
    transientTimer = null;
  }
  updateState = next;
  paintUpdateRow();
  // 兩個瞬態都會自己退場，退到哪裡看那時候手上有沒有新版
  if (next === "uptodate" || next === "failed") {
    transientTimer = window.setTimeout(() => {
      transientTimer = null;
      setUpdateState(updateInfo ? "available" : "idle");
    }, TRANSIENT_MS);
  }
}

/**
 * 後端推來的更新資訊（啟動快照、config-changed、背景檢查的 update-available）。
 *
 * checking 與 busy 這兩個狀態由當下那個動作自己收尾，事件不准插隊改按鈕：
 * 手動檢查本身就會讓後端推一次 update-available，那個事件很可能比 invoke 的
 * resolve 還早到，照收的話按鈕會先閃一下 available 再被 resolve 蓋回去。
 * 標題還是要跟著更新，它反映的是事實而不是進行中的動作。
 */
function applyUpdateInfo(info: Snapshot["update"]) {
  updateInfo = info;
  if (updateState === "checking" || updateState === "busy") {
    paintUpdateRow();
    return;
  }
  if (info) {
    setUpdateState("available");
    return;
  }
  // 新版沒了（例如使用者把背景檢查關掉）就退回常態；
  // 剛查完的那兩個瞬態則留給它們自己的計時器收尾，不要中途打斷
  if (updateState === "available") setUpdateState("idle");
  else paintUpdateRow();
}

/** 後端推了新設定就把 toggle 對齊回去 */
export function syncSettingsPage(snap: Snapshot) {
  setToggle(tgClose(), snap.closeToTray);
  setToggle(tgAutostart(), snap.autostart);
  setToggle(tgUpdates(), snap.checkForUpdates);
  applyUpdateInfo(snap.update ?? null);
}

function wireToggle(node: HTMLElement, apply: (on: boolean) => Promise<unknown>) {
  node.addEventListener("click", async () => {
    const next = !isToggleOn(node);
    setToggle(node, next);
    settingsError("");
    try {
      await apply(next);
    } catch (e) {
      setToggle(node, !next);
      settingsError(String(e));
    }
  });
}

/**
 * About 的「Config file」一列：本身是非互動列，只顯示實際生效的完整路徑；
 * 開檔案總管的動作收進右側獨立的圖示按鈕，路徑問不到之前先停用它。
 * 路徑問不到時（後端還沒起來之類）留一個破折號，不讓這一列開天窗。
 * dev-mock 模式的假路徑與 no-op 由 mockIPC 那邊給。
 */
function initConfigPathRow() {
  const label = el<HTMLDivElement>("config-path");
  const openBtn = el<HTMLButtonElement>("btn-open-config-dir");

  void getConfigPath()
    .then((p) => {
      label.textContent = p;
      // 省略號會吃掉路徑尾巴，滑過去至少看得到全文
      label.title = p;
      openBtn.disabled = false;
    })
    .catch(() => {
      label.textContent = "—";
    });

  openBtn.addEventListener("click", () => {
    void openConfigDir().catch((e) => settingsError(String(e)));
  });
}

/**
 * 選單現在是不是開著。
 *
 * 讀 `hidden` 一定要經過這裡轉成真正的布林值再用：DOM 的 `hidden` 型別是
 * `string | boolean`，規格為了 `hidden="until-found"` 把它放寬成
 * boolean／double／DOMString 的聯集了。寫入端不受影響（setter 照收布林），
 * 只有讀出來當布林用的地方需要這一手，而 `!` 的結果一定是布林。
 */
function isUpdateMenuOpen(): boolean {
  return !updateMenu().hidden;
}

function setUpdateMenuOpen(open: boolean) {
  updateMenu().hidden = !open;
  updateChevron().setAttribute("aria-expanded", String(open));
}

function closeUpdateMenu() {
  setUpdateMenuOpen(false);
}

/**
 * 手動檢查一次。
 *
 * **刻意不看 checkForUpdates 開關**：那個開關管的是背景自動連外，使用者親手按下
 * 這顆鈕就是對這一次連外的明示同意，再拿背景開關擋他只會變成按了沒反應。
 * 三種結果都直接反映在同一顆鈕上：有新版就變綠、已最新與失敗各閃一下兩秒的
 * 瞬態再退回去。失敗的詳細原因後端已經寫進活動日誌，不必再彈一次錯誤列。
 */
function runUpdateCheck() {
  if (updateState === "checking" || updateState === "busy") return;
  closeUpdateMenu();
  settingsError("");
  setUpdateState("checking");
  void checkForUpdatesNow()
    .then((info) => {
      updateInfo = info;
      setUpdateState(info ? "available" : "uptodate");
    })
    .catch(() => setUpdateState("failed"));
}

/**
 * 綠色主鈕按下去之後。兩條車道的收尾方式不一樣：
 *
 * 安裝版按下去就沒有回頭路了（安裝程式接手、程式退出），所以先進 busy 把鈕鎖住
 * 避免連按兩次，而且那個 promise 成功時根本不會 resolve——只有失敗要處理，
 * 把鈕放回 available 並把原因寫在設定頁的錯誤列。
 *
 * 可攜版只是開一個瀏覽器分頁到那一版的 release 頁，按幾次都無所謂，鈕不必鎖。
 */
function startUpdate() {
  const info = updateInfo;
  if (!info) return;
  settingsError("");
  if (!info.installed) {
    void openReleasePage(info.version).catch((e) => settingsError(String(e)));
    return;
  }
  setUpdateState("busy");
  void installUpdate().catch((e) => {
    setUpdateState("available");
    settingsError(String(e));
  });
}

/**
 * 版本列右側的 split button。主鈕依當下狀態分岔（有新版就更新、其餘一律是
 * 檢查一次），柄點開的下拉收三個次要動作。
 */
function initUpdateControl() {
  updateMain().addEventListener("click", () => {
    if (updateState === "available") startUpdate();
    else runUpdateCheck();
  });

  updateChevron().addEventListener("click", (e) => {
    e.stopPropagation();
    setUpdateMenuOpen(!isUpdateMenuOpen());
  });

  // 點到 split 以外的任何地方就關；用 mousedown 才不會被按鈕自己的 click 蓋掉
  document.addEventListener("mousedown", (e) => {
    if (!isUpdateMenuOpen()) return;
    const target = e.target;
    if (target instanceof Element && target.closest("#update-split")) return;
    closeUpdateMenu();
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && isUpdateMenuOpen()) {
      e.stopPropagation();
      closeUpdateMenu();
    }
  });

  el<HTMLButtonElement>("mi-check-now").addEventListener("click", runUpdateCheck);

  // 查到新版就開那一版的 release 頁，還沒查到就退回 releases/latest
  el<HTMLButtonElement>("mi-release-notes").addEventListener("click", () => {
    closeUpdateMenu();
    void openReleasePage(updateInfo?.version ?? null).catch((e) => settingsError(String(e)));
  });

  el<HTMLButtonElement>("mi-downloads").addEventListener("click", () => {
    closeUpdateMenu();
    void openReleasesPage().catch((e) => settingsError(String(e)));
  });
}

export function initSettingsPage() {
  wireToggle(tgClose(), setCloseToTray);
  wireToggle(tgAutostart(), setAutostart);
  wireToggle(tgUpdates(), setCheckForUpdates);
  initUpdateControl();
  initConfigPathRow();
  void loadAppVersion().then((v) => {
    el<HTMLDivElement>("app-version").textContent = v;
  });
}
