import assert from "node:assert/strict";
import test from "node:test";
import vm from "node:vm";
import { readFileSync } from "node:fs";
import { DEFAULT_SETTINGS } from "./panel-state.js";
import { barViewModel } from "./bar-state.js";
import { createSettingsAuthority } from "./settings-revision.js";
import { createStatusRequests } from "./status-requests.js";

const source = readFileSync(new URL("./bar.js", import.meta.url), "utf8").replaceAll("\r\n", "\n");
const settle = () => new Promise(resolve => setImmediate(resolve));

function between(first, last) {
  const start = source.indexOf(first);
  const end = source.indexOf(last, start + first.length);
  assert.ok(start >= 0 && end > start, first);
  return source.slice(start, end);
}

function body(name) {
  const match = source.match(new RegExp(`(?:async )?function ${name}\\b[\\s\\S]*?\\n}\\n`));
  assert.ok(match, `${name} must come from bar.js`);
  return match[0];
}

function deferred() {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

function fixture(invoke = async () => null) {
  const calls = [], timers = new Map();
  let clock = 1_700_000_000_000;
  const root = { dataset: {}, style: { setProperty() {} } };
  const menu = { hidden: true, style: { setProperty() {} } };
  const item = { hidden: false, scrollWidth: 120, scrollHeight: 48,
    getBoundingClientRect: () => ({ width: 120, height: 48 }) };
  class FakeDate extends Date {
    constructor(...args) { super(...(args.length ? args : [clock])); }
    static now() { return clock; }
  }
  const context = vm.createContext({
    DEFAULT_SETTINGS, barViewModel, createSettingsAuthority, createStatusRequests, URLSearchParams,
    Date: FakeDate,
    document: {
      documentElement: { clientWidth: 400, clientHeight: 48 },
      querySelector: selector => selector === "#bar" ? root : selector === "#bar-menu" ? menu : null,
    },
    window: { location: { search: "?tool=codex" }, innerWidth: 400, innerHeight: 48,
      devicePixelRatio: 1, __TAURI__: { core: {
        invoke(command, args) { calls.push({ command, args }); return invoke(command, args); },
      } } },
    setTimeout(callback, delay) {
      const timer = { callback, delay, due: clock + delay, unref() {} };
      timers.set(timer, timer);
      return timer;
    },
    clearTimeout(timer) { timers.delete(timer); },
    clearInterval() {},
    systemTextScale: { factor: 1, dispose() {} },
    stopTooltipClock() {}, fitRingNumbers() {},
    toolElement: tool => tool === "codex" ? item : null,
    renderTool(model) { if (model.tool === "codex") item.hidden = false; },
  });
  for (const name of ["closeRefreshMenu", "cancelRefreshMenuRecovery", "retryRefreshMenuClose"]) body(name);
  // Menu state, recovery helpers, native IPC, and render/cleanup remain source-exact.
  vm.runInContext(`
    ${between("let settings =", 'document.addEventListener("contextmenu"')}
    ${between("function tauriApi()", "function setText(")}
    ${body("scheduleTaskbarContentWidthSync")}
    ${body("syncNativeTooltip")}
    let currentBarView = null;
    ${body("renderBar")}
    ${body("unlistenSafely")}
    ${body("cleanupListeners")}
    pendingNativeTooltip = {tool: CURRENT_TOOL, text: "fixture usage"};
    globalThis.menuApi = {
      show: () => showRefreshMenu({clientX: 10, clientY: 10}),
      hide: hideRefreshMenu, render: renderBar, cleanup: cleanupListeners,
      inspect: () => ({state: refreshMenuState, desired: refreshMenuDesiredOpen,
        disposed: listenersDisposed, request: refreshMenuRequest,
        recoveryTimer: refreshMenuRecoveryTimer, recoveryAt: refreshMenuRecoveryAt,
        inFlight: refreshMenuCloseInFlight}),
    };
  `, context, { filename: "taskbar-menu-recovery.source-exact.vm.js" });

  return {
    api: context.menuApi, menu, calls, timers,
    closeCalls: () => calls.filter(call => call.command === "set_taskbar_menu_open" && !call.args.open),
    tooltipCalls: () => calls.filter(call => call.command === "set_taskbar_tooltip"),
    widthCalls: () => calls.filter(call => call.command === "set_taskbar_content_width"),
    async advance(ms) {
      const target = clock + ms;
      let fired = 0;
      while (true) {
        const timer = [...timers.values()].filter(value => value.due <= target)
          .sort((first, second) => first.due - second.due)[0];
        if (!timer) break;
        assert.ok(++fired <= 30, "timer loop must remain bounded");
        clock = timer.due;
        timers.delete(timer);
        timer.callback();
        await settle();
      }
      clock = target;
      await settle();
    },
  };
}

test("failed menu close retries automatically and restores tooltip and width only after ACK", async () => {
  let closes = 0;
  const h = fixture((command, args) => {
    if (command === "set_taskbar_menu_open" && !args.open && ++closes === 1) {
      throw new Error("synthetic settings read failure before native close");
    }
    return null;
  });
  await h.api.show();
  h.api.hide(); await settle();
  assert.equal(h.api.inspect().state, "closing");
  assert.equal(h.menu.hidden, true);
  assert.equal(h.tooltipCalls().length, 0);
  assert.equal(h.widthCalls().length, 0);
  assert.equal(h.timers.size, 1);
  await h.advance(249);
  assert.equal(closes, 1);
  await h.advance(1);
  assert.equal(closes, 2);
  assert.equal(h.api.inspect().state, "closed");
  assert.equal(h.api.inspect().recoveryTimer, null);
  assert.equal(h.tooltipCalls().length, 1);
  await h.advance(80);
  assert.equal(h.widthCalls().length, 1);
  assert.equal(h.timers.size, 0);
  h.api.cleanup();
});

for (const outcome of ["success", "rejection"]) {
  test(`retired open-failure cleanup ${outcome} cannot overwrite a reopened menu`, async () => {
    const oldClose = deferred();
    let opens = 0, closes = 0;
    const h = fixture((command, args) => {
      if (command !== "set_taskbar_menu_open") return null;
      if (args.open && ++opens === 1) throw new Error("synthetic initial geometry failure");
      if (!args.open && ++closes === 1) return oldClose.promise;
      return null;
    });
    const firstOpen = h.api.show(); await settle();
    assert.equal(h.api.inspect().state, "closing");
    assert.equal(closes, 1);
    await h.api.show();
    const request = h.api.inspect().request;
    if (outcome === "success") oldClose.resolve(null);
    else oldClose.reject(new Error("synthetic delayed close failure"));
    await firstOpen; await settle();
    assert.equal(h.api.inspect().state, "open");
    assert.equal(h.api.inspect().desired, true);
    assert.equal(h.api.inspect().request, request);
    assert.equal(h.menu.hidden, false);
    assert.equal(h.timers.size, 0);
    assert.equal(h.tooltipCalls().length + h.widthCalls().length, 0);
    h.api.hide(); await settle();
    assert.equal(closes, 2, "the reopened menu still owns a native close");
    assert.equal(h.api.inspect().state, "closed");
    h.api.cleanup();
    assert.equal(h.timers.size, 0);
  });
}

test("all close failures bound retries and keep native ownership truthful through cooldown", async () => {
  const h = fixture((command, args) => {
    if (command === "set_taskbar_menu_open" && !args.open) return Promise.reject(new Error("synthetic close failure"));
    return null;
  });
  await h.api.show(); h.api.hide(); await settle();
  await h.advance(750);
  assert.equal(h.closeCalls().length, 4, "initial close plus three 250ms retries");
  assert.equal(h.api.inspect().state, "closing");
  assert.equal(h.menu.hidden, true);
  assert.equal(h.api.inspect().recoveryTimer, null);
  assert.equal(h.api.inspect().inFlight, null);
  assert.equal(h.timers.size, 0, "exhaustion must not leave an unbounded timer loop");
  for (let i = 0; i < 10; i++) h.api.render();
  await h.advance(4999); h.api.render(); await settle();
  assert.equal(h.closeCalls().length, 4);
  assert.equal(h.tooltipCalls().length + h.widthCalls().length, 0);
  await h.advance(1);
  for (let i = 0; i < 10; i++) h.api.render();
  await settle();
  assert.equal(h.closeCalls().length, 5, "render recovery remains single-flight");
  await h.advance(750);
  assert.equal(h.closeCalls().length, 8, "each recovery burst also has only three retries");
  assert.equal(h.api.inspect().state, "closing");
  assert.equal(h.timers.size, 0);
  assert.equal(h.tooltipCalls().length + h.widthCalls().length, 0);
  h.api.cleanup(); await settle();
  assert.equal(h.timers.size, 0);
});

test("a later render recovers exhausted close after five seconds without another dismissal", async () => {
  let closes = 0;
  const h = fixture((command, args) => {
    if (command === "set_taskbar_menu_open" && !args.open && ++closes <= 4) throw new Error("synthetic transient close error");
    return null;
  });
  await h.api.show(); h.api.hide(); await settle(); await h.advance(750);
  const request = h.api.inspect().request;
  await h.advance(4999); h.api.render(); await settle();
  assert.equal(closes, 4);
  await h.advance(1); h.api.render(); await settle();
  assert.equal(closes, 5);
  assert.equal(h.api.inspect().request, request, "recovery must use the existing dismissal owner");
  assert.equal(h.api.inspect().state, "closed");
  assert.equal(h.tooltipCalls().length, 1);
  await h.advance(80);
  assert.equal(h.widthCalls().length, 1);
  assert.equal(h.timers.size, 0);
  h.api.cleanup();
});

test("reopening cancels recovery and a queued cancelled retry cannot dispatch IPC", async () => {
  const h = fixture((command, args) => {
    if (command === "set_taskbar_menu_open" && !args.open) throw new Error("synthetic close error");
    return null;
  });
  await h.api.show(); h.api.hide(); await settle();
  const retired = h.api.inspect().recoveryTimer;
  assert.ok(retired);
  await h.api.show();
  assert.equal(h.timers.has(retired), false);
  assert.equal(h.api.inspect().recoveryTimer, null);
  const calls = h.calls.length;
  retired.callback(); await settle(); await h.advance(6000); h.api.render(); await settle();
  assert.equal(h.closeCalls().length, 1);
  assert.equal(h.calls.slice(calls).filter(call => call.command === "set_taskbar_menu_open").length, 0);
  assert.equal(h.api.inspect().state, "open");
  assert.equal(h.menu.hidden, false);
  h.api.cleanup(); await settle();
  assert.equal(h.timers.size, 0);
});

test("queued cancelled retry preserves the new dismissal's recovery timer and owner", async () => {
  let closes = 0;
  const h = fixture((command, args) => {
    if (command === "set_taskbar_menu_open" && !args.open && ++closes <= 2) throw new Error("synthetic close error");
    return null;
  });
  await h.api.show(); h.api.hide(); await settle();
  const retired = h.api.inspect().recoveryTimer;
  h.api.hide(); await settle();
  const current = h.api.inspect().recoveryTimer;
  assert.ok(retired && current);
  assert.notEqual(current, retired);
  assert.equal(h.timers.has(retired), false);
  const calls = h.calls.length;
  retired.callback(); await settle();
  assert.equal(h.calls.length, calls, "queued old callbacks must not send native close");
  assert.equal(h.api.inspect().recoveryTimer, current, "queued old callbacks must not erase the new timer pointer");
  assert.equal(h.timers.has(current), true);
  await h.advance(250);
  assert.equal(closes, 3);
  assert.equal(h.api.inspect().state, "closed");
  await h.advance(80); h.api.cleanup();
  assert.equal(h.timers.size, 0);
});

test("disposal cancels recovery and queued timers/render cannot restart native IPC", async () => {
  const h = fixture((command, args) => {
    if (command === "set_taskbar_menu_open" && !args.open) throw new Error("synthetic close error");
    return null;
  });
  await h.api.show(); h.api.hide(); await settle();
  const retired = h.api.inspect().recoveryTimer;
  assert.ok(retired);
  h.api.cleanup(); h.api.cleanup(); await settle();
  assert.equal(h.api.inspect().disposed, true);
  assert.equal(h.timers.size, 0);
  const calls = h.calls.length;
  retired.callback(); h.api.render(); h.api.hide(); await h.api.show(); await h.advance(6000);
  assert.equal(h.calls.length, calls);
  assert.equal(h.menu.hidden, true);
  assert.equal(h.timers.size, 0);
});

for (const outcome of ["success", "rejection"]) {
  test(`late in-flight native close ${outcome} after disposal cannot mutate or retry`, async () => {
    const pending = deferred();
    const h = fixture((command, args) => command === "set_taskbar_menu_open" && !args.open ? pending.promise : null);
    await h.api.show(); h.api.hide(); await settle(); h.api.cleanup();
    const calls = h.calls.length;
    if (outcome === "success") pending.resolve(null);
    else pending.reject(new Error("synthetic late close failure"));
    await settle(); await h.advance(6000); h.api.render();
    assert.equal(h.calls.length, calls);
    assert.equal(h.api.inspect().state, "closed");
    assert.equal(h.api.inspect().disposed, true);
    assert.equal(h.api.inspect().recoveryTimer, null);
    assert.equal(h.menu.hidden, true);
    assert.equal(h.timers.size, 0);
  });
}
