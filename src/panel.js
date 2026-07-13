import { DEFAULT_SETTINGS, viewModelForTool } from "./panel-state.js";
import { applyFont } from "./font.js";
import { applyTranslations } from "./i18n.js";
import { applyTheme } from "./theme.js";

const TOOLS = ["claude", "codex"];
const WINDOW_ACTION_COMMANDS = {
  close: "hide_panel_window",
  minimize: "minimize_panel",
  "toggle-maximize": "toggle_panel_maximized",
};
let settings = { ...DEFAULT_SETTINGS };
let lastStatuses = [];
let statusEventGeneration = 0;
let settingsEventGeneration = 0;
let snapshotFallbackTimer = null;
const LISTENER_RETRY_DELAYS_MS = [0, 100, 250];

function setPanelVisible(visible) {
  document.documentElement.dataset.panelVisible = visible ? "true" : "false";
}

document.addEventListener("contextmenu", (event) => event.preventDefault());

function tauriApi() {
  return window.__TAURI__ ?? {};
}

async function invoke(command) {
  const fn = tauriApi().core?.invoke;
  if (!fn) return null;
  return fn(command);
}

function bindWindowControls() {
  document.addEventListener("click", (event) => {
    const action = event.target?.closest?.("[data-window-action]")?.dataset?.windowAction;
    const command = WINDOW_ACTION_COMMANDS[action];
    if (!command) return;

    event.preventDefault();
    const hidesPanel = action === "close" || action === "minimize";
    if (hidesPanel) setPanelVisible(false);
    void invoke(command).catch(() => {
      if (hidesPanel) setPanelVisible(true);
    });
  });
}

function currentPanelWindow() {
  const api = tauriApi();
  const candidates = [
    api.webviewWindow?.getCurrentWebviewWindow?.(),
    api.window?.getCurrentWindow?.(),
  ];

  return candidates.find((candidate) => typeof candidate?.startDragging === "function") ?? null;
}

async function startPanelDrag() {
  const currentWindow = currentPanelWindow();
  if (currentWindow) {
    try {
      await currentWindow.startDragging();
      return;
    } catch {
      // Fall through to the guarded Rust command when the core window IPC is denied.
    }
  }

  try {
    await invoke("start_panel_drag");
  } catch {
    // Drag fallback is best-effort; controls and settings should keep working.
  }
}

function bindPanelDragFallback() {
  document.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    if (event.target?.closest?.("[data-window-action]")) return;
    if (!event.target?.closest?.("[data-tauri-drag-region]")) return;

    event.preventDefault();
    void startPanelDrag();
  });
}

function setText(scope, selector, value) {
  const element = scope.querySelector(selector);
  if (element) element.textContent = value;
}

function toolEnabled(tool) {
  if (tool === "claude") return settings.show_claude !== false;
  if (tool === "codex") return settings.show_codex !== false;
  return true;
}

function setBar(scope, selector, model) {
  const root = scope.querySelector(selector);
  if (!root) return;

  const fill = root.querySelector(".fill");
  if (fill) {
    fill.style.width = model.width;
    fill.style.background = model.color;
  }

  setText(root, ".val", model.value);
  setText(root, ".reset", model.reset);
}

function renderTool(tool, now) {
  const card = document.querySelector(`[data-tool="${tool}"]`);
  if (!card) return;

  if (!toolEnabled(tool)) {
    card.hidden = true;
    return;
  }
  card.hidden = false;

  const vm = viewModelForTool(lastStatuses, tool, settings, now);

  card.dataset.state = vm.exists ? (vm.active ? "live" : "stale") : "empty";
  card.style?.setProperty("--tool-brand", vm.brandColor);
  setBar(card, ".p5h", vm.primary);
  setBar(card, ".pweek", vm.secondary);
  setText(card, ".pc", vm.pcId);
  setText(card, ".meta", vm.meta);
  setText(card, ".empty-hint", vm.emptyHint);
}

export function renderStatuses(statuses, now = new Date()) {
  lastStatuses = Array.isArray(statuses) ? statuses : [];
  for (const tool of TOOLS) renderTool(tool, now);
}

window.addEventListener("settings-updated", (event) => {
  if (event.detail && typeof event.detail === "object") {
    settings = { ...DEFAULT_SETTINGS, ...event.detail };
    applyTheme(settings);
    applyFont(settings);
    applyTranslations(settings);
    renderStatuses(lastStatuses);
  }
});

async function loadSettings() {
  const requestGeneration = settingsEventGeneration;
  try {
    const loaded = await invoke("get_settings");
    if (settingsEventGeneration !== requestGeneration) return;
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

async function loadStatus() {
  const requestGeneration = statusEventGeneration;
  try {
    const statuses = await invoke("get_status");
    if (statusEventGeneration === requestGeneration) renderStatuses(statuses);
  } catch {
    if (statusEventGeneration === requestGeneration) renderStatuses([]);
  }
}

function scheduleSnapshotFallback() {
  if (snapshotFallbackTimer) return;
  snapshotFallbackTimer = setInterval(() => {
    void loadSettings();
    void loadStatus();
  }, 30_000);
  snapshotFallbackTimer?.unref?.();
}

async function listenWithRetry(listen, eventName, handler) {
  for (const delay of LISTENER_RETRY_DELAYS_MS) {
    if (delay) await new Promise((resolve) => setTimeout(resolve, delay));
    try {
      await listen(eventName, handler);
      return true;
    } catch {
      // Retry each event independently during WebView startup.
    }
  }
  scheduleSnapshotFallback();
  return false;
}

function bindStatusUpdates() {
  const listen = tauriApi().event?.listen;
  if (listen) {
    void listenWithRetry(listen, "status-updated", (event) => {
      statusEventGeneration += 1;
      renderStatuses(event.payload);
    });
    void listenWithRetry(listen, "settings-updated", (event) => {
        if (event.payload && typeof event.payload === "object") {
          settingsEventGeneration += 1;
          settings = { ...DEFAULT_SETTINGS, ...event.payload };
          applyTheme(settings);
          applyFont(settings);
          applyTranslations(settings);
          renderStatuses(lastStatuses);
        }
      });
    void listenWithRetry(listen, "panel-visibility-updated", (event) => {
        setPanelVisible(event.payload !== false);
      });
  }
}

async function bootstrap() {
  setPanelVisible(!document.hidden);
  window.addEventListener("focus", () => setPanelVisible(true));
  document.addEventListener("visibilitychange", () => setPanelVisible(!document.hidden));
  applyTranslations(settings);
  bindWindowControls();
  bindPanelDragFallback();
  bindStatusUpdates();
  await Promise.all([loadSettings(), loadStatus()]);
}

bootstrap();
