import { formStateFromSettings, payloadFromEntries } from "./settings-state.js";
import { applyFont } from "./font.js";
import { applyTranslations, t } from "./i18n.js";
import { applyTheme } from "./theme.js";

const form = document.querySelector("#settings-form");
const statusEl = document.querySelector("#settings-status");
const customRow = document.querySelector("[data-custom-palette]");
let autosaveTimer = null;
let isHydrating = false;

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

function setStatus(text) {
  if (statusEl) statusEl.textContent = text;
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

function updateOutputs() {
  if (!form) return;
  const pairs = [
    ["warn_threshold", "warn-output"],
    ["danger_threshold", "danger-output"],
    ["poll_interval_secs", "poll-output"],
    ["stale_after_secs", "stale-output"],
    ["ring_size_px", "ring-size-output"],
    ["ring_thickness_px", "ring-thickness-output"],
    ["ring_gap_px", "ring-gap-output"],
    ["ring_center_gap_px", "ring-center-gap-output"],
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

  if (customRow) {
    customRow.hidden = form.elements.namedItem("palette")?.value !== "custom";
  }
}

function fillForm(settings) {
  if (!form) return;
  isHydrating = true;
  const state = formStateFromSettings(settings);

  setField("palette", state.palette);
  setField("warn_threshold", state.warnThreshold);
  setField("danger_threshold", state.dangerThreshold);
  setField("poll_interval_secs", state.pollIntervalSecs);
  setField("stale_after_secs", state.staleAfterSecs);
  setField("bar_mode", state.barMode);
  setField("limit_order", state.limitOrder);
  setField("fullscreen_hide_on", state.fullscreenHideOn);
  setField("maximized_hide_on", state.maximizedHideOn);
  setField("indicator_style", state.indicatorStyle);
  setField("ring_on", state.ringOn);
  setField("ring_numbers_on", state.ringNumbersOn);
  setField("ring_number_outline_on", state.ringNumberOutlineOn);
  setField("ring_size_px", state.ringSizePx);
  setField("ring_thickness_px", state.ringThicknessPx);
  setField("ring_gap_px", state.ringGapPx);
  setField("ring_center_gap_px", state.ringCenterGapPx);
  setField("ring_number_font_size_px", state.ringNumberFontSizePx);
  setField("ring_number_font_weight", state.ringNumberFontWeight);
  setField("bar_text_font_size_px", state.barTextFontSizePx);
  setField("bar_text_font_weight", state.barTextFontWeight);
  setField("autostart_on", state.autostartOn);
  setField("language", state.language);
  setField("theme", state.theme);
  setField("font_mode", state.fontMode);
  setField("claude_taskbar_offset_ratio", state.claudeTaskbarOffsetRatio);
  setField("codex_taskbar_offset_ratio", state.codexTaskbarOffsetRatio);
  setField("show_claude", state.showClaude);
  setField("show_codex", state.showCodex);
  setField("custom_safe", state.customSafe);
  setField("custom_warn", state.customWarn);
  setField("custom_danger", state.customDanger);
  updateOutputs();
  applyTheme({ theme: state.theme });
  applyFont({ font_mode: state.fontMode });
  applyTranslations({ language: state.language });
  isHydrating = false;
}

async function loadSettings() {
  try {
    const settings = await invoke("get_settings");
    fillForm(settings || {});
  } catch {
    fillForm({});
  }
}

async function saveSettings() {
  const input = payloadFromEntries(new FormData(form));
  const saved = await invoke("save_settings", { input });
  const next = saved || input;
  window.dispatchEvent(new CustomEvent("settings-updated", { detail: next }));
  fillForm(next);
  setStatus(t("status.saved", next));
}

function scheduleAutosave() {
  if (isHydrating) return;
  updateOutputs();
  setStatus(t("status.saving", currentLanguageSettings()));
  clearTimeout(autosaveTimer);
  autosaveTimer = setTimeout(() => {
    saveSettings().catch((error) => setStatus(String(error)));
  }, 120);
}

async function runAction(action) {
  if (action === "connect-statusline") {
    await invoke("install_statusline");
    setStatus(t("status.connected", currentLanguageSettings()));
  }

  if (action === "restore-statusline") {
    await invoke("restore_statusline");
    setStatus(t("status.restored", currentLanguageSettings()));
  }
}

async function bindSettingsUpdates() {
  const listen = tauriApi().event?.listen;
  if (!listen) return;

  try {
    await listen("settings-updated", (event) => {
      if (event.payload && typeof event.payload === "object") {
        fillForm(event.payload);
      }
    });
  } catch {
    // Form save still applies settings locally.
  }
}

if (form) {
  form.addEventListener("input", scheduleAutosave);
  form.addEventListener("change", scheduleAutosave);
  form.addEventListener("submit", (event) => event.preventDefault());
  form.addEventListener("click", (event) => {
    const action = event.target?.dataset?.action;
    if (action) runAction(action).catch((error) => setStatus(String(error)));
  });
  bindSettingsUpdates();
  loadSettings();
}
