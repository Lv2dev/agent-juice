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
    cursor: toolCardStub(),
  };

  global.window = {
    addEventListener(name, handler) {
      listeners[`window:${name}`] = handler;
    },
    __TAURI__: {
      core: {
        async invoke(command) {
          invokedCommands.push(command);
          if (command === "get_settings") {
            return { show_claude: false, show_codex: true, show_cursor: true };
          }
          if (command === "get_status") {
            return [
              {
                tool: "codex",
                pc_id: "DESKTOP",
                captured_at: "2026-07-07T00:00:00Z",
                primary: null,
                secondary: { used_percent: 42, resets_at: null },
                session: { active: true },
              },
              {
                tool: "cursor",
                pc_id: "DESKTOP",
                captured_at: "2026-08-21T00:00:00Z",
                primary: { label: "cursor_models", used_percent: 1, resets_at: "09-21" },
                secondary: { label: "other_models", used_percent: 0, resets_at: "09-21" },
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
      if (selector === '[data-tool="cursor"]') return cards.cursor;
      return null;
    },
  };

  await import(`./panel.js?test=${Date.now()}-hidden-focus`);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(cards.claude.hidden, true);
  assert.equal(cards.codex.hidden, false);
  assert.equal(cards.cursor.hidden, false);
  assert.equal(cards.codex.style.getPropertyValue("--tool-brand"), "#2fac7d");
  assert.equal(cards.codex.querySelector(".p5h").hidden, true);
  assert.equal(cards.codex.querySelector(".pweek").hidden, false);
  assert.equal(cards.cursor.style.getPropertyValue("--tool-brand"), "#72716d");
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
  assert.ok(!invokedCommands.includes("get_activity"));
  listeners["window:focus"]?.();
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(invokedCommands.includes("get_activity"));
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

test("panel retries listeners, keeps newer events, and rolls back failed window commands", async () => {
  const listeners = {};
  const eventHandlers = {};
  const attempts = new Map();
  const cards = { claude: toolCardStub(), codex: toolCardStub() };
  let resolveStatus;
  const pendingStatus = new Promise((resolve) => {
    resolveStatus = resolve;
  });

  global.window = {
    addEventListener() {},
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") return {};
          if (command === "get_status") return pendingStatus;
          if (command === "hide_panel_window") throw new Error("window unavailable");
          return null;
        },
      },
      event: {
        async listen(name, handler) {
          const count = (attempts.get(name) ?? 0) + 1;
          attempts.set(name, count);
          if (name === "status-updated" && count === 1) throw new Error("transient");
          eventHandlers[name] = handler;
        },
      },
    },
  };
  global.document = {
    hidden: false,
    documentElement: { dataset: {}, removeAttribute() {} },
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
    querySelector(selector) {
      if (selector === '[data-tool="claude"]') return cards.claude;
      if (selector === '[data-tool="codex"]') return cards.codex;
      return null;
    },
  };

  await import(`./panel.js?test=${Date.now()}-listener-retry`);
  await new Promise((resolve) => setTimeout(resolve, 130));
  eventHandlers["status-updated"]?.({
    payload: [{
      tool: "codex",
      pc_id: "NEW",
      captured_at: "2026-07-13T00:00:00Z",
      primary: { used_percent: 10 },
      secondary: { used_percent: 20 },
      session: { active: true },
    }],
  });
  resolveStatus([{
    tool: "codex",
    pc_id: "OLD",
    captured_at: "2026-07-12T00:00:00Z",
    primary: { used_percent: 90 },
    secondary: { used_percent: 90 },
    session: { active: true },
  }]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(cards.codex.querySelector(".pc").textContent, "NEW");
  assert.ok(attempts.get("status-updated") >= 2);

  listeners.click?.({
    target: {
      closest(selector) {
        return selector === "[data-window-action]"
          ? { dataset: { windowAction: "close" } }
          : null;
      },
    },
    preventDefault() {},
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(global.document.documentElement.dataset.panelVisible, "true");

  delete global.window;
  delete global.document;
});

test("panel bounds pending listeners, cleans late registrations, and preserves newer settings", async () => {
  const windowListeners = {};
  const eventHandlers = {};
  const pendingListenerResolvers = [];
  const attempts = new Map();
  const cards = { claude: toolCardStub(), codex: toolCardStub() };
  const originalSetInterval = global.setInterval;
  let fallbackIntervals = 0;
  let lateUnlistenCalls = 0;
  let normalUnlistenCalls = 0;
  let rejectSettings;
  const pendingSettings = new Promise((_, reject) => {
    rejectSettings = reject;
  });

  global.setInterval = (callback, delay) => {
    fallbackIntervals += 1;
    const timer = originalSetInterval(callback, delay);
    timer.unref?.();
    return timer;
  };

  global.window = {
    addEventListener(name, handler) {
      windowListeners[name] = handler;
    },
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") return pendingSettings;
          if (command === "get_status") return [];
          return null;
        },
      },
      event: {
        listen(name, handler) {
          attempts.set(name, (attempts.get(name) ?? 0) + 1);
          if (name === "panel-visibility-updated") {
            return new Promise((resolve) => pendingListenerResolvers.push(resolve));
          }
          eventHandlers[name] = handler;
          return Promise.resolve(() => {
            normalUnlistenCalls += 1;
          });
        },
      },
    },
  };
  global.document = {
    hidden: false,
    documentElement: { dataset: {}, removeAttribute() {} },
    addEventListener() {},
    querySelector(selector) {
      if (selector === '[data-tool="claude"]') return cards.claude;
      if (selector === '[data-tool="codex"]') return cards.codex;
      return null;
    },
  };

  try {
    await import(`./panel.js?test=${Date.now()}-listener-timeout`);
    await new Promise((resolve) => setImmediate(resolve));

    windowListeners["settings-updated"]?.({ detail: { display_basis: "used" } });
    rejectSettings(new Error("late bootstrap failure"));
    await new Promise((resolve) => setImmediate(resolve));
    eventHandlers["status-updated"]?.({
      payload: [{
        tool: "codex",
        captured_at: new Date().toISOString(),
        primary: { used_percent: 10 },
        secondary: { used_percent: 20 },
        session: { active: true },
      }],
    });
    assert.equal(cards.codex.querySelector(".p5h").querySelector(".val").textContent, "10%");

    await new Promise((resolve) => setTimeout(resolve, 1_950));
    assert.equal(attempts.get("panel-visibility-updated"), 3);
    assert.equal(fallbackIntervals, 2);

    pendingListenerResolvers[0](() => {
      lateUnlistenCalls += 1;
    });
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(lateUnlistenCalls, 1);

    windowListeners.pagehide?.();
    await new Promise((resolve) => setImmediate(resolve));
    assert.ok(attempts.has("collection-health-updated"));
    assert.equal(normalUnlistenCalls, 4);

    for (const resolve of pendingListenerResolvers.slice(1)) {
      resolve(() => {
        lateUnlistenCalls += 1;
      });
    }
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(lateUnlistenCalls, 3);
  } finally {
    global.setInterval = originalSetInterval;
    delete global.window;
    delete global.document;
  }
});
