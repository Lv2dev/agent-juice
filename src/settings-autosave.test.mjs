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
    style: makeStyle(),
    ...options,
  };
}

test("settings form auto-saves changed values without a submit button", async () => {
  const listeners = {};
  const dispatched = [];
  const savedInputs = [];
  const pendingSaveResponses = [];
  let deferSaveResponses = false;
  const eventHandlers = {};
  const listenerOrder = [];
  const listenerAttempts = new Map();
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
  const fullResetRow = { hidden: false };
  const updateStatusEl = { textContent: "", dataset: {} };
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
    fullscreen_hide_on: makeField(true, "checkbox"),
    maximized_hide_on: makeField(true, "checkbox"),
    indicator_style: makeField("ring"),
    indicator_effect_style: makeField("flat"),
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
    dispatchEvent(event) {
      dispatched.push(event);
    },
    __TAURI__: {
      core: {
        async invoke(command, args) {
          invokedCommands.push(command);
          if (command === "get_settings") return { maximized_hide_on: true, language: "ko" };
          if (command === "save_settings") {
            savedInputs.push(args.input);
            if (deferSaveResponses) {
              return new Promise((resolve) => {
                pendingSaveResponses.push({ input: args.input, resolve });
              });
            }
            return { settings: args.input, warnings: ["taskbar retry"] };
          }
          if (command === "check_for_updates") throw new Error("offline");
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
      if (selector === "[data-full-reset-toggle]") return fullResetRow;
      if (selector === "#update-check-status") return updateStatusEl;
      return null;
    },
  };

  await import(`./settings.js?test=${Date.now()}-autosave`);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(listenerOrder[0], "app-quit-requested");

  assert.equal(statusHost.hidden, true, "initial hydration must not show a completion state");
  assert.equal(toastLayer.hidden, true, "initial hydration must not show a completion toast");
  assert.equal(fields.warn_threshold.style.getPropertyValue("--range-progress"), "30%");
  assert.equal(fields.ring_size_px.style.getPropertyValue("--range-progress"), "66.7%");
  assert.equal(fullResetRow.hidden, false);

  fields.bar_mode.value = "dual";
  fields.claude_primary_color.value = "#123456";
  fields.display_basis.value = "used";
  listeners.input?.({ target: fields.display_basis });
  fields.theme.value = "dark";
  listeners.input?.({ target: { ...fields.theme, name: "theme" } });
  assert.equal(global.document.documentElement.dataset.theme, "dark");
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
  assert.equal(savedInputs[0].fullscreen_hide_on, true);
  assert.equal(savedInputs[0].maximized_hide_on, true);
  assert.equal(savedInputs[0].indicator_style, "ring");
  assert.equal(savedInputs[0].indicator_effect_style, "flat");
  assert.equal(savedInputs[0].ring_numbers_on, true);
  assert.equal(savedInputs[0].ring_number_outline_on, true);
  assert.equal(savedInputs[0].ring_number_outline_width_px, 1.2);
  assert.equal(savedInputs[0].ring_size_px, 36);
  assert.equal(savedInputs[0].ring_thickness_px, 4);
  assert.equal(savedInputs[0].ring_gap_px, 6);
  assert.equal(savedInputs[0].ring_center_size_px, 16);
  assert.equal(savedInputs[0].ring_number_font_size_px, 9);
  assert.equal(savedInputs[0].ring_number_font_weight, 600);
  assert.equal(savedInputs[0].bar_text_font_size_px, 11);
  assert.equal(savedInputs[0].bar_text_font_weight, 500);
  assert.equal(savedInputs[0].update_check_on, true);
  assert.equal(savedInputs[0].claude_primary_color, "#123456");
  assert.equal(savedInputs[0].claude_secondary_color, "#d36b86");
  assert.equal(toolColorRow.hidden, false);
  assert.equal(fullResetRow.hidden, true);
  assert.equal(dispatched.at(-1)?.type, "settings-updated");
  assert.equal(statusEl.textContent, "");
  assert.equal(statusHost.hidden, true);
  assert.equal(toastLayer.hidden, false);
  assert.equal(toastLayer.dataset.visible, "true");
  assert.equal(toastText.textContent, "저장 완료 · 시스템 적용 재시도 중");

  listeners.click?.({ target: { dataset: { action: "check-updates" } } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(updateStatusEl.dataset.state, "error");
  assert.equal(statusHost.hidden, true, "update errors must not become settings save errors");

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
  assert.equal(savedInputs.length, 2, "a second save must wait for the in-flight save");
  pendingSaveResponses.shift().resolve(savedInputs[1]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fields.bar_mode.value, "quad", "an old response must not overwrite a newer edit");
  assert.equal(savedInputs.length, 3);
  pendingSaveResponses.shift().resolve(savedInputs[2]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fields.bar_mode.value, "quad");

  deferSaveResponses = false;
  fields.bar_mode.value = "compact";
  listeners.input?.({ target: fields.bar_mode });
  eventHandlers["app-quit-requested"]?.({ payload: null });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(savedInputs.at(-1).bar_mode, "compact");
  assert.equal(invokedCommands.at(-1), "complete_app_quit");
  assert.ok(listenerAttempts.get("settings-updated") >= 2);

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
    indicator_style: makeField("bar"),
    indicator_effect_style: makeField("flat"),
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
