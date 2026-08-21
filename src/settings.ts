import { invoke } from "@tauri-apps/api/core";
import type { Snapshot } from "./types";

const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const inHost = el<HTMLInputElement>("in-host");
const inUser = el<HTMLInputElement>("in-user");
const inProxy = el<HTMLInputElement>("in-proxy");
const inForwards = el<HTMLTextAreaElement>("in-forwards");
const tgAutostart = el<HTMLDivElement>("tg-autostart");
const tgClose = el<HTMLDivElement>("tg-close");
const msgbox = el<HTMLDivElement>("msgbox");
const msgboxText = el<HTMLDivElement>("msgbox-text");

function showMessage(text: string) {
  msgboxText.textContent = text;
  msgbox.classList.add("show");
}

function setToggle(node: HTMLElement, on: boolean) {
  node.classList.toggle("on", on);
}

function isOn(node: HTMLElement) {
  return node.classList.contains("on");
}

async function init() {
  const snap = await invoke<Snapshot>("get_state");
  inHost.value = snap.config.host;
  inUser.value = snap.config.user;
  inProxy.value = snap.config.proxyCommand;
  inForwards.value = snap.config.forwards
    .map((f) => `${f.name}  ${f.local}  ${f.remote}`)
    .join("\n");
  setToggle(tgAutostart, snap.autostart);
  setToggle(tgClose, snap.config.closeToTray);
  inHost.focus();
}

// 兩個 toggle 都是即時生效
tgAutostart.addEventListener("click", async () => {
  const next = !isOn(tgAutostart);
  setToggle(tgAutostart, next);
  try {
    await invoke("set_autostart", { on: next });
  } catch (e) {
    setToggle(tgAutostart, !next);
    showMessage(String(e));
  }
});

tgClose.addEventListener("click", async () => {
  const next = !isOn(tgClose);
  setToggle(tgClose, next);
  try {
    await invoke("set_close_to_tray", { on: next });
  } catch (e) {
    setToggle(tgClose, !next);
    showMessage(String(e));
  }
});

el<HTMLButtonElement>("btn-save").addEventListener("click", async () => {
  try {
    await invoke("save_config", {
      input: {
        host: inHost.value,
        user: inUser.value,
        proxyCommand: inProxy.value,
        forwards: inForwards.value,
      },
    });
    await invoke("close_settings");
  } catch (e) {
    showMessage(String(e));
  }
});

el<HTMLButtonElement>("btn-cancel").addEventListener("click", () => invoke("close_settings"));
el<HTMLButtonElement>("btn-close").addEventListener("click", () => invoke("close_settings"));
el<HTMLButtonElement>("msgbox-ok").addEventListener("click", () => msgbox.classList.remove("show"));

init();
