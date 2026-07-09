import { DEFAULT_SETTINGS } from "./panel-state.js";
import { barViewModel } from "./bar-state.js";
import { applyFont } from "./font.js";
import { applyTranslations } from "./i18n.js";
import { applyTheme } from "./theme.js";

let settings = { ...DEFAULT_SETTINGS };
let statuses = [];
const TOOLS = ["claude", "codex"];

function currentWindowTool() {
  const search = window.location?.search ?? globalThis.location?.search ?? "";
  const tool = new URLSearchParams(search).get("tool");
  return TOOLS.includes(tool) ? tool : null;
}

const CURRENT_TOOL = currentWindowTool();
const MENU_WIDTH = 88;
const MENU_HEIGHT = 28;
const MENU_MARGIN = 4;

document.addEventListener("contextmenu", (event) => {
  event.preventDefault();
  showRefreshMenu(event);
});

document.addEventListener("click", (event) => {
  const menu = refreshMenu();
  if (!menu || menu.hidden || menu.contains(event.target)) return;
  hideRefreshMenu();
});

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") hideRefreshMenu();
});

function tauriApi() {
  return window.__TAURI__ ?? {};
}

async function invoke(command, args) {
  const fn = tauriApi().core?.invoke;
  if (!fn) return null;
  return fn(command, args);
}

async function refreshTaskbarStatus() {
  try {
    await invoke("refresh_status");
  } catch {
    // The bar should still suppress the browser menu even if IPC is unavailable.
  }
}

function refreshMenu() {
  return document.querySelector("#bar-menu");
}

function clampMenuPosition(value, size, max) {
  const coordinate = Number.isFinite(value) ? value : MENU_MARGIN;
  const upper = Math.max(MENU_MARGIN, max - size - MENU_MARGIN);
  return Math.max(MENU_MARGIN, Math.min(coordinate, upper));
}

function showRefreshMenu(event) {
  const menu = refreshMenu();
  if (!menu) {
    void refreshTaskbarStatus();
    return;
  }

  const x = clampMenuPosition(event?.clientX, MENU_WIDTH, window.innerWidth ?? MENU_WIDTH);
  const y = clampMenuPosition(event?.clientY, MENU_HEIGHT, window.innerHeight ?? MENU_HEIGHT);
  menu.style?.setProperty("--menu-x", `${x}px`);
  menu.style?.setProperty("--menu-y", `${y}px`);
  menu.hidden = false;
}

function hideRefreshMenu() {
  const menu = refreshMenu();
  if (menu) menu.hidden = true;
}

function bindRefreshMenu() {
  const button = document.querySelector('[data-bar-action="refresh"]');
  button?.addEventListener("click", (event) => {
    event.preventDefault();
    hideRefreshMenu();
    void refreshTaskbarStatus();
  });
}

function setText(scope, selector, value) {
  const element = scope.querySelector(selector);
  if (element) element.textContent = value;
}

function orderedLimits(vm, limitOrder) {
  return limitOrder === "secondary_first" ? [vm.secondary, vm.primary] : [vm.primary, vm.secondary];
}

function setDisplayLimitVars(scope, first, second) {
  scope.style?.setProperty("--primary-color", first.color);
  scope.style?.setProperty("--primary-arc", first.arc);
  scope.style?.setProperty("--primary-percent", `${first.percent ?? 0}%`);
  scope.style?.setProperty("--secondary-color", second.color);
  scope.style?.setProperty("--secondary-arc", second.arc);
  scope.style?.setProperty("--secondary-percent", `${second.percent ?? 0}%`);
}

function setRing(scope, vm, limitOrder) {
  const [first, second] = orderedLimits(vm, limitOrder);
  setDisplayLimitVars(scope, first, second);

  const ring = scope.querySelector(".bar-ring");
  if (!ring) return;

  setDisplayLimitVars(ring, first, second);
}

function toolElement(tool) {
  return (
    document.querySelector(`.bar-tool[data-tool="${tool}"]`) ??
    document.querySelector(`[data-tool="${tool}"]`)
  );
}

function renderTool(vm, limitOrder) {
  const item = toolElement(vm.tool);
  if (!item) return;
  const [first, second] = orderedLimits(vm, limitOrder);

  item.hidden = false;
  item.dataset.state = vm.state;
  item.dataset.severity = vm.severity;
  setRing(item, vm, limitOrder);
  setText(item, ".bar-tool-name", vm.label);
  setText(item, ".bar-worst", vm.worst);
  setText(item, ".quad-primary-number", first.number);
  setText(item, ".quad-secondary-number", second.number);
  setText(item, ".primary-text", first.text);
  setText(item, ".primary-reset", first.reset);
  setText(item, ".secondary-text", second.text);
  setText(item, ".secondary-reset", second.reset);
}

function renderBar() {
  const root = document.querySelector("#bar");
  if (!root) return;

  const vm = barViewModel(statuses, settings);
  root.dataset.mode = vm.mode;
  root.dataset.limitOrder = vm.limitOrder.replace("_", "-");
  root.dataset.indicator = vm.indicatorStyle;
  root.dataset.ring = vm.ringOn ? "on" : "off";
  root.dataset.ringNumbers = vm.ringNumbersOn ? "on" : "off";
  root.dataset.numberOutline = vm.ringNumberOutlineOn ? "on" : "off";
  root.style?.setProperty("--ring-size", `${vm.ringSizePx}px`);
  root.style?.setProperty("--ring-thickness", `${vm.ringThicknessPx}px`);
  root.style?.setProperty("--ring-gap", `${vm.ringGapPx}px`);
  root.style?.setProperty("--ring-center-gap", `${vm.ringCenterGapPx}px`);
  root.style?.setProperty("--ring-number-font-size", `${vm.ringNumberFontSizePx}px`);
  root.style?.setProperty("--ring-number-font-weight", String(vm.ringNumberFontWeight));
  root.style?.setProperty("--bar-text-font-size", `${vm.barTextFontSizePx}px`);
  root.style?.setProperty("--bar-text-font-weight", String(vm.barTextFontWeight));
  delete root.dataset.tool;
  root.dataset.currentTool = CURRENT_TOOL ?? "all";
  for (const tool of TOOLS) {
    const item = toolElement(tool);
    if (item) item.hidden = true;
  }
  for (const tool of vm.tools) {
    if (CURRENT_TOOL && tool.tool !== CURRENT_TOOL) continue;
    renderTool(tool, vm.limitOrder);
  }
}

function setDragging(payload) {
  const root = document.querySelector("#bar");
  if (!root) return;

  const isDragging =
    typeof payload === "boolean"
      ? payload
      : payload?.tool === CURRENT_TOOL && payload?.dragging === true;
  if (isDragging) {
    root.dataset.dragging = "true";
  } else {
    delete root.dataset.dragging;
  }
}

async function loadSettings() {
  try {
    const loaded = await invoke("get_settings");
    if (loaded && typeof loaded === "object") {
      settings = { ...DEFAULT_SETTINGS, ...loaded };
      applyTheme(settings);
      applyFont(settings);
      applyTranslations(settings);
    }
  } catch {
    settings = { ...DEFAULT_SETTINGS };
    applyTheme(settings);
    applyFont(settings);
    applyTranslations(settings);
  }
}

function listenSafely(listen, eventName, handler) {
  try {
    Promise.resolve(listen(eventName, handler)).catch(() => {});
  } catch {
    // Initial and periodic status loads still render the bar.
  }
}

function bindEvents() {
  const listen = tauriApi().event?.listen;
  if (listen) {
    listenSafely(listen, "status-updated", (event) => {
      statuses = Array.isArray(event.payload) ? event.payload : [];
      renderBar();
    });
    listenSafely(listen, "settings-updated", (event) => {
      if (event.payload && typeof event.payload === "object") {
        settings = { ...DEFAULT_SETTINGS, ...event.payload };
        applyTheme(settings);
        applyFont(settings);
        applyTranslations(settings);
        renderBar();
      }
    });
    listenSafely(listen, "taskbar-dragging-updated", (event) => {
      setDragging(event.payload);
    });
  }
}

async function loadStatus() {
  try {
    statuses = (await invoke("get_status")) || [];
  } catch {
    statuses = [];
  }
  renderBar();
}

async function bootstrap() {
  applyTranslations(settings);
  bindRefreshMenu();
  renderBar();
  await loadSettings();
  renderBar();
  bindEvents();
  await loadStatus();
}

bootstrap();
