import {
  activityTooltipPosition,
  buildActivityView,
  formatActivityDate,
  formatActivityMonth,
  formatActivityTokens,
} from "./activity-state.js";
import { DEFAULT_SETTINGS, toolBrandColor, viewModelForTool } from "./panel-state.js";
import { applyFont } from "./font.js";
import { applyTranslations, resolveLanguage, t } from "./i18n.js";
import { applyTheme } from "./theme.js";

const TOOLS = ["claude", "codex"];
const WINDOW_ACTION_COMMANDS = {
  close: "hide_panel_window",
  minimize: "minimize_panel",
  "toggle-maximize": "toggle_panel_maximized",
};
let settings = { ...DEFAULT_SETTINGS };
let lastStatuses = [];
let activitySnapshot = { days: [], partial: false };
let activityFilter = "all";
let activityDataRevision = 0;
let lastActivityRenderSignature = "";
let statusEventGeneration = 0;
let activityEventGeneration = 0;
let settingsEventGeneration = 0;
let snapshotFallbackTimer = null;
let activityRefreshTimer = null;
let panelVisible = true;
let listenerLifecycleGeneration = 0;
let listenersDisposed = false;
const activeUnlisteners = new Set();
const LISTENER_RETRY_DELAYS_MS = [0, 100, 250];
const LISTENER_REGISTRATION_TIMEOUT_MS = 500;

function setPanelVisible(visible) {
  panelVisible = visible;
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
    const filter = event.target?.closest?.("[data-activity-filter]")?.dataset?.activityFilter;
    if (filter) {
      activityFilter = filter;
      hideActivityTooltip();
      renderActivity();
      return;
    }

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

function activityLanguage() {
  return resolveLanguage(settings);
}

function tooltipCopy(cell) {
  const language = activityLanguage();
  const tokens = t("activity.tokens", settings);
  const total = formatActivityTokens(cell.tokens, language);
  const title = `${formatActivityDate(cell.date, language)} · ${total} ${tokens}`;
  const details = [];
  if (settings.show_claude !== false) {
    details.push(`Claude ${formatActivityTokens(cell.claudeTokens, language)}`);
  }
  if (settings.show_codex !== false) {
    details.push(`Codex ${formatActivityTokens(cell.codexTokens, language)}`);
  }
  return { title, detail: details.join(" · ") };
}

function hideActivityTooltip() {
  const tooltip = document.querySelector("[data-activity-tooltip]");
  if (tooltip) tooltip.hidden = true;
}

function showActivityTooltip(cellElement, cell) {
  if (cell.future) return;
  const card = document.querySelector("#activity-card");
  const tooltip = card?.querySelector?.("[data-activity-tooltip]");
  if (!card || !tooltip) return;
  const copy = tooltipCopy(cell);
  setText(tooltip, "[data-activity-tooltip-title]", copy.title);
  setText(tooltip, "[data-activity-tooltip-detail]", copy.detail);
  tooltip.hidden = false;
  const cardRect = card.getBoundingClientRect();
  const chartRect = card.querySelector("[data-activity-chart]").getBoundingClientRect();
  const cellRect = cellElement.getBoundingClientRect();
  const tooltipRect = tooltip.getBoundingClientRect();
  const { left, top } = activityTooltipPosition(cardRect, chartRect, cellRect, tooltipRect);
  tooltip.style.left = `${left}px`;
  tooltip.style.top = `${top}px`;
}

function activityCellLabel(cell) {
  const copy = tooltipCopy(cell);
  return copy.detail ? `${copy.title}. ${copy.detail}` : copy.title;
}

function renderActivity(now = new Date()) {
  const card = document.querySelector("#activity-card");
  if (!card) return;
  if (settings.show_claude === false && settings.show_codex === false) {
    card.hidden = true;
    return;
  }
  card.hidden = false;
  const renderSignature = JSON.stringify([
    activityDataRevision,
    activityFilter,
    settings.activity_weeks,
    settings.activity_scale_mode,
    settings.activity_tokens_per_level,
    settings.show_claude,
    settings.show_codex,
    settings.language,
    toolBrandColor("claude", settings),
    toolBrandColor("codex", settings),
    `${now.getFullYear()}-${now.getMonth()}-${now.getDate()}`,
  ]);
  if (renderSignature === lastActivityRenderSignature) return;
  lastActivityRenderSignature = renderSignature;
  hideActivityTooltip();
  const view = buildActivityView(activitySnapshot, settings, activityFilter, now);
  activityFilter = view.filter;
  const language = activityLanguage();
  card.dataset.filter = view.filter;
  card.dataset.state = view.backfillPending
    ? "loading"
    : view.empty
      ? "empty"
      : view.partial
        ? "partial"
        : "ready";
  card.style.setProperty("--activity-weeks", String(view.weeks));
  card.style.setProperty("--activity-chart-width", `${view.weeks * 11 - 2}px`);
  card.style.setProperty("--activity-color-claude", toolBrandColor("claude", settings));
  card.style.setProperty("--activity-color-codex", toolBrandColor("codex", settings));

  for (const button of card.querySelectorAll("[data-activity-filter]")) {
    const filter = button.dataset.activityFilter;
    button.hidden = (filter === "claude" && settings.show_claude === false)
      || (filter === "codex" && settings.show_codex === false);
    button.setAttribute("aria-pressed", String(filter === view.filter));
  }

  setText(card, "[data-activity-total]", formatActivityTokens(view.totalTokens, language, true));
  const tokenLabel = t("activity.tokens", settings);
  const period = language === "en" ? `last ${view.weeks} weeks` : `최근 ${view.weeks}주`;
  setText(card, "[data-activity-period]", `${tokenLabel} · ${period}`);
  setText(card, "[data-activity-days]", `${view.activeDays}${t("activity.activeDays", settings)}`);

  const monthHost = card.querySelector("[data-activity-months]");
  monthHost?.replaceChildren?.();
  for (const label of view.monthLabels) {
    const element = document.createElement("span");
    element.style.gridColumn = `${label.column + 1}`;
    element.textContent = formatActivityMonth(label.date, language);
    monthHost?.append(element);
  }

  const grid = card.querySelector("[data-activity-grid]");
  grid?.setAttribute?.("aria-rowcount", "7");
  grid?.setAttribute?.("aria-colcount", String(view.weeks));
  const fragment = document.createDocumentFragment();
  let rovingIndex = view.cells.findLastIndex?.((cell) => !cell.future && cell.tokens > 0) ?? -1;
  if (rovingIndex < 0) rovingIndex = view.cells.findLastIndex?.((cell) => !cell.future) ?? 0;
  view.cells.forEach((cell, index) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "activity-cell";
    button.dataset.level = String(cell.level);
    button.dataset.activityCellIndex = String(index);
    button.tabIndex = index === rovingIndex ? 0 : -1;
    button.disabled = cell.future;
    button.setAttribute("role", "gridcell");
    button.setAttribute("aria-label", activityCellLabel(cell));
    button.addEventListener("pointerenter", () => showActivityTooltip(button, cell));
    button.addEventListener("pointerleave", () => {
      if (document.activeElement !== button) hideActivityTooltip();
    });
    button.addEventListener("focus", () => showActivityTooltip(button, cell));
    button.addEventListener("blur", hideActivityTooltip);
    fragment.append(button);
  });
  grid?.replaceChildren?.(fragment);

  const empty = card.querySelector("[data-activity-empty]");
  if (empty) {
    empty.hidden = !view.empty;
    empty.textContent = t(
      view.backfillPending ? "activity.emptyCollecting" : "activity.empty",
      settings,
    );
  }
  const scope = card.querySelector("[data-activity-scope]");
  if (scope) {
    const suffix = view.backfillPending
      ? t("activity.backfill", settings)
      : view.partial
        ? t("activity.partial", settings)
        : "";
    scope.textContent = suffix
      ? `${t("activity.localRecord", settings)} · ${suffix}`
      : t("activity.localRecord", settings);
  }
}

window.addEventListener("settings-updated", (event) => {
  if (event.detail && typeof event.detail === "object") {
    settingsEventGeneration += 1;
    settings = { ...DEFAULT_SETTINGS, ...event.detail };
    applyTheme(settings);
    applyFont(settings);
    applyTranslations(settings);
    renderStatuses(lastStatuses);
    renderActivity();
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
      renderStatuses(lastStatuses);
      renderActivity();
    }
  } catch {
    if (settingsEventGeneration !== requestGeneration) return;
    settings = { ...DEFAULT_SETTINGS };
    applyTheme(settings);
    applyFont(settings);
    applyTranslations(settings);
    renderStatuses(lastStatuses);
    renderActivity();
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

async function loadActivity() {
  const requestGeneration = activityEventGeneration;
  try {
    const snapshot = await invoke("get_activity");
    if (activityEventGeneration !== requestGeneration) return;
    activitySnapshot = snapshot && typeof snapshot === "object"
      ? snapshot
      : { days: [], partial: true };
    activityDataRevision += 1;
    renderActivity();
  } catch {
    if (activityEventGeneration !== requestGeneration) return;
    activitySnapshot = { days: [], partial: true };
    activityDataRevision += 1;
    renderActivity();
  }
}

function scheduleSnapshotFallback() {
  if (snapshotFallbackTimer || listenersDisposed) return;
  snapshotFallbackTimer = setInterval(() => {
    void loadSettings();
    void loadStatus();
  }, 30_000);
  snapshotFallbackTimer?.unref?.();
}

function scheduleActivityRefresh() {
  if (activityRefreshTimer || listenersDisposed) return;
  activityRefreshTimer = setInterval(() => {
    if (panelVisible) void loadActivity();
  }, 300_000);
  activityRefreshTimer?.unref?.();
}

function unlistenSafely(unlisten) {
  if (typeof unlisten !== "function") return;
  try {
    Promise.resolve(unlisten()).catch(() => {});
  } catch {
    // Listener teardown is best-effort while the WebView is closing.
  }
}

function cleanupListeners() {
  if (listenersDisposed) return;
  listenersDisposed = true;
  listenerLifecycleGeneration += 1;
  if (snapshotFallbackTimer) {
    clearInterval(snapshotFallbackTimer);
    snapshotFallbackTimer = null;
  }
  if (activityRefreshTimer) {
    clearInterval(activityRefreshTimer);
    activityRefreshTimer = null;
  }
  const unlisteners = [...activeUnlisteners];
  activeUnlisteners.clear();
  for (const unlisten of unlisteners) unlistenSafely(unlisten);
}

function registerListenerAttempt(listen, eventName, handler) {
  const lifecycleGeneration = listenerLifecycleGeneration;
  let timedOut = false;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      timedOut = true;
      reject(new Error(`${eventName} listener registration timed out`));
    }, LISTENER_REGISTRATION_TIMEOUT_MS);

    let registration;
    try {
      registration = listen(eventName, handler);
    } catch (error) {
      clearTimeout(timer);
      reject(error);
      return;
    }

    Promise.resolve(registration).then(
      (unlisten) => {
        clearTimeout(timer);
        if (
          timedOut ||
          listenersDisposed ||
          lifecycleGeneration !== listenerLifecycleGeneration
        ) {
          unlistenSafely(unlisten);
          if (!timedOut) reject(new Error(`${eventName} listener lifecycle ended`));
          return;
        }
        resolve(unlisten);
      },
      (error) => {
        clearTimeout(timer);
        if (!timedOut) reject(error);
      },
    );
  });
}

async function listenWithRetry(listen, eventName, handler) {
  for (const delay of LISTENER_RETRY_DELAYS_MS) {
    if (delay) await new Promise((resolve) => setTimeout(resolve, delay));
    if (listenersDisposed) return false;
    try {
      const unlisten = await registerListenerAttempt(listen, eventName, handler);
      if (typeof unlisten === "function") activeUnlisteners.add(unlisten);
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
          renderActivity();
        }
      });
    void listenWithRetry(listen, "activity-updated", (event) => {
      activityEventGeneration += 1;
      activitySnapshot = event.payload && typeof event.payload === "object"
        ? event.payload
        : { days: [], partial: true };
      activityDataRevision += 1;
      renderActivity();
    });
    void listenWithRetry(listen, "panel-visibility-updated", (event) => {
        setPanelVisible(event.payload !== false);
      });
  }
}

async function bootstrap() {
  setPanelVisible(!document.hidden);
  window.addEventListener("focus", () => setPanelVisible(true));
  window.addEventListener("pagehide", cleanupListeners);
  window.addEventListener("beforeunload", cleanupListeners);
  document.addEventListener("visibilitychange", () => setPanelVisible(!document.hidden));
  applyTranslations(settings);
  bindWindowControls();
  bindPanelDragFallback();
  bindStatusUpdates();
  scheduleActivityRefresh();
  document.addEventListener("keydown", (event) => {
    const cell = event.target?.closest?.("[data-activity-cell-index]");
    if (!cell) return;
    const cells = [...document.querySelectorAll("[data-activity-cell-index]:not(:disabled)")];
    const current = cells.indexOf(cell);
    if (current < 0) return;
    let next = current;
    if (event.key === "ArrowRight") next = Math.min(cells.length - 1, current + 7);
    else if (event.key === "ArrowLeft") next = Math.max(0, current - 7);
    else if (event.key === "ArrowDown") next = Math.min(cells.length - 1, current + 1);
    else if (event.key === "ArrowUp") next = Math.max(0, current - 1);
    else return;
    event.preventDefault();
    cells.forEach((item, index) => { item.tabIndex = index === next ? 0 : -1; });
    cells[next]?.focus?.();
  });
  await Promise.all([loadSettings(), loadStatus(), loadActivity()]);
}

bootstrap();
