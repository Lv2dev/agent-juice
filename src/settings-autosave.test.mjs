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
  let settingsEventHandler = null;
  const statusEl = { textContent: "" };
  const customRow = { hidden: true };
  const fields = {
    palette: makeField("traffic"),
    warn_threshold: makeField("30", "range", { min: "0", max: "100" }),
    "warn-output": makeField("30"),
    danger_threshold: makeField("10", "range", { min: "0", max: "100" }),
    "danger-output": makeField("10"),
    poll_interval_secs: makeField("2"),
    "poll-output": makeField("2"),
    stale_after_secs: makeField("90"),
    "stale-output": makeField("90"),
    bar_mode: makeField("full"),
    limit_order: makeField("primary_first"),
    fullscreen_hide_on: makeField(true, "checkbox"),
    maximized_hide_on: makeField(true, "checkbox"),
    indicator_style: makeField("ring"),
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
    ring_center_gap_px: makeField("0", "range", { min: "0", max: "8" }),
    "ring-center-gap-output": makeField("0"),
    ring_number_font_size_px: makeField("9", "range", { min: "6", max: "16" }),
    "ring-number-font-size-output": makeField("9"),
    ring_number_font_weight: makeField("600", "range", { min: "100", max: "900" }),
    "ring-number-font-weight-output": makeField("600"),
    bar_text_font_size_px: makeField("11", "range", { min: "8", max: "16" }),
    "bar-text-font-size-output": makeField("11"),
    bar_text_font_weight: makeField("500", "range", { min: "100", max: "900" }),
    "bar-text-font-weight-output": makeField("500"),
    autostart_on: makeField(true, "checkbox"),
    theme: makeField("system"),
    font_mode: makeField("system"),
    claude_taskbar_offset_ratio: makeField("0.5"),
    codex_taskbar_offset_ratio: makeField("0.5"),
    show_claude: makeField(true, "checkbox"),
    show_codex: makeField(true, "checkbox"),
    custom_safe: makeField("#22c55e"),
    custom_warn: makeField("#f59e0b"),
    custom_danger: makeField("#ef4444"),
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
          if (command === "get_settings") return { maximized_hide_on: true };
          if (command === "save_settings") {
            savedInputs.push(args.input);
            if (deferSaveResponses) {
              return new Promise((resolve) => {
                pendingSaveResponses.push({ input: args.input, resolve });
              });
            }
            return args.input;
          }
          return null;
        },
      },
      event: {
        async listen(_name, handler) {
          settingsEventHandler = handler;
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
      if (selector === "[data-custom-palette]") return customRow;
      return null;
    },
  };

  await import(`./settings.js?test=${Date.now()}-autosave`);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(fields.warn_threshold.style.getPropertyValue("--range-progress"), "30%");
  assert.equal(fields.ring_size_px.style.getPropertyValue("--range-progress"), "66.7%");

  fields.bar_mode.value = "dual";
  listeners.input?.({ target: fields.bar_mode });
  await new Promise((resolve) => setTimeout(resolve, 180));

  assert.equal(savedInputs.length, 1);
  assert.equal(savedInputs[0].bar_mode, "dual");
  assert.equal(savedInputs[0].limit_order, "primary_first");
  assert.equal(savedInputs[0].fullscreen_hide_on, true);
  assert.equal(savedInputs[0].maximized_hide_on, true);
  assert.equal(savedInputs[0].indicator_style, "ring");
  assert.equal(savedInputs[0].ring_numbers_on, true);
  assert.equal(savedInputs[0].ring_number_outline_on, true);
  assert.equal(savedInputs[0].ring_number_outline_width_px, 1.2);
  assert.equal(savedInputs[0].ring_size_px, 36);
  assert.equal(savedInputs[0].ring_thickness_px, 4);
  assert.equal(savedInputs[0].ring_gap_px, 6);
  assert.equal(savedInputs[0].ring_center_gap_px, 0);
  assert.equal(savedInputs[0].ring_number_font_size_px, 9);
  assert.equal(savedInputs[0].ring_number_font_weight, 600);
  assert.equal(savedInputs[0].bar_text_font_size_px, 11);
  assert.equal(savedInputs[0].bar_text_font_weight, 500);
  assert.equal(dispatched.at(-1)?.type, "settings-updated");
  assert.match(statusEl.textContent, /자동 적용/);

  deferSaveResponses = true;
  fields.bar_mode.value = "compact";
  listeners.input?.({ target: fields.bar_mode });
  await new Promise((resolve) => setTimeout(resolve, 150));
  fields.bar_mode.value = "quad";
  listeners.input?.({ target: fields.bar_mode });
  await new Promise((resolve) => setTimeout(resolve, 150));

  settingsEventHandler?.({ payload: { bar_mode: "full" } });
  assert.equal(fields.bar_mode.value, "quad", "an old event must not overwrite a newer edit");
  assert.equal(savedInputs.length, 2, "a second save must wait for the in-flight save");
  pendingSaveResponses.shift().resolve(savedInputs[1]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fields.bar_mode.value, "quad", "an old response must not overwrite a newer edit");
  assert.equal(savedInputs.length, 3);
  pendingSaveResponses.shift().resolve(savedInputs[2]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fields.bar_mode.value, "quad");

  delete global.window;
  delete global.document;
  delete global.FormData;
});

test("settings form ignores early input events until stored settings hydrate", async () => {
  const listeners = {};
  const savedInputs = [];
  const customRow = { hidden: true };
  const fields = {
    palette: makeField("custom"),
    warn_threshold: makeField("30", "range", { min: "0", max: "100" }),
    "warn-output": makeField("30"),
    danger_threshold: makeField("10", "range", { min: "0", max: "100" }),
    "danger-output": makeField("10"),
    poll_interval_secs: makeField("2"),
    "poll-output": makeField("2"),
    stale_after_secs: makeField("90"),
    "stale-output": makeField("90"),
    bar_mode: makeField("full"),
    limit_order: makeField("primary_first"),
    fullscreen_hide_on: makeField(false, "checkbox"),
    maximized_hide_on: makeField(false, "checkbox"),
    indicator_style: makeField("bar"),
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
    ring_center_gap_px: makeField("0", "range", { min: "0", max: "8" }),
    "ring-center-gap-output": makeField("0"),
    ring_number_font_size_px: makeField("9", "range", { min: "6", max: "16" }),
    "ring-number-font-size-output": makeField("9"),
    ring_number_font_weight: makeField("600", "range", { min: "100", max: "900" }),
    "ring-number-font-weight-output": makeField("600"),
    bar_text_font_size_px: makeField("11", "range", { min: "8", max: "16" }),
    "bar-text-font-size-output": makeField("11"),
    bar_text_font_weight: makeField("500", "range", { min: "100", max: "900" }),
    "bar-text-font-weight-output": makeField("500"),
    autostart_on: makeField(true, "checkbox"),
    language: makeField("system"),
    theme: makeField("system"),
    font_mode: makeField("system"),
    claude_taskbar_offset_ratio: makeField("0.5"),
    codex_taskbar_offset_ratio: makeField("0.5"),
    show_claude: makeField(true, "checkbox"),
    show_codex: makeField(true, "checkbox"),
    claude_usage_auto_refresh_lab_on: makeField(false, "checkbox"),
    custom_safe: makeField("#22c55e"),
    custom_warn: makeField("#f59e0b"),
    custom_danger: makeField("#ef4444"),
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
          if (command === "get_settings") return settingsLoaded;
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
      return null;
    },
  };

  await import(`./settings.js?test=${Date.now()}-early-input`);
  listeners.input?.({ target: fields.bar_mode });
  await new Promise((resolve) => setTimeout(resolve, 180));
  assert.equal(savedInputs.length, 0);

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

  fields.bar_mode.value = "dual";
  listeners.input?.({ target: fields.bar_mode });
  await new Promise((resolve) => setTimeout(resolve, 180));
  assert.equal(savedInputs.length, 1);
  assert.equal(savedInputs[0].palette, "traffic");
  assert.equal(savedInputs[0].bar_mode, "dual");

  delete global.window;
  delete global.document;
  delete global.FormData;
});
