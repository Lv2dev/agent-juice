import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { DEFAULT_SETTINGS } from "./panel-state.js";
import { createSettingsAuthority } from "./settings-revision.js";
import { formStateFromSettings, payloadFromEntries } from "./settings-state.js";

const source = fs.readFileSync(new URL("./settings.js", import.meta.url), "utf8").replace(/\r\n/g, "\n");
function between(first, next) {
  const start = source.indexOf(first), end = source.indexOf(next, start + first.length);
  assert.ok(start >= 0 && end > start, first);
  return source.slice(start, end);
}
function deferred() {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
}
function plain(value) { return JSON.parse(JSON.stringify(value)); }
const empty = () => ({ monitor_keys: [], monitor_modes: [] });
const laptopMode = { monitor_key: "laptop", width: 1920, height: 1080, dpi: 96 };
const officeMode = { monitor_key: "office", width: 3840, height: 2160, dpi: 144 };
const laptop = { monitor_keys: ["laptop"], monitor_modes: [laptopMode] };
const docked = { monitor_keys: ["laptop", "office"], monitor_modes: [laptopMode, officeMode] };
const history = [{ ...docked, ...Object.fromEntries(
  ["claude", "codex", "grok", "cursor", "antigravity"].map(tool => [tool, { monitor_key: "office", offset_ratio: 0.4 }]),
) }];
function snapshot(topology, revision = "1", extra = {}) {
  return { ...DEFAULT_SETTINGS, bar_mode: "full", taskbar_layout_memory_on: true,
    taskbar_profile_presentation_on: true, taskbar_layout_profiles: history,
    taskbar_topology: topology, settings_revision: revision, ...extra };
}

// Execute the product form, save, event and recovery functions with in-memory IPC only.
function formContext(loaded, invoke = async () => null) {
  const fields = new Map();
  const form = { elements: { namedItem(name) {
    if (!fields.has(name)) fields.set(name, {
      type: name.startsWith("show_") || name.startsWith("taskbar_show_") || name.endsWith("_on") ? "checkbox" : "text",
      value: "", checked: false,
    });
    return fields.get(name);
  } } };
  class FormDataMock {
    constructor() {
      this.entries = [...fields].filter(([, field]) => field.type !== "checkbox" || field.checked)
        .map(([name, field]) => [name, field.type === "checkbox" ? "on" : field.value]);
    }
    get(name) { return this.entries.find(([key]) => key === name)?.[1] ?? null; }
    [Symbol.iterator]() { return this.entries[Symbol.iterator](); }
  }
  const context = vm.createContext({ form, loaded, invoke, FormData: FormDataMock, createSettingsAuthority,
    formStateFromSettings, payloadFromEntries,
    window: { events: [], dispatchEvent(event) { this.events.push(event); } },
    CustomEvent: class { constructor(type, args) { this.type = type; this.detail = args.detail; } },
    applyTheme() {}, applyPanelSkin() {}, applyFont() {}, applyTranslations() {}, updateOutputs() {}, renderUpdateStatus() {},
    publishPreview() {}, setSettingsFormEnabled() {}, setStatus() {}, showSettingsToast() {}, t: key => key, wait: async () => {},
    currentLanguageSettings: () => ({ language: "ko" }),
  });
  const event = between('void register("settings-updated", ', 'void register("update-status",')
    .slice('void register("settings-updated", '.length).trim();
  assert.ok(event.endsWith(");"));
  vm.runInContext(`let localRevision=0,savedRevision=0,settingsEventGeneration=0,isHydrating=false,hasLoadedSettings=true,currentDisplayBasis='remaining',currentUpdateStatus=null;
    let editSession={baseline:null,topology:{monitor_keys:[],monitor_modes:[]}};
    const settingsAuthority=createSettingsAuthority();
    const SETTINGS_LOAD_RETRY_DELAYS_MS=[0];
    ${source.match(/const TASKBAR_VISIBILITY_TOOLS = [^\n]+/)[0]}
    ${between("function rememberAuthoritativeSettings(", "function currentLanguageSettings(")}
    ${between("function setField(", "function formatRangeProgress(")}
    ${between("function settingsInputFromForm(", "function syncRangeFromNumberEditor(")}
    ${between("function fillForm(", "function publishPreview(")}
    ${between("function hydrateSettings(", "function renderUpdateStatus(")}
    ${between("async function saveSettings(", "function enqueueLatestSettingsSave(")}
    globalThis.onSettings=${event.slice(0, -2)};
    globalThis.submit=()=>saveSettings(settingsInputFromForm(),localRevision,editSession);
    globalThis.rollback=()=>rollbackFailedSave(localRevision,new Error('monitor layout changed; settings reloaded, please retry the edit'));
    globalThis.load=loadSettings;
    globalThis.edit=(mode)=>{localRevision+=1;form.elements.namedItem('bar_mode').value=mode;};
    globalThis.session=()=>editSession;
    globalThis.inspect=()=>({mode:form.elements.namedItem('bar_mode').value,localRevision,savedRevision,revision:settingsAuthority.current?.settings_revision});
    rememberAuthoritativeSettings(loaded);
    fillForm(loaded);`, context);
  return context;
}

test("D1 fresh laptop metadata permits repeated edits while every historical bar remains on disconnected office", async () => {
  const loaded = snapshot(laptop);
  const originalHistory = plain(history);
  const requests = [];
  const form = formContext(loaded, async (command, args) => {
    assert.equal(command, "save_settings");
    requests.push(plain(args));
    assert.deepEqual(plain(args.editTopology), laptop, "match the current layout, not profile MRU");
    assert.equal(args.editBaseline.bar_mode, loaded.bar_mode);
    assert.equal(args.input.bar_mode, "compact");
    assert.equal(Object.hasOwn(args.input, "taskbar_topology"), false);
    assert.equal(Object.hasOwn(args.editBaseline, "taskbar_topology"), false);
    return { settings: snapshot(laptop, String(requests.length * 2 + 1), { bar_mode: "compact" }), warnings: [] };
  });
  for (let attempt = 0; attempt < 4; attempt += 1) {
    form.edit("compact");
    await form.submit();
    assert.equal(form.inspect().mode, "compact");
    if (attempt < 3) form.onSettings({ payload: snapshot(laptop, String(attempt * 2 + 4)) });
  }
  assert.equal(requests.length, 4);
  assert.equal(form.inspect().localRevision, form.inspect().savedRevision);
  assert.deepEqual(loaded.taskbar_layout_profiles, originalHistory, "do not repair or reorder historical profiles");
});

test("current-profile normal control and empty-history snapshots both use supplied topology", () => {
  for (const taskbar_layout_profiles of [[...history, laptop], []]) {
    const form = formContext(snapshot(laptop, "1", { taskbar_layout_profiles }));
    assert.deepEqual(plain(form.session().topology), laptop);
    assert.equal(form.session().baseline.bar_mode, "full");
  }
});

test("missing, unstable and malformed topology never fall back to historical profiles", () => {
  const invalid = [undefined, null, false, 42, "laptop", [], {}, empty(),
    { monitor_keys: "laptop", monitor_modes: [] },
    { monitor_keys: [""], monitor_modes: [] },
    { monitor_keys: ["  "], monitor_modes: [] },
    { monitor_keys: [42], monitor_modes: [] },
    { monitor_keys: new Array(1), monitor_modes: [] },
    { monitor_keys: ["laptop", "laptop"], monitor_modes: [] },
    { monitor_keys: ["laptop"], monitor_modes: null },
    { monitor_keys: ["laptop"], monitor_modes: {} },
    { monitor_keys: ["laptop"], monitor_modes: [null] },
    { monitor_keys: ["laptop"], monitor_modes: new Array(1) },
    { monitor_keys: ["laptop"], monitor_modes: [officeMode] },
    { monitor_keys: ["laptop", "office"], monitor_modes: [laptopMode] },
    { monitor_keys: [], monitor_modes: [laptopMode] },
  ];
  for (const field of ["width", "height", "dpi"]) {
    for (const value of [0, -1, 1.5, "96", null, NaN, Infinity, 4294967296]) {
      invalid.push({ monitor_keys: ["laptop"], monitor_modes: [{ ...laptopMode, [field]: value }] });
    }
  }
  for (const field of ["width", "height"]) {
    invalid.push({ monitor_keys: ["laptop"], monitor_modes: [{ ...laptopMode, [field]: 2147483648 }] });
  }
  for (const taskbar_topology of invalid) {
    const loaded = snapshot(taskbar_topology);
    if (taskbar_topology === undefined) delete loaded.taskbar_topology;
    const form = formContext(loaded);
    assert.deepEqual(plain(form.session().topology), empty(), JSON.stringify(taskbar_topology));
    assert.equal(form.inspect().mode, "full", "keep legacy scalar hydration");
  }
});

test("valid topology is copied without retaining arrays, mode objects or unknown fields", () => {
  const metadata = plain(docked);
  metadata.untrusted = { nested: true };
  metadata.monitor_modes[0].extra = { nested: true };
  const form = formContext(snapshot(metadata));
  assert.deepEqual(plain(form.session().topology), docked);
  metadata.monitor_keys[0] = "changed";
  metadata.monitor_modes[0].dpi = 192;
  metadata.monitor_modes.push(officeMode);
  assert.deepEqual(plain(form.session().topology), docked);
  form.session().topology.monitor_modes[1].dpi = 96;
  assert.equal(metadata.monitor_modes[1].dpi, 144);
  for (const topology of [{ monitor_keys: ["laptop"] }, { monitor_keys: ["laptop"], monitor_modes: [] }]) {
    assert.deepEqual(plain(formContext(snapshot(topology)).session().topology), { monitor_keys: ["laptop"], monitor_modes: [] });
  }
  const largest = { monitor_keys: ["laptop"], monitor_modes: [{ ...laptopMode, width: 2147483647, height: 2147483647, dpi: 4294967295 }] };
  assert.deepEqual(plain(formContext(snapshot(largest)).session().topology), largest);
});

test("topology-only newer revisions are authoritative while older, duplicate and legacy replies stay rejected", () => {
  const authority = createSettingsAuthority();
  const first = snapshot(docked, "9007199254740992");
  const next = snapshot(laptop, "9007199254740993");
  assert.equal(authority.accept(first), true);
  assert.equal(authority.accept(next), true);
  assert.equal(authority.current, next);
  assert.equal(authority.accept(first), false);
  assert.equal(authority.accept({ ...first, settings_revision: next.settings_revision }), false);
  const legacy = { ...first }; delete legacy.settings_revision;
  assert.equal(authority.reply(legacy, true), next);
  assert.equal(authority.reply(first), next);
  assert.equal(authority.accept(snapshot(empty(), "9007199254740994")), true);
  assert.deepEqual(authority.current.taskbar_topology, empty(), "unstable topology must supersede old stable metadata");
});

test("clean topology-only event replaces edit ownership and reverse events cannot restore old topology", () => {
  const form = formContext(snapshot(docked));
  const baseline = plain(form.session().baseline);
  form.onSettings({ payload: snapshot(laptop, "2") });
  assert.deepEqual(plain(form.session().topology), laptop);
  assert.deepEqual(plain(form.session().baseline), baseline, "metadata-only changes must hydrate without changing user settings");
  form.onSettings({ payload: snapshot(laptop, "3", { bar_content_gap_px: 3 }) });
  form.onSettings({ payload: snapshot(docked, "1") });
  form.onSettings({ payload: snapshot(docked, "3") });
  assert.deepEqual(plain(form.session().topology), laptop);
  assert.equal(form.session().baseline.bar_content_gap_px, 3);
  assert.equal(form.inspect().revision, "3");
});

test("dirty input retains its original topology and baseline until recovery adopts one newer snapshot", async () => {
  const response = deferred();
  const requests = [];
  const form = formContext(snapshot(docked), async (command, args) => {
    if (command === "get_settings") return response.promise;
    requests.push(plain(args));
    throw new Error("monitor layout changed; settings reloaded, please retry the edit");
  });
  const original = plain(form.session());
  form.edit("compact");
  form.onSettings({ payload: snapshot(laptop, "2", { bar_content_gap_px: 3 }) });
  assert.deepEqual(plain(form.session()), original);
  await assert.rejects(form.submit(), /monitor layout changed/);
  assert.deepEqual(requests[0].editTopology, docked);
  assert.equal(requests[0].editBaseline.bar_content_gap_px, original.baseline.bar_content_gap_px);
  const pending = form.rollback();
  form.onSettings({ payload: snapshot(laptop, "3", { bar_mode: "dual", bar_content_gap_px: 5 }) });
  form.onSettings({ payload: snapshot(docked, "1") });
  response.resolve(snapshot(docked, "1"));
  await pending;
  assert.equal(form.inspect().mode, "dual");
  assert.equal(form.session().baseline.bar_mode, "dual");
  assert.equal(form.session().baseline.bar_content_gap_px, 5);
  assert.deepEqual(plain(form.session().topology), laptop);
  assert.equal(form.inspect().localRevision, form.inspect().savedRevision);
});

test("old save and startup replies cannot split a newer topology from its baseline", async () => {
  for (const operation of ["save", "load"]) {
    const response = deferred();
    const first = snapshot(docked);
    const next = snapshot(laptop, "2", { bar_mode: "dual", bar_content_gap_px: 5 });
    const form = formContext(first, async () => response.promise);
    if (operation === "save") form.edit("compact");
    const pending = operation === "save" ? form.submit() : form.load();
    form.onSettings({ payload: next });
    response.resolve(operation === "save" ? { settings: first } : first);
    await pending;
    assert.equal(form.session().baseline.bar_mode, "dual");
    assert.equal(form.session().baseline.bar_content_gap_px, 5);
    assert.deepEqual(plain(form.session().topology), laptop);
    assert.equal(form.inspect().revision, "2");
    if (operation === "save") assert.equal(form.window.events.at(-1).detail, next);
  }
});

test("new local edits keep their owned session when old save or recovery replies arrive", async () => {
  for (const operation of ["save", "rollback"]) {
    const response = deferred();
    const form = formContext(snapshot(docked), async () => response.promise);
    form.edit("compact");
    const owned = form.session();
    const pending = operation === "save" ? form.submit() : form.rollback();
    form.onSettings({ payload: snapshot(laptop, "2", { bar_mode: "dual" }) });
    form.edit("quad");
    response.resolve(operation === "save" ? { settings: snapshot(docked) } : snapshot(docked));
    await pending;
    assert.equal(form.inspect().mode, "quad");
    assert.equal(form.session(), owned);
    assert.deepEqual(plain(form.session().topology), docked);
    assert.equal(form.session().baseline.bar_mode, operation === "save" ? "compact" : "full");
  }
});
