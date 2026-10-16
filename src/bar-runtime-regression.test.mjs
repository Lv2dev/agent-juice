import assert from "node:assert/strict";
import test from "node:test";
import vm from "node:vm";
import { readFileSync } from "node:fs";
import { DEFAULT_SETTINGS } from "./panel-state.js";
import { barToolViewModel, barViewModel } from "./bar-state.js";
import { createTextScaleState, fittedRingNumberSize, TEXT_SCALE_EVENT } from "./text-scale.js";
import { createSettingsAuthority } from "./settings-revision.js";
import { createStatusRequests } from "./status-requests.js";

const source = readFileSync(new URL("./bar.js", import.meta.url), "utf8");
const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");
const settle = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
};

function nativeMenuState(revision = 0n) {
  let open = false;
  const snapshot = () => ({ open, revision: String(revision) });
  return {
    snapshot,
    write(value) { open = value; revision += 1n; return snapshot(); },
  };
}

async function fixture({ orientation = "horizontal", orientationInvoke, menuInvoke, settings = {}, length = 120,
  viewportSize, menuSize = { width: 82, height: 32 }, textScale = 1, menuListen } = {}) {
  const events = new Map(), windowEvents = new Map(), timers = new Map(), calls = [];
  const properties = new Map();
  const root = { dataset: {}, style: { setProperty: (key, value) => properties.set(key, value) } };
  const menuProperties = new Map(), menuMeasurements = [];
  const menu = { hidden: true, contains: () => false,
    style: { setProperty: (key, value) => menuProperties.set(key, value) },
    getBoundingClientRect() {
      menuMeasurements.push({ hidden: this.hidden, visibility: this.style.visibility });
      return this.hidden ? { width: 0, height: 0 } : { ...menuSize };
    },
  };
  const viewport = viewportSize ?? { clientWidth: orientation === "vertical" ? 40 : 160, clientHeight: 40 };
  let clock = 0, revision = 0n, measuredLength = length;
  const item = { hidden: true, dataset: {}, style: { setProperty() {} },
    setAttribute() {}, removeAttribute() {}, querySelector: () => null, querySelectorAll: () => [],
    getBoundingClientRect: () => orientation === "vertical"
      ? { width: 40, height: measuredLength } : { width: measuredLength, height: 40 },
  };
  const document = {
    documentElement: viewport,
    addEventListener() {},
    querySelector: selector => selector === "#bar" ? root : selector === "#bar-menu" ? menu
      : selector.includes('[data-tool="codex"]') ? item : null,
  };
  let menuListenerAttempts = 0;
  const listen = async (name, handler) => {
    if (name === "taskbar-menu-state-updated") {
      menuListenerAttempts += 1;
      if (menuListen) await menuListen(menuListenerAttempts);
    }
    events.set(name, handler); return () => events.delete(name);
  };
  const window = {
    location: { search: "?tool=codex" }, devicePixelRatio: 1,
    get innerWidth() { return viewport.clientWidth; }, get innerHeight() { return viewport.clientHeight; },
    addEventListener: (name, handler) => windowEvents.set(name, handler),
    removeEventListener: name => windowEvents.delete(name),
    getComputedStyle: () => ({ paddingTop: orientation === "vertical" ? "8px" : "0px",
      paddingBottom: orientation === "vertical" ? "8px" : "0px" }),
    __TAURI__: {
      event: { listen, emit: async () => {} },
      window: { getCurrentWindow: () => ({ listen }) },
      core: { invoke(command, args) {
        calls.push({ command, args });
        if (command === "get_settings") return { ...settings };
        if (command === "get_status") return [];
        if (command === "get_collection_health") return {};
        if (command === "get_taskbar_orientation") return orientationInvoke ? orientationInvoke() : orientation;
        if (command === "get_system_text_scale") return { factor: textScale, revision: 0 };
        if (command === "set_taskbar_menu_open") {
          const result = { open: args.open, revision: String(++revision) };
          return menuInvoke ? menuInvoke(args, result) : result;
        }
        return null;
      } },
    },
  };
  const context = vm.createContext({
    DEFAULT_SETTINGS, barToolViewModel, barViewModel, createTextScaleState, fittedRingNumberSize,
    TEXT_SCALE_EVENT, createSettingsAuthority, createStatusRequests, URLSearchParams, document, window,
    applyFont() {}, applyTheme() {}, applyTranslations() {},
    setTimeout(callback, delay) { const timer = { callback, due: clock + delay, unref() {} }; timers.set(timer, timer); return timer; },
    clearTimeout: timer => timers.delete(timer), setInterval: () => ({ unref() {} }), clearInterval() {},
  });
  const executable = source.replace(/^import .* from .*;\r?\n/gm, "");
  assert.doesNotMatch(executable, /^import /m);
  vm.runInContext(`${executable}\nglobalThis.api = {show: showRefreshMenu, hide: hideRefreshMenu,
    loadOrientation: loadTaskbarOrientation, cleanup: cleanupListeners};`, context);
  await settle();
  return {
    api: context.api, calls, menu, root, properties, menuProperties, menuMeasurements, viewport, window,
    setLength: value => { measuredLength = value; },
    resize: () => windowEvents.get("resize")?.(),
    event(name, payload) { assert.ok(events.has(name), `${name} must be registered`); events.get(name)({ payload }); },
    widths: () => calls.filter(call => call.command === "set_taskbar_content_width"),
    menuListenerAttempts: () => menuListenerAttempts,
    async advance(ms = 80) {
      clock += ms;
      for (let count = 0; ; count++) {
        const timer = [...timers.values()].find(value => value.due <= clock);
        if (!timer) break;
        assert.ok(count < 30, "timer dispatch must be bounded");
        timers.delete(timer); timer.callback(); await settle();
      }
    },
  };
}

test("R3 failed native menu subscription uses direct refresh without opening an unobservable menu", async () => {
  const h = await fixture({ menuListen: () => Promise.reject(new Error("listener unavailable")) });
  try {
    await h.advance(100); await h.advance(250);
    assert.equal(h.menuListenerAttempts(), 3);
    await h.api.show({ clientX: 20, clientY: 20 });
    assert.equal(h.menu.hidden, true);
    assert.equal(h.root.dataset.menuState ?? "closed", "closed");
    assert.equal(h.calls.filter(call => call.command === "set_taskbar_menu_open").length, 0);
    assert.equal(h.calls.filter(call => call.command === "refresh_status").length, 1);
    const widths = h.widths().length;
    h.setLength(180); h.resize(); await h.advance(100);
    assert.ok(h.widths().length > widths, "unavailable menu events must not block normal width sync");
  } finally { h.api.cleanup(); }
});

test("R3 pending menu registration falls back until success and then accepts native policy close", async () => {
  const registration = deferred();
  const h = await fixture({ menuListen: () => registration.promise });
  try {
    await h.api.show({});
    assert.equal(h.menu.hidden, true);
    assert.equal(h.calls.filter(call => call.command === "set_taskbar_menu_open").length, 0);
    registration.resolve(); await settle();
    await h.api.show({});
    assert.equal(h.menu.hidden, false);
    h.event("taskbar-menu-state-updated", { open: false, revision: "2" });
    assert.equal(h.menu.hidden, true);
    assert.equal(h.root.dataset.menuState, "closed");
  } finally { h.api.cleanup(); }
});

test("R3 transient registration failure recovers while late registration after disposal cannot reopen", async () => {
  const h = await fixture({ menuListen: attempt => attempt < 3 ? Promise.reject(new Error("startup")) : undefined });
  try {
    await h.api.show({});
    assert.equal(h.menu.hidden, true);
    await h.advance(100); await h.advance(250);
    assert.equal(h.menuListenerAttempts(), 3);
    await h.api.show({});
    assert.equal(h.menu.hidden, false);
  } finally { h.api.cleanup(); }
  const registration = deferred(), disposed = await fixture({ menuListen: () => registration.promise });
  disposed.api.cleanup();
  const calls = disposed.calls.length;
  registration.resolve(); await settle();
  await disposed.api.show({});
  assert.equal(disposed.menu.hidden, true);
  assert.equal(disposed.calls.length, calls);
});

test("R3 exhausted registration can recover later and repeated clicks share one registration", async () => {
  const recovery = deferred();
  const h = await fixture({ menuListen: attempt => attempt <= 3 ? Promise.reject(new Error("startup")) : recovery.promise });
  try {
    await h.advance(100); await h.advance(250);
    assert.equal(h.menuListenerAttempts(), 3);
    await h.api.show({}); await h.api.show({}); await h.api.show({});
    assert.equal(h.menuListenerAttempts(), 4);
    assert.equal(h.menu.hidden, true);
    assert.equal(h.calls.filter(call => call.command === "set_taskbar_menu_open").length, 0);
    recovery.resolve(); await settle();
    await h.api.show({});
    assert.equal(h.menu.hidden, false);
    h.event("taskbar-menu-state-updated", { open: false, revision: "2" });
    assert.equal(h.menu.hidden, true);
  } finally { h.api.cleanup(); }
});

test("E2 constrains the complete indicator footprint at every text scale without mutating settings", () => {
  for (const indicator_style of ["ring", "bar"]) for (const bar_mode of ["full", "compact", "dual", "quad"]) {
    for (const ring_size_px of [20, 36, 44]) for (const ring_thickness_px of [1, 4, 10]) {
      for (const ring_gap_px of [2, 6, 14]) for (const textScale of [1, 1.25, 1.5, 2, 2.25]) {
        for (const crossAxisSize of [16, 24, 32, 40, 48, 64]) {
          const settings = Object.freeze({ indicator_style, bar_mode, ring_size_px, ring_thickness_px, ring_gap_px });
          const view = barViewModel([], settings, new Date(), { textScale, crossAxisSize });
          const footprint = indicator_style === "bar"
            ? Math.max(view.ringSizePx, 2 * view.ringThicknessPx + view.ringGapPx) : view.ringSizePx;
          assert.ok(footprint <= crossAxisSize + 1e-9, JSON.stringify({ settings, textScale, crossAxisSize, footprint }));
          assert.equal(settings.ring_size_px, ring_size_px);
        }
      }
    }
  }
  assert.equal(barViewModel([], { ring_size_px: 36 }, new Date(), { crossAxisSize: 40 }).ringSizePx, 36);
});

test("E3 uses intrinsic vertical packing and includes parent padding without viewport feedback", async () => {
  const shell = css.match(/\.bar-shell\[data-taskbar-orientation="vertical"\] \{([^}]+)\}/)?.[1] ?? "";
  const tool = css.match(/\.bar-shell\[data-taskbar-orientation="vertical"\] \.bar-tool \{([^}]+)\}/)?.[1] ?? "";
  assert.match(shell, /grid-auto-rows:\s*max-content/);
  assert.match(tool, /height:\s*max-content/);
  assert.match(shell, /padding:\s*8px 0/);
  for (const [mode, packed, total] of [["dual", 48, 64], ["quad", 91, 107]]) {
    const h = await fixture({ orientation: "vertical", settings: { bar_mode: mode }, length: packed });
    try {
      for (let pass = 0; pass < 3; pass++) {
        await h.advance();
        assert.equal(h.widths().at(-1)?.args.width, total);
        h.viewport.clientHeight = total;
        h.resize();
        h.event("taskbar-topology-updated", "vertical");
      }
      assert.equal(h.properties.get("--ring-size"), "36px");
    } finally { h.api.cleanup(); }
  }
});

test("E1 defers every window measurement during global drag and forces same-key measurement on drop", async () => {
  for (const tool of ["codex", "claude"]) {
    const h = await fixture();
    try {
      await h.advance();
      assert.equal(h.widths().length, 1);
      h.event("taskbar-dragging-updated", { tool, dragging: true });
      h.event("status-updated", []);
      await h.advance(500);
      assert.equal(h.widths().length, 1);
      h.event("taskbar-dragging-updated", { tool, dragging: false });
      await h.advance();
      assert.equal(h.widths().length, 2, "drop invalidates an otherwise unchanged successful key");
      h.event("taskbar-dragging-updated", { tool, dragging: true });
      h.setLength(200); h.window.devicePixelRatio = 2;
      h.event("taskbar-topology-updated", "vertical");
      await h.advance(500);
      assert.equal(h.widths().length, 2, "orientation/DPI/content updates cannot apply the old native target during drag");
      h.event("taskbar-dragging-updated", { tool, dragging: false });
      await h.advance();
      assert.equal(h.widths().at(-1)?.args.devicePixelRatio, 2);
      assert.equal(h.root.dataset.dragging, undefined);
    } finally { h.api.cleanup(); }
  }
});

test("E1 queued measurements stay deferred across all DPI pairs until native drop notification", async () => {
  for (const from of [1, 1.25, 1.5, 2]) for (const to of [1, 1.25, 1.5, 2]) {
    const h = await fixture();
    try {
      h.window.devicePixelRatio = from;
      h.event("taskbar-dragging-updated", { tool: "claude", dragging: true });
      h.window.devicePixelRatio = to;
      h.setLength(180);
      h.resize();
      await h.advance(500);
      assert.equal(h.widths().length, 0, "drag must cancel even the first queued measurement of another window");
      h.event("taskbar-dragging-updated", { tool: "claude", dragging: false });
      await h.advance();
      assert.equal(h.widths().length, 1);
      assert.equal(h.widths()[0].args.devicePixelRatio, to);
      assert.equal(h.widths()[0].args.width, 180);
    } finally { h.api.cleanup(); }
  }
});

test("E2 normal text re-derives constrained geometry after viewport resize and orientation changes", async () => {
  const h = await fixture({ settings: { ring_size_px: 44 } });
  try {
    assert.equal(h.properties.get("--ring-size"), "40px");
    h.viewport.clientHeight = 32;
    h.resize();
    assert.equal(h.properties.get("--ring-size"), "32px");
    h.viewport.clientWidth = 24;
    h.event("taskbar-topology-updated", "vertical");
    assert.equal(h.properties.get("--ring-size"), "24px");
    h.event("taskbar-topology-updated", "horizontal");
    assert.equal(h.properties.get("--ring-size"), "32px");
  } finally { h.api.cleanup(); }
});

for (const reply of [undefined, null, {}, { open: true, revision: 1 }, { open: true, revision: "18446744073709551616" }, { open: false, revision: "2" }]) {
  test(`C3 does not open for an absent, invalid, or policy-cancelled ACK: ${JSON.stringify(reply)}`, async () => {
    const h = await fixture({ menuInvoke: args => args.open ? reply : { open: false, revision: "3" } });
    try {
      await h.api.show({});
      assert.equal(h.menu.hidden, true);
      assert.equal(h.root.dataset.menuState, "closed");
    } finally { h.api.cleanup(); }
  });
}

test("C3 a newer native close dismisses OPEN and restores tooltip/width without reopening on events", async () => {
  const h = await fixture({ menuInvoke: (args, result) => args.open ? { open: true, revision: "9007199254740992" } : result });
  try {
    await h.api.show({});
    assert.equal(h.menu.hidden, false);
    const before = h.widths().length;
    h.event("taskbar-menu-state-updated", { open: false, revision: "9007199254740993" });
    assert.equal(h.menu.hidden, true);
    assert.equal(h.root.dataset.menuState, "closed");
    await h.advance();
    assert.equal(h.widths().length, before + 1);
    h.event("taskbar-menu-state-updated", { open: true, revision: "9007199254740994" });
    assert.equal(h.menu.hidden, true, "native events alone do not create local open intent");
  } finally { h.api.cleanup(); }
});

for (const closeRevision of ["9", "10", "11"]) {
  test(`C3 OPENING keeps intent until ACK revision comparison: close ${closeRevision}, ACK 10`, async () => {
    const pending = deferred();
    const h = await fixture({ menuInvoke: args => args.open ? pending.promise : { open: false, revision: "12" } });
    try {
      const opening = h.api.show({});
      h.event("taskbar-menu-state-updated", { open: false, revision: closeRevision });
      assert.equal(h.root.dataset.menuState, "opening");
      pending.resolve({ open: true, revision: "10" });
      await opening;
      assert.equal(h.menu.hidden, closeRevision !== "9");
      assert.equal(h.root.dataset.menuState, closeRevision === "9" ? "open" : "closed");
    } finally { h.api.cleanup(); }
  });
}

test("C3 stale native closes and late ACK after disposal cannot dismiss/reopen a newer intent", async () => {
  const pending = deferred();
  let opens = 0;
  const h = await fixture({ menuInvoke: args => args.open
    ? ++opens === 1 ? { open: true, revision: "10" } : pending.promise
    : { open: false, revision: "11" } });
  try {
    await h.api.show({});
    h.event("taskbar-menu-state-updated", { open: false, revision: "9" });
    assert.equal(h.menu.hidden, false);
    h.api.hide(); await settle();
    const opening = h.api.show({});
    h.api.cleanup();
    pending.resolve({ open: true, revision: "12" });
    await opening;
    assert.equal(h.menu.hidden, true);
    const calls = h.calls.length;
    await h.advance(1000);
    assert.equal(h.calls.length, calls);
  } finally { h.api.cleanup(); }
});

test("C3 a retired open ACK cannot consume the close revision needed by the current OPEN menu", async () => {
  const pending = deferred();
  let opens = 0;
  const h = await fixture({ menuInvoke: args => args.open
    ? ++opens === 1 ? pending.promise : { open: true, revision: "12" }
    : { open: false, revision: "11" } });
  try {
    const retired = h.api.show({});
    await h.api.show({});
    assert.equal(h.menu.hidden, false);
    pending.resolve({ open: false, revision: "13" });
    await retired;
    assert.equal(h.menu.hidden, false, "retired local request cannot decide the new intent");
    h.event("taskbar-menu-state-updated", { open: false, revision: "13" });
    assert.equal(h.menu.hidden, true, "the authoritative close must not be lost as an ACK duplicate");
  } finally { h.api.cleanup(); }
});

test("C3 native close during CLOSING restores pending tooltip/width before a retired close ACK", async () => {
  const pending = deferred();
  const h = await fixture({ menuInvoke: args => args.open ? { open: true, revision: "1" } : pending.promise });
  try {
    await h.api.show({});
    const tooltips = () => h.calls.filter(call => call.command === "set_taskbar_tooltip").length;
    const before = tooltips();
    h.event("status-updated", [{ tool: "codex", captured_at: new Date().toISOString(),
      primary: { used_percent: 20 }, secondary: null, session: { active: true } }]);
    assert.equal(tooltips(), before);
    h.api.hide();
    assert.equal(h.root.dataset.menuState, "closing");
    h.event("taskbar-menu-state-updated", { open: false, revision: "2" });
    assert.equal(h.root.dataset.menuState, "closed");
    assert.equal(tooltips(), before + 1);
    await h.advance();
    assert.equal(h.widths().length, 1);
    pending.resolve({ open: true, revision: "1" });
    await settle();
    assert.equal(h.root.dataset.menuState, "closed");
    assert.equal(h.menu.hidden, true);
  } finally { h.api.cleanup(); }
});

test("C3 an explicit still-open close ACK keeps native ownership until a newer close event", async () => {
  const h = await fixture({ menuInvoke: args => ({ open: true, revision: args.open ? "1" : "2" }) });
  try {
    await h.api.show({});
    h.api.hide(); await settle();
    assert.equal(h.root.dataset.menuState, "closing");
    assert.equal(h.menu.hidden, true);
    await h.advance();
    assert.equal(h.widths().length, 0);
    h.event("taskbar-menu-state-updated", { open: false, revision: "3" });
    await h.advance();
    assert.equal(h.root.dataset.menuState, "closed");
    assert.equal(h.widths().length, 1);
  } finally { h.api.cleanup(); }
});

for (const eventOrder of ["before-ack", "after-ack", "absent"]) {
  test(`C3 actual-state overlapping open ACKs keep the current intent: ${eventOrder}`, async () => {
    const native = nativeMenuState(), first = deferred(), second = deferred();
    let opens = 0;
    const h = await fixture({ menuInvoke: args => args.open
      ? ++opens === 1 ? first.promise : second.promise
      : native.write(false) });
    try {
      const retired = h.api.show({});
      const current = h.api.show({});
      native.write(true); // The newer request writes first; the older request follows.
      const actual = native.write(true);
      if (eventOrder === "before-ack") h.event("taskbar-menu-state-updated", actual);
      first.resolve(native.snapshot()); await retired;
      second.resolve(native.snapshot()); await current;
      if (eventOrder === "after-ack") h.event("taskbar-menu-state-updated", actual);
      assert.equal(h.root.dataset.menuState, "open");
      assert.equal(h.menu.hidden, false);
      assert.equal(native.snapshot().open, true);
      assert.equal(h.calls.filter(call => call.command === "set_taskbar_menu_open" && !call.args.open).length, 0);
    } finally { h.api.cleanup(); }
  });

  test(`C3 actual-state cancelled opening retries a superseded in-flight close: ${eventOrder}`, async () => {
    const native = nativeMenuState(), opening = deferred(), closing = deferred();
    let closes = 0;
    const h = await fixture({ menuInvoke: args => args.open ? opening.promise
      : ++closes === 1 ? closing.promise : native.write(false) });
    try {
      const retired = h.api.show({});
      h.api.hide();
      native.write(false); // The close applies before the cancelled open resumes.
      const actual = native.write(true);
      if (eventOrder === "before-ack") h.event("taskbar-menu-state-updated", actual);
      opening.resolve(native.snapshot()); await retired;
      assert.equal(closes, 1, "retired open cleanup shares the in-flight close");
      closing.resolve(native.snapshot()); await settle();
      if (eventOrder === "after-ack") h.event("taskbar-menu-state-updated", actual);
      assert.equal(h.root.dataset.menuState, "closing", "an actual open ACK is not a close confirmation");
      assert.equal(h.menu.hidden, true);
      assert.equal(h.widths().length, 0);
      await h.advance(249);
      assert.equal(closes, 1);
      await h.advance(1);
      assert.equal(closes, 2);
      assert.equal(native.snapshot().open, false);
      assert.equal(h.root.dataset.menuState, "closed");
      await h.advance(80);
      assert.equal(h.widths().length, 1, "measurement resumes only after a real close");
    } finally { h.api.cleanup(); }
  });
}

for (const closeSnapshot of ["before-reopen", "after-reopen"]) {
  test(`C3 actual-state superseded close ACK cannot dismiss a reopened menu: ${closeSnapshot}`, async () => {
    const native = nativeMenuState(), closing = deferred(), reopening = deferred();
    let opens = 0;
    const h = await fixture({ menuInvoke: args => args.open
      ? ++opens === 1 ? native.write(true) : reopening.promise
      : closing.promise });
    try {
      await h.api.show({});
      h.api.hide();
      const current = h.api.show({});
      const closed = native.write(false);
      const opened = native.write(true);
      h.event("taskbar-menu-state-updated", opened);
      reopening.resolve(native.snapshot()); await current;
      closing.resolve(closeSnapshot === "before-reopen" ? closed : native.snapshot());
      await settle(); await h.advance(1000);
      assert.equal(h.root.dataset.menuState, "open");
      assert.equal(h.menu.hidden, false);
      assert.equal(native.snapshot().open, true);
      assert.equal(h.calls.filter(call => call.command === "set_taskbar_menu_open" && !call.args.open).length, 1);
    } finally { h.api.cleanup(); }
  });
}

for (const revision of [0n, 9007199254740991n]) {
  test(`C3 actual-state open uses a newer event than its truthful closed ACK: ${revision}`, async () => {
    const native = nativeMenuState(revision), first = deferred(), closing = deferred(), current = deferred();
    let opens = 0;
    const h = await fixture({ menuInvoke: args => args.open
      ? ++opens === 1 ? first.promise : current.promise
      : closing.promise });
    try {
      const retired = h.api.show({});
      h.api.hide();
      const latest = h.api.show({});
      native.write(true); // The latest open applies, then a retired close applies.
      const earlierSnapshot = native.write(false);
      const actual = native.write(true); // The first open resumes after both.
      h.event("taskbar-menu-state-updated", actual);
      first.resolve(native.snapshot()); await retired;
      closing.resolve(earlierSnapshot); await settle();
      current.resolve(earlierSnapshot); await latest;
      assert.equal(h.root.dataset.menuState, "open", "an older ACK cannot replace the newer actual state");
      assert.equal(h.menu.hidden, false);
      assert.equal(native.snapshot().open, true);
      assert.equal(h.calls.filter(call => call.command === "set_taskbar_menu_open" && !call.args.open).length, 1);
    } finally { h.api.cleanup(); }
  });
}

for (const pendingKind of ["open", "close"]) {
  test(`C3 actual-state late ${pendingKind} ACK after disposal cannot reopen or retry`, async () => {
    const native = nativeMenuState(), pending = deferred();
    const h = await fixture({ menuInvoke: args => pendingKind === "open" && args.open
      ? pending.promise : !args.open ? pending.promise : native.write(true) });
    try {
      const opening = h.api.show({});
      if (pendingKind === "close") { await opening; h.api.hide(); }
      h.api.cleanup();
      const calls = h.calls.length;
      pending.resolve(native.write(true));
      await opening; await settle(); await h.advance(6000);
      assert.equal(h.menu.hidden, true);
      assert.equal(h.calls.length, calls);
    } finally { h.api.cleanup(); }
  });
}

for (const orientation of ["horizontal", "vertical"]) for (const cross of [32, 40, 64]) {
  for (const textScale of [1, 2.25]) {
    test(`R2 menu measures its hidden layout after the truthful ACK: ${orientation}/${cross}/${textScale}`, async () => {
      const pending = deferred();
      let opens = 0;
      const size = orientation === "vertical"
        ? { width: 32, height: textScale === 1 ? 82 : 94 }
        : { width: textScale === 1 ? 82 : 95.5, height: 32 };
      const h = await fixture({ orientation, textScale, menuSize: size,
        viewportSize: orientation === "vertical"
          ? { clientWidth: cross, clientHeight: 64 } : { clientWidth: 64, clientHeight: cross },
        menuInvoke: args => args.open
          ? ++opens === 1 ? pending.promise : { open: true, revision: "2" }
          : { open: false, revision: "3" } });
      try {
        const opening = h.api.show({ clientX: orientation === "vertical" ? cross - 1 : 60,
          clientY: orientation === "vertical" ? 60 : cross - 1 });
        assert.equal(h.menu.hidden, true);
        assert.equal(h.menuMeasurements.length, 0, "native expansion must precede DOM measurement");
        if (orientation === "vertical") h.viewport.clientHeight = 96;
        else h.viewport.clientWidth = 96;
        pending.resolve({ open: true, revision: "1" });
        await opening;
        assert.equal(h.menu.hidden, false);
        assert.equal(h.root.dataset.menuState, "open");
        assert.equal(h.menuMeasurements.length, 1);
        assert.deepEqual(h.menuMeasurements[0], { hidden: false, visibility: "hidden" });
        assert.equal(h.menu.style.visibility, "", "only the positioned menu may become visible");
        const x = Number.parseFloat(h.menuProperties.get("--menu-x"));
        const y = Number.parseFloat(h.menuProperties.get("--menu-y"));
        assert.ok(x >= 0 && x + size.width <= h.viewport.clientWidth, `x=${x}, width=${size.width}`);
        assert.ok(y >= 0 && y + size.height <= h.viewport.clientHeight, `y=${y}, height=${size.height}`);
        if (cross === 32) assert.equal(orientation === "vertical" ? x : y, 0);
        if (orientation === "horizontal" && textScale > 1) assert.equal(x, 0.25, "fractional bounds reduce both margins");
        await h.api.show({ clientX: h.viewport.clientWidth - 1, clientY: h.viewport.clientHeight - 1 });
        assert.equal(h.menuMeasurements.length, 2, "already-open menus must be measured again");
        assert.deepEqual(h.menuMeasurements[1], { hidden: false, visibility: "hidden" });
        const nextX = Number.parseFloat(h.menuProperties.get("--menu-x"));
        const nextY = Number.parseFloat(h.menuProperties.get("--menu-y"));
        assert.ok(nextX >= 0 && nextX + size.width <= h.viewport.clientWidth);
        assert.ok(nextY >= 0 && nextY + size.height <= h.viewport.clientHeight);
      } finally { h.api.cleanup(); }
    });
  }
}

test("R2 menu CSS removes anonymous line boxes and preserves intrinsic vertical writing", () => {
  const menu = css.match(/\.bar-context-menu \{([^}]+)\}/)?.[1] ?? "";
  const button = css.match(/\.bar-context-menu button \{([^}]+)\}/)?.[1] ?? "";
  const vertical = css.match(/\.bar-shell\[data-taskbar-orientation="vertical"\] \.bar-context-menu button \{([^}]+)\}/)?.[1] ?? "";
  const enlarged = css.match(/html\[data-text-scale="enlarged"\] \.bar-context-menu button \{([^}]+)\}/)?.[1] ?? "";
  assert.match(menu, /display:\s*grid/);
  assert.match(button, /white-space:\s*nowrap/);
  assert.match(vertical, /width:\s*24px/);
  assert.match(vertical, /min-height:\s*74px/);
  assert.match(vertical, /height:\s*max-content/);
  assert.match(vertical, /padding:\s*3px 0;/);
  assert.match(vertical, /writing-mode:\s*vertical-rl/);
  assert.match(enlarged, /padding-block:\s*0(?:px)?;/);
  assert.match(enlarged, /padding-inline:\s*7px;/);
});

test("R4 vertical menu fits the actual native minimum length instead of an additive expansion", async () => {
  const native = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  const minimum = Number(native.match(/const TASKBAR_MENU_WIDTH: i32 = (\d+);/)?.[1]);
  assert.ok(minimum > 0);
  assert.match(native, /fn taskbar_width_with_menu[\s\S]*?width\.max\(TASKBAR_MENU_WIDTH\)/);
  const h = await fixture({ orientation: "vertical", textScale: 2.25,
    viewportSize: { clientWidth: 40, clientHeight: Math.max(64, minimum) },
    menuSize: { width: 32, height: 90.5 } });
  try {
    await h.api.show({ clientX: 39, clientY: minimum - 1 });
    assert.equal(h.menu.hidden, false);
    const y = Number.parseFloat(h.menuProperties.get("--menu-y"));
    assert.ok(y >= 0 && y + 90.5 <= minimum);
  } finally { h.api.cleanup(); }
});

for (const outcome of ["success", "failure"]) {
  test(`R2 topology event retires a pending orientation ${outcome}`, async () => {
    const pending = deferred();
    let reads = 0;
    const h = await fixture({ orientationInvoke: () => ++reads === 1 ? pending.promise : "vertical",
      settings: { bar_mode: "dual", ring_size_px: 44 },
      viewportSize: { clientWidth: 40, clientHeight: 104 } });
    try {
      h.event("taskbar-topology-updated", "vertical");
      assert.equal(h.root.dataset.taskbarOrientation, "vertical");
      if (outcome === "success") pending.resolve("horizontal");
      else pending.reject(new Error("retired shell lookup failure"));
      await settle();
      assert.equal(h.root.dataset.taskbarOrientation, "vertical");
      assert.equal(h.properties.get("--ring-size"), "40px");
      await h.api.loadOrientation();
      assert.equal(reads, 2, "fresh queries still work after an authoritative event");
      assert.equal(h.root.dataset.taskbarOrientation, "vertical");
    } finally { h.api.cleanup(); }
  });
}

test("R2 retired or disposed open ACKs never expose a menu for layout measurement", async () => {
  for (const dispose of [false, true]) {
    const pending = deferred();
    const h = await fixture({ menuInvoke: args => args.open ? pending.promise : { open: false, revision: "2" } });
    try {
      const opening = h.api.show({});
      if (dispose) h.api.cleanup();
      else h.api.hide();
      pending.resolve({ open: true, revision: "1" });
      await opening; await settle();
      assert.equal(h.menuMeasurements.length, 0);
      assert.equal(h.menu.hidden, true);
    } finally { h.api.cleanup(); }
  }
});

for (const resizeOrder of ["before-ack", "after-ack"]) {
  test(`R2 native expansion resize ${resizeOrder} keeps the menu open at its original anchor`, async () => {
    const pending = deferred();
    const h = await fixture({ viewportSize: { clientWidth: 64, clientHeight: 40 },
      menuInvoke: args => args.open ? pending.promise : { open: false, revision: "2" } });
    try {
      const opening = h.api.show({ clientX: 60, clientY: 36 });
      if (resizeOrder === "before-ack") {
        h.viewport.clientWidth = 96; h.resize();
        assert.equal(h.root.dataset.menuState, "opening");
        assert.equal(h.menuMeasurements.length, 0);
      }
      pending.resolve({ open: true, revision: "1" });
      await opening;
      if (resizeOrder === "after-ack") { h.viewport.clientWidth = 96; h.resize(); }
      assert.equal(h.root.dataset.menuState, "open");
      assert.equal(h.menu.hidden, false);
      assert.equal(h.menuProperties.get("--menu-x"), "10px");
      assert.equal(h.calls.filter(call => call.command === "set_taskbar_menu_open" && !call.args.open).length, 0);
    } finally { h.api.cleanup(); }
  });
}

test("R2 an OPEN menu re-clamps after a native horizontal-to-vertical topology event", async () => {
  const size = { width: 82, height: 32 };
  const h = await fixture({ menuSize: size, viewportSize: { clientWidth: 96, clientHeight: 40 } });
  try {
    await h.api.show({ clientX: 90, clientY: 36 });
    assert.equal(h.menuProperties.get("--menu-x"), "10px");
    h.viewport.clientWidth = 40; h.viewport.clientHeight = 96;
    size.width = 32; size.height = 82;
    h.event("taskbar-topology-updated", "vertical");
    assert.equal(h.root.dataset.menuState, "open");
    assert.equal(h.menuProperties.get("--menu-x"), "4px");
    assert.equal(h.menuProperties.get("--menu-y"), "10px");
    assert.equal(h.menuMeasurements.length, 2);
  } finally { h.api.cleanup(); }
});

test("R2 an OPEN menu re-clamps on text scale and render without losing its original anchor", async () => {
  const size = { width: 82, height: 32 };
  const h = await fixture({ menuSize: size, viewportSize: { clientWidth: 96, clientHeight: 32 } });
  try {
    await h.api.show({ clientX: 60, clientY: 31 });
    assert.equal(h.menuProperties.get("--menu-x"), "10px");
    size.width = 95.5;
    h.event(TEXT_SCALE_EVENT, { factor: 2.25, revision: 1 });
    assert.equal(h.root.dataset.menuState, "open");
    assert.equal(h.menuProperties.get("--menu-x"), "0.25px");
    assert.equal(h.menuProperties.get("--menu-y"), "0px");
    size.width = 82;
    h.event("status-updated", []);
    assert.equal(h.menuProperties.get("--menu-x"), "10px", "reflow must not overwrite the original pointer anchor");
    assert.equal(h.menuMeasurements.length, 3);
  } finally { h.api.cleanup(); }
});
