import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { t } from "./i18n.js";

const source = fs.readFileSync(new URL("./settings.js", import.meta.url), "utf8").replace(/\r\n/g, "\n");
const start = source.indexOf("function renderUpdateStatus(");
const end = source.indexOf("async function saveSettings(", start);
assert.ok(start >= 0 && end > start);
const cached = { status: "update_available", current_version: "0.1.39", latest_version: "0.1.40",
  release_url: "https://github.com/Lv2dev/agent-juice/releases/tag/v0.1.40" };
const latest = { ...cached, latest_version: "0.1.41", release_url: "https://github.com/Lv2dev/agent-juice/releases/tag/v0.1.41" };
const current = { status: "current", current_version: "0.1.41", latest_version: "0.1.41", release_url: null };
function deferred() {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
const tick = () => new Promise(resolve => setImmediate(resolve));
function harness({ initial = cached, language = "ko", save = async () => {}, install, check = async () => latest } = {}) {
  const progress = () => ({ hidden: true, dataset: {}, attributes: {}, setAttribute(key, value) { this.attributes[key] = value; }, removeAttribute(key) { delete this.attributes[key]; } });
  const state = {
    form: { inert: false, setAttribute() {} }, updateStatusEl: { textContent: "", dataset: {} },
    updateBand: { hidden: true, setAttribute() {} }, updateVersionEl: { textContent: "" }, appVersionEls: [{ textContent: "" }],
    updateInstallButtons: [{ hidden: true }, { hidden: true }], updateFallbackButtons: [{ hidden: true }],
    updateInstallProgressEls: [progress(), progress()], updateProgressBars: [{ style: {} }, { style: {} }],
    updateProgressTexts: [{ textContent: "" }, { textContent: "" }], calls: [], events: null,
  };
  state.updateActionButtons = [...state.updateInstallButtons, {}];
  const context = vm.createContext({ ...state, initial, t, currentLanguageSettings: () => ({ language }),
    enqueueLatestSettingsSave: save, showSettingsToast() {}, tauriApi: () => ({ core: { Channel: class {} } }),
    async invoke(command, args) { state.calls.push({ command, args }); if (command === "check_for_updates") return check(); state.events = args.onEvent; return install(args); },
  });
  vm.runInContext(`let currentUpdateStatus=initial,updateInstallPromise=null,updateCheckPromise=null,updateInstallBusy=false,updateCheckBusy=false;
    ${source.slice(start, end)}
    renderUpdateStatus(initial);
    globalThis.begin=installAvailableUpdate;`, context);
  return { ...state, begin: context.begin, context, get events() { return state.events; } };
}

test("install uses the freshly checked native target instead of the cached version", async () => {
  const pending = deferred();
  const ui = harness({ install: async ({ onEvent }) => {
    onEvent.onmessage({ event: "checking" });
    await pending.promise;
    onEvent.onmessage({ event: "started", status: latest });
    return { outcome: "installing", status: latest };
  } });
  const attempt = ui.begin();
  await tick();
  assert.equal(ui.calls.length, 1);
  assert.equal(ui.calls[0].command, "install_update");
  assert.equal(ui.calls[0].args.expectedVersion, undefined, "cached targets must not constrain a fresh update");
  assert.equal(ui.updateInstallProgressEls[0].dataset.state, "checking");
  assert.equal(ui.updateInstallProgressEls[0].attributes["aria-valuenow"], undefined);
  assert.match(ui.updateProgressTexts[0].textContent, /업데이트를 확인/);
  pending.resolve();
  await attempt;
  assert.equal(ui.updateVersionEl.textContent, "v0.1.41");
  assert.equal(ui.form.inert, true, "successful handoff stays locked until exit");
  assert.ok(ui.updateInstallButtons.every(button => button.disabled));
});

test("an install click can recover even without a cached target", async () => {
  const ui = harness({ initial: null, install: async () => ({ outcome: "installing", status: latest }) });
  await ui.begin();
  assert.equal(ui.calls.length, 1);
  assert.equal(ui.form.inert, true);
});

test("already-current response clears both progress displays and unlocks without installing", async () => {
  const ui = harness({ install: async () => ({ outcome: "current", status: current }) });
  await ui.begin();
  assert.equal(ui.updateStatusEl.dataset.state, "current");
  assert.match(ui.updateStatusEl.textContent, /최신 버전/);
  assert.equal(ui.updateBand.hidden, true);
  assert.ok(ui.updateInstallButtons.every(button => button.hidden && !button.disabled));
  assert.ok(ui.updateInstallProgressEls.every(progress => progress.hidden));
  assert.equal(ui.updateFallbackButtons[0].hidden, true);
  assert.equal(ui.form.inert, false);
});

test("duplicate clicks share one operation while settings are being saved", async () => {
  const saved = deferred();
  const ui = harness({ save: () => saved.promise, install: async () => ({ outcome: "current", status: current }) });
  const first = ui.begin(), second = ui.begin();
  assert.equal(ui.form.inert, true);
  assert.equal(ui.calls.length, 0);
  saved.resolve();
  await Promise.all([first, second]);
  assert.equal(ui.calls.length, 1);
  assert.equal(ui.form.inert, false);
});

test("a failed check remains retryable and never falls back to a cached target", async () => {
  let fail = true;
  const ui = harness({ install: async () => { if (fail) throw "update check failed"; return { outcome: "installing", status: latest }; } });
  await ui.begin();
  assert.equal(ui.calls.length, 1);
  assert.equal(ui.updateStatusEl.dataset.state, "error");
  assert.equal(ui.form.inert, false);
  assert.equal(ui.updateFallbackButtons[0].hidden, false);
  fail = false;
  await ui.begin();
  assert.equal(ui.calls.length, 2);
  assert.equal(ui.calls[1].args.expectedVersion, undefined);
  assert.equal(ui.form.inert, true);
});

test("settings save failure still prevents native update admission", async () => {
  const ui = harness({ save: async () => { throw new Error("save failed"); }, install: async () => assert.fail("must not install") });
  await ui.begin();
  assert.equal(ui.calls.length, 0);
  assert.match(ui.updateProgressTexts[0].textContent, /설정을 저장하지 못해/);
  assert.equal(ui.form.inert, false);
});

test("unknown native outcomes do not leave the UI locked as a successful handoff", async () => {
  const ui = harness({ install: async () => null });
  await ui.begin();
  assert.equal(ui.form.inert, false);
  assert.equal(ui.updateStatusEl.dataset.state, "error");
});

test("checking and refreshed target feedback works in English", async () => {
  const gate = deferred();
  const handoff = deferred();
  const ui = harness({ language: "en", install: async ({ onEvent }) => {
    onEvent.onmessage({ event: "checking" });
    await gate.promise;
    onEvent.onmessage({ event: "started", status: latest });
    onEvent.onmessage({ event: "progress", downloaded_bytes: 25, content_length: 100 });
    await handoff.promise;
    return { outcome: "installing", status: latest };
  } });
  const attempt = ui.begin();
  await tick();
  assert.match(ui.updateProgressTexts[0].textContent, /Checking for updates/);
  gate.resolve();
  await tick();
  assert.equal(ui.updateVersionEl.textContent, "v0.1.41");
  assert.equal(ui.updateProgressTexts[0].textContent, "Downloading update 25%");
  assert.equal(ui.updateInstallProgressEls[0].attributes["aria-valuenow"], "25");
  handoff.resolve();
  await attempt;
  assert.match(ui.updateProgressTexts[0].textContent, /Installing now/);
  assert.equal(ui.updateInstallProgressEls[0].attributes["aria-valuenow"], "100");
});

test("late channel messages cannot resurrect a completed current operation", async () => {
  const ui = harness({ install: async () => ({ outcome: "current", status: current }) });
  await ui.begin();
  for (const event of [{ event: "checking" }, { event: "started", status: latest },
    { event: "progress", downloaded_bytes: 1, content_length: 2 }, { event: "verifying" }]) ui.events.onmessage(event);
  assert.equal(ui.updateStatusEl.dataset.state, "current");
  assert.ok(ui.updateInstallProgressEls.every(progress => progress.hidden));
  assert.equal(ui.form.inert, false);
});

test("failed attempt messages cannot overwrite errors or a later attempt", async () => {
  const pending = deferred();
  let attempt = 0, oldChannel;
  const ui = harness({ install: async ({ onEvent }) => {
    if (++attempt === 1) { oldChannel = onEvent; throw "update check failed"; }
    onEvent.onmessage({ event: "checking" });
    await pending.promise;
    return { outcome: "current", status: current };
  } });
  await ui.begin();
  oldChannel.onmessage({ event: "progress", downloaded_bytes: 1, content_length: 2 });
  assert.equal(ui.updateStatusEl.dataset.state, "error");
  const retry = ui.begin();
  await tick();
  oldChannel.onmessage({ event: "started", status: latest });
  oldChannel.onmessage({ event: "verifying" });
  assert.equal(ui.updateStatusEl.dataset.state, "checking");
  assert.equal(ui.form.inert, true);
  pending.resolve();
  await retry;
  assert.equal(ui.calls.length, 2);
  assert.equal(ui.updateStatusEl.dataset.state, "current");
});

test("installing outcome finalizes progress even when the final channel event is late", async () => {
  const ui = harness({ install: async () => ({ outcome: "installing", status: latest }) });
  await ui.begin();
  ui.events.onmessage({ event: "checking" });
  assert.equal(ui.updateStatusEl.dataset.state, "installing");
  assert.ok(ui.updateInstallProgressEls.every(progress => progress.attributes["aria-valuenow"] === "100"));
  assert.equal(ui.form.inert, true);
});

test("older general status cannot replace the active install target or its progress", async () => {
  const pending = deferred();
  const fresh = { ...latest, checked_at: "2026-10-28T01:01:00Z" };
  const ui = harness({ install: async ({ onEvent }) => {
    onEvent.onmessage({ event: "started", status: fresh });
    onEvent.onmessage({ event: "progress", downloaded_bytes: 50, content_length: 100 });
    await pending.promise;
    return { outcome: "installing", status: fresh };
  } });
  const attempt = ui.begin();
  await tick();
  vm.runInContext(`renderUpdateStatus(${JSON.stringify({ ...cached, checked_at: "2026-10-28T01:00:00Z" })})`, ui.context);
  assert.equal(ui.updateVersionEl.textContent, "v0.1.41");
  assert.equal(ui.updateStatusEl.dataset.state, "downloading");
  pending.resolve();
  await attempt;
});

test("an older general status cannot resurrect an outdated target after completion", async () => {
  const fresh = { ...current, checked_at: "2026-10-28T01:01:00Z" };
  const ui = harness({ install: async () => ({ outcome: "current", status: fresh }) });
  await ui.begin();
  vm.runInContext(`renderUpdateStatus(${JSON.stringify({ ...cached, checked_at: "2026-10-28T01:00:00Z" })})`, ui.context);
  assert.equal(ui.updateStatusEl.dataset.state, "current");
  assert.equal(ui.updateBand.hidden, true);
});

test("the matching general publication cannot erase an installation error", async () => {
  const fresh = { ...latest, checked_at: "2026-10-28T01:01:00Z" };
  const ui = harness({ install: async ({ onEvent }) => {
    onEvent.onmessage({ event: "started", status: fresh });
    throw "update download or signature verification failed";
  } });
  await ui.begin();
  const error = ui.updateStatusEl.textContent;
  vm.runInContext(`renderUpdateStatus(${JSON.stringify(fresh)})`, ui.context);
  assert.equal(ui.updateStatusEl.dataset.state, "error");
  assert.equal(ui.updateStatusEl.textContent, error);
  assert.equal(ui.form.inert, false);
});

test("installing reply supplies the target even when started arrives after completion", async () => {
  const ui = harness({ install: async () => ({ outcome: "installing", status: latest }) });
  await ui.begin();
  ui.events.onmessage({ event: "started", status: latest });
  assert.equal(ui.updateVersionEl.textContent, "v0.1.41");
  assert.equal(ui.updateStatusEl.dataset.state, "installing");
  assert.equal(ui.updateInstallProgressEls[0].attributes["aria-valuenow"], "100");
});

test("explicit update checks can recover from a prior error with the same snapshot", async () => {
  const fresh = { ...latest, checked_at: "2026-10-28T01:01:00Z" };
  const ui = harness({ check: async () => fresh, install: async ({ onEvent }) => {
    onEvent.onmessage({ event: "started", status: fresh });
    throw "update check failed";
  } });
  await ui.begin();
  assert.equal(ui.updateStatusEl.dataset.state, "error");
  await vm.runInContext("checkForUpdates()", ui.context);
  assert.equal(ui.updateStatusEl.dataset.state, "update_available");
  assert.equal(ui.updateVersionEl.textContent, "v0.1.41");
  assert.equal(ui.form.inert, false);
});
