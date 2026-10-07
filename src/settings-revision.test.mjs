import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { formStateFromSettings, payloadFromEntries } from "./settings-state.js";
import { DEFAULT_SETTINGS } from "./panel-state.js";
import { barViewModel } from "./bar-state.js";
import { createSettingsAuthority } from "./settings-revision.js";

const settingsSource = fs.readFileSync(new URL("./settings.js", import.meta.url), "utf8").replace(/\r\n/g, "\n");
const barSource = fs.readFileSync(new URL("./bar.js", import.meta.url), "utf8").replace(/\r\n/g, "\n");
const panelSource = fs.readFileSync(new URL("./panel.js", import.meta.url), "utf8").replace(/\r\n/g, "\n");
function between(source, first, next) {
  const start = source.indexOf(first);
  const end = source.indexOf(next, start + first.length);
  assert.ok(start >= 0 && end > start, first);
  return source.slice(start, end);
}
function helpers(source, next) {
  const first = ["function settingsSnapshotRevision(", "function rememberAuthoritativeSettings(", "function acceptSettingsSnapshot("]
    .find(marker => source.includes(marker));
  return first ? between(source, first, next) : "";
}
function handler(source, first, next) {
  const body = between(source, first, next).slice(first.length).trim();
  assert.ok(body.endsWith(");"));
  return body.slice(0, -2);
}
function deferred() {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
}
const initial = { ...DEFAULT_SETTINGS, bar_mode: "full", settings_revision: "9007199254740992" };
const latest = { ...initial, bar_mode: "compact", settings_revision: "9007199254740993" };

function formContext(invoke, loaded = initial) {
  const fields = new Map();
  const form = { elements: { namedItem(name) {
    if (!fields.has(name)) fields.set(name, {
      type: name.startsWith("show_") || name.endsWith("_on") ? "checkbox" : "text", value: "", checked: false,
    });
    return fields.get(name);
  } } };
  class FormDataMock {
    constructor() {
      this.entries = [...fields].filter(([, field]) => field.type !== "checkbox" || field.checked)
        .map(([name, field]) => [name, field.type === "checkbox" ? "on" : field.value]);
    }
    [Symbol.iterator]() { return this.entries[Symbol.iterator](); }
  }
  const context = vm.createContext({ form, FormData: FormDataMock, formStateFromSettings, payloadFromEntries, invoke, createSettingsAuthority,
    window: { events: [], dispatchEvent(event) { this.events.push(event); } }, CustomEvent: class { constructor(type, args) { this.type = type; this.detail = args.detail; } },
    applyTheme() {}, applyPanelSkin() {}, applyFont() {}, applyTranslations() {}, updateOutputs() {}, renderUpdateStatus() {},
    publishPreview() {}, setSettingsFormEnabled() {}, setStatus() {}, showSettingsToast() {}, t: key => key, wait: async () => {},
    currentLanguageSettings: () => ({ language: "ko" }),
  });
  vm.runInContext(`let localRevision=0,savedRevision=0,settingsEventGeneration=0,isHydrating=false,hasLoadedSettings=true,currentDisplayBasis='remaining',currentUpdateStatus=null;
    const settingsAuthority=createSettingsAuthority();
    let editSession={baseline:null,topology:{monitor_keys:[],monitor_modes:[]}};
    const SETTINGS_LOAD_RETRY_DELAYS_MS=[0];
    ${helpers(settingsSource, "function currentLanguageSettings(")}
    ${between(settingsSource, "function setField(", "function formatRangeProgress(")}
    ${between(settingsSource, "function fillForm(", "function publishPreview(")}
    ${between(settingsSource, "function hydrateSettings(", "function renderUpdateStatus(")}
    ${between(settingsSource, "async function saveSettings(", "function enqueueLatestSettingsSave(")}
    globalThis.onSettings=${handler(settingsSource, 'void register("settings-updated", ', 'void register("update-status",')};
    globalThis.startSave=()=>saveSettings({bar_mode:'full'},localRevision,editSession);
    globalThis.startRollback=()=>rollbackFailedSave(localRevision,new Error('synthetic storage failure'));
    globalThis.startLoad=loadSettings;
    globalThis.edit=(mode)=>{localRevision+=1;form.elements.namedItem('bar_mode').value=mode;};
    globalThis.inspect=()=>({mode:form.elements.namedItem('bar_mode').value,localRevision,savedRevision,revision:settingsAuthority.current?.settings_revision??null});
    if(typeof rememberAuthoritativeSettings==='function')rememberAuthoritativeSettings(${JSON.stringify(loaded)});
    fillForm(${JSON.stringify(loaded)});`, context);
  return context;
}

function barContext(invoke = async () => null) {
  const context = vm.createContext({ DEFAULT_SETTINGS, invoke, barViewModel, createSettingsAuthority, applyTheme() {}, applyFont() {}, applyTranslations() {}, loadTaskbarOrientation: async () => {} });
  vm.runInContext(`let settings={...DEFAULT_SETTINGS},settingsEventGeneration=0;
    const settingsAuthority=createSettingsAuthority();
    ${helpers(barSource, "function withTimeout(")}
    function renderBar(){globalThis.view=barViewModel([],settings,new Date(),{});}
    ${between(barSource, "async function loadSettings()", "async function loadTaskbarOrientation(")}
    globalThis.onSettings=${handler(barSource, 'void listenWithRetry(listen, "settings-updated", ', 'void listenWithRetry(listen, "taskbar-dragging-updated",')};
    globalThis.startLoad=loadSettings;
    globalThis.inspect=()=>({mode:settings.bar_mode,revision:settingsAuthority.current?.settings_revision??null});`, context);
  return context;
}

function panelContext(invoke = async () => null) {
  const context = vm.createContext({ DEFAULT_SETTINGS, createSettingsAuthority, invoke,
    applyTheme() {}, applyPanelSkin() {}, applyFont() {}, applyTranslations() {}, renderActivity() {},
  });
  vm.runInContext(`let settings={...DEFAULT_SETTINGS},settingsEventGeneration=0,lastStatuses=[];
    const settingsAuthority=createSettingsAuthority();
    ${between(panelSource, "function acceptPanelSettings(", "function selectPanelView(")}
    ${between(panelSource, "async function loadSettings()", "async function loadStatus(")}
    function renderStatuses() {globalThis.lastMode=settings.bar_mode;}
    globalThis.onLocal=${handler(panelSource, 'window.addEventListener("settings-updated", ', 'async function loadSettings()')};
    globalThis.onNative=${handler(panelSource, 'void listenWithRetry(listen, "settings-updated", ', 'void listenWithRetry(listen, "activity-updated",')};
    globalThis.startLoad=loadSettings;
    globalThis.inspect=()=>({mode:settings.bar_mode,revision:settingsAuthority.current?.settings_revision??null});`, context);
  return context;
}

test("settings metadata stays flat and all backend publication sites read current settings", () => {
  const source = fs.readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  assert.match(source, /#\[serde\(flatten\)\]\s*settings: Settings,\s*settings_revision: String/);
  assert.equal([...source.matchAll(/app\.emit\("settings-updated"/g)].length, 1);
  assert.match(source, /async fn get_settings\(\) -> Result<SettingsSnapshot, String>/);
  assert.match(between(source, "fn with_current_settings_snapshot<T>(", "fn current_settings_snapshot("), /with_taskbar_settings_read[\s\S]*Settings::try_load\(\)[\s\S]*reader\(&snapshot\)/);
  assert.match(between(source, "async fn save_settings(", "fn preserve_taskbar_leading_edges("), /publish_current_settings\(&app\)[\s\S]*settings_apply_report\(published/);
});

test("dirty form keeps input while tracking a newer event and chooses it over an old save reply", async () => {
  const response = deferred();
  const form = formContext(async () => response.promise);
  form.edit("full");
  const pending = form.startSave();
  form.onSettings({ payload: latest });
  assert.equal(form.inspect().mode, "full", "pending user input must not be hydrated by an event");
  response.resolve({ settings: initial, warnings: [] });
  await pending;
  assert.equal(form.inspect().mode, "compact");
  assert.equal(form.inspect().localRevision, form.inspect().savedRevision);
});

test("failed save recovery rejects an old read after a newer authoritative event", async () => {
  const response = deferred();
  const form = formContext(async () => response.promise);
  form.edit("quad");
  const pending = form.startRollback();
  form.onSettings({ payload: latest });
  assert.equal(form.inspect().mode, "quad");
  response.resolve(initial);
  await pending;
  assert.equal(form.inspect().mode, "compact");
  assert.equal(form.inspect().localRevision, form.inspect().savedRevision);
});

test("bar rejects older and duplicate snapshots without losing decimal u64 precision", () => {
  const bar = barContext();
  bar.onSettings({ payload: latest });
  bar.onSettings({ payload: initial });
  assert.equal(bar.inspect().mode, "compact");
  assert.equal(bar.view.mode, "compact");
  bar.onSettings({ payload: { ...latest, bar_mode: "quad" } });
  assert.equal(bar.inspect().mode, "compact");
  const maximum = { ...latest, bar_mode: "dual", settings_revision: "18446744073709551615" };
  bar.onSettings({ payload: maximum });
  bar.onSettings({ payload: { ...initial, settings_revision: "18446744073709551614" } });
  assert.equal(bar.inspect().mode, "dual");
  assert.equal(bar.inspect().revision, maximum.settings_revision);
});

test("new local edits still reject old successful-save and rollback responses", async () => {
  for (const rollback of [false, true]) {
    const response = deferred(), form = formContext(async () => response.promise);
    form.edit("full");
    const pending = rollback ? form.startRollback() : form.startSave();
    form.edit("quad");
    response.resolve(rollback ? initial : { settings: initial });
    await pending;
    assert.equal(form.inspect().mode, "quad");
  }
});

test("clean form and bar startup reads cannot replace a newer settings event", async () => {
  const a = deferred(), b = deferred();
  const form = formContext(async () => a.promise), bar = barContext(async () => b.promise);
  const reads = [form.startLoad(), bar.startLoad()];
  form.onSettings({ payload: latest }); bar.onSettings({ payload: latest });
  a.resolve(initial); b.resolve(initial);
  await Promise.all(reads);
  assert.equal(form.inspect().mode, "compact");
  assert.equal(bar.inspect().mode, "compact");
});

test("legacy flat settings remain compatible but cannot overwrite a versioned snapshot", async () => {
  const legacy = { ...DEFAULT_SETTINGS, bar_mode: "full" };
  const response = deferred(), form = formContext(async () => response.promise, legacy), bar = barContext();
  bar.onSettings({ payload: legacy }); assert.equal(bar.inspect().mode, "full");
  form.edit("quad"); const pending = form.startRollback();
  form.onSettings({ payload: { ...legacy, bar_mode: "compact" } });
  response.resolve(legacy); await pending;
  assert.equal(form.inspect().mode, "compact");
  bar.onSettings({ payload: latest }); bar.onSettings({ payload: legacy });
  assert.equal(bar.inspect().mode, "compact");
});

test("malformed revision metadata cannot displace an authoritative snapshot", () => {
  const bar = barContext(); bar.onSettings({ payload: latest });
  for (const settings_revision of [42, "", "-1", "1.5", "18446744073709551616", "01"]) {
    bar.onSettings({ payload: { ...initial, bar_mode: "quad", settings_revision } });
    assert.equal(bar.inspect().mode, "compact");
  }
});

test("panel native and local success events reject older and duplicate revisions", () => {
  const panel = panelContext();
  panel.onNative({ payload: latest });
  panel.onLocal({ detail: initial });
  assert.equal(panel.inspect().mode, latest.bar_mode);
  panel.onLocal({ detail: { ...latest, bar_mode: "quad" } });
  panel.onNative({ payload: initial });
  assert.equal(panel.inspect().mode, latest.bar_mode);
  assert.equal(panel.inspect().revision, latest.settings_revision);
});

test("panel startup read cannot replace the newer snapshot received on either event channel", async () => {
  for (const local of [false, true]) {
    const response = deferred(), panel = panelContext(async () => response.promise);
    const pending = panel.startLoad();
    if (local) panel.onLocal({ detail: latest });
    else panel.onNative({ payload: latest });
    response.resolve(initial); await pending;
    assert.equal(panel.inspect().mode, latest.bar_mode);
  }
});

test("form success passes the same selected snapshot to the local panel consumer", async () => {
  const response = deferred(), form = formContext(async () => response.promise), panel = panelContext();
  panel.onNative({ payload: initial });
  form.edit("full"); const pending = form.startSave();
  form.onSettings({ payload: latest });
  response.resolve({ settings: initial }); await pending;
  const event = form.window.events.find(event => event.type === "settings-updated");
  assert.equal(event.detail, latest);
  panel.onLocal(event);
  assert.equal(panel.inspect().mode, latest.bar_mode);
  assert.equal(panel.inspect().revision, latest.settings_revision);
});

test("common authority rejects malformed replies instead of treating them as a successful recovery", () => {
  const authority = createSettingsAuthority();
  assert.equal(authority.accept(latest), true);
  for (const settings_revision of [null, "", "01", "18446744073709551616"]) {
    assert.equal(authority.reply({ ...initial, settings_revision }), null);
    assert.equal(authority.current, latest);
  }
});
