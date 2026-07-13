import { formStateFromSettings, payloadFromEntries } from "./settings-state.js";
import { applyFont } from "./font.js";
import { applyTranslations, t } from "./i18n.js";
import { applyTheme } from "./theme.js";

const form = document.querySelector("#settings-form");
const statusEl = document.querySelector("#settings-status");
const statusHost = statusEl?.closest?.("[data-settings-save-state]");
const toastLayer = document.querySelector("[data-settings-toast]");
const toastText = document.querySelector("[data-settings-toast-text]");
const customRow = document.querySelector("[data-custom-palette]");
const monoRow = document.querySelector("[data-mono-palette]");
const toolColorRow = document.querySelector("[data-tool-palette]");
const fullResetRow = document.querySelector("[data-full-reset-toggle]");
const palettePicker = document.querySelector("[data-palette-picker]");
const effectPicker = document.querySelector("[data-effect-picker]");
const updateBand = document.querySelector("#update-band");
const updateVersionEl = document.querySelector("[data-update-version]");
const updateStatusEl = document.querySelector("#update-check-status");
const appVersionEls = document.querySelectorAll?.("[data-app-version]") ?? [];
let autosaveTimer = null;
let isHydrating = false;
let hasLoadedSettings = false;
let localRevision = 0;
let savedRevision = 0;
let saveQueue = Promise.resolve();
let currentDisplayBasis = "remaining";
let currentUpdateStatus = null;
let toastTimer = null;
let toastHideTimer = null;
let settingsEventGeneration = 0;
let quitListenerReady = false;
let quitFlushPromise = null;
const SETTINGS_LOAD_RETRY_DELAYS_MS = [0, 100, 250];
const LISTENER_RETRY_DELAYS_MS = [0, 100, 250];
const LISTENER_REGISTRATION_TIMEOUT_MS = 500;

function tauriApi() {
  return window.__TAURI__ ?? {};
}

async function invoke(command, args) {
  const fn = tauriApi().core?.invoke;
  if (!fn) throw new Error(t("error.noTauri", currentLanguageSettings()));
  return fn(command, args);
}

function currentLanguageSettings() {
  return { language: form?.elements.namedItem("language")?.value ?? "system" };
}

function setStatus(text, state = "ready") {
  const value = String(text ?? "");
  if (statusEl) statusEl.textContent = value;
  if (statusHost) {
    statusHost.dataset.state = state;
    statusHost.hidden = value.length === 0;
  }
}

function wait(delay) {
  return new Promise((resolve) => setTimeout(resolve, delay));
}

function setSettingsFormEnabled(enabled) {
  if (!form) return;
  if (form.dataset) form.dataset.loadState = enabled ? "ready" : "loading";
  form.setAttribute?.("aria-busy", String(!enabled));
  for (const control of form.querySelectorAll?.("input, select, button") ?? []) {
    control.disabled = !enabled;
  }
}

function clearToastTimers() {
  clearTimeout(toastTimer);
  clearTimeout(toastHideTimer);
  toastTimer = null;
  toastHideTimer = null;
}

function scheduleToastTimer(callback, delay) {
  const timer = setTimeout(callback, delay);
  timer?.unref?.();
  return timer;
}

function hideSettingsToast() {
  clearToastTimers();
  if (!toastLayer) return;
  delete toastLayer.dataset.visible;
  toastLayer.hidden = true;
}

function showSettingsToast(text) {
  if (!toastLayer || !toastText) return;
  clearToastTimers();
  toastText.textContent = text;
  toastLayer.hidden = false;
  toastLayer.dataset.visible = "true";
  toastTimer = scheduleToastTimer(() => {
    delete toastLayer.dataset.visible;
    toastHideTimer = scheduleToastTimer(() => {
      toastLayer.hidden = true;
      toastHideTimer = null;
    }, 180);
    toastTimer = null;
  }, 1600);
}

function setField(name, value) {
  const field = form?.elements.namedItem(name);
  if (!field) return;

  if (field.type === "checkbox") {
    field.checked = Boolean(value);
  } else {
    field.value = String(value);
  }
}

function formatRangeProgress(field) {
  const min = Number(field?.min ?? 0);
  const max = Number(field?.max ?? 100);
  const value = Number(field?.value);
  if (!Number.isFinite(min) || !Number.isFinite(max) || !Number.isFinite(value) || max <= min) {
    return "0%";
  }

  const percent = Math.min(100, Math.max(0, ((value - min) / (max - min)) * 100));
  const rounded = Math.round(percent * 10) / 10;
  return `${Number.isInteger(rounded) ? rounded.toFixed(0) : rounded.toFixed(1)}%`;
}

function updateRangeProgress(field) {
  if (field?.type !== "range") return;
  field.style?.setProperty("--range-progress", formatRangeProgress(field));
}

function displayBasis(value) {
  return String(value || "remaining").toLowerCase() === "used" ? "used" : "remaining";
}

function updateDisplayBasisCopy() {
  if (!form) return;
  const basis = displayBasis(form.elements.namedItem("display_basis")?.value);
  const keys = basis === "used"
    ? { warning: "field.usedWarning", danger: "field.usedDanger", help: "help.usedThresholds" }
    : { warning: "field.remainingWarning", danger: "field.remainingDanger", help: "help.remainingThresholds" };
  for (const element of form.querySelectorAll?.("[data-display-basis-copy]") ?? []) {
    const key = keys[element.dataset.displayBasisCopy];
    if (key) element.textContent = t(key, currentLanguageSettings());
  }
}

function transformThresholdInputs(nextBasis) {
  if (!form || nextBasis === currentDisplayBasis) return;
  for (const name of ["warn_threshold", "danger_threshold"]) {
    const field = form.elements.namedItem(name);
    const value = Math.min(100, Math.max(0, Number(field?.value)));
    if (field && Number.isFinite(value)) field.value = String(100 - value);
  }
  currentDisplayBasis = nextBasis;
}

function updateOutputs() {
  if (!form) return;
  const pairs = [
    ["warn_threshold", "warn-output"],
    ["danger_threshold", "danger-output"],
    ["poll_interval_secs", "poll-output"],
    ["stale_after_secs", "stale-output"],
    ["ring_number_outline_width_px", "ring-number-outline-width-output"],
    ["ring_size_px", "ring-size-output"],
    ["ring_thickness_px", "ring-thickness-output"],
    ["ring_gap_px", "ring-gap-output"],
    ["ring_center_size_px", "ring-center-size-output"],
    ["ring_number_font_size_px", "ring-number-font-size-output"],
    ["ring_number_font_weight", "ring-number-font-weight-output"],
    ["bar_text_font_size_px", "bar-text-font-size-output"],
    ["bar_text_font_weight", "bar-text-font-weight-output"],
  ];

  for (const [fieldName, outputName] of pairs) {
    const field = form.elements.namedItem(fieldName);
    const output = form.elements.namedItem(outputName);
    if (field && output) output.value = field.value;
    updateRangeProgress(field);
  }

  const palette = form.elements.namedItem("palette")?.value ?? "traffic";
  if (customRow) customRow.hidden = palette !== "custom";
  if (monoRow) monoRow.hidden = palette !== "mono";
  if (toolColorRow) toolColorRow.hidden = palette !== "traffic";
  if (fullResetRow) {
    fullResetRow.hidden = (form.elements.namedItem("bar_mode")?.value ?? "full") !== "full";
  }

  for (const option of palettePicker?.querySelectorAll?.("[data-palette-value]") ?? []) {
    const selected = option.dataset.paletteValue === palette;
    option.setAttribute("aria-checked", String(selected));
    option.tabIndex = selected ? 0 : -1;
  }
  const effect = form.elements.namedItem("indicator_effect_style")?.value ?? "flat";
  for (const option of effectPicker?.querySelectorAll?.("[data-effect-value]") ?? []) {
    const selected = option.dataset.effectValue === effect;
    option.setAttribute("aria-checked", String(selected));
    option.tabIndex = selected ? 0 : -1;
  }
  palettePicker?.style?.setProperty(
    "--mono-swatch",
    form.elements.namedItem("mono_color")?.value ?? "#4f8a73",
  );
  palettePicker?.style?.setProperty(
    "--custom-safe-swatch",
    form.elements.namedItem("custom_safe")?.value ?? "#22c55e",
  );
  palettePicker?.style?.setProperty(
    "--custom-warn-swatch",
    form.elements.namedItem("custom_warn")?.value ?? "#f59e0b",
  );
  palettePicker?.style?.setProperty(
    "--custom-danger-swatch",
    form.elements.namedItem("custom_danger")?.value ?? "#ef4444",
  );
  palettePicker?.style?.setProperty(
    "--tool-claude-primary-swatch",
    form.elements.namedItem("claude_primary_color")?.value ?? "#d79a32",
  );
  palettePicker?.style?.setProperty(
    "--tool-claude-secondary-swatch",
    form.elements.namedItem("claude_secondary_color")?.value ?? "#d36b86",
  );
  palettePicker?.style?.setProperty(
    "--tool-codex-primary-swatch",
    form.elements.namedItem("codex_primary_color")?.value ?? "#2fac7d",
  );
  palettePicker?.style?.setProperty(
    "--tool-codex-secondary-swatch",
    form.elements.namedItem("codex_secondary_color")?.value ?? "#4d86d6",
  );
  updateDisplayBasisCopy();
}

function fillForm(settings) {
  if (!form) return;
  isHydrating = true;
  const state = formStateFromSettings(settings);

  setField("palette", state.palette);
  setField("display_basis", state.displayBasis);
  currentDisplayBasis = state.displayBasis;
  setField("warn_threshold", state.warnThreshold);
  setField("danger_threshold", state.dangerThreshold);
  setField("poll_interval_secs", state.pollIntervalSecs);
  setField("stale_after_secs", state.staleAfterSecs);
  setField("bar_mode", state.barMode);
  setField("full_reset_time_on", state.fullResetTimeOn);
  setField("limit_order", state.limitOrder);
  setField("fullscreen_hide_on", state.fullscreenHideOn);
  setField("maximized_hide_on", state.maximizedHideOn);
  setField("indicator_style", state.indicatorStyle);
  setField("indicator_effect_style", state.indicatorEffectStyle);
  setField("ring_on", state.ringOn);
  setField("ring_numbers_on", state.ringNumbersOn);
  setField("ring_number_outline_on", state.ringNumberOutlineOn);
  setField("ring_number_outline_width_px", state.ringNumberOutlineWidthPx);
  setField("ring_size_px", state.ringSizePx);
  setField("ring_thickness_px", state.ringThicknessPx);
  setField("ring_gap_px", state.ringGapPx);
  setField("ring_center_size_px", state.ringCenterSizePx);
  setField("ring_number_font_size_px", state.ringNumberFontSizePx);
  setField("ring_number_font_weight", state.ringNumberFontWeight);
  setField("bar_text_font_size_px", state.barTextFontSizePx);
  setField("bar_text_font_weight", state.barTextFontWeight);
  setField("autostart_on", state.autostartOn);
  setField("update_check_on", state.updateCheckOn);
  setField("language", state.language);
  setField("theme", state.theme);
  setField("font_mode", state.fontMode);
  setField("claude_taskbar_offset_ratio", state.claudeTaskbarOffsetRatio);
  setField("codex_taskbar_offset_ratio", state.codexTaskbarOffsetRatio);
  setField("show_claude", state.showClaude);
  setField("show_codex", state.showCodex);
  setField("claude_account_auto_collect_on", state.claudeAccountAutoCollectOn);
  setField("mono_color", state.monoColor);
  setField("custom_safe", state.customSafe);
  setField("custom_warn", state.customWarn);
  setField("custom_danger", state.customDanger);
  setField("claude_primary_color", state.claudePrimaryColor);
  setField("claude_secondary_color", state.claudeSecondaryColor);
  setField("codex_primary_color", state.codexPrimaryColor);
  setField("codex_secondary_color", state.codexSecondaryColor);
  applyTheme({ theme: state.theme });
  applyFont({ font_mode: state.fontMode });
  applyTranslations({ language: state.language });
  updateOutputs();
  renderUpdateStatus(currentUpdateStatus);
  isHydrating = false;
}

function hydrateSettings(settings) {
  fillForm(settings);
  hasLoadedSettings = true;
  setSettingsFormEnabled(true);
}

async function loadSettings() {
  setSettingsFormEnabled(false);
  const requestGeneration = settingsEventGeneration;
  for (const delay of SETTINGS_LOAD_RETRY_DELAYS_MS) {
    if (delay) await wait(delay);
    if (hasLoadedSettings && settingsEventGeneration !== requestGeneration) return;
    try {
      const settings = await invoke("get_settings");
      if (settingsEventGeneration !== requestGeneration) return;
      if (!settings || typeof settings !== "object") throw new Error("invalid settings payload");
      hydrateSettings(settings);
      setStatus("", "ready");
      return;
    } catch {
      // Retry while the backend or WebView IPC is still starting.
    }
  }
  setSettingsFormEnabled(false);
  if (form.dataset) form.dataset.loadState = "error";
  setStatus(t("status.settingsLoadFailed", currentLanguageSettings()), "error");
}

function renderUpdateStatus(result) {
  if (result && typeof result === "object") currentUpdateStatus = result;
  const state = currentUpdateStatus;
  const language = currentLanguageSettings();
  const currentVersion = state?.current_version;
  for (const element of appVersionEls) {
    element.textContent = currentVersion ? `v${currentVersion}` : "–";
  }

  const updateAvailable = state?.status === "update_available" && state?.release_url;
  if (updateBand) updateBand.hidden = !updateAvailable;
  if (updateVersionEl) {
    updateVersionEl.textContent = state?.latest_version ? `v${state.latest_version}` : "";
  }
  if (!updateStatusEl) return;

  const key = state?.status === "update_available"
    ? "status.updateAvailable"
    : state?.status === "current"
      ? "status.updateCurrent"
      : state?.status === "error"
        ? "status.updateFailed"
        : "status.updateUnknown";
  updateStatusEl.textContent = t(key, language);
  updateStatusEl.dataset.state = state?.status ?? "unknown";
}

async function loadUpdateStatus() {
  try {
    renderUpdateStatus(await invoke("get_update_status"));
  } catch {
    renderUpdateStatus(null);
  }
}

async function saveSettings(input, revision) {
  const response = await invoke("save_settings", { input });
  if (revision !== localRevision) return;

  const saved = response?.settings ?? response;
  const warnings = Array.isArray(response?.warnings) ? response.warnings : [];
  const next = saved || input;
  savedRevision = revision;
  window.dispatchEvent(new CustomEvent("settings-updated", { detail: next }));
  fillForm(next);
  setStatus("", "ready");
  showSettingsToast(t(warnings.length > 0 ? "status.savedRetrying" : "status.saved", next));
}

function enqueueLatestSettingsSave() {
  clearTimeout(autosaveTimer);
  autosaveTimer = null;
  if (!hasLoadedSettings || localRevision <= savedRevision) return saveQueue;

  const revision = localRevision;
  const input = payloadFromEntries(new FormData(form));
  saveQueue = saveQueue.catch(() => {}).then(() => saveSettings(input, revision));
  saveQueue.catch((error) => {
    if (revision === localRevision) setStatus(String(error), "error");
  });
  return saveQueue;
}

async function flushSettingsAndQuit() {
  if (quitFlushPromise) return quitFlushPromise;
  quitFlushPromise = (async () => {
    await enqueueLatestSettingsSave();
    await invoke("complete_app_quit");
  })();
  return quitFlushPromise;
}

function scheduleAutosave() {
  if (isHydrating || !hasLoadedSettings) return;
  localRevision += 1;
  updateOutputs();
  hideSettingsToast();
  setStatus(t("status.saving", currentLanguageSettings()), "saving");
  clearTimeout(autosaveTimer);
  autosaveTimer = setTimeout(enqueueLatestSettingsSave, quitListenerReady ? 120 : 0);
}

function handleSettingsMutation(event) {
  if (event?.target?.name === "display_basis") {
    transformThresholdInputs(displayBasis(event.target.value));
  }
  if (["theme", "font_mode", "language"].includes(event?.target?.name)) {
    applyTheme({ theme: form.elements.namedItem("theme")?.value });
    applyFont({ font_mode: form.elements.namedItem("font_mode")?.value });
    applyTranslations({ language: form.elements.namedItem("language")?.value });
  }
  scheduleAutosave();
}

async function runAction(action) {
  if (action === "restore-statusline") {
    await invoke("restore_statusline");
    setStatus("", "ready");
    showSettingsToast(t("status.restored", currentLanguageSettings()));
    return;
  }
  if (action === "check-updates") {
    if (updateStatusEl) {
      updateStatusEl.textContent = t("status.updateChecking", currentLanguageSettings());
      updateStatusEl.dataset.state = "checking";
    }
    try {
      renderUpdateStatus(await invoke("check_for_updates"));
    } catch {
      renderUpdateStatus({ status: "error" });
    }
    return;
  }
  if (action === "open-releases") {
    await invoke("open_release_page", { url: null });
    return;
  }
  if (action === "open-available-release" && currentUpdateStatus?.release_url) {
    await invoke("open_release_page", { url: currentUpdateStatus.release_url });
  }
}

async function bindSettingsUpdates() {
  const listen = tauriApi().event?.listen;
  if (!listen) return;

  const register = async (eventName, handler, critical = false) => {
    for (const delay of LISTENER_RETRY_DELAYS_MS) {
      if (delay) await wait(delay);
      try {
        await new Promise((resolve, reject) => {
          const timer = setTimeout(
            () => reject(new Error(`${eventName} listener registration timed out`)),
            LISTENER_REGISTRATION_TIMEOUT_MS,
          );
          Promise.resolve(listen(eventName, handler)).then(
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
        if (critical) quitListenerReady = true;
        return;
      } catch {
        // Each listener retries independently so one event cannot block another.
      }
    }
    if (critical) quitListenerReady = false;
  };

  void register(
    "app-quit-requested",
    () => {
      void flushSettingsAndQuit().catch((error) => setStatus(String(error), "error"));
    },
    true,
  );
  void register("settings-updated", (event) => {
      if (
        localRevision === savedRevision &&
        event.payload &&
        typeof event.payload === "object"
      ) {
        settingsEventGeneration += 1;
        hydrateSettings(event.payload);
      }
    });
  void register("update-status", (event) => {
      if (event.payload && typeof event.payload === "object") {
        renderUpdateStatus(event.payload);
      }
    });
}

if (form) {
  form.addEventListener("input", handleSettingsMutation);
  form.addEventListener("change", handleSettingsMutation);
  form.addEventListener("submit", (event) => event.preventDefault());
  form.addEventListener("click", (event) => {
    const paletteOption = event.target?.closest?.("[data-palette-value]");
    if (paletteOption) {
      setField("palette", paletteOption.dataset.paletteValue);
      scheduleAutosave();
      return;
    }

    const effectOption = event.target?.closest?.("[data-effect-value]");
    if (effectOption) {
      setField("indicator_effect_style", effectOption.dataset.effectValue);
      scheduleAutosave();
      return;
    }

    const action = event.target?.dataset?.action;
    if (action) runAction(action).catch((error) => setStatus(String(error), "error"));
  });
  form.addEventListener("keydown", (event) => {
    const option = event.target?.closest?.("[data-palette-value], [data-effect-value]");
    if (!option) return;
    const isPalette = option.dataset.paletteValue !== undefined;
    const picker = isPalette ? palettePicker : effectPicker;
    const selector = isPalette ? "[data-palette-value]" : "[data-effect-value]";
    const options = [...(picker?.querySelectorAll?.(selector) ?? [])];
    const current = options.indexOf(option);
    if (current < 0) return;

    let next = current;
    if (["ArrowRight", "ArrowDown"].includes(event.key)) next = (current + 1) % options.length;
    else if (["ArrowLeft", "ArrowUp"].includes(event.key)) {
      next = (current - 1 + options.length) % options.length;
    } else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = options.length - 1;
    else if (![" ", "Enter"].includes(event.key)) return;

    event.preventDefault();
    const selected = options[next];
    setField(
      isPalette ? "palette" : "indicator_effect_style",
      isPalette ? selected.dataset.paletteValue : selected.dataset.effectValue,
    );
    updateOutputs();
    selected.focus?.();
    scheduleAutosave();
  });
  setSettingsFormEnabled(false);
  bindSettingsUpdates();
  loadSettings();
  loadUpdateStatus();
}

updateBand?.addEventListener("click", (event) => {
  const action = event.target?.dataset?.action;
  if (action) runAction(action).catch((error) => setStatus(String(error)));
});
