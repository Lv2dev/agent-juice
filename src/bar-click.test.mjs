import assert from "node:assert/strict";
import test from "node:test";

function toolStub() {
  const textNodes = new Map();
  const styleProps = new Map();
  const ringStyleProps = new Map();
  return {
    dataset: {},
    hidden: false,
    style: {
      setProperty(name, value) {
        styleProps.set(name, value);
      },
      getPropertyValue(name) {
        return styleProps.get(name) ?? "";
      },
    },
    querySelector(selector) {
      if (selector === ".bar-ring") {
        return {
          style: {
            setProperty(name, value) {
              ringStyleProps.set(name, value);
            },
            getPropertyValue(name) {
              return ringStyleProps.get(name) ?? "";
            },
          },
        };
      }
      if (!textNodes.has(selector)) {
        textNodes.set(selector, { textContent: "" });
      }
      return textNodes.get(selector);
    },
    textContentFor(selector) {
      return textNodes.get(selector)?.textContent ?? "";
    },
  };
}

test("bar render writes severity attributes for animation states", async () => {
  const root = { dataset: {} };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") return {};
          if (command === "get_status") {
            return [
              {
                tool: "claude",
                captured_at: "2026-07-07T00:00:00Z",
                primary: { used_percent: 91, resets_at: null },
                secondary: null,
                session: { active: true },
              },
            ];
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
    addEventListener() {},
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-severity`);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(tools.claude.dataset.severity, "danger");
  assert.equal(tools.codex.dataset.severity, "empty");
  delete global.window;
  delete global.document;
});

test("bar render uses the current window tool and does not apply configured gap", async () => {
  const gapValues = [];
  const root = {
    dataset: {},
    style: {
      setProperty(name, value) {
        if (name === "--tool-gap") gapValues.push(value);
      },
    },
  };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=codex" },
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") {
            return { show_claude: true, show_codex: true };
          }
          if (command === "get_status") return [];
          return null;
        },
      },
      event: {
        async listen() {},
      },
    },
  };
  global.document = {
    addEventListener() {},
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-hidden-gap`);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(tools.claude.hidden, true);
  assert.equal(tools.codex.hidden, false);
  assert.deepEqual(gapValues, []);
  delete global.window;
  delete global.document;
});

test("bar render applies ring number and geometry settings to the root", async () => {
  const rootStyleProps = new Map();
  const root = {
    dataset: {},
    style: {
      setProperty(name, value) {
        rootStyleProps.set(name, value);
      },
      getPropertyValue(name) {
        return rootStyleProps.get(name) ?? "";
      },
    },
  };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=codex" },
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") {
            return {
              indicator_style: "bar",
              ring_numbers_on: false,
              ring_number_outline_on: true,
              ring_size_px: 34.5,
              ring_thickness_px: 6.5,
              ring_gap_px: 8.5,
              ring_number_font_size_px: 10.5,
              ring_number_font_weight: 650,
              bar_text_font_size_px: 12.5,
              bar_text_font_weight: 550,
            };
          }
          if (command === "get_status") return [];
          return null;
        },
      },
      event: {
        async listen() {},
      },
    },
  };
  global.document = {
    addEventListener() {},
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-ring-options`);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(root.dataset.indicator, "bar");
  assert.equal(root.dataset.ringNumbers, "off");
  assert.equal(root.dataset.numberOutline, "on");
  assert.equal(root.style.getPropertyValue("--ring-size"), "34.5px");
  assert.equal(root.style.getPropertyValue("--ring-thickness"), "6.5px");
  assert.equal(root.style.getPropertyValue("--ring-gap"), "8.5px");
  assert.equal(root.style.getPropertyValue("--ring-number-font-size"), "10.5px");
  assert.equal(root.style.getPropertyValue("--ring-number-font-weight"), "650");
  assert.equal(root.style.getPropertyValue("--bar-text-font-size"), "12.5px");
  assert.equal(root.style.getPropertyValue("--bar-text-font-weight"), "550");
  delete global.window;
  delete global.document;
});

test("bar right click suppresses the browser context menu and pauses taskbar bars", async () => {
  const listeners = {};
  const invocations = [];
  const root = { dataset: {} };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=codex" },
    __TAURI__: {
      core: {
        async invoke(command, args) {
          invocations.push({ command, args });
          if (command === "get_settings") return {};
          if (command === "get_status") return [];
          return null;
        },
      },
      event: {
        async listen() {},
      },
    },
  };
  global.document = {
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-contextmenu`);
  await new Promise((resolve) => setImmediate(resolve));
  let prevented = false;
  listeners.contextmenu?.({ preventDefault() { prevented = true; } });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(prevented, true);
  assert.equal(
    invocations.filter((item) => item.command === "pause_taskbar_bars").length,
    1
  );
  delete global.window;
  delete global.document;
});

test("bar renders the current tool before event subscription resolves", async () => {
  const root = { dataset: {}, style: { setProperty() {} } };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=codex" },
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") return {};
          if (command === "get_status") return [];
          return null;
        },
      },
      event: {
        async listen() {
          return new Promise(() => {});
        },
      },
    },
  };
  global.document = {
    addEventListener() {},
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-early-render`);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(tools.claude.hidden, true);
  assert.equal(tools.codex.hidden, false);
  assert.equal(root.dataset.currentTool, "codex");
  assert.equal(root.dataset.tool, undefined);
  delete global.window;
  delete global.document;
});

test("bar render exposes ring CSS variables on each tool for quad mode", async () => {
  const root = { dataset: {}, style: { setProperty() {} } };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=claude" },
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") return { bar_mode: "quad" };
          if (command === "get_status") {
            return [
              {
                tool: "claude",
                captured_at: "2026-07-07T00:00:00Z",
                primary: { used_percent: 88, resets_at: null },
                secondary: { used_percent: 41, resets_at: null },
                session: { active: true },
              },
            ];
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
    addEventListener() {},
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-quad-vars`);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(root.dataset.mode, "quad");
  assert.equal(tools.claude.style.getPropertyValue("--primary-color"), "#f59e0b");
  assert.equal(tools.claude.style.getPropertyValue("--primary-arc"), "43.2deg");
  assert.equal(tools.claude.style.getPropertyValue("--primary-percent"), "12%");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-color"), "#2563eb");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-arc"), "212.4deg");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-percent"), "59%");
  assert.equal(tools.claude.textContentFor(".quad-primary-number"), "12");
  assert.equal(tools.claude.textContentFor(".quad-secondary-number"), "59");
  delete global.window;
  delete global.document;
});

test("bar render can show weekly limits before 5h without changing semantic limits", async () => {
  const root = { dataset: {}, style: { setProperty() {} } };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=claude" },
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") {
            return { bar_mode: "quad", indicator_style: "bar", limit_order: "secondary_first" };
          }
          if (command === "get_status") {
            return [
              {
                tool: "claude",
                captured_at: "2026-07-07T00:00:00Z",
                primary: { used_percent: 88, resets_at: null },
                secondary: { used_percent: 41, resets_at: null },
                session: { active: true },
              },
            ];
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
    addEventListener() {},
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-limit-order`);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(root.dataset.limitOrder, "secondary-first");
  assert.equal(tools.claude.textContentFor(".primary-text"), "주간 59%");
  assert.equal(tools.claude.textContentFor(".secondary-text"), "5h 12%");
  assert.equal(tools.claude.style.getPropertyValue("--primary-color"), "#2563eb");
  assert.equal(tools.claude.style.getPropertyValue("--primary-arc"), "212.4deg");
  assert.equal(tools.claude.style.getPropertyValue("--primary-percent"), "59%");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-color"), "#f59e0b");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-arc"), "43.2deg");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-percent"), "12%");
  assert.equal(tools.claude.textContentFor(".bar-worst"), "12");
  assert.equal(tools.claude.textContentFor(".quad-primary-number"), "59");
  assert.equal(tools.claude.textContentFor(".quad-secondary-number"), "12");
  delete global.window;
  delete global.document;
});

test("bar click does not open or focus the panel window", async () => {
  const listeners = {};
  const calls = [];
  const root = { dataset: {} };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };
  const panel = {
    label: "panel",
    async show() {
      calls.push("show");
    },
    async setFocus() {
      calls.push("setFocus");
    },
  };

  global.window = {
    location: { search: "?tool=codex" },
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") return {};
          if (command === "get_status") return [];
          return null;
        },
      },
      event: {
        async listen() {},
      },
      webviewWindow: {
        WebviewWindow: {
          getByLabel(label) {
            assert.equal(label, "panel");
            return panel;
          },
        },
      },
    },
  };
  global.document = {
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}`);
  listeners.click?.();
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(calls, []);
  delete global.window;
  delete global.document;
});

test("bar pointer drag does not issue native move commands or open the panel", async () => {
  const listeners = {};
  const invocations = [];
  const calls = [];
  const root = { dataset: {} };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };
  const panel = {
    label: "panel",
    async show() {
      calls.push("show");
    },
    async setFocus() {
      calls.push("setFocus");
    },
  };

  global.window = {
    location: { search: "?tool=codex" },
    __TAURI__: {
      core: {
        async invoke(command, args) {
          invocations.push({ command, args });
          if (command === "get_settings") return {};
          if (command === "get_status") return [];
          if (command === "move_taskbar_bar") return { taskbar_offset_ratio: 0.75 };
          return null;
        },
      },
      event: {
        async listen() {},
      },
      webviewWindow: {
        WebviewWindow: {
          getByLabel() {
            return panel;
          },
        },
      },
    },
  };
  global.document = {
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-drag`);
  await new Promise((resolve) => setImmediate(resolve));

  listeners.pointerdown?.({ button: 0, clientX: 20, screenX: 700, preventDefault() {} });
  listeners.pointermove?.({ clientX: 44, screenX: 724, preventDefault() {} });
  listeners.pointerup?.({ clientX: 44, screenX: 724, preventDefault() {} });
  assert.equal(listeners.click, undefined);
  listeners.click?.();
  await new Promise((resolve) => setImmediate(resolve));

  const moves = invocations.filter((item) => item.command === "move_taskbar_bar");
  assert.equal(moves.length, 0);
  assert.deepEqual(calls, []);
  delete global.window;
  delete global.document;
});

test("bar move outline is only enabled while dragging", async () => {
  const listeners = {};
  const eventHandlers = {};
  const root = { dataset: {} };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=claude" },
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") return {};
          if (command === "get_status") return [];
          return null;
        },
      },
      event: {
        async listen(name, handler) {
          eventHandlers[name] = handler;
        },
      },
    },
  };
  global.document = {
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-drag-outline`);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(root.dataset.dragging, undefined);
  eventHandlers["taskbar-dragging-updated"]?.({ payload: { tool: "codex", dragging: true } });
  assert.equal(root.dataset.dragging, undefined);
  eventHandlers["taskbar-dragging-updated"]?.({ payload: { tool: "claude", dragging: true } });
  assert.equal(root.dataset.dragging, "true");
  eventHandlers["taskbar-dragging-updated"]?.({ payload: { tool: "claude", dragging: false } });
  assert.equal(root.dataset.dragging, undefined);

  listeners.pointerdown?.({ button: 0, clientX: 10, screenX: 400, preventDefault() {} });
  assert.equal(root.dataset.dragging, undefined);
  listeners.pointermove?.({ clientX: 30, screenX: 420, preventDefault() {} });
  assert.equal(root.dataset.dragging, undefined);
  listeners.pointerup?.({ clientX: 30, screenX: 420, preventDefault() {} });
  assert.equal(root.dataset.dragging, undefined);
  delete global.window;
  delete global.document;
});

test("bar still renders when event listen rejects without keyboard panel opening", async () => {
  const listeners = {};
  const calls = [];
  const root = { dataset: {} };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };
  const panel = {
    label: "panel",
    async show() {
      calls.push("show");
    },
    async setFocus() {
      calls.push("setFocus");
    },
  };

  global.window = {
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") return {};
          if (command === "get_status") return [];
          return null;
        },
      },
      event: {
        async listen() {
          throw new Error("listen failed");
        },
      },
      webviewWindow: {
        getAllWebviewWindows() {
          return [panel];
        },
      },
    },
  };
  global.document = {
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-fallback`);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(root.dataset.mode, "full");

  listeners.keydown?.({ key: "Enter", preventDefault() {} });
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(calls, []);
  delete global.window;
  delete global.document;
});
