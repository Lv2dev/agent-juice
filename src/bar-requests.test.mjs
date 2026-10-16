import assert from "node:assert/strict";
import test from "node:test";
import vm from "node:vm";
import { readFileSync } from "node:fs";
import { DEFAULT_SETTINGS } from "./panel-state.js";
import { barViewModel } from "./bar-state.js";
import { createSettingsAuthority } from "./settings-revision.js";
import { createTextScaleState, TEXT_SCALE_EVENT } from "./text-scale.js";
import { createStatusRequests } from "./status-requests.js";

const source = readFileSync(new URL("./bar.js", import.meta.url), "utf8").replaceAll("\r\n", "\n");
function between(first, last) {
  const start = source.indexOf(first), end = source.indexOf(last, start + first.length);
  assert.ok(start >= 0 && end > start, first);
  return source.slice(start, end);
}
function deferred() {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
const settle = () => new Promise(resolve => setImmediate(resolve));
const status = value => [{
  schema_version: "agent_status.v1", pc_id: "fixture-pc", tool: "codex", session_id: "app-server-account",
  captured_at: new Date().toISOString(), primary: { label: "week", used_percent: value, resets_at: null },
  secondary: null, session: {active: true, context_used_percent: null}, approx: false, cost_estimate_usd: null,
}];

function harness(invoke = async () => null) {
  const commands = [], renders = [], events = new Map(), timers = new Map(), intervals = new Map();
  let context;
  const root = {style: {setProperty() {}}, dataset: new Proxy({}, {
    set(target, name, value) {
      target[name] = value;
      if (name === "mode") renders.push(JSON.parse(JSON.stringify(context.bar.inspect())));
      return true;
    },
  })};
  const menu = {hidden: true, style: {setProperty() {}}};
  context = vm.createContext({
    DEFAULT_SETTINGS, createSettingsAuthority, TEXT_SCALE_EVENT, URLSearchParams, barViewModel,
    createStatusRequests: options => createStatusRequests({...options, setTimer: context.setTimeout, clearTimer: context.clearTimeout}),
    window: {location: {search: "?tool=codex"}, __TAURI__: {
      core: {invoke(name, args) { commands.push(name); return invoke(name, args); }},
      event: {listen(name, handler) {events.set(name, handler); return () => {}; }},
    }},
    document: {querySelector: selector => selector === "#bar" ? root : selector === "#bar-menu" ? menu : null, documentElement: {clientHeight: 48, clientWidth: 300}},
    setTimeout(callback, delay) {const timer = {callback, delay, unref() {}}; timers.set(timer, timer); return timer;},
    clearTimeout(timer) {timers.delete(timer);},
    setInterval(callback, delay) {const timer = {callback, delay, unref() {}}; intervals.set(timer, timer); return timer;},
    clearInterval(timer) {intervals.delete(timer);},
    systemTextScale: createTextScaleState(() => {}, () => root),
    applyTheme() {}, applyFont() {}, applyTranslations() {},
    stopTooltipClock() {}, observeTaskbarContentSize() {}, scheduleTaskbarContentWidthSync() {},
    currentBarIsVisible: () => true, syncNativeTooltip() {}, toolElement: () => null, renderTool() {}, setDragging() {},
  });
  vm.runInContext(`
    ${between("let settings =", 'document.addEventListener("contextmenu"')}
    ${between("function tauriApi()", "function refreshTaskbarStatus()")}
    ${between("function refreshTaskbarStatus()", "function setText(")}
    let currentBarView = null;
    ${between("function renderBar()", "function setDragging(")}
    ${between("async function loadSettings()", "async function bootstrap()")}
    globalThis.bar = {loadStatus, loadSettings, loadTaskbarOrientation, cleanup: cleanupListeners,
      fallback: scheduleSnapshotFallback, bind: bindEvents, showMenu: showRefreshMenu,
      inspect: () => ({statuses, health: collectionHealth, loading: startupStatusLoading, disposed: listenersDisposed,
        menuState: refreshMenuState, desiredMenu: refreshMenuDesiredOpen})};
  `, context, {filename: "bar-requests.vm.js"});
  return {bar: context.bar, commands, renders, events, timers, intervals, menu,
    async timeout(delay) {
      const timer = [...timers.values()].find(value => value.delay === delay);
      assert.ok(timer, `timer ${delay}`); timers.delete(timer); timer.callback(); await settle();
    }};
}

test("bar reverse RPC replies and stale failures cannot overwrite a newer quota", async () => {
  for (const fails of [false, true]) {
    const first = deferred(), second = deferred(); let calls = 0;
    const h = harness(command => command === "get_status" ? (++calls === 1 ? first.promise : second.promise) : {codex: "ready"});
    const old = h.bar.loadStatus(), next = h.bar.loadStatus();
    second.resolve(status(90)); await next;
    if (fails) first.reject(new Error("old fixture failure")); else first.resolve(status(10));
    await old; await settle();
    assert.equal(h.bar.inspect().statuses[0].primary.used_percent, 90);
    assert.equal(h.renders.length, 1);
    assert.equal(h.timers.size, 0);
  }
});

test("bar valid quota leaves startup loading after a bounded missing health reply", async () => {
  const health = deferred();
  const h = harness(command => command === "get_status" ? status(10) : health.promise);
  const load = h.bar.loadStatus(); await settle();
  assert.equal(h.bar.inspect().loading, true);
  await h.timeout(2_000); await load;
  assert.equal(h.bar.inspect().loading, false);
  assert.equal(h.bar.inspect().statuses[0].primary.used_percent, 10);
  assert.equal(h.renders.length, 1);
  health.resolve({codex: "late"}); await settle();
  assert.equal(Object.keys(h.bar.inspect().health).length, 0);
});

test("bar disposal cancels pending status and health without follow-up IPC or rendering", async () => {
  for (const phase of ["status", "health"]) {
    const response = deferred();
    const h = harness(command => command === "get_status" && phase === "health" ? status(10) : response.promise);
    const load = h.bar.loadStatus(); await settle();
    h.bar.cleanup(); await load;
    const calls = h.commands.length;
    response.resolve(phase === "status" ? status(10) : {codex: "ready"}); await settle();
    await h.bar.loadStatus();
    assert.equal(h.commands.length, calls);
    assert.equal(h.renders.length, 0);
    assert.equal(Object.keys(h.bar.inspect().health).length, 0);
    assert.equal(h.timers.size, 0);
  }
});

test("bar queued fallback and native events cannot restart work after disposal", async () => {
  const h = harness(); h.bar.bind(); await settle();
  h.bar.fallback(); const tick = [...h.intervals.values()][0].callback;
  h.bar.cleanup(); h.commands.length = 0; tick();
  h.events.get("status-updated")({payload: status(90)});
  h.events.get("collection-health-updated")({payload: {codex: "ready"}});
  h.events.get("settings-updated")({payload: {...DEFAULT_SETTINGS, settings_revision: "1"}});
  await settle();
  assert.deepEqual(h.commands, []);
  assert.equal(h.renders.length, 0);
  assert.equal(h.intervals.size, 0);
});

test("bar native status and health events still win over pending RPC snapshots", async () => {
  const response = deferred();
  const h = harness(command => command === "get_status" ? response.promise : {codex: "ready"});
  h.bar.bind(); await settle();
  const pending = h.bar.loadStatus();
  h.events.get("status-updated")({payload: status(90)});
  response.resolve(status(10)); await pending;
  assert.equal(h.bar.inspect().statuses[0].primary.used_percent, 90);
  assert.deepEqual(h.commands, ["get_status"]);
  const health = deferred();
  const other = harness(command => command === "get_status" ? status(10) : health.promise);
  other.bar.bind(); await settle();
  const loaded = other.bar.loadStatus(); await settle();
  other.events.get("collection-health-updated")({payload: {codex: "current"}});
  health.resolve({codex: "old"}); await loaded;
  assert.equal(other.bar.inspect().health.codex, "current");
});

test("bar late settings and orientation cannot mutate or render a disposed root", async () => {
  for (const command of ["get_settings", "get_taskbar_orientation"]) {
    const response = deferred(); const h = harness(() => response.promise);
    const pending = command === "get_settings" ? h.bar.loadSettings() : h.bar.loadTaskbarOrientation();
    h.bar.cleanup(); response.resolve(command === "get_settings" ? {...DEFAULT_SETTINGS, settings_revision: "1"} : "vertical");
    await pending;
    assert.equal(h.renders.length, 0);
    assert.deepEqual(h.commands, [command]);
  }
});

test("bar pending menu opening cannot resurrect local state after owned teardown", async () => {
  const response = deferred(), menuCalls = [];
  const h = harness((command, args) => {
    if (command === "set_taskbar_menu_open") {
      menuCalls.push(args.open);
      return args.open ? response.promise : true;
    }
    return null;
  });
  h.bar.bind();
  await settle();
  const pending = h.bar.showMenu({clientX: 10, clientY: 10});
  h.bar.cleanup(); response.resolve(true); await pending; await settle();
  assert.deepEqual(menuCalls, [true, false]);
  assert.equal(h.menu.hidden, true);
  assert.equal(h.bar.inspect().menuState, "closed");
  assert.equal(h.bar.inspect().desiredMenu, false);
  const calls = h.commands.length;
  await h.bar.showMenu({clientX: 10, clientY: 10});
  assert.equal(h.commands.length, calls);
  assert.equal(h.renders.length, 0);
});
