import assert from "node:assert/strict";
import test from "node:test";

function toolStub() {
  const textNodes = new Map();
  const styleProps = new Map();
  const ringStyleProps = new Map();
  const attributes = new Map();
  return {
    dataset: {},
    hidden: false,
    title: "",
    setAttribute(name, value) {
      attributes.set(name, value);
      if (name === "title") this.title = value;
    },
    removeAttribute(name) {
      attributes.delete(name);
      if (name === "title") this.title = "";
    },
    getAttribute(name) {
      return attributes.get(name) ?? null;
    },
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
          if (command === "get_settings") return { language: "ko", full_reset_time_on: true };
          if (command === "get_status") {
            return [
              {
                tool: "claude",
                captured_at: "2026-07-07T00:00:00Z",
                primary: {
                  used_percent: 91,
                  resets_at: new Date(Date.now() + 3_600_000).toISOString(),
                },
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
  assert.equal(root.dataset.fullResetTime, "on");
  assert.equal(tools.claude.textContentFor(".primary-reset"), "(1시간 0분)");
  assert.equal(tools.claude.getAttribute("title"), null);
  assert.equal(tools.claude.title, "");
  assert.equal(tools.claude.getAttribute("aria-label"), "Claude, 5h 9%, 주간 –");
  assert.equal(tools.claude.style.getPropertyValue("--primary-ring-visibility"), "visible");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-ring-visibility"), "hidden");
  delete global.window;
  delete global.document;
});

test("bar render hides a zero-percent ring arc without hiding a positive sibling", async () => {
  const root = { dataset: {} };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=codex" },
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") return { language: "ko" };
          if (command === "get_status") {
            return [
              {
                tool: "codex",
                captured_at: new Date().toISOString(),
                primary: { used_percent: 100, resets_at: null },
                secondary: { used_percent: 40, resets_at: null },
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

  await import(`./bar.js?test=${Date.now()}-zero-ring-arc`);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(tools.codex.style.getPropertyValue("--primary-dash"), "0");
  assert.equal(tools.codex.style.getPropertyValue("--primary-ring-visibility"), "hidden");
  assert.equal(tools.codex.style.getPropertyValue("--secondary-ring-visibility"), "visible");
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

test("bar loads the selected shell taskbar orientation instead of inferring it from viewport shape", async () => {
  const root = { dataset: {} };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };
  const calls = [];

  global.window = {
    location: { search: "?tool=codex" },
    __TAURI__: {
      core: {
        async invoke(command, args) {
          calls.push({ command, args });
          if (command === "get_settings") return {};
          if (command === "get_status") return [];
          if (command === "get_taskbar_orientation") return "vertical";
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

  await import(`./bar.js?test=${Date.now()}-taskbar-orientation`);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(root.dataset.taskbarOrientation, "vertical");
  assert.deepEqual(
    calls.find(({ command }) => command === "get_taskbar_orientation"),
    { command: "get_taskbar_orientation", args: { tool: "codex" } },
  );
  delete global.window;
  delete global.document;
});

test("full bar reports packed content width once after horizontal rendering", async () => {
  const root = { dataset: {} };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };
  tools.codex.scrollWidth = 187;
  tools.codex.getBoundingClientRect = () => ({ width: 186.2 });
  const calls = [];
  const eventHandlers = {};

  global.window = {
    location: { search: "?tool=codex" },
    __TAURI__: {
      core: {
        async invoke(command, args) {
          calls.push({ command, args });
          if (command === "get_settings") return { bar_mode: "full" };
          if (command === "get_status") return [];
          if (command === "get_taskbar_orientation") return "horizontal";
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
    addEventListener() {},
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-content-width`);
  await new Promise((resolve) => setTimeout(resolve, 120));
  assert.deepEqual(
    calls.filter(({ command }) => command === "set_taskbar_content_width"),
    [{ command: "set_taskbar_content_width", args: { tool: "codex", width: 187 } }],
  );

  eventHandlers["status-updated"]?.({ payload: [] });
  await new Promise((resolve) => setTimeout(resolve, 120));
  assert.equal(
    calls.filter(({ command }) => command === "set_taskbar_content_width").length,
    1,
  );
  delete global.window;
  delete global.document;
});

test("bar render applies ring number and geometry settings to the root", async () => {
  const rootStyleProps = new Map();
  const eventHandlers = {};
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
              indicator_effect_style: "depth",
              indicator_track_color_auto: false,
              indicator_track_color: [0x12, 0x34, 0x56],
              indicator_track_opacity_percent: 37.5,
              taskbar_text_colors: {
                claude: [0x11, 0x22, 0x44],
                claude_on: true,
                codex: [0x33, 0x55, 0x77],
                codex_on: false,
                info: [0x44, 0x66, 0x88],
                info_on: true,
                ring: [0x55, 0x77, 0x99],
                ring_on: true,
              },
              ring_numbers_on: false,
              ring_number_outline_on: true,
              ring_number_outline_width_px: 1.4,
              ring_size_px: 34.5,
              ring_thickness_px: 6.5,
              ring_gap_px: 8.5,
              ring_center_size_px: 18.5,
              ring_number_font_size_px: 10.5,
              ring_number_font_weight: 650,
              bar_text_font_size_px: 12.5,
              bar_text_font_weight: 550,
              bar_content_gap_px: 3.5,
            };
          }
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
  assert.equal(root.dataset.effect, "depth");
  assert.equal(root.dataset.indicatorTrackColor, "custom");
  assert.equal(root.dataset.claudeTextColor, "custom");
  assert.equal(root.dataset.codexTextColor, "auto");
  assert.equal(root.dataset.infoTextColor, "custom");
  assert.equal(root.dataset.ringTextColor, "custom");
  assert.equal(root.dataset.ringNumbers, "off");
  assert.equal(root.dataset.numberOutline, "on");
  assert.equal(root.style.getPropertyValue("--ring-number-outline-width"), "1.4px");
  assert.equal(root.style.getPropertyValue("--indicator-track-color"), "#123456");
  assert.equal(root.style.getPropertyValue("--indicator-track-opacity"), "37.5%");
  assert.equal(root.style.getPropertyValue("--claude-text-color"), "#112244");
  assert.equal(root.style.getPropertyValue("--codex-text-color"), "#335577");
  assert.equal(root.style.getPropertyValue("--info-text-color"), "#446688");
  assert.equal(root.style.getPropertyValue("--ring-text-color"), "#557799");
  assert.equal(root.style.getPropertyValue("--ring-size"), "34.5px");
  assert.equal(root.style.getPropertyValue("--ring-thickness"), "6.5px");
  assert.equal(root.style.getPropertyValue("--ring-gap"), "8.5px");
  assert.equal(root.style.getPropertyValue("--ring-svg-stroke"), "11.6");
  assert.equal(root.style.getPropertyValue("--outer-radius"), "44.2");
  assert.equal(root.style.getPropertyValue("--inner-radius"), "32.6");
  assert.equal(root.style.getPropertyValue("--quad-svg-stroke"), "18.8");
  assert.equal(root.style.getPropertyValue("--quad-radius"), "36.2");
  assert.equal(root.style.getPropertyValue("--ring-number-font-size"), "10.5px");
  assert.equal(root.style.getPropertyValue("--ring-number-font-weight"), "650");
  assert.equal(root.style.getPropertyValue("--bar-text-font-size"), "12.5px");
  assert.equal(root.style.getPropertyValue("--bar-text-font-weight"), "550");
  assert.equal(root.style.getPropertyValue("--bar-content-gap"), "3.5px");

  eventHandlers["settings-updated"]?.({ payload: { indicator_effect_style: "breathe" } });
  assert.equal(root.dataset.effect, "breathe");
  assert.equal(root.dataset.indicatorTrackColor, "theme");
  assert.equal(root.dataset.claudeTextColor, "auto");
  assert.equal(root.dataset.codexTextColor, "auto");
  assert.equal(root.dataset.infoTextColor, "auto");
  assert.equal(root.dataset.ringTextColor, "auto");
  assert.equal(root.style.getPropertyValue("--indicator-track-color"), "var(--text)");
  assert.equal(root.style.getPropertyValue("--indicator-track-opacity"), "11%");
  await new Promise((resolve) => setTimeout(resolve, 100));
  delete global.window;
  delete global.document;
});

test("bar right click waits for native resize before showing the refresh menu", async () => {
  const listeners = {};
  const eventHandlers = {};
  const invocations = [];
  let resolveMenuOpen;
  let resolveMenuClose;
  const root = { dataset: {} };
  const refreshButton = {
    handlers: {},
    addEventListener(name, handler) {
      this.handlers[name] = handler;
    },
  };
  const menu = {
    hidden: true,
    style: {
      props: {},
      setProperty(name, value) {
        this.props[name] = value;
      },
      getPropertyValue(name) {
        return this.props[name] ?? "";
      },
    },
    contains(target) {
      return target === this || target === refreshButton;
    },
  };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=codex" },
    innerWidth: 260,
    innerHeight: 40,
    __TAURI__: {
      core: {
        async invoke(command, args) {
          invocations.push({ command, args });
          if (command === "get_settings") return {};
          if (command === "get_status") return [];
          if (command === "set_taskbar_menu_open" && args.open) {
            return new Promise((resolve) => {
              resolveMenuOpen = resolve;
            });
          }
          if (command === "set_taskbar_menu_open" && !args.open) {
            return new Promise((resolve) => {
              resolveMenuClose = resolve;
            });
          }
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
      if (selector === "#bar-menu") return menu;
      if (selector === '[data-bar-action="refresh"]') return refreshButton;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-contextmenu`);
  await new Promise((resolve) => setImmediate(resolve));
  let prevented = false;
  listeners.contextmenu?.({
    clientX: 120,
    clientY: 20,
    target: root,
    preventDefault() { prevented = true; },
  });

  assert.equal(prevented, true);
  assert.equal(menu.hidden, true);
  assert.equal(root.dataset.menuState, "opening");

  const tooltipCallsBeforeMenu = invocations.filter(
    (item) => item.command === "set_taskbar_tooltip",
  ).length;
  eventHandlers["status-updated"]?.({
    payload: [{
      tool: "codex",
      primary: { used_percent: 20, resets_at: new Date(Date.now() + 3600000).toISOString() },
      secondary: null,
      session: { active: true },
    }],
  });
  assert.equal(
    invocations.filter((item) => item.command === "set_taskbar_tooltip").length,
    tooltipCallsBeforeMenu,
  );

  resolveMenuOpen();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(menu.hidden, false);
  assert.equal(root.dataset.menuState, "open");
  assert.equal(menu.style.getPropertyValue("--menu-x"), "120px");
  assert.equal(menu.style.getPropertyValue("--menu-y"), "8px");
  assert.equal(
    invocations.filter((item) => item.command === "refresh_status").length,
    0
  );
  await refreshButton.handlers.click?.({ preventDefault() {} });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(menu.hidden, true);
  assert.equal(root.dataset.menuState, "closing");
  assert.equal(
    invocations.filter((item) => item.command === "set_taskbar_tooltip").length,
    tooltipCallsBeforeMenu,
  );

  resolveMenuClose();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(root.dataset.menuState, "closed");
  assert.equal(
    invocations.filter((item) => item.command === "set_taskbar_tooltip").length,
    tooltipCallsBeforeMenu + 1,
  );
  assert.equal(
    invocations.filter((item) => item.command === "refresh_status").length,
    1
  );
  assert.equal(
    invocations.filter((item) => item.command === "pause_taskbar_bars").length,
    0
  );
  delete global.window;
  delete global.document;
});

test("bar refresh menu rolls back when native resize fails", async () => {
  const listeners = {};
  const menuStates = [];
  const root = { dataset: {} };
  const menu = {
    hidden: true,
    style: { setProperty() {} },
    contains() { return false; },
  };
  const tools = { claude: toolStub(), codex: toolStub() };

  global.window = {
    location: { search: "?tool=claude" },
    innerWidth: 260,
    innerHeight: 40,
    __TAURI__: {
      core: {
        async invoke(command, args) {
          if (command === "get_settings") return {};
          if (command === "get_status") return [];
          if (command === "set_taskbar_menu_open") {
            menuStates.push(args.open);
            if (args.open) throw new Error("resize failed");
          }
          return null;
        },
      },
      event: { async listen() {} },
    },
  };
  global.document = {
    addEventListener(name, handler) { listeners[name] = handler; },
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === "#bar-menu") return menu;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-menu-rollback`);
  await new Promise((resolve) => setImmediate(resolve));
  listeners.contextmenu?.({ clientX: 10, clientY: 10, preventDefault() {} });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(menu.hidden, true);
  assert.equal(root.dataset.menuState, "closed");
  assert.deepEqual(menuStates, [true, false]);
  delete global.window;
  delete global.document;
});

test("bar refresh menu tolerates transient window leave and closes after a grace period", async () => {
  const listeners = {};
  const root = { dataset: {} };
  const menu = {
    hidden: true,
    style: {
      setProperty() {},
      getPropertyValue() {
        return "";
      },
    },
    contains() {
      return false;
    },
  };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=codex" },
    innerWidth: 260,
    innerHeight: 40,
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
    },
  };
  global.document = {
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === "#bar-menu") return menu;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-menu-leave`);
  await new Promise((resolve) => setImmediate(resolve));

  listeners.contextmenu?.({
    clientX: 120,
    clientY: 20,
    target: root,
    preventDefault() {},
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(menu.hidden, false);

  listeners.mouseout?.({ relatedTarget: null });
  assert.equal(menu.hidden, false);

  listeners.mouseover?.({});
  await new Promise((resolve) => setTimeout(resolve, 150));
  assert.equal(menu.hidden, false);

  listeners.mouseout?.({ relatedTarget: null });
  await new Promise((resolve) => setTimeout(resolve, 150));
  assert.equal(menu.hidden, true);

  delete global.window;
  delete global.document;
});

test("bar refresh menu closes when another tool bar opens its menu", async () => {
  const listeners = {};
  const eventHandlers = {};
  const emissions = [];
  const root = { dataset: {} };
  const menu = {
    hidden: true,
    style: {
      setProperty() {},
      getPropertyValue() {
        return "";
      },
    },
    contains() {
      return false;
    },
  };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=claude" },
    innerWidth: 260,
    innerHeight: 40,
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
        async emit(name, payload) {
          emissions.push({ name, payload });
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
      if (selector === "#bar-menu") return menu;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-menu-cross-window`);
  await new Promise((resolve) => setImmediate(resolve));

  listeners.contextmenu?.({
    clientX: 120,
    clientY: 20,
    target: root,
    preventDefault() {},
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(menu.hidden, false);
  assert.deepEqual(emissions.at(-1), {
    name: "bar-refresh-menu-opened",
    payload: { tool: "claude" },
  });

  eventHandlers["bar-refresh-menu-opened"]?.({ payload: { tool: "codex" } });
  assert.equal(menu.hidden, true);

  delete global.window;
  delete global.document;
});

test("bar renders the current tool before event subscription resolves", async () => {
  const windowListeners = {};
  const root = { dataset: {}, style: { setProperty() {} } };
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=codex" },
    addEventListener(name, handler) {
      windowListeners[name] = handler;
    },
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
  windowListeners.pagehide?.();
  assert.equal(root.dataset.tool, undefined);
  delete global.window;
  delete global.document;
});

test("bar render exposes ring CSS variables on each tool for quad mode", async () => {
  const root = { dataset: {}, style: { setProperty() {} } };
  const tooltipCalls = [];
  const tools = {
    claude: toolStub(),
    codex: toolStub(),
  };

  global.window = {
    location: { search: "?tool=claude" },
    __TAURI__: {
      core: {
        async invoke(command, args) {
          if (command === "get_settings") return { language: "ko", bar_mode: "quad" };
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
          if (command === "set_taskbar_tooltip") {
            tooltipCalls.push(args);
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
  assert.equal(tools.claude.style.getPropertyValue("--tool-brand"), "#d79a32");
  assert.equal(tools.claude.style.getPropertyValue("--primary-color"), "#f59e0b");
  assert.equal(tools.claude.style.getPropertyValue("--primary-arc"), "43.2deg");
  assert.equal(tools.claude.style.getPropertyValue("--primary-dash"), "12");
  assert.equal(tools.claude.style.getPropertyValue("--primary-percent"), "12%");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-color"), "#d36b86");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-arc"), "212.4deg");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-dash"), "59");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-percent"), "59%");
  assert.equal(tools.claude.textContentFor(".quad-primary-number"), "12");
  assert.equal(tools.claude.textContentFor(".quad-secondary-number"), "59");
  assert.deepEqual(tooltipCalls.at(-1), {
    tool: "claude",
    text: "Claude\n5h –\n주간 –",
  });
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
            return {
              bar_mode: "quad",
              indicator_style: "bar",
              limit_order: "secondary_first",
              language: "ko",
            };
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
  assert.equal(tools.claude.style.getPropertyValue("--tool-brand"), "#d79a32");
  assert.equal(tools.claude.textContentFor(".primary-text"), "주간 59%");
  assert.equal(tools.claude.textContentFor(".secondary-text"), "5h 12%");
  assert.equal(tools.claude.style.getPropertyValue("--primary-color"), "#d36b86");
  assert.equal(tools.claude.style.getPropertyValue("--primary-arc"), "212.4deg");
  assert.equal(tools.claude.style.getPropertyValue("--primary-dash"), "59");
  assert.equal(tools.claude.style.getPropertyValue("--primary-percent"), "59%");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-color"), "#f59e0b");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-arc"), "43.2deg");
  assert.equal(tools.claude.style.getPropertyValue("--secondary-dash"), "12");
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
  const windowListeners = {};
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
    addEventListener(name, handler) {
      windowListeners[name] = handler;
    },
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
  windowListeners.pagehide?.();
  delete global.window;
  delete global.document;
});

test("bar retries status listener and ignores an older initial response", async () => {
  const eventHandlers = {};
  const attempts = new Map();
  const root = { dataset: {}, style: { setProperty() {} } };
  const tools = { claude: toolStub(), codex: toolStub() };
  let resolveStatus;
  const pendingStatus = new Promise((resolve) => {
    resolveStatus = resolve;
  });

  global.window = {
    location: { search: "?tool=codex" },
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") return {};
          if (command === "get_status") return pendingStatus;
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
    addEventListener() {},
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-listener-retry`);
  await new Promise((resolve) => setTimeout(resolve, 130));
  eventHandlers["status-updated"]?.({
    payload: [{
      tool: "codex",
      captured_at: "2026-07-13T00:00:00Z",
      primary: { used_percent: 10 },
      secondary: { used_percent: 20 },
      session: { active: true },
    }],
  });
  resolveStatus([{
    tool: "codex",
    captured_at: "2026-07-12T00:00:00Z",
    primary: { used_percent: 90 },
    secondary: { used_percent: 90 },
    session: { active: true },
  }]);
  await new Promise((resolve) => setImmediate(resolve));

  assert.match(tools.codex.textContentFor(".primary-text"), /90%/);
  assert.ok(attempts.get("status-updated") >= 2);
  delete global.window;
  delete global.document;
});

test("bar admits only one refresh invoke while a refresh is in flight", async () => {
  const listeners = {};
  const refreshButton = {
    handlers: {},
    addEventListener(name, handler) {
      this.handlers[name] = handler;
    },
  };
  const root = { dataset: {}, style: { setProperty() {} } };
  const tools = { claude: toolStub(), codex: toolStub() };
  const refreshResolvers = [];
  let refreshInvocations = 0;

  global.window = {
    location: { search: "?tool=codex" },
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") return {};
          if (command === "get_status") return [];
          if (command === "get_taskbar_orientation") return "horizontal";
          if (command === "refresh_status") {
            refreshInvocations += 1;
            return new Promise((resolve) => refreshResolvers.push(resolve));
          }
          return null;
        },
      },
      event: { async listen() {} },
    },
  };
  global.document = {
    addEventListener(name, handler) {
      listeners[name] = handler;
    },
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-bar-action="refresh"]') return refreshButton;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  await import(`./bar.js?test=${Date.now()}-refresh-admission`);
  await new Promise((resolve) => setImmediate(resolve));
  for (let index = 0; index < 20; index += 1) {
    refreshButton.handlers.click?.({ preventDefault() {} });
  }
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(refreshInvocations, 1);

  refreshResolvers[0]();
  await new Promise((resolve) => setImmediate(resolve));
  refreshButton.handlers.click?.({ preventDefault() {} });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(refreshInvocations, 2);
  refreshResolvers[1]();

  delete global.window;
  delete global.document;
});

test("bar bounds pending listeners, cleans late registrations, and preserves newer settings", async () => {
  const windowListeners = {};
  const eventHandlers = {};
  const pendingListenerResolvers = [];
  const attempts = new Map();
  const root = { dataset: {}, style: { setProperty() {} } };
  const tools = { claude: toolStub(), codex: toolStub() };
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
    location: { search: "?tool=codex" },
    addEventListener(name, handler) {
      windowListeners[name] = handler;
    },
    __TAURI__: {
      core: {
        async invoke(command) {
          if (command === "get_settings") return pendingSettings;
          if (command === "get_status") {
            return [{
              tool: "codex",
              captured_at: new Date().toISOString(),
              primary: { used_percent: 10 },
              secondary: { used_percent: 20 },
              session: { active: true },
            }];
          }
          if (command === "get_taskbar_orientation") return "horizontal";
          return null;
        },
      },
      event: {
        listen(name, handler) {
          attempts.set(name, (attempts.get(name) ?? 0) + 1);
          if (name === "status-updated") {
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
    addEventListener() {},
    querySelector(selector) {
      if (selector === "#bar") return root;
      if (selector === '[data-tool="claude"]') return tools.claude;
      if (selector === '[data-tool="codex"]') return tools.codex;
      return null;
    },
  };

  try {
    await import(`./bar.js?test=${Date.now()}-listener-timeout`);
    await new Promise((resolve) => setImmediate(resolve));
    eventHandlers["settings-updated"]?.({ payload: { display_basis: "used" } });
    rejectSettings(new Error("late bootstrap failure"));
    await new Promise((resolve) => setImmediate(resolve));
    assert.match(tools.codex.textContentFor(".primary-text"), /10%/);

    await new Promise((resolve) => setTimeout(resolve, 1_950));
    assert.equal(attempts.get("status-updated"), 3);
    assert.equal(fallbackIntervals, 1);

    pendingListenerResolvers[0](() => {
      lateUnlistenCalls += 1;
    });
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(lateUnlistenCalls, 1);

    windowListeners.beforeunload?.();
    await new Promise((resolve) => setImmediate(resolve));
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
