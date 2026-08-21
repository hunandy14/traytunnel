import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { bootstrap } from "./bootstrap";
import type { Config, ExitPayload, Snapshot, StatusPayload } from "./types";

// Segoe MDL2 Assets：E71A 是 Stop，E768 是 Play
const GLYPH_STOP = "";
const GLYPH_START = "";

const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const statusDot = el<HTMLDivElement>("status-dot");
const statusText = el<HTMLDivElement>("status-text");
const statusSub = el<HTMLDivElement>("status-sub");
const cards = el<HTMLDivElement>("cards");
const sectionLog = el<HTMLDivElement>("section-log");
const logPanel = el<HTMLDivElement>("log-panel");
const logBox = el<HTMLDivElement>("log-box");
const btnToggle = el<HTMLButtonElement>("btn-toggle");

const dots = new Map<number, HTMLElement>();
const results = new Map<number, HTMLElement>();

function color(kind: string): string {
  switch (kind) {
    case "accent":
      return "var(--accent)";
    case "amber":
      return "var(--amber)";
    case "red":
      return "var(--red)";
    default:
      return "var(--muted)";
  }
}

let renderedForwards = "";

/** 只在 forwards 真的變動時重建卡片，避免切換其他設定時清掉自測結果 */
function applyConfig(cfg: Config) {
  statusSub.textContent = `ssh ${cfg.user}@${cfg.host}`;
  const signature = JSON.stringify(cfg.forwards);
  if (signature === renderedForwards) return;
  renderedForwards = signature;
  renderCards(cfg);
}

function renderCards(cfg: Config) {
  cards.textContent = "";
  dots.clear();
  results.clear();

  for (const f of cfg.forwards) {
    const card = document.createElement("div");
    card.className = "card";

    const dot = document.createElement("div");
    dot.className = "dot";
    dot.textContent = "●";
    card.appendChild(dot);

    const name = document.createElement("div");
    name.className = "name";
    name.textContent = f.name;
    card.appendChild(name);

    const port = document.createElement("div");
    port.className = "port";
    port.textContent = `socks5://127.0.0.1:${f.local}`;
    card.appendChild(port);

    const result = document.createElement("div");
    result.className = "result";
    result.textContent = "-";
    card.appendChild(result);

    cards.appendChild(card);
    dots.set(f.local, dot);
    results.set(f.local, result);
  }

  // 版面高度公式沿用原版：卡片區高度決定 ACTIVITY 的位置
  const cardsHeight = Math.max(10, cfg.forwards.length * 68 - 10);
  const logY = 138 + cardsHeight + 14;
  sectionLog.style.top = `${logY}px`;
  logPanel.style.top = `${logY + 22}px`;
  statusSub.textContent = `ssh ${cfg.user}@${cfg.host}`;
}

function applyStatus(s: StatusPayload) {
  statusText.textContent = s.text;
  statusDot.style.color = color(s.kind);
}

function applyExit(e: ExitPayload) {
  const dot = dots.get(e.port);
  const result = results.get(e.port);
  if (!dot || !result) return;
  result.textContent = e.text;
  switch (e.state) {
    case "testing":
      dot.style.color = color("amber");
      result.style.color = color("muted");
      break;
    case "ok":
      dot.style.color = color("accent");
      result.style.color = "var(--text)";
      break;
    case "fail":
      dot.style.color = color("red");
      result.style.color = color("red");
      break;
    default:
      dot.style.color = color("muted");
      result.style.color = color("muted");
  }
}

function appendLog(line: string) {
  const atBottom = logBox.scrollHeight - logBox.scrollTop - logBox.clientHeight < 4;
  const row = document.createElement("div");
  row.textContent = line;
  logBox.appendChild(row);
  while (logBox.childElementCount > 500) {
    logBox.removeChild(logBox.firstChild as ChildNode);
  }
  if (atBottom) logBox.scrollTop = logBox.scrollHeight;
}

function applyRunState(on: boolean) {
  btnToggle.textContent = on ? GLYPH_STOP : GLYPH_START;
  btnToggle.style.color = on ? "var(--red)" : "var(--accent)";
  btnToggle.title = on ? "Stop" : "Start";
}

function applySnapshot(snap: Snapshot) {
  applyConfig(snap.config);
  applyStatus(snap.status);
  applyRunState(snap.wantRun);
  logBox.textContent = "";
  for (const line of snap.logs) appendLog(line);
  for (const e of snap.exits) applyExit(e);
}

/**
 * 視窗是在 setup() 之前就建好的，頁面有可能比 Rust 端 manage 狀態還早問，
 * 所以第一次取狀態失敗時重試幾輪，真的拿不到才把錯誤寫進活動區。
 */
async function loadSnapshot() {
  for (let attempt = 0; attempt < 20; attempt++) {
    try {
      applySnapshot(await invoke<Snapshot>("get_state"));
      return;
    } catch (e) {
      if (attempt === 19) appendLog(`ui error: ${String(e)}`);
      await new Promise((r) => setTimeout(r, 250));
    }
  }
}

async function init() {
  try {
    await listen<string>("log", (e) => appendLog(e.payload));
    await listen<StatusPayload>("status", (e) => applyStatus(e.payload));
    await listen<ExitPayload>("exit", (e) => applyExit(e.payload));
    await listen<Config>("config", (e) => applyConfig(e.payload));
    await listen<boolean>("run-state", (e) => applyRunState(e.payload));
  } catch (e) {
    appendLog(`ui error: ${String(e)}`);
  }
  await loadSnapshot();
}

el<HTMLButtonElement>("btn-min").addEventListener("click", () => invoke("window_minimize"));
el<HTMLButtonElement>("btn-close").addEventListener("click", () => invoke("window_close"));
el<HTMLButtonElement>("btn-settings").addEventListener("click", () => invoke("open_settings"));
el<HTMLButtonElement>("btn-retest").addEventListener("click", () => invoke("retest"));
btnToggle.addEventListener("click", () => invoke("toggle_run"));

bootstrap(init);
