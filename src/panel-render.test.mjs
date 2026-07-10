import assert from "node:assert/strict";
import test from "node:test";

function metricStub() {
  const textNodes = new Map();
  return {
    querySelector(selector) {
      if (selector === ".fill") {
        return { style: { width: "", background: "" } };
      }
      if (!textNodes.has(selector)) {
        textNodes.set(selector, { textContent: "" });
      }
      return textNodes.get(selector);
    },
  };
}

function toolCardStub() {
  const metrics = new Map([
    [".p5h", metricStub()],
    [".pweek", metricStub()],
  ]);
  const textNodes = new Map();
  const styles = new Map();

  return {
    dataset: {},
    hidden: false,
    style: {
      setProperty(name, value) {
        styles.set(name, value);
      },
      getPropertyValue(name) {
        return styles.get(name) ?? "";
      },
    },
    querySelector(selector) {
      if (metrics.has(selector)) return metrics.get(selector);
      if (!textNodes.has(selector)) {
        textNodes.set(selector, { textContent: "" });
      }
      return textNodes.get(selector);
    },
  };
}

test("panel render hides disabled tools and does not auto-hide on focus loss", async () => {
  let focusListenerCount = 0;
  let dragStartCount = 0;
  const listeners = {};
  const invokedCommands = [];
  const cards = {
    claude: toolCardStub(),
    codex: toolCardStub(),
  };

  global.window = {
    addEventListener() {},
    __TAURI__: {
      core: {
        async invoke(command) {
          invokedCommands.push(command);
          if (command === "get_settings") {
            return { show_claude: false, show_codex: true };
          }
          if (command === "get_status") {
            return [
              {
                tool: "codex",
                pc_id: "DESKTOP",
                captured_at: "2026-07-07T00:00:00Z",
                primary: { used_percent: 18, resets_at: null },
                secondary: { used_percent: 42, resets_at: null },
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
      webviewWindow: {
        getCurrentWebviewWindow() {
          return {
            async startDragging() {
              dragStartCount += 1;
            },
          };
        },
      },
      window: {
        async getCurrentWindow() {
          return {
            async onFocusChanged() {
              focusListenerCount += 1;
            },
            async hide() {},
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
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
    querySelector(selector) {
      if (selector === '[data-tool="claude"]') return cards.claude;
      if (selector === '[data-tool="codex"]') return cards.codex;
      return null;
    },
  };

  await import(`./panel.js?test=${Date.now()}-hidden-focus`);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(cards.claude.hidden, true);
  assert.equal(cards.codex.hidden, false);
  assert.equal(cards.codex.style.getPropertyValue("--tool-brand"), "#2fac7d");
  assert.equal(focusListenerCount, 0);
  let prevented = false;
  listeners.contextmenu?.({ preventDefault() { prevented = true; } });
  assert.equal(prevented, true);
  let clickPrevented = false;
  listeners.click?.({
    target: {
      closest(selector) {
        if (selector === "[data-window-action]") {
          return { dataset: { windowAction: "toggle-maximize" } };
        }
        return null;
      },
    },
    preventDefault() {
      clickPrevented = true;
    },
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(clickPrevented, true);
  assert.ok(invokedCommands.includes("toggle_panel_maximized"));
  let dragPrevented = false;
  listeners.pointerdown?.({
    button: 0,
    target: {
      closest(selector) {
        if (selector === "[data-window-action]") return null;
        if (selector === "[data-tauri-drag-region]") return {};
        return null;
      },
    },
    preventDefault() {
      dragPrevented = true;
    },
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(dragPrevented, true);
  assert.equal(dragStartCount, 1);
  delete global.window;
  delete global.document;
});

test("panel drag uses the callable current window fallback", async () => {
  let dragStartCount = 0;
  const listeners = {};
  const invokedCommands = [];

  global.window = {
    addEventListener() {},
    __TAURI__: {
      core: {
        async invoke(command) {
          invokedCommands.push(command);
          if (command === "get_settings") return {};
          if (command === "get_status") return [];
          return null;
        },
      },
      event: {
        async listen() {},
      },
      webviewWindow: {
        getCurrentWebviewWindow() {
          return {};
        },
      },
      window: {
        getCurrentWindow() {
          return {
            async startDragging() {
              dragStartCount += 1;
            },
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
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
    querySelector() {
      return null;
    },
  };

  await import(`./panel.js?test=${Date.now()}-drag-window-fallback`);
  await new Promise((resolve) => setImmediate(resolve));

  let dragPrevented = false;
  listeners.pointerdown?.({
    button: 0,
    target: {
      closest(selector) {
        if (selector === "[data-window-action]") return null;
        if (selector === "[data-tauri-drag-region]") return {};
        return null;
      },
    },
    preventDefault() {
      dragPrevented = true;
    },
  });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(dragPrevented, true);
  assert.equal(dragStartCount, 1);
  assert.ok(!invokedCommands.includes("start_panel_drag"));
  delete global.window;
  delete global.document;
});

test("panel drag falls back to guarded IPC when startDragging fails", async () => {
  let dragStartCount = 0;
  const listeners = {};
  const invokedCommands = [];

  global.window = {
    addEventListener() {},
    __TAURI__: {
      core: {
        async invoke(command) {
          invokedCommands.push(command);
          if (command === "get_settings") return {};
          if (command === "get_status") return [];
          if (command === "start_panel_drag") return null;
          return null;
        },
      },
      event: {
        async listen() {},
      },
      webviewWindow: {
        getCurrentWebviewWindow() {
          return {
            async startDragging() {
              dragStartCount += 1;
              throw new Error("startDragging denied");
            },
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
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
    querySelector() {
      return null;
    },
  };

  await import(`./panel.js?test=${Date.now()}-drag-ipc-fallback`);
  await new Promise((resolve) => setImmediate(resolve));

  let dragPrevented = false;
  listeners.pointerdown?.({
    button: 0,
    target: {
      closest(selector) {
        if (selector === "[data-window-action]") return null;
        if (selector === "[data-tauri-drag-region]") return {};
        return null;
      },
    },
    preventDefault() {
      dragPrevented = true;
    },
  });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(dragPrevented, true);
  assert.equal(dragStartCount, 1);
  assert.ok(invokedCommands.includes("start_panel_drag"));
  delete global.window;
  delete global.document;
});
