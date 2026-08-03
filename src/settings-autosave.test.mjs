import assert from "node:assert/strict";
import test from "node:test";

function makeStyle() {
  const values = new Map();
  return {
    setProperty(name, value) {
      values.set(name, value);
    },
    getPropertyValue(name) {
      return values.get(name) ?? "";
    },
  };
}

function makeField(value, type = "text", options = {}) {
  return {
    type,
    value,
    checked: Boolean(value),
    disabled: false,
    style: makeStyle(),
    ...options,
  };
}

test("settings form auto-saves changed values without a submit button", async () => {
  const listeners = {};
  const windowListeners = {};
  const dispatched = [];
  const savedInputs = [];
  const pendingSaveResponses = [];
  let pendingClearResponse = null;
  let deferSaveResponses = false;
  let deferClearResponse = false;
  let failNextSave = false;
  let installUpdateCalls = 0;
  let resolveInstallUpdate = null;
  let installEventChannel = null;
  let persistedSettings = { maximized_hide_on: true, language: "ko" };
  const eventHandlers = {};
  const listenerOrder = [];
  const listenerAttempts = new Map();
  let unlistenCalls = 0;
  const invokedCommands = [];
  const statusHost = { dataset: {} };
  const statusEl = {
    textContent: "",
    closest(selector) {
      assert.equal(selector, "[data-settings-save-state]");
      return statusHost;
    },
  };
  const toastLayer = { dataset: {}, hidden: true };
  const toastText = { textContent: "" };
  const customRow = { hidden: true };
  const toolColorRow = { hidden: true };
  const indicatorTrackColorRow = { dataset: {} };
  const fullResetRow = { hidden: false };
  const updateStatusEl = { textContent: "", dataset: {} };
  const updateBand = {
    hidden: true,
    attributes: {},
    addEventListener() {},
    setAttribute(name, value) {
      this.attributes[name] = value;
    },
  };
  const updateVersionEl = { textContent: "" };
  const updateInstallButtons = [
    { hidden: true, disabled: false },
    { hidden: true, disabled: false },
  ];
  const updateCheckButton = { disabled: false };
  const updateInstallProgress = {
    hidden: true,
    dataset: {},
    attributes: {},
    setAttribute(name, value) {
      this.attributes[name] = value;
    },
    removeAttribute(name) {
      delete this.attributes[name];
    },
  };
  const updateBandProgress = {
    hidden: true,
    dataset: {},
    attributes: {},
    setAttribute(name, value) {
      this.attributes[name] = value;
    },
    removeAttribute(name) {
      delete this.attributes[name];
    },
  };
  const updateProgressBar = { style: {} };
  const updateBandProgressBar = { style: {} };
  const updateProgressText = { textContent: "" };
  const updateBandProgressText = { textContent: "" };
  const updateFallbackButton = { hidden: true };
  const effectOptions = ["flat", "soft", "depth", "glow", "breathe"].map((value) => ({
    dataset: { effectValue: value },
    setAttribute(name, next) {
      this[name] = next;
    },
    closest(selector) {
      return selector.includes("[data-effect-value]") ? this : null;
    },
  }));
  const effectPicker = {
    querySelectorAll() {
      return effectOptions;
    },
  };
  let focusedSettingsTab = null;
  const settingsTabs = ["general", "collection", "taskbar", "colors", "details"].map((value) => ({
    dataset: { settingsTab: value },
    tabIndex: -1,
    setAttribute(name, next) {
      this[name] = next;
    },
    closest(selector) {
      return selector === "[data-settings-tab]" ? this : null;
    },
    focus() {
      focusedSettingsTab = this;
    },
  }));
  const settingsTabPanels = settingsTabs.map((tab) => ({
    dataset: { settingsTabPanel: tab.dataset.settingsTab },
    hidden: true,
  }));
  const fields = {
    palette: makeField("traffic"),
    display_basis: makeField("remaining", "text", { name: "display_basis" }),
    warn_threshold: makeField("30", "range", { min: "0", max: "100" }),
    "warn-output": makeField("30"),
    danger_threshold: makeField("10", "range", { min: "0", max: "100" }),
    "danger-output": makeField("10"),
    poll_interval_secs: makeField("2"),
    "poll-output": makeField("2"),
    stale_after_secs: makeField("90"),
    "stale-output": makeField("90"),
    bar_mode: makeField("full"),
    full_reset_time_on: makeField(false, "checkbox"),
    limit_order: makeField("primary_first"),
    fullscreen_hide_on: makeField(false, "checkbox"),
    maximized_hide_on: makeField(true, "checkbox"),
    taskbar_avoid_overlap_on: makeField(true, "checkbox"),
    taskbar_layout_memory_on: makeField(true, "checkbox"),
    indicator_style: makeField("ring"),
    indicator_effect_style: makeField("flat"),
    indicator_track_color_auto: makeField(true, "checkbox"),
    indicator_track_color: makeField("#6b7280", "color"),
    indicator_track_opacity_percent: makeField("11", "range", { min: "0", max: "100" }),
    "indicator-track-opacity-output": makeField("11"),
    ring_on: makeField(true, "checkbox"),
    ring_numbers_on: makeField(true, "checkbox"),
    ring_number_outline_on: makeField(true, "checkbox"),
    ring_number_outline_width_px: makeField("1.2", "range", { min: "0", max: "4" }),
    "ring-number-outline-width-output": makeField("1.2"),
    ring_size_px: makeField("36", "range", { min: "20", max: "44" }),
    "ring-size-output": makeField("36"),
    ring_thickness_px: makeField("4", "range", { min: "1", max: "10" }),
    "ring-thickness-output": makeField("4"),
    ring_gap_px: makeField("6", "range", { min: "2", max: "14" }),
    "ring-gap-output": makeField("6"),
    ring_center_size_px: makeField("16", "range", { min: "4", max: "32" }),
    "ring-center-size-output": makeField("16"),
    ring_number_font_size_px: makeField("9", "range", { min: "6", max: "16" }),
    "ring-number-font-size-output": makeField("9"),
    ring_number_font_weight: makeField("600", "range", { min: "100", max: "900" }),
    "ring-number-font-weight-output": makeField("600"),
    bar_text_font_size_px: makeField("11", "range", { min: "8", max: "16" }),
    "bar-text-font-size-output": makeField("11"),
    bar_text_font_weight: makeField("500", "range", { min: "100", max: "900" }),
    bar_content_gap_px: makeField("4", "range", { min: "0", max: "24" }),
    "bar-text-font-weight-output": makeField("500"),
    autostart_on: makeField(true, "checkbox"),
    update_check_on: makeField(true, "checkbox"),
    language: makeField("ko"),
    theme: makeField("system"),
    font_mode: makeField("system"),
    claude_taskbar_offset_ratio: makeField("0.5"),
    codex_taskbar_offset_ratio: makeField("0.5"),
    show_claude: makeField(true, "checkbox"),
    show_codex: makeField(true, "checkbox"),
    claude_account_auto_collect_on: makeField(true, "checkbox"),
    mono_color: makeField("#4f8a73", "color"),
    custom_safe: makeField("#22c55e"),
    custom_warn: makeField("#f59e0b"),
    custom_danger: makeField("#ef4444"),
    claude_primary_color: makeField("#d79a32", "color"),
    claude_secondary_color: makeField("#d36b86", "color"),
    codex_primary_color: makeField("#2fac7d", "color"),
    codex_secondary_color: makeField("#4d86d6", "color"),
    tool_warning_color: makeField("#f59e0b", "color"),
    tool_danger_color: makeField("#ef4444", "color"),
    tool_warning_color_on: makeField(true, "checkbox"),
    tool_danger_color_on: makeField(true, "checkbox"),
  };
  const ringSizeEditor = makeField("36", "number", {
    dataset: { rangeNumberFor: "ring_size_px" },
  });
  const trackOpacityEditor = makeField("11", "number", {
    dataset: { rangeNumberFor: "indicator_track_opacity_percent" },
  });
  const form = {
    elements: {
      namedItem(name) {
        return fields[name] ?? null;
      },
    },
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
    querySelectorAll(selector) {
      return selector === "[data-range-number-for]" ? [ringSizeEditor, trackOpacityEditor] : [];
    },
  };

  global.FormData = class {
    get(name) {
      const field = fields[name];
      if (!field) return null;
      if (field.type === "checkbox") return field.checked ? "on" : null;
      return field.value;
    }
  };
  global.window = {
    addEventListener(name, handler) {
      windowListeners[name] = handler;
    },
    dispatchEvent(event) {
      dispatched.push(event);
    },
    __TAURI__: {
      core: {
        Channel: class {
          onmessage = null;
        },
        async invoke(command, args) {
          invokedCommands.push(command);
          if (command === "get_settings") return persistedSettings;
          if (command === "get_update_status") {
            return {
              status: "update_available",
              current_version: "0.1.11",
              latest_version: "0.1.12",
              release_url: "https://github.com/Lv2dev/agent-juice/releases/tag/v0.1.12",
            };
          }
          if (command === "clear_taskbar_layout_profiles") {
            const cleared = { ...persistedSettings, taskbar_layout_profiles: [] };
            if (deferClearResponse) {
              return new Promise((resolve) => {
                pendingClearResponse = () => resolve(cleared);
              });
            }
            return cleared;
          }
          if (command === "save_settings") {
            savedInputs.push(args.input);
            if (failNextSave) {
              failNextSave = false;
              throw new Error("Claude statusline conflict");
            }
            if (deferSaveResponses) {
              return new Promise((resolve) => {
                pendingSaveResponses.push({ input: args.input, resolve });
              });
            }
            persistedSettings = args.input;
            return { settings: args.input, warnings: ["taskbar retry"] };
          }
          if (command === "check_for_updates") throw new Error("offline");
          if (command === "install_update") {
            installUpdateCalls += 1;
            installEventChannel = args.onEvent;
            args.onEvent.onmessage?.({ event: "started", version: "0.1.12" });
            args.onEvent.onmessage?.({
              event: "progress",
              downloaded_bytes: 50,
              content_length: 100,
            });
            return new Promise((resolve) => {
              resolveInstallUpdate = resolve;
            });
          }
          return null;
        },
      },
      event: {
        async listen(name, handler) {
          listenerOrder.push(name);
          const attempts = (listenerAttempts.get(name) ?? 0) + 1;
          listenerAttempts.set(name, attempts);
          if (name === "settings-updated" && attempts === 1) throw new Error("transient");
          if (name === "update-status") throw new Error("unavailable");
          eventHandlers[name] = handler;
          return () => {
            unlistenCalls += 1;
          };
        },
      },
    },
  };
  global.document = {
    documentElement: {
      dataset: {},
      removeAttribute() {},
    },
    querySelector(selector) {
      if (selector === "#settings-form") return form;
      if (selector === "#settings-status") return statusEl;
      if (selector === "[data-settings-toast]") return toastLayer;
      if (selector === "[data-settings-toast-text]") return toastText;
      if (selector === "[data-custom-palette]") return customRow;
      if (selector === "[data-tool-palette]") return toolColorRow;
      if (selector === "[data-indicator-track-custom-color]") return indicatorTrackColorRow;
      if (selector === "[data-full-reset-toggle]") return fullResetRow;
      if (selector === "[data-effect-picker]") return effectPicker;
      if (selector === "#update-check-status") return updateStatusEl;
      if (selector === "#update-band") return updateBand;
      if (selector === "[data-update-version]") return updateVersionEl;
      return null;
    },
    querySelectorAll(selector) {
      if (selector === "[data-settings-tab]") return settingsTabs;
      if (selector === "[data-settings-tab-panel]") return settingsTabPanels;
      if (selector === '[data-action="install-update"]') return updateInstallButtons;
      if (selector === "[data-update-install-progress]") {
        return [updateBandProgress, updateInstallProgress];
      }
      if (selector === "[data-update-progress-bar]") {
        return [updateBandProgressBar, updateProgressBar];
      }
      if (selector === "[data-update-progress-text]") {
        return [updateBandProgressText, updateProgressText];
      }
      if (selector === "[data-update-fallback]") return [updateFallbackButton];
      if (selector === '[data-action^="install-update"], [data-action="check-updates"]') {
        return [...updateInstallButtons, updateCheckButton];
      }
      return [];
    },
  };

  await import(`./settings.js?test=${Date.now()}-autosave`);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(listenerOrder[0], "app-quit-requested");

  assert.equal(statusHost.hidden, true, "initial hydration must not show a completion state");
  assert.equal(toastLayer.hidden, true, "initial hydration must not show a completion toast");
  assert.equal(fields.warn_threshold.style.getPropertyValue("--range-progress"), "30%");
  assert.equal(fields.ring_size_px.style.getPropertyValue("--range-progress"), "66.7%");
  assert.equal(fields.indicator_track_opacity_percent.style.getPropertyValue("--range-progress"), "11%");
  assert.equal(fields.indicator_track_color.disabled, false);
  assert.equal(fields.indicator_track_color.ariaDisabled, "true");
  assert.equal(fields.indicator_track_color.inert, true);
  assert.equal(fields.indicator_track_color.tabIndex, -1);
  assert.equal(indicatorTrackColorRow.dataset.disabled, "true");
  assert.equal(fullResetRow.hidden, false);
  assert.equal(settingsTabs[0]["aria-selected"], "true");
  assert.equal(settingsTabs[0].tabIndex, 0);
  assert.equal(settingsTabPanels[0].hidden, false);
  assert.ok(settingsTabPanels.slice(1).every((panel) => panel.hidden));
  assert.equal(updateBand.hidden, false);
  assert.ok(updateInstallButtons.every((button) => button.hidden === false));

  listeners.click?.({ target: { dataset: { action: "install-update" } } });
  listeners.click?.({ target: { dataset: { action: "install-update" } } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(installUpdateCalls, 1, "duplicate update clicks must share one install operation");
  assert.ok(updateInstallButtons.every((button) => button.disabled));
  assert.equal(form.inert, true, "settings editing must stay locked during updater handoff");
  assert.equal(updateInstallProgress.hidden, false);
  assert.equal(updateBandProgress.hidden, false);
  assert.equal(updateInstallProgress.attributes["aria-valuenow"], "50");
  assert.equal(updateBandProgress.attributes["aria-valuenow"], "50");
  assert.equal(updateProgressBar.style.width, "50%");
  assert.equal(updateBandProgressBar.style.width, "50%");
  assert.match(updateProgressText.textContent, /50%/);
  assert.match(updateBandProgressText.textContent, /50%/);
  installEventChannel.onmessage?.({
    event: "progress",
    downloaded_bytes: 75,
    content_length: null,
  });
  assert.equal(updateInstallProgress.attributes["aria-valuenow"], undefined);
  assert.equal(updateProgressBar.style.width, "35%");
  installEventChannel.onmessage?.({ event: "verifying" });
  assert.match(updateProgressText.textContent, /서명을 확인/);
  assert.equal(updateInstallProgress.attributes["aria-valuetext"], updateProgressText.textContent);
  resolveInstallUpdate();
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(updateInstallButtons.every((button) => button.disabled));
  assert.equal(form.inert, true, "a successful handoff stays locked until the app exits");

  listeners.click?.({ target: settingsTabs[3] });
  assert.equal(settingsTabs[3]["aria-selected"], "true");
  assert.equal(settingsTabs[3].tabIndex, 0);
  assert.equal(settingsTabPanels[3].hidden, false);
  assert.equal(savedInputs.length, 0, "tab navigation must not save settings");

  let preventedTabKey = false;
  listeners.keydown?.({
    target: settingsTabs[3],
    key: "End",
    preventDefault() {
      preventedTabKey = true;
    },
  });
  assert.equal(preventedTabKey, true);
  assert.equal(settingsTabs[4]["aria-selected"], "true");
  assert.equal(settingsTabPanels[4].hidden, false);
  assert.equal(focusedSettingsTab, settingsTabs[4]);
  assert.equal(savedInputs.length, 0, "keyboard tab navigation must not save settings");

  fields.bar_mode.value = "dual";
  fields.claude_primary_color.value = "#123456";
  fields.tool_warning_color.value = "#654321";
  fields.tool_danger_color_on.checked = false;
  fields.indicator_track_color_auto.checked = false;
  fields.indicator_track_color.value = "#123456";
  trackOpacityEditor.value = "37";
  listeners.input?.({ type: "input", target: trackOpacityEditor });
  assert.equal(fields.indicator_track_opacity_percent.value, "37");
  assert.equal(fields.indicator_track_color.disabled, false);
  assert.equal(fields.indicator_track_color.ariaDisabled, "false");
  assert.equal(fields.indicator_track_color.inert, false);
  assert.equal(fields.indicator_track_color.tabIndex, 0);
  fields.display_basis.value = "used";
  listeners.input?.({ target: fields.display_basis });
  ringSizeEditor.value = "";
  listeners.change?.({ type: "change", target: ringSizeEditor });
  assert.equal(ringSizeEditor.value, "36");
  ringSizeEditor.value = "40.5";
  listeners.input?.({ type: "input", target: ringSizeEditor });
  assert.equal(fields.ring_size_px.value, "40.5");
  fields.theme.value = "dark";
  listeners.input?.({ target: { ...fields.theme, name: "theme" } });
  listeners.click?.({ target: effectOptions[2] });
  assert.equal(global.document.documentElement.dataset.theme, "dark");
  assert.equal(fields.indicator_effect_style.value, "depth");
  assert.equal(effectOptions[2]["aria-checked"], "true");
  assert.equal(effectOptions[0]["aria-checked"], "false");
  assert.equal(fields.warn_threshold.value, "70");
  assert.equal(fields.danger_threshold.value, "90");
  await new Promise((resolve) => setTimeout(resolve, 180));

  assert.equal(savedInputs.length, 1);
  assert.equal(savedInputs[0].bar_mode, "dual");
  assert.equal(savedInputs[0].full_reset_time_on, true);
  assert.equal(savedInputs[0].display_basis, "used");
  assert.equal(savedInputs[0].warn_threshold, 70);
  assert.equal(savedInputs[0].danger_threshold, 90);
  assert.equal(savedInputs[0].limit_order, "primary_first");
  assert.equal(savedInputs[0].fullscreen_hide_on, false);
  assert.equal(savedInputs[0].maximized_hide_on, true);
  assert.equal(savedInputs[0].taskbar_avoid_overlap_on, true);
  assert.equal(savedInputs[0].taskbar_layout_memory_on, true);
  assert.equal(savedInputs[0].indicator_style, "ring");
  assert.equal(savedInputs[0].indicator_effect_style, "depth");
  assert.equal(savedInputs[0].indicator_track_color_auto, false);
  assert.equal(savedInputs[0].indicator_track_color, "#123456");
  assert.equal(savedInputs[0].indicator_track_opacity_percent, 37);
  assert.equal(savedInputs[0].ring_numbers_on, true);
  assert.equal(savedInputs[0].ring_number_outline_on, true);
  assert.equal(savedInputs[0].ring_number_outline_width_px, 1.2);
  assert.equal(savedInputs[0].ring_size_px, 40.5);
  assert.equal(savedInputs[0].ring_thickness_px, 4);
  assert.equal(savedInputs[0].ring_gap_px, 6);
  assert.equal(savedInputs[0].ring_center_size_px, 16);
  assert.equal(savedInputs[0].ring_number_font_size_px, 9);
  assert.equal(savedInputs[0].ring_number_font_weight, 600);
  assert.equal(savedInputs[0].bar_text_font_size_px, 11);
  assert.equal(savedInputs[0].bar_text_font_weight, 500);
  assert.equal(savedInputs[0].bar_content_gap_px, 14);
  assert.equal(savedInputs[0].update_check_on, true);
  assert.equal(savedInputs[0].claude_primary_color, "#123456");
  assert.equal(savedInputs[0].claude_secondary_color, "#d36b86");
  assert.equal(savedInputs[0].tool_warning_color, "#654321");
  assert.equal(savedInputs[0].tool_danger_color, "#ef4444");
  assert.equal(savedInputs[0].tool_warning_color_on, true);
  assert.equal(savedInputs[0].tool_danger_color_on, false);
  assert.equal(toolColorRow.hidden, false);
  assert.equal(fullResetRow.hidden, true);
  assert.equal(dispatched.at(-1)?.type, "settings-updated");
  assert.equal(statusEl.textContent, "");
  assert.equal(statusHost.hidden, true);
  assert.equal(toastLayer.hidden, false);
  assert.equal(toastLayer.dataset.visible, "true");
  assert.equal(toastText.textContent, "저장 완료 · 시스템 적용 재시도 중");

  listeners.click?.({ target: { dataset: { action: "clear-taskbar-layouts" } } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(invokedCommands.includes("clear_taskbar_layout_profiles"));
  assert.equal(toastText.textContent, "저장된 모니터 배치를 초기화했습니다.");

  const clearsBeforeDelayedSave = invokedCommands.filter(
    (command) => command === "clear_taskbar_layout_profiles",
  ).length;
  deferSaveResponses = true;
  fields.bar_mode.value = "compact";
  listeners.input?.({ target: fields.bar_mode });
  listeners.click?.({ target: { dataset: { action: "clear-taskbar-layouts" } } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(pendingSaveResponses.length, 1);
  fields.bar_mode.value = "dual";
  listeners.input?.({ target: fields.bar_mode });
  pendingSaveResponses.shift().resolve(savedInputs.at(-1));
  await new Promise((resolve) => setImmediate(resolve));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(
    invokedCommands.filter((command) => command === "clear_taskbar_layout_profiles").length,
    clearsBeforeDelayedSave + 1,
  );
  assert.equal(
    fields.bar_mode.value,
    "dual",
    "a clear response must not overwrite edits made while its leading save was in flight",
  );
  deferSaveResponses = false;
  await new Promise((resolve) => setTimeout(resolve, 150));

  deferClearResponse = true;
  listeners.click?.({ target: { dataset: { action: "clear-taskbar-layouts" } } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(typeof pendingClearResponse, "function");
  fields.bar_mode.value = "full";
  listeners.input?.({ target: fields.bar_mode });
  pendingClearResponse();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(
    fields.bar_mode.value,
    "full",
    "a delayed clear response must not overwrite edits made while it was in flight",
  );
  deferClearResponse = false;
  pendingClearResponse = null;
  await new Promise((resolve) => setTimeout(resolve, 150));

  deferClearResponse = true;
  listeners.click?.({ target: { dataset: { action: "clear-taskbar-layouts" } } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(typeof pendingClearResponse, "function");
  eventHandlers["settings-updated"]?.({
    payload: { ...persistedSettings, bar_mode: "dual" },
  });
  assert.equal(fields.bar_mode.value, "dual");
  pendingClearResponse();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(
    fields.bar_mode.value,
    "dual",
    "a delayed clear response must not overwrite a newer settings event",
  );
  deferClearResponse = false;
  pendingClearResponse = null;

  const installsBeforeFailedFlush = installUpdateCalls;
  persistedSettings = { ...savedInputs.at(-1) };
  failNextSave = true;
  fields.theme.value = "light";
  listeners.input?.({ target: fields.theme });
  listeners.click?.({ target: { dataset: { action: "install-update" } } });
  await new Promise((resolve) => setImmediate(resolve));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(installUpdateCalls, installsBeforeFailedFlush, "a failed settings flush must stop the updater");
  assert.equal(updateFallbackButton.hidden, false);
  assert.equal(updateInstallProgress.attributes["aria-valuenow"], "0");
  assert.equal(updateProgressBar.style.width, "0%");
  assert.match(updateProgressText.textContent, /설정을 저장하지 못해/);
  assert.ok(updateInstallButtons.every((button) => !button.disabled));
  assert.equal(form.inert, false, "a failed handoff must restore settings editing");
  fields.theme.value = "system";
  listeners.input?.({ target: fields.theme });
  await new Promise((resolve) => setTimeout(resolve, 150));
  assert.equal(statusHost.hidden, true);

  listeners.click?.({ target: { dataset: { action: "check-updates" } } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(updateStatusEl.dataset.state, "error");
  assert.equal(statusHost.hidden, true, "update errors must not become settings save errors");

  const savesBeforeDeferredQueue = savedInputs.length;
  deferSaveResponses = true;
  fields.bar_mode.value = "compact";
  listeners.input?.({ target: fields.bar_mode });
  assert.equal(toastLayer.hidden, true, "a new edit must dismiss the previous completion toast");
  assert.match(statusEl.textContent, /적용 중/);
  await new Promise((resolve) => setTimeout(resolve, 150));
  fields.bar_mode.value = "quad";
  listeners.input?.({ target: fields.bar_mode });
  await new Promise((resolve) => setTimeout(resolve, 150));

  eventHandlers["settings-updated"]?.({ payload: { bar_mode: "full" } });
  assert.equal(fields.bar_mode.value, "quad", "an old event must not overwrite a newer edit");
  assert.equal(
    savedInputs.length,
    savesBeforeDeferredQueue + 1,
    "a second save must wait for the in-flight save",
  );
  pendingSaveResponses.shift().resolve(savedInputs[savesBeforeDeferredQueue]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fields.bar_mode.value, "quad", "an old response must not overwrite a newer edit");
  assert.equal(savedInputs.length, savesBeforeDeferredQueue + 2);
  pendingSaveResponses.shift().resolve(savedInputs[savesBeforeDeferredQueue + 1]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fields.bar_mode.value, "quad");

  deferSaveResponses = false;
  persistedSettings = { ...savedInputs.at(-1), show_claude: true, show_codex: true };
  failNextSave = true;
  fields.show_claude.checked = false;
  listeners.change?.({ target: fields.show_claude });
  await new Promise((resolve) => setTimeout(resolve, 180));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fields.show_claude.checked, true, "a failed collection toggle must roll back");
  assert.match(statusEl.textContent, /Claude statusline conflict/);

  fields.bar_mode.value = "compact";
  listeners.input?.({ target: fields.bar_mode });
  eventHandlers["app-quit-requested"]?.({ payload: null });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(savedInputs.at(-1).bar_mode, "compact");
  assert.equal(invokedCommands.at(-1), "complete_app_quit");
  eventHandlers["app-quit-cancelled"]?.({ payload: "activation" });
  eventHandlers["app-quit-requested"]?.({ payload: null });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(
    invokedCommands.filter((command) => command === "complete_app_quit").length,
    2,
  );
  assert.ok(listenerAttempts.get("settings-updated") >= 2);

  windowListeners.pagehide?.();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(unlistenCalls, 3);

  delete global.window;
  delete global.document;
  delete global.FormData;
});

test("settings form ignores early input events until stored settings hydrate", async () => {
  const listeners = {};
  const savedInputs = [];
  const customRow = { hidden: true };
  const monoRow = { hidden: true };
  const toolColorRow = { hidden: true };
  const indicatorTrackColorRow = { dataset: {} };
  let focusedOption = null;
  const paletteOptions = ["traffic", "ocean", "mono", "custom"].map((value) => ({
    dataset: { paletteValue: value },
    setAttribute(name, next) {
      this[name] = next;
    },
    closest(selector) {
      return selector.includes("[data-palette-value]") ? this : null;
    },
    focus() {
      focusedOption = this;
    },
  }));
  const palettePicker = {
    style: makeStyle(),
    querySelectorAll() {
      return paletteOptions;
    },
  };
  const fullResetRow = { hidden: false };
  const fields = {
    palette: makeField("custom"),
    display_basis: makeField("remaining", "text", { name: "display_basis" }),
    warn_threshold: makeField("30", "range", { min: "0", max: "100" }),
    "warn-output": makeField("30"),
    danger_threshold: makeField("10", "range", { min: "0", max: "100" }),
    "danger-output": makeField("10"),
    poll_interval_secs: makeField("2"),
    "poll-output": makeField("2"),
    stale_after_secs: makeField("90"),
    "stale-output": makeField("90"),
    bar_mode: makeField("full"),
    full_reset_time_on: makeField(false, "checkbox"),
    limit_order: makeField("primary_first"),
    fullscreen_hide_on: makeField(false, "checkbox"),
    maximized_hide_on: makeField(false, "checkbox"),
    taskbar_avoid_overlap_on: makeField(true, "checkbox"),
    taskbar_layout_memory_on: makeField(true, "checkbox"),
    indicator_style: makeField("bar"),
    indicator_effect_style: makeField("flat"),
    indicator_track_color_auto: makeField(true, "checkbox"),
    indicator_track_color: makeField("#6b7280", "color"),
    indicator_track_opacity_percent: makeField("11", "range", { min: "0", max: "100" }),
    "indicator-track-opacity-output": makeField("11"),
    ring_on: makeField(true, "checkbox"),
    ring_numbers_on: makeField(true, "checkbox"),
    ring_number_outline_on: makeField(true, "checkbox"),
    ring_number_outline_width_px: makeField("1.2", "range", { min: "0", max: "4" }),
    "ring-number-outline-width-output": makeField("1.2"),
    ring_size_px: makeField("36", "range", { min: "20", max: "44" }),
    "ring-size-output": makeField("36"),
    ring_thickness_px: makeField("4", "range", { min: "1", max: "10" }),
    "ring-thickness-output": makeField("4"),
    ring_gap_px: makeField("6", "range", { min: "2", max: "14" }),
    "ring-gap-output": makeField("6"),
    ring_center_size_px: makeField("16", "range", { min: "4", max: "32" }),
    "ring-center-size-output": makeField("16"),
    ring_number_font_size_px: makeField("9", "range", { min: "6", max: "16" }),
    "ring-number-font-size-output": makeField("9"),
    ring_number_font_weight: makeField("600", "range", { min: "100", max: "900" }),
    "ring-number-font-weight-output": makeField("600"),
    bar_text_font_size_px: makeField("11", "range", { min: "8", max: "16" }),
    "bar-text-font-size-output": makeField("11"),
    bar_text_font_weight: makeField("500", "range", { min: "100", max: "900" }),
    bar_content_gap_px: makeField("4", "range", { min: "0", max: "24" }),
    "bar-text-font-weight-output": makeField("500"),
    autostart_on: makeField(true, "checkbox"),
    update_check_on: makeField(true, "checkbox"),
    language: makeField("system"),
    theme: makeField("system"),
    font_mode: makeField("system"),
    claude_taskbar_offset_ratio: makeField("0.5"),
    codex_taskbar_offset_ratio: makeField("0.5"),
    show_claude: makeField(true, "checkbox"),
    show_codex: makeField(true, "checkbox"),
    claude_account_auto_collect_on: makeField(true, "checkbox"),
    mono_color: makeField("#4f8a73", "color"),
    custom_safe: makeField("#22c55e"),
    custom_warn: makeField("#f59e0b"),
    custom_danger: makeField("#ef4444"),
    claude_primary_color: makeField("#d79a32", "color"),
    claude_secondary_color: makeField("#d36b86", "color"),
    codex_primary_color: makeField("#2fac7d", "color"),
    codex_secondary_color: makeField("#4d86d6", "color"),
    tool_warning_color: makeField("#f59e0b", "color"),
    tool_danger_color: makeField("#ef4444", "color"),
    tool_warning_color_on: makeField(true, "checkbox"),
    tool_danger_color_on: makeField(true, "checkbox"),
  };
  const form = {
    elements: {
      namedItem(name) {
        return fields[name] ?? null;
      },
    },
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
  };
  let resolveSettings;
  let settingsLoadAttempts = 0;
  const settingsLoaded = new Promise((resolve) => {
    resolveSettings = resolve;
  });

  global.FormData = class {
    get(name) {
      const field = fields[name];
      if (!field) return null;
      if (field.type === "checkbox") return field.checked ? "on" : null;
      return field.value;
    }
  };
  global.window = {
    addEventListener() {},
    dispatchEvent() {},
    __TAURI__: {
      core: {
        async invoke(command, args) {
          if (command === "get_settings") {
            settingsLoadAttempts += 1;
            if (settingsLoadAttempts === 1) throw new Error("transient settings IPC failure");
            return settingsLoaded;
          }
          if (command === "save_settings") {
            savedInputs.push(args.input);
            return args.input;
          }
          return null;
        },
      },
      event: {
        async listen() {},
      },
    },
  };
  global.document = {
    documentElement: {
      dataset: {},
      removeAttribute() {},
    },
    querySelector(selector) {
      if (selector === "#settings-form") return form;
      if (selector === "#settings-status") return { textContent: "" };
      if (selector === "[data-custom-palette]") return customRow;
      if (selector === "[data-mono-palette]") return monoRow;
      if (selector === "[data-tool-palette]") return toolColorRow;
      if (selector === "[data-indicator-track-custom-color]") return indicatorTrackColorRow;
      if (selector === "[data-palette-picker]") return palettePicker;
      if (selector === "[data-full-reset-toggle]") return fullResetRow;
      return null;
    },
  };

  await import(`./settings.js?test=${Date.now()}-early-input`);
  listeners.input?.({ target: fields.bar_mode });
  await new Promise((resolve) => setTimeout(resolve, 180));
  assert.equal(savedInputs.length, 0);
  assert.ok(settingsLoadAttempts >= 2);

  resolveSettings({
    palette: "Traffic",
    bar_mode: "compact",
    indicator_style: "bar",
    fullscreen_hide_on: false,
    maximized_hide_on: false,
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fields.palette.value, "traffic");
  assert.equal(fields.bar_mode.value, "compact");
  assert.equal(fullResetRow.hidden, true);
  assert.equal(paletteOptions[0]["aria-checked"], "true");
  assert.equal(paletteOptions[0].tabIndex, 0);
  assert.equal(paletteOptions[1].tabIndex, -1);

  listeners.keydown?.({
    target: paletteOptions[0],
    key: "ArrowRight",
    preventDefault() {},
  });
  await new Promise((resolve) => setTimeout(resolve, 180));
  assert.equal(fields.palette.value, "ocean");
  assert.equal(focusedOption, paletteOptions[1]);
  assert.equal(paletteOptions[1].tabIndex, 0);
  assert.equal(savedInputs.length, 1);
  assert.equal(savedInputs[0].palette, "ocean");

  fields.bar_mode.value = "dual";
  listeners.input?.({ target: fields.bar_mode });
  await new Promise((resolve) => setTimeout(resolve, 180));
  assert.equal(savedInputs.length, 2);
  assert.equal(savedInputs[1].palette, "ocean");
  assert.equal(savedInputs[1].bar_mode, "dual");

  delete global.window;
  delete global.document;
  delete global.FormData;
});
