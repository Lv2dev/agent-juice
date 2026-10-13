import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { DEFAULT_SETTINGS } from "./panel-state.js";
import { createSettingsAuthority } from "./settings-revision.js";
import { createTextScaleState, TEXT_SCALE_EVENT } from "./text-scale.js";
import { createStatusRequests } from "./status-requests.js";

const source = readFileSync(new URL("./panel.js", import.meta.url), "utf8").replace(/\r\n/g, "\n");

function between(first, next) {
  const start = source.indexOf(first);
  const end = source.indexOf(next, start + first.length);
  assert.ok(start >= 0 && end > start, first);
  return source.slice(start, end);
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

const settle = () => new Promise((resolve) => setImmediate(resolve));
const snapshot = { days: [{ date: "2026-10-13", tokens: 42 }], partial: false };

function panelHarness({ invoke = async () => null, listen } = {}) {
  const commands = [];
  const renders = { activity: 0, statuses: [] };
  const nativeEvents = new Map();
  const windowEvents = new Map();
  const documentEvents = new Map();
  const intervals = new Map();
  const timeouts = new Map();
  let unlistenCalls = 0;
  const root = { dataset: {}, style: { setProperty() {} } };
  const document = {
    hidden: false,
    documentElement: root,
    addEventListener(name, handler) { documentEvents.set(name, handler); },
    querySelectorAll: () => [],
  };
  const context = vm.createContext({
    DEFAULT_SETTINGS, createSettingsAuthority, TEXT_SCALE_EVENT,
    createStatusRequests: options => createStatusRequests({
      ...options, setTimer: context.setTimeout, clearTimer: context.clearTimeout,
    }),
    createTextScaleState: (onChange) => createTextScaleState(onChange, () => root),
    createBarPreview: () => ({ update() {}, setActive() {} }),
    document,
    window: {
      addEventListener(name, handler) { windowEvents.set(name, handler); },
      __TAURI__: {
        core: { invoke(command) { commands.push(command); return invoke(command); } },
        event: { listen: listen ?? ((name, handler) => {
          nativeEvents.set(name, handler);
          return () => { unlistenCalls += 1; };
        }) },
      },
    },
    setInterval(callback, delay) {
      const timer = { callback, delay, unref() {} };
      intervals.set(timer, timer);
      return timer;
    },
    clearInterval(timer) { intervals.delete(timer); },
    setTimeout(callback, delay) {
      const timer = { callback, delay };
      timeouts.set(timer, timer);
      return timer;
    },
    clearTimeout(timer) { timeouts.delete(timer); },
    applyTheme() {}, applyPanelSkin() {}, applyFont() {}, applyTranslations() {},
    hideActivityTooltip() {}, selectPanelView() {}, bindPanelNavigation() {}, bindPanelDragFallback() {},
    renderActivity() { renders.activity += 1; },
    renderStatuses(statuses) { renders.statuses.push(statuses); },
    WINDOW_ACTION_COMMANDS: { close: "hide_panel_window", minimize: "minimize_panel", "toggle-maximize": "toggle_panel_maximized" },
  });
  vm.runInContext(`
    ${between("let settings =", 'document.addEventListener("contextmenu"')}
    ${between("function tauriApi()", "function selectPanelView(")}
    ${between("function bindWindowControls()", "function currentPanelWindow()")}
    ${between("async function loadSettings()", "async function bootstrap()")}
    ${between("async function bootstrap()", "\nbootstrap();")}
    globalThis.panel = {
      setVisible: setPanelVisible, fallback: scheduleSnapshotFallback, poll: scheduleActivityRefresh,
      loadActivity, loadStatus, cleanup: cleanupListeners, bind: bindStatusUpdates,
      bindControls: bindWindowControls, bootstrap,
      seed(value) { activitySnapshot = value; activityDataRevision = 7; lastActivityRenderSignature = "rendered"; },
      inspect: () => ({ snapshot: activitySnapshot, revision: activityDataRevision,
        signature: lastActivityRenderSignature, visible: panelVisible, disposed: listenersDisposed,
        health: collectionHealth }),
    };
  `, context, { filename: "panel-automatic-refresh.vm.js" });

  function interval(delay) {
    const timer = [...intervals.values()].find((item) => item.delay === delay);
    assert.ok(timer, `interval ${delay}`);
    return timer.callback;
  }

  return {
    panel: context.panel, commands, renders, nativeEvents, windowEvents, documentEvents, document,
    intervals, timeouts, interval,
    get unlistenCalls() { return unlistenCalls; },
    async tick(delay) { interval(delay)(); await settle(); },
    async timeout(delay) {
      const timer = [...timeouts.values()].find((item) => item.delay === delay);
      assert.ok(timer, `timeout ${delay}`);
      timeouts.delete(timer);
      timer.callback();
      await settle();
    },
  };
}

test("hidden fallback does not invoke get_status, settings, or text scale", async () => {
  const h = panelHarness();
  h.panel.fallback();
  h.panel.fallback();
  assert.equal(h.intervals.size, 1);
  await h.tick(30_000);
  assert.deepEqual(h.commands, []);
});

test("panel reverse status replies cannot overwrite the latest request", async () => {
  const first = deferred(), second = deferred(); let calls = 0;
  const h = panelHarness({invoke: command => command === "get_status" ? (++calls === 1 ? first.promise : second.promise) : {codex: "ready"}});
  const old = h.panel.loadStatus(), next = h.panel.loadStatus();
  second.resolve([{tool: "codex", value: 90}]); await next;
  first.resolve([{tool: "codex", value: 10}]); await old;
  assert.deepEqual(h.renders.statuses, [[{tool: "codex", value: 90}]]);
  assert.equal(h.timeouts.size, 0);
});

test("panel supplemental health cannot prevent a valid snapshot indefinitely", async () => {
  const health = deferred();
  const h = panelHarness({invoke: command => command === "get_status" ? [{tool: "codex", value: 90}] : health.promise});
  const pending = h.panel.loadStatus(); await settle();
  await h.timeout(2_000); await pending;
  assert.deepEqual(h.renders.statuses, [[{tool: "codex", value: 90}]]);
  health.resolve({codex: "late"}); await settle();
  assert.equal(Object.keys(h.panel.inspect().health).length, 0);
});

test("visible fallback reads all snapshots and follows native visibility changes", async () => {
  const h = panelHarness();
  h.panel.bind();
  await settle();
  h.panel.fallback();
  h.nativeEvents.get("panel-visibility-updated")({ payload: true });
  await settle();
  h.commands.length = 0;
  await h.tick(30_000);
  assert.deepEqual([...h.commands].sort(), ["get_collection_health", "get_settings", "get_status", "get_system_text_scale"]);
  h.nativeEvents.get("panel-visibility-updated")({ payload: false });
  h.commands.length = 0;
  await h.tick(30_000);
  assert.deepEqual(h.commands, []);
});

test("queued fallback and activity ticks do not launch requests after disposal", async () => {
  const h = panelHarness();
  h.panel.setVisible(true);
  await settle();
  h.panel.fallback();
  h.panel.poll();
  const fallback = h.interval(30_000);
  const poll = h.interval(300_000);
  h.panel.cleanup();
  h.commands.length = 0;
  fallback();
  poll();
  await settle();
  assert.deepEqual(h.commands, []);
  h.panel.fallback();
  h.panel.poll();
  assert.equal(h.intervals.size, 0);
});

test("disposal blocks deferred text-scale IPC and follow-up health IPC", async () => {
  const status = deferred();
  const h = panelHarness({ invoke: (command) => command === "get_status" ? status.promise : null });
  h.panel.setVisible(true);
  await settle();
  h.panel.fallback();
  h.commands.length = 0;
  h.interval(30_000)();
  h.panel.cleanup();
  status.resolve([]);
  await settle();
  assert.deepEqual(h.commands, ["get_settings", "get_status"]);
  assert.deepEqual(h.renders.statuses, []);
});

test("a pending status response cannot launch health collection after disposal", async () => {
  const status = deferred();
  const h = panelHarness({ invoke: () => status.promise });
  const pending = h.panel.loadStatus();
  h.panel.cleanup();
  status.resolve([]);
  await pending;
  assert.deepEqual(h.commands, ["get_status"]);
  assert.deepEqual(h.renders.statuses, []);
});

test("pending health and rejected status callbacks do not render after disposal", async () => {
  const health = deferred();
  const h = panelHarness({ invoke: (command) => command === "get_status" ? [] : health.promise });
  const pending = h.panel.loadStatus();
  await settle();
  h.panel.cleanup();
  health.resolve({ codex: "ready" });
  await pending;
  assert.equal(Object.keys(h.panel.inspect().health).length, 0);
  assert.deepEqual(h.renders.statuses, []);

  const status = deferred();
  const failed = panelHarness({ invoke: () => status.promise });
  const rejection = failed.panel.loadStatus();
  failed.panel.cleanup();
  status.reject(new Error("late status failure"));
  await rejection;
  assert.deepEqual(failed.renders.statuses, []);
});

test("late native visibility and focus callbacks cannot launch activity after disposal", async () => {
  const h = panelHarness();
  await h.panel.bootstrap();
  await settle();
  const visibility = h.nativeEvents.get("panel-visibility-updated");
  h.windowEvents.get("pagehide")();
  h.commands.length = 0;
  visibility({ payload: true });
  h.windowEvents.get("focus")();
  await settle();
  assert.deepEqual(h.commands, []);
  assert.equal(h.panel.inspect().visible, false);
  assert.equal(h.unlistenCalls, 6);
  h.windowEvents.get("beforeunload")();
  assert.equal(h.unlistenCalls, 6);
});

test("failed close rollback cannot reopen activity loading after disposal", async () => {
  const close = deferred();
  const h = panelHarness({ invoke: (command) => command === "hide_panel_window" ? close.promise : null });
  h.panel.bindControls();
  h.documentEvents.get("click")({
    target: { closest: (selector) => selector === "[data-window-action]" ? { dataset: { windowAction: "close" } } : null },
    preventDefault() {},
  });
  h.panel.cleanup();
  close.reject(new Error("late close failure"));
  await settle();
  assert.deepEqual(h.commands, ["hide_panel_window"]);
  assert.equal(h.panel.inspect().visible, false);
});

test("native null activity preserves the last snapshot, revision, and render signature", async () => {
  const h = panelHarness();
  h.panel.seed(snapshot);
  await h.panel.loadActivity();
  assert.equal(h.panel.inspect().snapshot, snapshot);
  assert.equal(h.panel.inspect().revision, 7);
  assert.equal(h.panel.inspect().signature, "rendered");
  assert.equal(h.renders.activity, 0);
});

test("native null activity also preserves the initial empty non-error state", async () => {
  const h = panelHarness();
  const initial = h.panel.inspect().snapshot;
  await h.panel.loadActivity();
  assert.equal(h.panel.inspect().snapshot, initial);
  assert.equal(h.panel.inspect().snapshot.partial, false);
  assert.equal(h.panel.inspect().revision, 0);
  assert.equal(h.renders.activity, 0);
});

test("valid activity snapshots still update revision and render", async () => {
  const h = panelHarness({ invoke: async () => snapshot });
  await h.panel.loadActivity();
  assert.equal(h.panel.inspect().snapshot, snapshot);
  assert.equal(h.panel.inspect().revision, 1);
  assert.equal(h.renders.activity, 1);
});

test("undefined and malformed activity responses retain the existing error state", async () => {
  for (const response of [undefined, false, 0, "malformed"]) {
    const h = panelHarness({ invoke: async () => response });
    h.panel.seed(snapshot);
    await h.panel.loadActivity();
    assert.equal(h.panel.inspect().snapshot.days.length, 0);
    assert.equal(h.panel.inspect().snapshot.partial, true);
    assert.equal(h.panel.inspect().revision, 8);
    assert.equal(h.renders.activity, 1);
  }
});

test("activity request failures retain the existing partial error state", async () => {
  const h = panelHarness({ invoke: async () => { throw new Error("activity unavailable"); } });
  h.panel.seed(snapshot);
  await h.panel.loadActivity();
  assert.equal(h.panel.inspect().snapshot.days.length, 0);
  assert.equal(h.panel.inspect().snapshot.partial, true);
  assert.equal(h.panel.inspect().revision, 8);
  assert.equal(h.renders.activity, 1);
});

test("pending activity success and error callbacks preserve state after disposal", async () => {
  for (const fails of [false, true]) {
    const response = deferred();
    const h = panelHarness({ invoke: () => response.promise });
    h.panel.seed(snapshot);
    const pending = h.panel.loadActivity();
    h.panel.cleanup();
    if (fails) response.reject(new Error("late activity failure"));
    else response.resolve({ days: [], partial: false });
    await pending;
    assert.equal(h.panel.inspect().snapshot, snapshot);
    assert.equal(h.panel.inspect().revision, 7);
    assert.equal(h.renders.activity, 0);
  }
});

test("newer activity events still win over pending success, null, and error replies", async () => {
  for (const responseKind of ["snapshot", "null", "error"]) {
    const response = deferred();
    const h = panelHarness({ invoke: () => response.promise });
    h.panel.bind();
    await settle();
    const pending = h.panel.loadActivity();
    h.nativeEvents.get("activity-updated")({ payload: snapshot });
    if (responseKind === "error") response.reject(new Error("stale failure"));
    else response.resolve(responseKind === "null" ? null : { days: [], partial: false });
    await pending;
    assert.equal(h.panel.inspect().snapshot, snapshot);
    assert.equal(h.panel.inspect().revision, 1);
    assert.equal(h.renders.activity, 1);
  }
});

test("300-second activity polling remains visibility-gated and deduplicates in-flight loads", async () => {
  const response = deferred();
  let result = response.promise;
  const h = panelHarness({ invoke: () => result });
  h.panel.poll();
  h.panel.poll();
  assert.equal(h.intervals.size, 1);
  await h.tick(300_000);
  assert.deepEqual(h.commands, []);
  h.panel.setVisible(true);
  await h.tick(300_000);
  assert.deepEqual(h.commands, ["get_activity"]);
  response.resolve(null);
  await settle();
  result = snapshot;
  await h.tick(300_000);
  assert.deepEqual(h.commands, ["get_activity", "get_activity"]);
  assert.equal(h.panel.inspect().snapshot, snapshot);
  h.panel.setVisible(false);
  await h.tick(300_000);
  assert.equal(h.commands.length, 2);
});

test("bootstrap preserves initial snapshot reads and focus-triggered activity loading", async () => {
  const h = panelHarness();
  await h.panel.bootstrap();
  await settle();
  assert.deepEqual([...h.commands].sort(), ["get_collection_health", "get_settings", "get_status", "get_system_text_scale"]);
  assert.equal(h.panel.inspect().visible, false);
  h.windowEvents.get("focus")();
  await settle();
  assert.equal(h.commands.filter((command) => command === "get_activity").length, 1);
  h.document.hidden = true;
  h.documentEvents.get("visibilitychange")();
  assert.equal(h.panel.inspect().visible, false);
});

test("listener registration retains three retries and then enables fallback", async () => {
  const attempts = new Map();
  const h = panelHarness({ listen: (name) => {
    attempts.set(name, (attempts.get(name) ?? 0) + 1);
    throw new Error("listener unavailable");
  } });
  h.panel.bind();
  await settle();
  for (let index = 0; index < 6; index += 1) await h.timeout(100);
  for (let index = 0; index < 6; index += 1) await h.timeout(250);
  assert.equal(attempts.size, 6);
  assert.ok([...attempts.values()].every((count) => count === 3));
  assert.equal(h.intervals.size, 1);
  await h.tick(30_000);
  assert.deepEqual(h.commands, []);
});

test("late listener registrations are unlistened and cannot restart retries after disposal", async () => {
  const registration = deferred();
  let attempts = 0;
  let unlistenCalls = 0;
  const h = panelHarness({ listen: () => { attempts += 1; return registration.promise; } });
  h.panel.bind();
  h.panel.cleanup();
  registration.resolve(() => { unlistenCalls += 1; });
  await settle();
  assert.equal(unlistenCalls, 6);
  for (let index = 0; index < 6; index += 1) await h.timeout(100);
  assert.equal(attempts, 6);
  assert.equal(h.intervals.size, 0);
  assert.deepEqual(h.commands, []);
});
