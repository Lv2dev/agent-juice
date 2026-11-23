import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("./settings.js", import.meta.url), "utf8").replace(/\r\n/g, "\n");
const flush = source.slice(source.indexOf("async function flushSettingsAndQuit("), source.indexOf("function scheduleAutosave()"));
const cancel = source.slice(source.indexOf('const quitCancelled = register("app-quit-cancelled"'), source.indexOf('void register("settings-updated"'));
const lockStart = source.indexOf("let quitHandoffGeneration = null;");
const inputLock = lockStart < 0 ? "" : source.slice(lockStart, source.indexOf("function syncUpdateBusyState()", lockStart));
const isolatedInputLock = `let form = null, updateInstallBusy = false; ${inputLock}`;
const enqueue = source.slice(source.indexOf("let lastSettingsSaveFailed ="), source.indexOf("async function flushSettingsAndQuit("));
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
const tick = () => new Promise(resolve => setImmediate(resolve));

function harness() {
  const saves = [], calls = [], handlers = {};
  let failCompletion = false;
  const context = vm.createContext({
    enqueueLatestSettingsSave(options) {
      const save = deferred();
      saves.push({ ...save, options });
      return save.promise;
    },
    async invoke(command, args) {
      calls.push({ command, args });
      if (command === "complete_app_quit" && failCompletion) throw new Error("completion IPC unavailable");
      return true;
    },
    register(name, handler) { handlers[name] = handler; },
  });
  vm.runInContext(`${isolatedInputLock}
    let saveQueue = Promise.resolve();
    let hasLoadedSettings = true, listenersDisposed = false;
    let quitFlushPromise = null, quitAttemptGeneration = 0, quitRequestId = null, quitLatestRequestId = null, quitRetryId = "0", localRevision = 0;
    ${flush}
    ${cancel}
    globalThis.begin = flushSettingsAndQuit;
    globalThis.edit = () => { localRevision += 1; };
    globalThis.cancel = (id) => registerCancel(id);`, Object.assign(context, {
      registerCancel(id) { handlers["app-quit-cancelled"]({ payload: { request_id: id } }); },
    }));
  return { ...context, saves, calls, failCompletion() { failCompletion = true; } };
}

test("cancelled completion cannot consume a newer native request", async () => {
  const h = harness();
  const first = h.begin("9007199254740993");
  h.cancel("9007199254740993");
  const second = h.begin("9007199254740994");
  h.saves[0].resolve();
  await first;
  assert.equal(h.calls.length, 0);
  h.saves[1].resolve();
  await second;
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].args.requestId, "9007199254740994");
});

test("duplicate request or Wait shares its save, stale cancel does not cancel the current save", async () => {
  const h = harness();
  const first = h.begin("8");
  const duplicate = h.begin("8");
  h.cancel("7");
  assert.equal(h.saves.length, 1);
  assert.equal(h.saves[0].options.failOnRollback, true);
  h.saves[0].resolve();
  await Promise.all([first, duplicate]);
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].args.requestId, "8");
});

test("save failure immediately requests native recovery and never completes", async () => {
  const h = harness();
  const attempt = h.begin("21");
  const rejected = assert.rejects(attempt, /synthetic save failure/);
  h.saves[0].reject(new Error("synthetic save failure"));
  await rejected;
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].command, "fail_app_quit");
  assert.equal(h.calls[0].args.requestId, "21");
  assert.equal(h.calls[0].args.retryId, null);
  assert.equal(h.calls[0].args.retryable, true);
});

test("late failure from a cancelled save cannot recover another request", async () => {
  const h = harness();
  const first = h.begin("31");
  const rejected = assert.rejects(first, /old failure/);
  h.cancel("31");
  const second = h.begin("32");
  h.saves[0].reject(new Error("old failure"));
  await rejected;
  assert.equal(h.calls.length, 0);
  h.saves[1].resolve();
  await second;
  assert.equal(h.calls[0].args.requestId, "32");
});

test("invalid request IDs do not flush or invoke", async () => {
  const h = harness();
  for (const id of [undefined, null, 2, "", "0", "-1", "1e9", "01"]) await h.begin(id);
  assert.equal(h.saves.length, 0);
  assert.equal(h.calls.length, 0);
});

test("unresponsive save never completes on the frontend", async () => {
  const h = harness();
  const attempt = h.begin("41");
  await tick();
  assert.equal(h.calls.length, 0);
  h.cancel("41");
  h.saves[0].resolve();
  await attempt;
  assert.equal(h.calls.length, 0);
});

test("an edit during quit is also saved before native completion", async () => {
  const h = harness();
  const attempt = h.begin("51");
  h.edit();
  h.saves[0].resolve();
  await tick();
  assert.equal(h.calls.length, 0);
  assert.equal(h.saves.length, 2);
  h.saves[1].resolve();
  await attempt;
  assert.equal(h.calls[0].args.requestId, "51");
});

test("late requested event cannot restart a retired request", async () => {
  const h = harness();
  const attempt = h.begin("62");
  h.cancel("62");
  await h.begin("61");
  await h.begin("62");
  assert.equal(h.saves.length, 1);
  h.saves[0].resolve();
  await attempt;
  assert.equal(h.calls.length, 0);
});

test("native recovery preserves the failed draft without waiting for rollback", async () => {
  const recovery = deferred();
  const calls = [];
  let rollbacks = 0;
  const context = vm.createContext({
    clearTimeout() {},
    settingsInputFromForm: () => ({}),
    saveSettings: async () => { throw new Error("save rejected"); },
    rollbackFailedSave: () => { rollbacks += 1; return recovery.promise; },
    invoke: async (command, args) => { calls.push({ command, args }); },
  });
  vm.runInContext(`${isolatedInputLock}
    let quitFlushPromise = null, quitAttemptGeneration = 0, quitRequestId = null, quitLatestRequestId = null, quitRetryId = "0";
    let localRevision = 1, savedRevision = 0, hasLoadedSettings = true, saveQueue = Promise.resolve(), editSession = {}, autosaveTimer = null;
    ${enqueue}\n${flush}\nglobalThis.begin = flushSettingsAndQuit;`, context);
  const attempt = context.begin("71");
  const rejected = assert.rejects(attempt, /save rejected/);
  await tick();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].command, "fail_app_quit");
  assert.equal(rollbacks, 0, "native retry must retain the draft instead of saving a rolled-back baseline");
  recovery.resolve();
  await rejected;
  assert.equal(calls.length, 1);
});

test("native retry reuses the request and a duplicate retry ticket saves only once", async () => {
  const h = harness();
  const first = h.begin("81");
  const rejected = assert.rejects(first, /save failed/);
  h.saves[0].reject(new Error("save failed"));
  await rejected;
  const retry = h.begin("81", "9007199254740993");
  await h.begin("81", "9007199254740993");
  await h.begin("81", "9007199254740992");
  assert.equal(h.saves.length, 2);
  h.saves[1].resolve();
  await retry;
  assert.equal(h.calls.filter(call => call.command === "complete_app_quit").length, 1);
  assert.equal(h.calls.at(-1).args.requestId, "81");
});

test("a retry waits for the previous promise instead of duplicating an in-flight save", async () => {
  const h = harness();
  const first = h.begin("91");
  const retry = h.begin("91", "1");
  await tick();
  assert.equal(h.saves.length, 1);
  h.saves[0].resolve();
  await first;
  await tick();
  assert.equal(h.calls.length, 0, "the superseded waiter must not complete the retry");
  assert.equal(h.saves.length, 2);
  h.saves[1].resolve();
  await retry;
  assert.equal(h.calls.length, 1);
});

test("activation cancels a queued retry before it can save into a new attempt", async () => {
  const h = harness();
  const first = h.begin("101");
  const retry = h.begin("101", "1");
  h.cancel("101");
  const current = h.begin("102");
  await h.begin("101", "2");
  h.saves[0].resolve();
  await Promise.all([first, retry]);
  assert.equal(h.saves.length, 2);
  assert.equal(h.calls.length, 0);
  h.saves[1].resolve();
  await current;
  assert.equal(h.calls[0].args.requestId, "102");
});

test("completion IPC errors do not claim a definite save failure", async () => {
  const h = harness();
  h.failCompletion();
  const first = h.begin("111");
  const rejected = assert.rejects(first, /completion IPC unavailable/);
  h.saves[0].resolve();
  await rejected;
  assert.equal(h.calls[1].command, "fail_app_quit");
  assert.equal(h.calls[1].args.retryId, null);
  assert.equal(h.calls[1].args.retryable, false);
});

for (const retryable of [true, false]) {
  test(`native Retry reports its exact ID on ${retryable ? "save" : "completion IPC"} failure`, async () => {
    const h = harness();
    const requestId = "112";
    const retryId = "9007199254740993";
    const first = h.begin(requestId);
    const firstRejected = assert.rejects(first, /initial save failed/);
    h.saves[0].reject(new Error("initial save failed"));
    await firstRejected;

    const retry = h.begin(requestId, retryId);
    const rejected = assert.rejects(retry, retryable ? /retry save failed/ : /completion IPC unavailable/);
    if (retryable) {
      const error = new Error("retry save failed");
      h.saves[1].options.onFailure(error);
      h.saves[1].reject(error);
    } else {
      h.failCompletion();
      h.saves[1].resolve();
    }
    await rejected;
    assert.deepEqual(
      h.calls.filter(call => call.command === "fail_app_quit")
        .map(({ args }) => [args.requestId, args.retryId, args.retryable]),
      [[requestId, null, true], [requestId, retryId, retryable]],
    );
  });
}

test("a failed quit save retries the retained input rather than a rolled-back no-op", async () => {
  const inputs = [], calls = [];
  const draft = { theme: "dark" };
  let rollbacks = 0;
  const context = vm.createContext({
    clearTimeout() {},
    settingsInputFromForm: () => ({ ...draft }),
    async saveSettings(input, revision) {
      inputs.push(input);
      if (inputs.length === 1) throw new Error("save rejected");
      context.saved(revision);
    },
    async rollbackFailedSave() { rollbacks += 1; draft.theme = "light"; },
    async invoke(command, args) { calls.push({ command, args }); },
  });
  vm.runInContext(`${isolatedInputLock}
    let quitFlushPromise = null, quitAttemptGeneration = 0, quitRequestId = null, quitLatestRequestId = null, quitRetryId = "0";
    let localRevision = 1, savedRevision = 0, hasLoadedSettings = true, saveQueue = Promise.resolve(), editSession = {}, autosaveTimer = null;
    ${enqueue}\n${flush}\nglobalThis.begin = flushSettingsAndQuit;
    globalThis.saved = revision => { savedRevision = revision; };`, context);
  await assert.rejects(context.begin("121"), /save rejected/);
  assert.equal(rollbacks, 0);
  assert.equal(draft.theme, "dark");
  await context.begin("121", "1");
  assert.equal(inputs.length, 2);
  assert.equal(inputs[1].theme, "dark");
  assert.equal(calls.at(-1).command, "complete_app_quit");
});

test("the native recovery module never reads settings from disk", () => {
  const native = readFileSync(new URL("../src-tauri/src/quit.rs", import.meta.url), "utf8");
  assert.doesNotMatch(native, /Settings::(?:try_)?load|(?:std|tokio)::fs|read_to_string/);
  assert.match(native, /SETTINGS_PUBLICATION_STATE\s*\.try_lock\(\)/);
  assert.match(native, /unwrap_or_else\(\|\| "system"\.into\(\)\)/);
});

test("an older autosave failing during quit cannot roll back the draft needed by native retry", async () => {
  const inputs = [];
  const draft = { theme: "dark" };
  let rollbacks = 0;
  const context = vm.createContext({
    clearTimeout() {},
    settingsInputFromForm: () => ({ ...draft }),
    async saveSettings(input, revision) {
      inputs.push(input);
      if (inputs.length <= 2) throw new Error("disk unavailable");
      context.saved(revision);
    },
    async rollbackFailedSave() { rollbacks += 1; draft.theme = "light"; },
    async invoke() {},
  });
  vm.runInContext(`${isolatedInputLock}
    let quitFlushPromise = null, quitAttemptGeneration = 0, quitRequestId = null, quitLatestRequestId = null, quitRetryId = "0";
    let localRevision = 1, savedRevision = 0, hasLoadedSettings = true, saveQueue = Promise.resolve(), editSession = {}, autosaveTimer = null;
    ${enqueue}\n${flush}\nglobalThis.begin = flushSettingsAndQuit;
    globalThis.enqueue = enqueueLatestSettingsSave;
    globalThis.saved = revision => { savedRevision = revision; };`, context);
  const autosave = context.enqueue();
  const autoRejected = assert.rejects(autosave, /disk unavailable/);
  const quitRejected = assert.rejects(context.begin("131"), /disk unavailable/);
  await Promise.all([autoRejected, quitRejected]);
  assert.equal(rollbacks, 0);
  assert.equal(draft.theme, "dark");
  await context.begin("131", "1");
  assert.equal(inputs.length, 3);
  assert.equal(inputs[2].theme, "dark");
});

function lifecycleHarness({ hydrated = true, cancelRegistration = Promise.resolve() } = {}) {
  const saves = [], reads = [], completions = [], snapshots = [], calls = [], handlers = {};
  const hydration = deferred();
  const timers = new Map();
  let timerId = 0;
  const draft = { theme: "dark" };
  const form = { inert: false, attributes: {}, setAttribute(key, value) { this.attributes[key] = value; } };
  const context = vm.createContext({
    form, updateActionButtons: [], updateBand: null,
    settingsInputFromForm: () => ({ ...draft }),
    settingsForReply: value => value,
    renderForm(value) { Object.assign(draft, value); },
    setStatus() {}, showSettingsToast() {}, hideSettingsToast() {}, updateOutputs() {}, publishPreview() {},
    currentLanguageSettings: () => ({ language: "en" }), t: key => key,
    window: { dispatchEvent() {} }, CustomEvent: class {},
    setTimeout(callback) { const id = ++timerId; timers.set(id, callback); return id; },
    clearTimeout(id) { timers.delete(id); },
    register(name, callback) { handlers[name] = callback; },
    tauriApi: () => ({
      event: {
        async listen(name, callback) {
          if (name === "app-quit-cancelled") await cancelRegistration;
          handlers[name] = callback;
          return () => { delete handlers[name]; };
        },
      },
    }),
    ensureSettingsLoad: () => hydration.promise,
    async invoke(command, args) {
      calls.push({ command, args });
      const operation = { ...deferred(), args };
      if (command === "save_settings") { saves.push(operation); return operation.promise; }
      if (command === "get_settings") { reads.push(operation); return operation.promise; }
      if (command === "complete_app_quit") { completions.push(operation); return operation.promise; }
      if (command === "get_pending_quit_request") { snapshots.push(operation); return operation.promise; }
    },
  });
  const saving = source.slice(source.indexOf("async function saveSettings("), source.indexOf("function handleSettingsMutation("));
  const updateBusy = source.slice(source.indexOf("function syncUpdateBusyState()"), source.indexOf("function setUpdateFallbackVisible("));
  const binding = source.slice(source.indexOf("function unlistenSafely("), source.indexOf('window.addEventListener?.("pagehide"'));
  vm.runInContext(`
    let quitFlushPromise = null, quitAttemptGeneration = 0, quitRequestId = null, quitLatestRequestId = null, quitRetryId = "0";
    let localRevision = 1, savedRevision = 0, hasLoadedSettings = ${hydrated}, saveQueue = Promise.resolve();
    let editSession = { baseline: { theme: "light" }, topology: {} }, autosaveTimer = null;
    let isHydrating = false, settingsEventGeneration = 0, quitListenerReady = true;
    let updateCheckBusy = false, updateInstallBusy = false;
    let listenersDisposed = false, listenerLifecycleGeneration = 0;
    const activeUnlisteners = new Set(), LISTENER_RETRY_DELAYS_MS = [0], LISTENER_REGISTRATION_TIMEOUT_MS = 500;
    function fillForm(value) { renderForm(value); editSession = { baseline: { ...value }, topology: {} }; }
    ${inputLock}\n${updateBusy}\n${saving}\n${binding}\n${cancel}
    globalThis.begin = flushSettingsAndQuit;
    globalThis.enqueue = enqueueLatestSettingsSave;
    globalThis.mutate = scheduleAutosave;
    globalThis.installBusy = setUpdateInstallBusy;
    globalThis.checkBusy = setUpdateCheckBusy;
    globalThis.inspect = () => ({ localRevision, savedRevision, quitRequestId });
    globalThis.bind = bindSettingsUpdates;
    globalThis.dispose = cleanupListeners;
    globalThis.finishHydrating = () => {
      hasLoadedSettings = true;
      savedRevision = localRevision;
    };
  `, context);
  return {
    ...context, context, saves, reads, completions, snapshots, calls, draft, timers, form,
    deliver(requestId, retryId = null) {
      handlers["app-quit-requested"]({ payload: { request_id: requestId, retry_id: retryId } });
    },
    cancel(id) { handlers["app-quit-cancelled"]({ payload: { request_id: id, reason: "activation" } }); },
    hydrate() {
      context.finishHydrating();
      hydration.resolve();
    },
    input(value) {
      if (form.inert) return false;
      draft.theme = value;
      context.mutate();
      return true;
    },
  };
}

test("R4: read a missed first request after both subscriptions, then dedupe a live copy", async () => {
  const registration = deferred();
  const h = lifecycleHarness({ cancelRegistration: registration.promise });
  const bound = h.bind();
  await tick();
  assert.equal(h.snapshots.length, 0, "the cancellation listener must be ready before the read");
  registration.resolve();
  await tick();
  assert.equal(h.snapshots.length, 1);
  assert.equal(h.saves.length, 0);
  h.snapshots[0].resolve({ pending: { request_id: "401", retry_id: null }, retired_through: "400" });
  await bound;
  h.deliver("401");
  await tick();
  assert.equal(h.saves.length, 1);
  h.saves[0].resolve({ settings: h.draft });
  await tick();
  assert.equal(h.completions.length, 1);
  assert.equal(h.completions[0].args.requestId, "401");
  assert.equal(h.form.inert, true);
  h.completions[0].resolve(true);
  await tick();
  h.deliver("401");
  h.deliver("401", "1");
  await tick();
  assert.equal(h.form.inert, true, "dedupe must preserve successful handoff");
  assert.equal(h.saves.length, 1);
});

test("R4: a late snapshot duplicate does not replace the live handoff", async () => {
  const h = lifecycleHarness();
  const bound = h.bind();
  await tick();
  h.deliver("402");
  await tick();
  h.saves[0].resolve({ settings: h.draft });
  await tick();
  assert.equal(h.form.inert, true);
  h.snapshots[0].resolve({ pending: { request_id: "402", retry_id: null }, retired_through: "401" });
  await bound;
  assert.equal(h.saves.length, 1);
  assert.equal(h.completions.length, 1);
  assert.equal(h.form.inert, true);
  h.cancel("402");
  h.completions[0].resolve(true);
  await tick();
  assert.equal(h.form.inert, false, "a late completion must not retake the lock");
});

for (const firstDelivery of ["snapshot", "live"]) {
  test(`R4: Retry can be the first delivery from ${firstDelivery} on a new panel`, async () => {
    const h = lifecycleHarness({ hydrated: false });
    const bound = h.bind();
    const requestId = "9007199254740993";
    const retryId = "9007199254740995";
    await tick();
    if (firstDelivery === "live") h.deliver(requestId, retryId);
    h.snapshots[0].resolve({
      pending: { request_id: requestId, retry_id: firstDelivery === "live" ? "9007199254740994" : retryId },
      retired_through: "9007199254740992",
    });
    await bound;
    h.deliver(requestId, retryId);
    h.deliver(requestId);
    await tick();
    assert.equal(h.saves.length, 0, "Retry must wait for loaded settings");
    assert.equal(h.completions.length, 0);
    h.hydrate();
    await tick();
    assert.equal(h.saves.length, 1, "Retry performs a real save even with a clean revision");
    h.saves[0].reject(new Error("retry failed"));
    await tick();
    assert.equal(h.completions.length, 0);
    assert.deepEqual(
      h.calls.filter(call => call.command === "fail_app_quit")
        .map(({ args }) => [args.requestId, args.retryId, args.retryable]),
      [[requestId, retryId, true]],
    );
  });
}

test("R4: an unseen cancellation retires delayed snapshot and live deliveries", async () => {
  const h = lifecycleHarness();
  const bound = h.bind();
  await tick();
  h.cancel("410");
  h.snapshots[0].resolve({ pending: { request_id: "410", retry_id: "2" }, retired_through: "409" });
  await bound;
  h.deliver("410", "3");
  h.deliver("409");
  await tick();
  assert.equal(h.saves.length, 0);
  h.deliver("411", "4");
  h.cancel("410");
  await tick();
  assert.equal(h.saves.length, 1);
  h.saves[0].resolve({ settings: h.draft });
  await tick();
  assert.equal(h.completions[0].args.requestId, "411");
  h.completions[0].resolve(true);
});

test("R4: native retirement watermark covers cancellations missed by a new panel", async () => {
  const h = lifecycleHarness();
  const bound = h.bind();
  await tick();
  h.snapshots[0].resolve({ pending: null, retired_through: "421" });
  await bound;
  h.deliver("420");
  h.deliver("421", "1");
  await tick();
  assert.equal(h.saves.length, 0);
  assert.equal(h.completions.length, 0);
});

test("R4: teardown ignores a pending startup snapshot", async () => {
  const h = lifecycleHarness();
  const bound = h.bind();
  await tick();
  h.dispose();
  h.snapshots[0].resolve({ pending: { request_id: "431", retry_id: null }, retired_through: null });
  await bound;
  assert.equal(h.saves.length, 0);
  assert.equal(h.completions.length, 0);
  assert.equal(h.form.inert, false);
});

test("G3.264 R1: rollback awaiting before quit cannot replace the draft or short-circuit retry", async () => {
  const h = lifecycleHarness();
  const autosave = h.enqueue().catch(error => error);
  await tick();
  h.saves[0].reject(new Error("ordinary save failed"));
  await tick();
  assert.equal(h.reads.length, 1);
  const quitting = h.begin("201").catch(error => error);
  h.reads[0].resolve({ theme: "light" });
  await autosave;
  await tick();
  assert.equal(h.draft.theme, "dark");
  assert.equal(h.inspect().savedRevision, 0);
  h.saves[1].reject(new Error("quit save failed"));
  assert.match(String(await quitting), /quit save failed/);
  const retry = h.begin("201", "1");
  await tick();
  assert.equal(h.saves.length, 3, "native Retry must perform a real save IPC");
  assert.equal(h.saves[2].args.input.theme, "dark");
  h.saves[2].resolve({ settings: { theme: "dark" } });
  await tick();
  h.completions[0].resolve(true);
  await retry;
  assert.equal(h.inspect().savedRevision, 1);
});

test("G3.264 R1: a quit started then cancelled still retires a pending rollback reply", async () => {
  const h = lifecycleHarness();
  const autosave = h.enqueue().catch(error => error);
  await tick();
  h.saves[0].reject(new Error("ordinary save failed"));
  await tick();
  const quitting = h.begin("202").catch(error => error);
  h.cancel("202");
  h.reads[0].resolve({ theme: "light" });
  await autosave;
  await tick();
  assert.equal(h.draft.theme, "dark");
  assert.equal(h.inspect().savedRevision, 0);
  h.saves[1].resolve({ settings: { theme: "dark" } });
  await quitting;
  assert.equal(h.completions.length, 0);
});

test("G3.264 R1 control: ordinary rollback without a quit still restores settings", async () => {
  const h = lifecycleHarness();
  const autosave = h.enqueue();
  await tick();
  h.saves[0].reject(new Error("ordinary save failed"));
  await tick();
  h.reads[0].resolve({ theme: "light" });
  await autosave;
  assert.equal(h.draft.theme, "light");
  assert.equal(h.inspect().savedRevision, 1);
});

test("G3.264 R2: saving accepts edits but final handoff locks the latest flushed input", async () => {
  const h = lifecycleHarness();
  const quitting = h.begin("203");
  await tick();
  assert.equal(h.form.inert, false);
  assert.equal(h.input("contrast"), true);
  h.saves[0].resolve({ settings: { theme: "dark" } });
  await tick();
  assert.equal(h.saves.length, 2);
  assert.equal(h.saves[1].args.input.theme, "contrast");
  h.saves[1].resolve({ settings: { theme: "contrast" } });
  await tick();
  assert.equal(h.completions.length, 1);
  assert.equal(h.form.inert, true);
  assert.equal(h.form.attributes["aria-disabled"], "true");
  assert.equal(h.input("lost"), false);
  assert.equal(h.inspect().localRevision, 2);
  h.completions[0].resolve(true);
  await quitting;
  assert.equal(h.form.inert, true, "successful native handoff stays locked through exit cleanup");
});

test("G3.264 R2: completion IPC failure restores input without claiming a save failure", async () => {
  const h = lifecycleHarness();
  const quitting = h.begin("204").catch(error => error);
  await tick();
  h.saves[0].resolve({ settings: { theme: "dark" } });
  await tick();
  assert.equal(h.form.inert, true);
  h.completions[0].reject(new Error("completion unavailable"));
  assert.match(String(await quitting), /completion unavailable/);
  assert.equal(h.form.inert, false);
  assert.equal(h.input("retry-edit"), true);
  assert.equal(h.calls.at(-1).command, "fail_app_quit");
  assert.equal(h.calls.at(-1).args.retryable, false);
});

test("G3.264 R2: native refusal retires the handoff and allows normal input", async () => {
  const h = lifecycleHarness();
  const quitting = h.begin("205");
  await tick();
  h.saves[0].resolve({ settings: { theme: "dark" } });
  await tick();
  h.completions[0].resolve(false);
  await quitting;
  assert.equal(h.form.inert, false);
  assert.equal(h.inspect().quitRequestId, null);
  assert.equal(h.input("kept"), true);
});

test("G3.264 R2: stale completion failure cannot unlock a newer handoff", async () => {
  const h = lifecycleHarness();
  const old = h.begin("206").catch(error => error);
  await tick();
  h.saves[0].resolve({ settings: { theme: "dark" } });
  await tick();
  h.cancel("206");
  assert.equal(h.form.inert, false);
  assert.equal(h.input("new-draft"), true);
  const current = h.begin("207");
  await tick();
  h.saves[1].resolve({ settings: { theme: "new-draft" } });
  await tick();
  assert.equal(h.form.inert, true);
  h.completions[0].reject(new Error("old completion unavailable"));
  await old;
  assert.equal(h.form.inert, true);
  assert.equal(h.calls.filter(call => call.command === "fail_app_quit").length, 0);
  h.completions[1].resolve(true);
  await current;
});

test("G3.264 R2: quit cancellation and updater busy changes retain each other's input lock", async () => {
  const h = lifecycleHarness();
  const quitting = h.begin("208");
  await tick();
  h.saves[0].resolve({ settings: { theme: "dark" } });
  await tick();
  h.checkBusy(true);
  h.checkBusy(false);
  h.installBusy(false);
  assert.equal(h.form.inert, true, "updater state sync must not release quit ownership");
  h.installBusy(true);
  h.cancel("208");
  assert.equal(h.form.inert, true, "quit cancellation must not release updater ownership");
  h.installBusy(false);
  assert.equal(h.form.inert, false);
  h.completions[0].resolve(false);
  await quitting;
});

test("G3.264 R2: a queued save failure releases handoff and native Retry performs a real save", async () => {
  const h = lifecycleHarness();
  const quitting = h.begin("209").catch(error => error);
  await tick();
  const queued = h.enqueue().catch(error => error);
  h.saves[0].resolve({ settings: { theme: "dark" } });
  await tick();
  assert.equal(h.completions.length, 0, "all previously admitted saves must settle before native handoff");
  assert.equal(h.form.inert, true);
  h.saves[1].reject(new Error("queued save failed"));
  await queued;
  assert.match(String(await quitting), /queued save failed/);
  assert.equal(h.form.inert, false);
  assert.equal(h.draft.theme, "dark");
  assert.equal(h.calls.at(-1).args.retryable, true);
  const retry = h.begin("209", "1").catch(error => error);
  await tick();
  assert.equal(h.saves.length, 3, "explicit Retry cannot reuse a rejected queue even if a prior save succeeded");
  h.saves[2].resolve({ settings: { theme: "dark" } });
  await tick();
  h.completions[0].resolve(true);
  await retry;
});

test("cancel then fresh quit retries a failed save even with an already clean revision", async () => {
  const h = lifecycleHarness();
  const autosave = h.enqueue();
  const firstQuit = h.begin("301").catch(error => error);
  await tick();
  h.saves[0].resolve({ settings: { theme: "dark" } });
  await autosave;
  await tick();
  assert.equal(h.inspect().localRevision, h.inspect().savedRevision);
  h.saves[1].reject(new Error("one-shot save failure"));
  assert.match(String(await firstQuit), /one-shot save failure/);
  assert.equal(h.completions.length, 0);
  h.cancel("301");

  const nextQuit = h.begin("302");
  await tick();
  assert.equal(h.saves.length, 3);
  h.saves[2].resolve({ settings: { theme: "dark" } });
  await tick();
  assert.equal(h.completions[0].args.requestId, "302");
  h.completions[0].resolve(true);
  await nextQuit;
});
