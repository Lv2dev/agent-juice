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
const indicatorTrackCustomColorRow = document.querySelector("[data-indicator-track-custom-color]");
const taskbarTextColorRows = document.querySelectorAll?.("[data-taskbar-text-color]") ?? [];
const fullResetRow = document.querySelector("[data-full-reset-toggle]");
const palettePicker = document.querySelector("[data-palette-picker]");
const effectPicker = document.querySelector("[data-effect-picker]");
const activityScalePicker = document.querySelector("[data-activity-scale-picker]");
const activityTokenLevelRow = document.querySelector("[data-activity-token-level]");
const activityScalePreview = document.querySelector("[data-activity-scale-preview]");
const updateBand = document.querySelector("#update-band");
const updateVersionEl = document.querySelector("[data-update-version]");
const updateStatusEl = document.querySelector("#update-check-status");
const updateInstallProgressEls = [...(document.querySelectorAll?.("[data-update-install-progress]") ?? [])];
const updateProgressBars = [...(document.querySelectorAll?.("[data-update-progress-bar]") ?? [])];
const updateProgressTexts = [...(document.querySelectorAll?.("[data-update-progress-text]") ?? [])];
const updateFallbackButtons = [...(document.querySelectorAll?.("[data-update-fallback]") ?? [])];
const updateInstallButtons = [...(document.querySelectorAll?.('[data-action="install-update"]') ?? [])];
const updateActionButtons = [...(document.querySelectorAll?.('[data-action^="install-update"], [data-action="check-updates"]') ?? [])];
const appVersionEls = document.querySelectorAll?.("[data-app-version]") ?? [];
const settingsTabs = [...(document.querySelectorAll?.('[role="tab"][data-settings-tab]') ?? [])];
const settingsTabPanels = [...(document.querySelectorAll?.("[data-settings-tab-panel]") ?? [])];
let autosaveTimer = null;
let isHydrating = false;
let hasLoadedSettings = false;
let localRevision = 0;
let savedRevision = 0;
let editSession = { baseline: null, topology: [] };
let saveQueue = Promise.resolve();
let currentDisplayBasis = "remaining";
let currentUpdateStatus = null;
let updateCheckPromise = null;
let updateCheckBusy = false;
let updateInstallBusy = false;
let updateInstallPromise = null;
let toastTimer = null;
let toastHideTimer = null;
let settingsEventGeneration = 0;
let quitListenerReady = false;
let quitFlushPromise = null;
let quitAttemptGeneration = 0;
let listenerLifecycleGeneration = 0;
let listenersDisposed = false;
const activeUnlisteners = new Set();
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
  if (enabled) {
    updateIndicatorTrackColorAvailability();
    updateTaskbarTextColorAvailability();
    updateTaskbarProfileOptionAvailability();
    updateActivityScaleAvailability();
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

function updateActivityScaleAvailability() {
  if (!form) return;
  const mode = form.elements.namedItem("activity_scale_mode")?.value === "fixed"
    ? "fixed"
    : "auto";
  const tokenField = form.elements.namedItem("activity_tokens_per_level");
  const formReady = form.dataset?.loadState === "ready";
  const editable = formReady && mode === "fixed";
  if (tokenField) {
    tokenField.disabled = !formReady;
    tokenField.readOnly = !editable;
    tokenField.setAttribute?.("aria-disabled", String(!editable));
    tokenField.tabIndex = editable ? 0 : -1;
  }
  if (activityTokenLevelRow?.dataset) activityTokenLevelRow.dataset.enabled = String(editable);

  for (const option of activityScalePicker?.querySelectorAll?.("[data-activity-scale-value]") ?? []) {
    const selected = option.dataset.activityScaleValue === mode;
    option.setAttribute("aria-checked", String(selected));
    option.tabIndex = selected ? 0 : -1;
  }

  if (!activityScalePreview) return;
  if (mode === "auto") {
    activityScalePreview.textContent = t("activity.scaleAuto", currentLanguageSettings());
    return;
  }
  const unit = Math.max(1, Math.round(Number(tokenField?.value) || 250_000));
  const format = (value) => value.toLocaleString();
  activityScalePreview.textContent = `${format(unit)} · ${format(unit * 2)} · ${format(unit * 3)} · ${format(unit * 4)}+`;
}

function syncRangeFromNumberEditor(editor, commit = false) {
  const fieldName = editor?.dataset?.rangeNumberFor;
  const field = fieldName ? form?.elements?.namedItem(fieldName) : null;
  const rawValue = String(editor?.value ?? "").trim();
  const value = Number(rawValue);
  if (!field) return false;
  if (!rawValue || !Number.isFinite(value)) {
    if (commit) editor.value = field.value;
    return false;
  }

  const min = Number(field.min);
  const max = Number(field.max);
  if (!commit && ((Number.isFinite(min) && value < min) || (Number.isFinite(max) && value > max))) {
    return false;
  }

  const clamped = Math.min(
    Number.isFinite(max) ? max : value,
    Math.max(Number.isFinite(min) ? min : value, value),
  );
  field.value = String(clamped);
  if (commit) editor.value = field.value;
  return true;
}

function syncRangeNumberEditors() {
  for (const editor of form?.querySelectorAll?.("[data-range-number-for]") ?? []) {
    const field = form.elements.namedItem(editor.dataset.rangeNumberFor);
    if (!field) continue;
    editor.value = field.value;
  }
}

function updateIndicatorTrackColorAvailability() {
  const autoColor = form?.elements.namedItem("indicator_track_color_auto");
  const customColor = form?.elements.namedItem("indicator_track_color");
  if (!customColor) return;
  const unavailable = autoColor?.checked !== false;
  customColor.setAttribute?.("aria-disabled", String(unavailable));
  customColor.ariaDisabled = String(unavailable);
  customColor.inert = unavailable;
  customColor.tabIndex = unavailable ? -1 : 0;
  if (indicatorTrackCustomColorRow?.dataset) {
    indicatorTrackCustomColorRow.dataset.disabled = String(unavailable);
  }
}

function updateTaskbarTextColorAvailability() {
  for (const row of taskbarTextColorRows) {
    const key = row.dataset?.taskbarTextColor;
    if (!key) continue;
    const enabled = form?.elements.namedItem(`${key}_text_color_on`)?.checked === true;
    const color = form?.elements.namedItem(`${key}_text_color`);
    if (!color) continue;
    color.setAttribute?.("aria-disabled", String(!enabled));
    color.ariaDisabled = String(!enabled);
    color.inert = !enabled;
    color.tabIndex = enabled ? 0 : -1;
    if (row.dataset) row.dataset.disabled = String(!enabled);
  }
}

function updateTaskbarProfileOptionAvailability() {
  if (!form) return;
  const enabled = form.dataset?.loadState === "ready"
    && form.elements.namedItem("taskbar_layout_memory_on")?.checked === true;
  for (const name of ["taskbar_profile_presentation_on", "taskbar_profile_colors_on"]) {
    const control = form.elements.namedItem(name);
    const row = control?.closest?.(".toggle-row");
    if (!control || !row) continue;
    control.setAttribute?.("aria-disabled", String(!enabled));
    control.tabIndex = enabled ? 0 : -1;
    row.inert = !enabled;
    if (row.dataset) row.dataset.disabled = String(!enabled);
  }
}

function selectSettingsTab(value, focus = false) {
  const selected = settingsTabs.find((tab) => tab.dataset?.settingsTab === value);
  if (!selected) return;

  for (const tab of settingsTabs) {
    const active = tab === selected;
    tab.setAttribute?.("aria-selected", String(active));
    tab.tabIndex = active ? 0 : -1;
  }
  for (const panel of settingsTabPanels) {
    panel.hidden = panel.dataset?.settingsTabPanel !== value;
  }
  if (form?.dataset) form.dataset.settingsTab = value;
  if (focus) selected.focus?.();
}

function updateOutputs() {
  if (!form) return;
  const pairs = [
    ["warn_threshold", "warn-output"],
    ["danger_threshold", "danger-output"],
    ["poll_interval_secs", "poll-output"],
    ["stale_after_secs", "stale-output"],
    ["indicator_track_opacity_percent", "indicator-track-opacity-output"],
    ["ring_number_outline_width_px", "ring-number-outline-width-output"],
    ["ring_size_px", "ring-size-output"],
    ["ring_thickness_px", "ring-thickness-output"],
    ["ring_gap_px", "ring-gap-output"],
    ["ring_center_size_px", "ring-center-size-output"],
    ["ring_number_font_size_px", "ring-number-font-size-output"],
    ["ring_number_font_weight", "ring-number-font-weight-output"],
    ["bar_text_font_size_px", "bar-text-font-size-output"],
    ["bar_text_font_weight", "bar-text-font-weight-output"],
    ["bar_content_gap_px", "bar-content-gap-output"],
  ];

  for (const [fieldName, outputName] of pairs) {
    const field = form.elements.namedItem(fieldName);
    const output = form.elements.namedItem(outputName);
    if (field && output) output.value = field.value;
    updateRangeProgress(field);
  }
  syncRangeNumberEditors();
  updateIndicatorTrackColorAvailability();
  updateTaskbarTextColorAvailability();
  updateTaskbarProfileOptionAvailability();
  updateActivityScaleAvailability();

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
  setField("activity_weeks", state.activityWeeks);
  setField("activity_scale_mode", state.activityScaleMode);
  setField("activity_tokens_per_level", state.activityTokensPerLevel);
  setField("bar_mode", state.barMode);
  setField("full_reset_time_on", state.fullResetTimeOn);
  setField("limit_order", state.limitOrder);
  setField("fullscreen_hide_on", state.fullscreenHideOn);
  setField("maximized_hide_on", state.maximizedHideOn);
  setField("taskbar_avoid_overlap_on", state.taskbarAvoidOverlapOn);
  setField("taskbar_layout_memory_on", state.taskbarLayoutMemoryOn);
  setField("taskbar_profile_presentation_on", state.taskbarProfilePresentationOn);
  setField("taskbar_profile_colors_on", state.taskbarProfileColorsOn);
  setField("indicator_style", state.indicatorStyle);
  setField("indicator_effect_style", state.indicatorEffectStyle);
  setField("indicator_track_color_auto", state.indicatorTrackColorAuto);
  setField("indicator_track_color", state.indicatorTrackColor);
  setField("indicator_track_opacity_percent", state.indicatorTrackOpacityPercent);
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
  setField("bar_content_gap_px", state.barContentGapPx);
  setField("autostart_on", state.autostartOn);
  setField("update_check_on", state.updateCheckOn);
  setField("language", state.language);
  setField("theme", state.theme);
  setField("font_mode", state.fontMode);
  setField("claude_taskbar_offset_ratio", state.claudeTaskbarOffsetRatio);
  setField("codex_taskbar_offset_ratio", state.codexTaskbarOffsetRatio);
  setField("grok_taskbar_offset_ratio", state.grokTaskbarOffsetRatio);
  setField("cursor_taskbar_offset_ratio", state.cursorTaskbarOffsetRatio);
  setField("show_claude", state.showClaude);
  setField("show_codex", state.showCodex);
  setField("show_grok", state.showGrok);
  setField("show_cursor", state.showCursor);
  setField("claude_account_auto_collect_on", state.claudeAccountAutoCollectOn);
  setField("mono_color", state.monoColor);
  setField("custom_safe", state.customSafe);
  setField("custom_warn", state.customWarn);
  setField("custom_danger", state.customDanger);
  setField("claude_primary_color", state.claudePrimaryColor);
  setField("claude_secondary_color", state.claudeSecondaryColor);
  setField("codex_primary_color", state.codexPrimaryColor);
  setField("codex_secondary_color", state.codexSecondaryColor);
  setField("grok_primary_color", state.grokPrimaryColor);
  setField("grok_secondary_color", state.grokSecondaryColor);
  setField("cursor_primary_color", state.cursorPrimaryColor);
  setField("cursor_secondary_color", state.cursorSecondaryColor);
  setField("tool_warning_color", state.toolWarningColor);
  setField("tool_danger_color", state.toolDangerColor);
  setField("tool_warning_color_on", state.toolWarningColorOn);
  setField("tool_danger_color_on", state.toolDangerColorOn);
  setField("claude_text_color", state.claudeTextColor);
  setField("claude_text_color_on", state.claudeTextColorOn);
  setField("codex_text_color", state.codexTextColor);
  setField("codex_text_color_on", state.codexTextColorOn);
  setField("grok_text_color", state.grokTextColor);
  setField("grok_text_color_on", state.grokTextColorOn);
  setField("cursor_text_color", state.cursorTextColor);
  setField("cursor_text_color_on", state.cursorTextColorOn);
  setField("info_text_color", state.infoTextColor);
  setField("info_text_color_on", state.infoTextColorOn);
  setField("ring_text_color", state.ringTextColor);
  setField("ring_text_color_on", state.ringTextColorOn);
  applyTheme({ theme: state.theme });
  applyFont({ font_mode: state.fontMode });
  applyTranslations({ language: state.language });
  updateOutputs();
  renderUpdateStatus(currentUpdateStatus);
  editSession = {
    baseline: payloadFromEntries(new FormData(form)),
    topology: [...(settings?.taskbar_layout_profiles?.at(-1)?.monitor_keys ?? [])],
  };
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
  for (const button of updateInstallButtons) button.hidden = !updateAvailable;
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

function syncUpdateBusyState() {
  const busy = updateCheckBusy || updateInstallBusy;
  for (const button of updateActionButtons) button.disabled = busy;
  if (updateBand) updateBand.setAttribute?.("aria-busy", String(busy));
  if (form) {
    form.inert = updateInstallBusy;
    form.setAttribute?.("aria-disabled", String(updateInstallBusy));
  }
}

function setUpdateCheckBusy(busy) {
  updateCheckBusy = busy;
  syncUpdateBusyState();
}

function setUpdateInstallBusy(busy) {
  updateInstallBusy = busy;
  syncUpdateBusyState();
}

function setUpdateFallbackVisible(visible) {
  for (const button of updateFallbackButtons) button.hidden = !visible;
}

function showUpdateProgress(percent, text, state = "progress") {
  const determinate = percent !== null && percent !== undefined && Number.isFinite(Number(percent));
  const value = determinate ? Math.max(0, Math.min(100, Math.round(Number(percent)))) : 0;
  for (const progress of updateInstallProgressEls) {
    progress.hidden = false;
    progress.dataset.state = state;
    progress.setAttribute?.("aria-valuetext", text);
    if (determinate) progress.setAttribute?.("aria-valuenow", String(value));
    else progress.removeAttribute?.("aria-valuenow");
  }
  for (const bar of updateProgressBars) bar.style.width = determinate ? `${value}%` : "35%";
  for (const progressText of updateProgressTexts) progressText.textContent = text;
}

function renderUpdateInstallEvent(event) {
  const language = currentLanguageSettings();
  if (event?.event === "progress") {
    const downloaded = Number(event.downloaded_bytes) || 0;
    const total = Number(event.content_length) || 0;
    const percent = total > 0 ? (downloaded / total) * 100 : null;
    const progress = total > 0
      ? t("status.updateProgress", language).replace("{percent}", String(Math.round(percent)))
      : t("status.updateDownloading", language);
    showUpdateProgress(percent, progress, total > 0 ? "downloading" : "indeterminate");
    if (updateStatusEl) updateStatusEl.dataset.state = "downloading";
    return;
  }

  const key = event?.event === "installing"
    ? "status.updateInstalling"
    : event?.event === "verifying"
      ? "status.updateVerifying"
    : event?.event === "started"
      ? "status.updateDownloading"
      : "status.updatePreparing";
  if (updateStatusEl) {
    updateStatusEl.textContent = t(key, language);
    updateStatusEl.dataset.state = event?.event ?? "preparing";
  }
  const percent = event?.event === "installing" ? 100 : event?.event === "verifying" ? null : 0;
  showUpdateProgress(percent, t(key, language), event?.event ?? "preparing");
}

async function installAvailableUpdate() {
  if (updateInstallPromise) return updateInstallPromise;
  const version = currentUpdateStatus?.latest_version;
  const Channel = tauriApi().core?.Channel;
  if (!version || typeof Channel !== "function") {
    const message = t("status.updateInstallFailed", currentLanguageSettings());
    if (updateStatusEl) {
      updateStatusEl.textContent = message;
      updateStatusEl.dataset.state = "error";
    }
    showUpdateProgress(0, message, "error");
    setUpdateFallbackVisible(true);
    return;
  }

  const onEvent = new Channel();
  onEvent.onmessage = renderUpdateInstallEvent;
  setUpdateInstallBusy(true);
  setUpdateFallbackVisible(false);
  renderUpdateInstallEvent({ event: "preparing" });
  let stage = "saving";
  let handoffAccepted = false;
  const attempt = (async () => {
    await enqueueLatestSettingsSave({ failOnRollback: true });
    stage = "installing";
    await invoke("install_update", { expectedVersion: version, onEvent });
    handoffAccepted = true;
  })();
  updateInstallPromise = attempt;
  try {
    await attempt;
  } catch {
    const key = stage === "saving" ? "status.updateSettingsSaveFailed" : "status.updateInstallFailed";
    const message = t(key, currentLanguageSettings());
    if (updateStatusEl) {
      updateStatusEl.textContent = message;
      updateStatusEl.dataset.state = "error";
    }
    showUpdateProgress(0, message, "error");
    setUpdateFallbackVisible(true);
  } finally {
    if (updateInstallPromise === attempt) updateInstallPromise = null;
    if (!handoffAccepted) setUpdateInstallBusy(false);
  }
}

async function loadUpdateStatus() {
  try {
    renderUpdateStatus(await invoke("get_update_status"));
  } catch {
    renderUpdateStatus(null);
  }
}

async function checkForUpdates() {
  if (updateCheckPromise) return updateCheckPromise;

  setUpdateCheckBusy(true);
  if (updateStatusEl) {
    updateStatusEl.textContent = t("status.updateChecking", currentLanguageSettings());
    updateStatusEl.dataset.state = "checking";
  }

  const attempt = (async () => {
    try {
      renderUpdateStatus(await invoke("check_for_updates"));
    } catch {
      renderUpdateStatus({ status: "error" });
    }
    if (updateStatusEl?.textContent) showSettingsToast(updateStatusEl.textContent);
  })();
  updateCheckPromise = attempt;
  try {
    await attempt;
  } finally {
    if (updateCheckPromise === attempt) updateCheckPromise = null;
    setUpdateCheckBusy(false);
  }
}

async function saveSettings(input, revision, edit) {
  const response = await invoke("save_settings", { input, editBaseline: edit.baseline, editTopology: edit.topology });
  edit.baseline = input;
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

async function rollbackFailedSave(revision, error) {
  if (revision !== localRevision) return;
  let recovered = false;
  try {
    const persisted = await invoke("get_settings");
    if (revision !== localRevision) return;
    if (!persisted || typeof persisted !== "object") throw new Error("invalid settings payload");
    fillForm(persisted);
    savedRevision = revision;
    recovered = true;
  } catch {
    // Keep the original save error when the recovery read also fails.
  }
  if (revision === localRevision) setStatus(String(error), "error");
  if (!recovered) throw error;
}

function enqueueLatestSettingsSave({ failOnRollback = false } = {}) {
  clearTimeout(autosaveTimer);
  autosaveTimer = null;
  if (!hasLoadedSettings || localRevision <= savedRevision) return saveQueue;

  const revision = localRevision;
  const input = payloadFromEntries(new FormData(form));
  const edit = editSession;
  saveQueue = saveQueue
    .catch(() => {})
    .then(() => saveSettings(input, revision, edit))
    .catch(async (error) => {
      await rollbackFailedSave(revision, error);
      if (failOnRollback) throw error;
    });
  return saveQueue;
}

async function flushSettingsAndQuit() {
  if (quitFlushPromise) return quitFlushPromise;
  const generation = ++quitAttemptGeneration;
  const attempt = (async () => {
    await enqueueLatestSettingsSave();
    await invoke("complete_app_quit");
  })();
  quitFlushPromise = attempt;
  try {
    return await attempt;
  } finally {
    if (quitAttemptGeneration === generation && quitFlushPromise === attempt) {
      quitFlushPromise = null;
    }
  }
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
  if (event?.target?.dataset?.rangeNumberFor) {
    if (!syncRangeFromNumberEditor(event.target, event.type === "change")) return;
  }
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
  if (action === "clear-taskbar-layouts") {
    const actionRevision = localRevision;
    const actionEventGeneration = settingsEventGeneration;
    await enqueueLatestSettingsSave();
    const settings = await invoke("clear_taskbar_layout_profiles");
    if (
      settings &&
      typeof settings === "object" &&
      localRevision === actionRevision &&
      settingsEventGeneration === actionEventGeneration
    ) {
      settingsEventGeneration += 1;
      hydrateSettings(settings);
    }
    showSettingsToast(t("status.taskbarLayoutsCleared", currentLanguageSettings()));
    return;
  }
  if (action === "check-updates") {
    await checkForUpdates();
    return;
  }
  if (action === "install-update") {
    await installAvailableUpdate();
    return;
  }
  if (action === "open-releases") {
    await invoke("open_release_page", { url: null });
    return;
  }
}

function actionFromEvent(event) {
  return event.target?.closest?.("[data-action]")?.dataset?.action
    ?? event.target?.dataset?.action
    ?? null;
}

function dispatchAction(event) {
  const action = actionFromEvent(event);
  if (action) runAction(action).catch((error) => setStatus(String(error), "error"));
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
  quitListenerReady = false;
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

async function bindSettingsUpdates() {
  const listen = tauriApi().event?.listen;
  if (!listen) return;

  const register = async (eventName, handler, critical = false) => {
    for (const delay of LISTENER_RETRY_DELAYS_MS) {
      if (delay) await wait(delay);
      if (listenersDisposed) return;
      try {
        const unlisten = await registerListenerAttempt(listen, eventName, handler);
        if (typeof unlisten === "function") activeUnlisteners.add(unlisten);
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
  void register("app-quit-cancelled", () => {
    quitAttemptGeneration += 1;
    quitFlushPromise = null;
  });
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

window.addEventListener?.("pagehide", cleanupListeners);
window.addEventListener?.("beforeunload", cleanupListeners);

if (form) {
  form.addEventListener("input", handleSettingsMutation);
  form.addEventListener("change", handleSettingsMutation);
  form.addEventListener("submit", (event) => event.preventDefault());
  form.addEventListener("click", (event) => {
    const settingsTab = event.target?.closest?.('[role="tab"][data-settings-tab]');
    if (settingsTab) {
      selectSettingsTab(settingsTab.dataset.settingsTab);
      return;
    }

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

    const activityScaleOption = event.target?.closest?.("[data-activity-scale-value]");
    if (activityScaleOption) {
      setField("activity_scale_mode", activityScaleOption.dataset.activityScaleValue);
      updateOutputs();
      scheduleAutosave();
      return;
    }

    dispatchAction(event);
  });
  form.addEventListener("keydown", (event) => {
    const settingsTab = event.target?.closest?.('[role="tab"][data-settings-tab]');
    if (settingsTab) {
      const current = settingsTabs.indexOf(settingsTab);
      if (current < 0) return;

      let next = current;
      if (event.key === "ArrowRight") next = (current + 1) % settingsTabs.length;
      else if (event.key === "ArrowLeft") next = (current - 1 + settingsTabs.length) % settingsTabs.length;
      else if (event.key === "Home") next = 0;
      else if (event.key === "End") next = settingsTabs.length - 1;
      else if (![" ", "Enter"].includes(event.key)) return;

      event.preventDefault();
      selectSettingsTab(settingsTabs[next].dataset.settingsTab, true);
      return;
    }

    const activityScaleOption = event.target?.closest?.("[data-activity-scale-value]");
    if (activityScaleOption) {
      setField("activity_scale_mode", activityScaleOption.dataset.activityScaleValue);
      updateOutputs();
      scheduleAutosave();
      return;
    }

    const option = event.target?.closest?.(
      "[data-palette-value], [data-effect-value], [data-activity-scale-value]",
    );
    if (!option) return;
    const isPalette = option.dataset.paletteValue !== undefined;
    const isEffect = option.dataset.effectValue !== undefined;
    const picker = isPalette ? palettePicker : isEffect ? effectPicker : activityScalePicker;
    const selector = isPalette
      ? "[data-palette-value]"
      : isEffect
        ? "[data-effect-value]"
        : "[data-activity-scale-value]";
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
      isPalette ? "palette" : isEffect ? "indicator_effect_style" : "activity_scale_mode",
      isPalette
        ? selected.dataset.paletteValue
        : isEffect
          ? selected.dataset.effectValue
          : selected.dataset.activityScaleValue,
    );
    updateOutputs();
    selected.focus?.();
    scheduleAutosave();
  });
  selectSettingsTab("general");
  setSettingsFormEnabled(false);
  bindSettingsUpdates();
  loadSettings();
  loadUpdateStatus();
}

updateBand?.addEventListener("click", dispatchAction);
