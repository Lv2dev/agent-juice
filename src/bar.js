import { DEFAULT_SETTINGS } from "./panel-state.js";
import { barViewModel } from "./bar-state.js";
import { applyFont } from "./font.js";
import { createTextScaleState, fittedRingNumberSize, TEXT_SCALE_EVENT } from "./text-scale.js";
import { applyTranslations } from "./i18n.js";
import { applyTheme } from "./theme.js";

let settings = { ...DEFAULT_SETTINGS };
let statuses = [];
let collectionHealth = {};
let startupStatusLoading = true;
let statusEventGeneration = 0;
let collectionHealthEventGeneration = 0;
let settingsEventGeneration = 0;
let snapshotFallbackTimer = null;
let listenerLifecycleGeneration = 0;
let listenersDisposed = false;
const activeUnlisteners = new Set();
const LISTENER_RETRY_DELAYS_MS = [0, 100, 250];
const LISTENER_REGISTRATION_TIMEOUT_MS = 500;
const STARTUP_STATUS_TIMEOUT_MS = 20_000;
const CONTENT_WIDTH_SYNC_DELAY_MS = 80;
const CONTENT_WIDTH_RETRY_DELAY_MS = 250;
const CONTENT_WIDTH_MAX_RETRIES = 20;
let lastNativeTooltip = "";
let pendingNativeTooltip = null;
let contentWidthSyncTimer = null;
let lastRequestedContentWidth = "";
let contentWidthRetryKey = "";
let contentWidthRetryCount = 0;
const TOOLS = ["claude", "codex", "grok", "cursor"];

function currentWindowTool() {
  const search = window.location?.search ?? globalThis.location?.search ?? "";
  const tool = new URLSearchParams(search).get("tool");
  return TOOLS.includes(tool) ? tool : null;
}

const CURRENT_TOOL = currentWindowTool();
const MENU_WIDTH = 88;
const MENU_HEIGHT = 28;
const MENU_MARGIN = 4;
const MENU_CLOSE_GRACE_MS = 120;
const REFRESH_MENU_OPENED_EVENT = "bar-refresh-menu-opened";
const MENU_STATES = Object.freeze({
  CLOSED: "closed",
  OPENING: "opening",
  OPEN: "open",
  CLOSING: "closing",
});
let refreshMenuCloseTimer = null;
let refreshMenuState = MENU_STATES.CLOSED;
let refreshMenuRequest = 0;
let refreshMenuDesiredOpen = false;
let refreshStatusInFlight = null;
let taskbarOrientationRequest = 0;

document.addEventListener("contextmenu", (event) => {
  event.preventDefault();
  void showRefreshMenu(event);
});

document.addEventListener("click", (event) => {
  const menu = refreshMenu();
  if (!menu || menu.hidden || menu.contains(event.target)) return;
  hideRefreshMenu();
});

document.addEventListener("pointerdown", (event) => {
  const menu = refreshMenu();
  if (!menu || menu.hidden || menu.contains(event.target)) return;
  hideRefreshMenu();
});

document.addEventListener("mouseout", (event) => {
  if (!event.relatedTarget) scheduleRefreshMenuClose();
});

document.addEventListener("mouseover", cancelRefreshMenuClose);

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

function withTimeout(promise, timeoutMs) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("status request timed out")), timeoutMs);
    timer?.unref?.();
    Promise.resolve(promise).then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

function refreshTaskbarStatus() {
  if (refreshStatusInFlight) return refreshStatusInFlight;
  const request = invoke("refresh_status")
    .catch(() => {
      // The bar should still suppress the browser menu even if IPC is unavailable.
    })
    .finally(() => {
      if (refreshStatusInFlight === request) refreshStatusInFlight = null;
    });
  refreshStatusInFlight = request;
  return request;
}

function emitSafely(eventName, payload) {
  const emit = tauriApi().event?.emit;
  if (!emit) return;
  try {
    Promise.resolve(emit(eventName, payload)).catch(() => {});
  } catch {
    // Cross-window menu sync is best-effort; local dismissal still works.
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

async function showRefreshMenu(event) {
  const menu = refreshMenu();
  if (!menu) {
    void refreshTaskbarStatus();
    return;
  }

  cancelRefreshMenuClose();
  const request = ++refreshMenuRequest;
  refreshMenuDesiredOpen = true;
  menu.hidden = true;
  setRefreshMenuState(MENU_STATES.OPENING);

  try {
    const nativeOpened = await setNativeMenuOpen(true);
    if (!nativeOpened) throw new Error("native menu state is unavailable");
    if (request !== refreshMenuRequest || refreshMenuState !== MENU_STATES.OPENING) {
      if (!refreshMenuDesiredOpen) {
        setRefreshMenuState(MENU_STATES.CLOSING);
        await setNativeMenuOpen(false).catch(() => false);
        if (!refreshMenuDesiredOpen) {
          setRefreshMenuState(MENU_STATES.CLOSED);
          syncNativeTooltip();
          scheduleTaskbarContentWidthSync();
        }
      }
      return;
    }

    const x = clampMenuPosition(event?.clientX, MENU_WIDTH, window.innerWidth ?? MENU_WIDTH);
    const y = clampMenuPosition(event?.clientY, MENU_HEIGHT, window.innerHeight ?? MENU_HEIGHT);
    menu.style?.setProperty("--menu-x", `${x}px`);
    menu.style?.setProperty("--menu-y", `${y}px`);
    menu.hidden = false;
    setRefreshMenuState(MENU_STATES.OPEN);
    emitSafely(REFRESH_MENU_OPENED_EVENT, { tool: CURRENT_TOOL ?? "all" });
  } catch {
    if (request !== refreshMenuRequest) return;
    refreshMenuDesiredOpen = false;
    menu.hidden = true;
    await setNativeMenuOpen(false).catch(() => false);
    setRefreshMenuState(MENU_STATES.CLOSED);
    syncNativeTooltip();
    scheduleTaskbarContentWidthSync();
  }
}

function hideRefreshMenu() {
  cancelRefreshMenuClose();
  const menu = refreshMenu();
  if (menu) menu.hidden = true;
  const shouldCloseNative = refreshMenuState !== MENU_STATES.CLOSED;
  const request = ++refreshMenuRequest;
  refreshMenuDesiredOpen = false;
  if (shouldCloseNative) {
    setRefreshMenuState(MENU_STATES.CLOSING);
    void setNativeMenuOpen(false)
      .then(() => {
        if (request !== refreshMenuRequest || refreshMenuDesiredOpen) return;
        setRefreshMenuState(MENU_STATES.CLOSED);
        syncNativeTooltip();
        scheduleTaskbarContentWidthSync();
      })
      .catch(() => {});
  } else {
    setRefreshMenuState(MENU_STATES.CLOSED);
    scheduleTaskbarContentWidthSync();
  }
}

function setRefreshMenuState(state) {
  refreshMenuState = state;
  const root = document.querySelector("#bar");
  if (root) root.dataset.menuState = state;
}

async function setNativeMenuOpen(open) {
  if (!CURRENT_TOOL || !tauriApi().core?.invoke) return false;
  await invoke("set_taskbar_menu_open", { tool: CURRENT_TOOL, open });
  return true;
}

function cancelRefreshMenuClose() {
  if (refreshMenuCloseTimer == null) return;
  clearTimeout(refreshMenuCloseTimer);
  refreshMenuCloseTimer = null;
}

function scheduleRefreshMenuClose() {
  cancelRefreshMenuClose();
  refreshMenuCloseTimer = setTimeout(() => {
    refreshMenuCloseTimer = null;
    hideRefreshMenu();
  }, MENU_CLOSE_GRACE_MS);
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
  const semanticOrder = limitOrder === "secondary_first"
    ? [vm.secondary, vm.primary]
    : [vm.primary, vm.secondary];
  return [
    ...semanticOrder.filter((limit) => limit.visible),
    ...semanticOrder.filter((limit) => !limit.visible),
  ];
}

function setDisplayLimitVars(scope, first, second) {
  scope.style?.setProperty("--primary-color", first.color);
  scope.style?.setProperty("--primary-arc", first.arc);
  scope.style?.setProperty("--primary-dash", first.dash);
  scope.style?.setProperty(
    "--primary-ring-visibility",
    first.percent != null && first.percent > 0 ? "visible" : "hidden",
  );
  scope.style?.setProperty("--primary-percent", `${first.percent ?? 0}%`);
  scope.style?.setProperty("--secondary-color", second.color);
  scope.style?.setProperty("--secondary-arc", second.arc);
  scope.style?.setProperty("--secondary-dash", second.dash);
  scope.style?.setProperty(
    "--secondary-ring-visibility",
    second.percent != null && second.percent > 0 ? "visible" : "hidden",
  );
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
  const visibleLimitCount = [vm.primary, vm.secondary]
    .filter((limit) => limit.visible)
    .length;
  item.dataset.limitCount = String(Math.max(1, visibleLimitCount));
  const singleLimit = visibleLimitCount < 2;
  for (const selector of [
    ".inner-track",
    ".inner-effect",
    ".inner-arc",
    ".quad-secondary",
    ".secondary-limit",
  ]) {
    const elements = typeof item.querySelectorAll === "function"
      ? item.querySelectorAll(selector)
      : [item.querySelector(selector)].filter(Boolean);
    for (const element of elements) {
      element.hidden = singleLimit;
    }
  }
  const secondaryLine = item.querySelector(".secondary-text")?.closest?.(".bar-line");
  if (secondaryLine) secondaryLine.hidden = singleLimit;
  item.removeAttribute?.("title");
  item.setAttribute?.("aria-label", vm.ariaLabel);
  item.style?.setProperty("--tool-brand", vm.brandColor);
  setRing(item, vm, limitOrder);
  setText(item, ".bar-tool-name", vm.label);
  setText(item, ".bar-worst", vm.worst);
  if (vm.state === "login_required") {
    setText(item, ".quad-primary-number", "-");
    setText(item, ".quad-secondary-number", "-");
    setText(item, ".primary-text", vm.loginText);
    setText(item, ".primary-reset", "");
    setText(item, ".secondary-text", "");
    setText(item, ".secondary-reset", "");
  } else if (vm.state === "loading") {
    setText(item, ".quad-primary-number", "…");
    setText(item, ".quad-secondary-number", "…");
    setText(item, ".primary-text", vm.loadingText);
    setText(item, ".primary-reset", "");
    setText(item, ".secondary-text", "");
    setText(item, ".secondary-reset", "");
  } else {
    setText(item, ".quad-primary-number", first.number);
    setText(item, ".quad-secondary-number", second.number);
    setText(item, ".primary-text", first.text);
    setText(item, ".primary-reset", first.reset ? `(${first.reset})` : "");
    setText(item, ".secondary-text", second.text);
    setText(item, ".secondary-reset", second.reset ? `(${second.reset})` : "");
  }
  if (CURRENT_TOOL === vm.tool) {
    pendingNativeTooltip = { tool: vm.tool, text: vm.tooltip };
    syncNativeTooltip();
  }
}

function fitRingNumbers() {
  const item = toolElement(CURRENT_TOOL);
  if (!item || !document.createRange || !currentBarView) return;
  for (const number of item.querySelectorAll?.(".bar-worst, .quad-number") ?? []) {
    number.style.removeProperty("font-size");
    if (currentBarView.textScale <= 1 || !number.getClientRects().length || !number.textContent) continue;
    const range = document.createRange();
    range.selectNodeContents(number);
    const rect = range.getBoundingClientRect();
    const size = fittedRingNumberSize(currentBarView.ringNumberFontSizePx, rect.width, rect.height,
      currentBarView.ringCenterSizePx, currentBarView.ringNumberOutlineOn ? currentBarView.ringNumberOutlineWidthPx : 0);
    number.style.fontSize = `${size}px`;
  }
}

function scheduleTaskbarContentWidthSync() {
  if (listenersDisposed || !CURRENT_TOOL || !tauriApi().core?.invoke) return;
  const root = document.querySelector("#bar");
  const item = toolElement(CURRENT_TOOL);
  if (
    !root ||
    !item ||
    item.hidden ||
    refreshMenuState !== MENU_STATES.CLOSED ||
    (typeof item.getBoundingClientRect !== "function" && !(Number(item.scrollWidth) > 0))
  ) {
    return;
  }

  clearTimeout(contentWidthSyncTimer);
  contentWidthSyncTimer = setTimeout(() => {
    contentWidthSyncTimer = null;
    if (
      listenersDisposed || item.hidden ||
      refreshMenuState !== MENU_STATES.CLOSED
    ) {
      return;
    }

    const vertical = root.dataset.taskbarOrientation === "vertical";
    fitRingNumbers();
    const rect = item.getBoundingClientRect?.() ?? {};
    const rectLength = Number(vertical ? rect.height : rect.width) || 0;
    const scrollLength = Number(vertical ? item.scrollHeight : item.scrollWidth) || 0;
    const width = Math.ceil(Math.max(rectLength, scrollLength));
    if (!Number.isFinite(width) || width < 1 || width > 2304) return;
    const mode = root.dataset.mode;
    const devicePixelRatio = Number(window.devicePixelRatio) || 1;
    const requestKey = `${vertical ? "vertical" : "horizontal"}:${mode}:${width}:${devicePixelRatio}`;
    if (requestKey === lastRequestedContentWidth) return;
    if (requestKey !== contentWidthRetryKey) {
      contentWidthRetryKey = requestKey;
      contentWidthRetryCount = 0;
    }
    lastRequestedContentWidth = requestKey;
    void invoke("set_taskbar_content_width", { tool: CURRENT_TOOL, width, mode, devicePixelRatio })
      .then(() => {
        if (contentWidthRetryKey === requestKey) {
          contentWidthRetryKey = "";
          contentWidthRetryCount = 0;
        }
      })
      .catch(() => {
        if (lastRequestedContentWidth === requestKey) lastRequestedContentWidth = "";
        if (
          contentWidthRetryKey !== requestKey ||
          contentWidthRetryCount >= CONTENT_WIDTH_MAX_RETRIES ||
          listenersDisposed
        ) {
          return;
        }
        contentWidthRetryCount += 1;
        clearTimeout(contentWidthSyncTimer);
        contentWidthSyncTimer = setTimeout(() => {
          contentWidthSyncTimer = null;
          scheduleTaskbarContentWidthSync();
        }, CONTENT_WIDTH_RETRY_DELAY_MS);
      });
  }, CONTENT_WIDTH_SYNC_DELAY_MS);
}

function observeTaskbarContentSize() {
  if (!CURRENT_TOOL) return;
  const item = toolElement(CURRENT_TOOL);
  const remeasure = () => {
    lastRequestedContentWidth = "";
    if (systemTextScale.factor > 1) renderBar();
    else scheduleTaskbarContentWidthSync();
  };
  const Observer = window.ResizeObserver;
  if (Observer && item) {
    const observer = new Observer(scheduleTaskbarContentWidthSync);
    observer.observe(item, { box: "border-box" });
    activeUnlisteners.add(() => observer.disconnect());
  }
  window.addEventListener?.("resize", remeasure);
  activeUnlisteners.add(() => window.removeEventListener?.("resize", remeasure));
  const fonts = document.fonts;
  fonts?.ready?.then(remeasure).catch(() => {});
  fonts?.addEventListener?.("loadingdone", remeasure);
  activeUnlisteners.add(() => fonts?.removeEventListener?.("loadingdone", remeasure));
  let removeDpiListener;
  const watchDpi = () => {
    removeDpiListener?.();
    if (listenersDisposed) return;
    const query = window.matchMedia?.(`(resolution: ${Number(window.devicePixelRatio) || 1}dppx)`);
    query?.addEventListener?.("change", watchDpi);
    removeDpiListener = () => query?.removeEventListener?.("change", watchDpi);
    remeasure();
  };
  watchDpi();
  activeUnlisteners.add(() => removeDpiListener?.());
}

function syncNativeTooltip() {
  if (refreshMenuState !== MENU_STATES.CLOSED || !pendingNativeTooltip) return;
  const { tool, text } = pendingNativeTooltip;
  if (lastNativeTooltip === text) return;
  lastNativeTooltip = text;
  void invoke("set_taskbar_tooltip", { tool, text }).catch(() => {
    lastNativeTooltip = "";
  });
}

let currentBarView = null;
const systemTextScale = createTextScaleState(() => renderBar());

function renderBar() {
  const root = document.querySelector("#bar");
  if (!root) return;

  const vm = barViewModel(statuses, settings, new Date(), {
    startupLoading: startupStatusLoading,
    collectionHealth,
    textScale: systemTextScale.factor,
    crossAxisSize: root.dataset.taskbarOrientation === "vertical"
      ? document.documentElement?.clientWidth : document.documentElement?.clientHeight,
  });
  currentBarView = vm;
  root.dataset.mode = vm.mode;
  root.dataset.menuState = refreshMenuState;
  root.dataset.fullResetTime = vm.fullResetTimeOn ? "on" : "off";
  root.dataset.limitOrder = vm.limitOrder.replace("_", "-");
  root.dataset.indicator = vm.indicatorStyle;
  root.dataset.effect = vm.indicatorEffectStyle;
  root.dataset.indicatorTrackColor = vm.indicatorTrackColorAuto ? "theme" : "custom";
  root.dataset.claudeTextColor = vm.claudeTextColorOn ? "custom" : "auto";
  root.dataset.codexTextColor = vm.codexTextColorOn ? "custom" : "auto";
  root.dataset.grokTextColor = vm.grokTextColorOn ? "custom" : "auto";
  root.dataset.cursorTextColor = vm.cursorTextColorOn ? "custom" : "auto";
  root.dataset.infoTextColor = vm.infoTextColorOn ? "custom" : "auto";
  root.dataset.ringTextColor = vm.ringTextColorOn ? "custom" : "auto";
  root.dataset.ring = vm.ringOn ? "on" : "off";
  root.dataset.ringNumbers = vm.ringNumbersOn ? "on" : "off";
  root.dataset.numberOutline = vm.ringNumberOutlineOn ? "on" : "off";
  root.style?.setProperty("--ring-number-outline-width", `${vm.ringNumberOutlineWidthPx}px`);
  root.style?.setProperty(
    "--indicator-track-color",
    vm.indicatorTrackColorAuto ? "var(--text)" : vm.indicatorTrackColor,
  );
  root.style?.setProperty(
    "--indicator-track-opacity",
    `${vm.indicatorTrackOpacityPercent}%`,
  );
  root.style?.setProperty("--claude-text-color", vm.claudeTextColor);
  root.style?.setProperty("--codex-text-color", vm.codexTextColor);
  root.style?.setProperty("--grok-text-color", vm.grokTextColor);
  root.style?.setProperty("--cursor-text-color", vm.cursorTextColor);
  root.style?.setProperty("--info-text-color", vm.infoTextColor);
  root.style?.setProperty("--ring-text-color", vm.ringTextColor);
  root.style?.setProperty("--ring-size", `${vm.ringSizePx}px`);
  root.style?.setProperty("--ring-thickness", `${vm.ringThicknessPx}px`);
  root.style?.setProperty("--ring-gap", `${vm.ringGapPx}px`);
  root.style?.setProperty("--ring-svg-stroke", vm.ringSvgStroke);
  root.style?.setProperty("--outer-radius", vm.outerRadius);
  root.style?.setProperty("--inner-radius", vm.innerRadius);
  root.style?.setProperty("--quad-svg-stroke", vm.quadSvgStroke);
  root.style?.setProperty("--quad-radius", vm.quadRadius);
  root.style?.setProperty("--ring-number-font-size", `${vm.ringNumberFontSizePx}px`);
  root.style?.setProperty("--ring-number-font-weight", String(vm.ringNumberFontWeight));
  root.style?.setProperty("--bar-text-font-size", `${vm.barTextFontSizePx}px`);
  root.style?.setProperty("--bar-text-font-weight", String(vm.barTextFontWeight));
  root.style?.setProperty("--bar-content-gap", `${vm.barContentGapPx}px`);
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
  scheduleTaskbarContentWidthSync();
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
    if (settingsEventGeneration !== requestGeneration) return;
    settings = { ...DEFAULT_SETTINGS };
    applyTheme(settings);
    applyFont(settings);
    applyTranslations(settings);
  }
}

async function loadTaskbarOrientation() {
  const root = document.querySelector("#bar");
  if (!root) return;

  const requestGeneration = ++taskbarOrientationRequest;
  if (!CURRENT_TOOL) {
    root.dataset.taskbarOrientation = "horizontal";
    return;
  }

  try {
    const orientation = await invoke("get_taskbar_orientation", { tool: CURRENT_TOOL });
    if (taskbarOrientationRequest !== requestGeneration) return;
    root.dataset.taskbarOrientation = orientation === "vertical" ? "vertical" : "horizontal";
    scheduleTaskbarContentWidthSync();
  } catch {
    if (taskbarOrientationRequest !== requestGeneration) return;
    root.dataset.taskbarOrientation = "horizontal";
    scheduleTaskbarContentWidthSync();
  }
}

function scheduleSnapshotFallback() {
  if (snapshotFallbackTimer || listenersDisposed) return;
  snapshotFallbackTimer = setInterval(() => {
    void systemTextScale.load(invoke);
    void loadSettings().then(renderBar);
    void loadStatus();
    void loadTaskbarOrientation();
  }, 30_000);
  snapshotFallbackTimer?.unref?.();
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
  systemTextScale.dispose();
  listenerLifecycleGeneration += 1;
  clearTimeout(contentWidthSyncTimer);
  if (snapshotFallbackTimer) {
    clearInterval(snapshotFallbackTimer);
    snapshotFallbackTimer = null;
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

function bindEvents() {
  observeTaskbarContentSize();
  const currentWindow = tauriApi().window?.getCurrentWindow?.();
  if (typeof currentWindow?.listen === "function") {
    void listenWithRetry(currentWindow.listen.bind(currentWindow), "tauri://scale-change", () => {
      lastRequestedContentWidth = "";
      renderBar();
    });
  }
  const listen = tauriApi().event?.listen;
  if (listen) {
    void listenWithRetry(listen, TEXT_SCALE_EVENT, (event) => systemTextScale.accept(event.payload));
    void listenWithRetry(listen, REFRESH_MENU_OPENED_EVENT, (event) => {
      const openedTool = event.payload?.tool;
      if (openedTool && openedTool !== (CURRENT_TOOL ?? "all")) {
        hideRefreshMenu();
      }
    });
    void listenWithRetry(listen, "status-updated", (event) => {
      statusEventGeneration += 1;
      startupStatusLoading = false;
      statuses = Array.isArray(event.payload) ? event.payload : [];
      renderBar();
    });
    void listenWithRetry(listen, "collection-health-updated", (event) => {
      collectionHealthEventGeneration += 1;
      collectionHealth = event.payload && typeof event.payload === "object" ? event.payload : {};
      renderBar();
    });
    void listenWithRetry(listen, "settings-updated", (event) => {
      if (event.payload && typeof event.payload === "object") {
        settingsEventGeneration += 1;
        settings = { ...DEFAULT_SETTINGS, ...event.payload };
        applyTheme(settings);
        applyFont(settings);
        applyTranslations(settings);
        renderBar();
        void loadTaskbarOrientation();
      }
    });
    void listenWithRetry(listen, "taskbar-dragging-updated", (event) => {
      setDragging(event.payload);
    });
    void listenWithRetry(listen, "taskbar-topology-updated", (event) => {
      const root = document.querySelector("#bar");
      if (!root) return;
      root.dataset.taskbarOrientation = event.payload === "vertical" ? "vertical" : "horizontal";
      lastRequestedContentWidth = "";
      scheduleTaskbarContentWidthSync();
    });
  }
}

async function loadStatus() {
  const requestGeneration = statusEventGeneration;
  const healthRequestGeneration = collectionHealthEventGeneration;
  try {
    const loaded =
      (await withTimeout(invoke("get_status"), STARTUP_STATUS_TIMEOUT_MS)) || [];
    try {
      const health = await invoke("get_collection_health");
      if (collectionHealthEventGeneration === healthRequestGeneration) {
        collectionHealth = health && typeof health === "object" ? health : {};
      }
    } catch {
      // Health is supplemental; keep rendering the last valid usage snapshot.
    }
    if (statusEventGeneration !== requestGeneration) return;
    statuses = loaded;
  } catch {
    if (statusEventGeneration !== requestGeneration) return;
    statuses = [];
  }
  startupStatusLoading = false;
  renderBar();
}

async function bootstrap() {
  applyTranslations(settings);
  window.addEventListener?.("pagehide", cleanupListeners);
  window.addEventListener?.("beforeunload", cleanupListeners);
  bindRefreshMenu();
  renderBar();
  bindEvents();
  void systemTextScale.load(invoke);
  await Promise.all([loadSettings().then(renderBar), loadStatus(), loadTaskbarOrientation()]);
  renderBar();
}

bootstrap();
