import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { DEFAULT_SETTINGS, viewModelForTool } from "./panel-state.js";
import { collectionRetryState, panelCollectionIssue } from "./collection-state.js";
import { resolveLanguage, t } from "./i18n.js";
import { createStatusRequests } from "./status-requests.js";

const source = readFileSync(new URL("./panel.js", import.meta.url), "utf8").replaceAll("\r\n", "\n");
const between = (first, next) => {
  const start = source.indexOf(first), end = source.indexOf(next, start + first.length);
  assert.ok(start >= 0 && end > start, first);
  return source.slice(start, end).replaceAll("export function ", "function ");
};
const settle = () => new Promise(resolve => setImmediate(resolve));
function deferred() {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
function node() {
  const children = new Map(), attributes = new Map();
  return {
    hidden: false, disabled: false, dataset: {}, textContent: "", style: { setProperty() {} },
    setAttribute(key, value) { attributes.set(key, value); },
    getAttribute(key) { return attributes.get(key); },
    querySelector(selector) { if (!children.has(selector)) children.set(selector, node()); return children.get(selector); },
  };
}
function sample(used = 40) {
  return { tool: "codex", captured_at: new Date().toISOString(), approx: false, session: { active: true }, primary: null, secondary: { used_percent: used } };
}
function harness({ language = "ko", health = "login_required", refresh, disabled = false } = {}) {
  const commands = [], timers = new Map(), handlers = new Map();
  const button = node(), feedback = node(), basis = node();
  const card = node();
  const document = {
    querySelector(selector) {
      return { '[data-tool="codex"]': card, "[data-overview-refresh]": button,
        "[data-status-refresh-feedback]": feedback, "[data-overview-basis]": basis }[selector] ?? null;
    },
    addEventListener(name, handler) { handlers.set(name, handler); },
  };
  let nextHealth = health;
  const context = vm.createContext({
    document, DEFAULT_SETTINGS, viewModelForTool, collectionRetryState, panelCollectionIssue, resolveLanguage, t,
    Date, TOOLS: ["claude", "codex", "grok", "cursor", "antigravity"],
    barPreview: { update() {} }, systemTextScale: { factor: 1 },
    invoke(command) { commands.push(command); return command === "refresh_status" ? refresh?.() : { codex: nextHealth }; },
    makeRequests: options => createStatusRequests({ ...options,
      setTimer(callback, delay) { const timer = {callback, delay}; timers.set(timer, timer); return timer; },
      clearTimer(timer) { timers.delete(timer); },
    }),
    tauriApi: () => ({event: {listen() {}}}),
    listenWithRetry(_listen, name, handle) { handlers.set(name, handle); },
    sendEvent(name, payload) { handlers.get(name)({payload}); },
    TEXT_SCALE_EVENT: "system-text-scale-updated",
  });
  vm.runInContext(`
    let settings = { ...DEFAULT_SETTINGS, language: ${JSON.stringify(language)}, show_claude: false, show_codex: ${!disabled}, show_grok: false, show_cursor: false, show_antigravity: false };
    let previewSettings = null, lastStatuses = [], collectionHealth = {codex: ${JSON.stringify(health)}};
    let listenersDisposed = false, usageRefreshBusy = false, usageRefreshPromise = null, usageRefreshResult = null;
    let statusEventGeneration = 0, collectionHealthEventGeneration = 0;
    ${between("function setText(", "function activityLanguage(")}
    const statusRequests = makeRequests({ invoke, getStatusGeneration: () => statusEventGeneration,
      getHealthGeneration: () => collectionHealthEventGeneration, acceptStatus: renderStatuses,
      acceptHealth: (health, renderCurrent) => { collectionHealth = health; if (renderCurrent) renderStatuses(lastStatuses); } });
    ${between("function bindWindowControls()", "function currentPanelWindow()")}
    ${between("function bindStatusUpdates()", "async function bootstrap()")}
    bindWindowControls();
    bindStatusUpdates();
    globalThis.panel = {
      refresh: refreshUsage, seed: renderStatuses,
      nativeStatusOnly: statuses => sendEvent("status-updated", statuses),
      native(statuses, health) {
        sendEvent("collection-health-updated", health);
        sendEvent("status-updated", statuses);
      },
      dispose() { listenersDisposed = true; statusRequests.dispose(); },
    };
  `, context);
  context.panel.seed([]);
  return {
    panel: context.panel, commands, timers, card, button, feedback,
    health(value) { nextHealth = value; },
    click(target = button) { handlers.get("click")({target: {closest: selector => selector === "[data-refresh-status]" ? target : null}, preventDefault() {}}); },
    async timeout(delay) { const timer = [...timers.values()].find(value => value.delay === delay); assert.ok(timer); timer.callback(); await settle(); },
  };
}

test("overview and all five tool cards provide localized refresh affordances and the Lucide asset", () => {
  const html = readFileSync(new URL("./index.html", import.meta.url), "utf8");
  assert.match(html, /data-overview-refresh data-refresh-status/);
  assert.match(html, /data-status-refresh-feedback role="status" aria-live="polite"/);
  assert.equal((html.match(/data-tool-retry hidden/g) ?? []).length, 5);
  assert.equal((html.match(/data-refresh-status data-retry-usage/g) ?? []).length, 5);
  const svg = readFileSync(new URL("./icons/refresh-cw.svg", import.meta.url), "utf8");
  assert.match(svg, /viewBox="0 0 24 24"/);
  assert.match(readFileSync(new URL("./icons/LICENSE.txt", import.meta.url), "utf8"), /Lucide Icons/);
  for (const language of ["ko", "en"]) {
    assert.notEqual(t("action.refreshUsage", language), "action.refreshUsage");
    assert.notEqual(t("action.retryUsage", language), "action.retryUsage");
  }
});

test("a missed health event does not keep valid numbers hidden after refresh", async () => {
  const reply = deferred();
  const h = harness({ refresh: () => reply.promise });
  const pending = h.panel.refresh();
  h.panel.nativeStatusOnly([sample(12)]);
  h.health("ready");
  reply.resolve([sample(50)]);
  await pending;
  assert.equal(h.card.querySelector(".hero-num").textContent, "88");
  assert.equal(h.feedback.textContent, t("status.refreshedUsage", "ko"));
  assert.deepEqual(h.commands, ["refresh_status", "get_collection_health"]);
  h.panel.dispose();
});

test("retry uses the common force path, blocks duplicates, and reconnects a signed-out card", async () => {
  for (const language of ["ko", "en"]) {
    const reply = deferred();
    const h = harness({ language, refresh: () => reply.promise });
    const retry = h.card.querySelector("[data-retry-usage]");
    assert.equal(h.card.querySelector("[data-tool-retry]").hidden, false);
    assert.match(retry.getAttribute("aria-label"), /Codex/);
    h.click(retry);
    const pending = h.panel.refresh();
    h.click();
    assert.deepEqual(h.commands, ["refresh_status"]);
    assert.equal(h.button.disabled, true);
    assert.equal(retry.disabled, true);
    assert.equal(h.feedback.textContent, t("status.refreshingUsage", language));
    h.health("ready");
    reply.resolve([sample()]);
    await pending;
    assert.equal(h.card.querySelector(".hero-num").textContent, "60");
    assert.equal(h.card.querySelector("[data-tool-retry]").hidden, true);
    assert.equal(h.button.disabled, false);
    assert.equal(h.button.getAttribute("aria-busy"), "false");
    assert.equal(h.feedback.textContent, t("status.refreshedUsage", language));
    assert.equal(h.timers.size, 0);
  }
});

test("failed manual IPC preserves the last quota and restores controls", async () => {
  const h = harness({ health: "transient_error", refresh: () => Promise.reject(new Error("offline")) });
  h.panel.seed([sample()]);
  await h.panel.refresh();
  assert.equal(h.card.querySelector(".hero-num").textContent, "60");
  assert.equal(h.feedback.textContent, t("status.refreshUsageFailed", "ko"));
  assert.equal(h.feedback.dataset.state, "error");
  assert.equal(h.button.disabled, false);
});

test("rate-limited card retries stay disabled and report the cooldown honestly", async () => {
  const h = harness({ health: "rate_limited", refresh: () => [sample()] });
  h.panel.seed([sample()]);
  const retry = h.card.querySelector("[data-retry-usage]");
  assert.equal(retry.disabled, true);
  h.click(retry);
  assert.deepEqual(h.commands, []);
  await h.panel.refresh();
  assert.equal(retry.disabled, true);
  assert.equal(h.feedback.textContent, t("status.refreshUsageRateLimited", "ko"));
});

test("still-signed-out responses do not claim a successful connection", async () => {
  const h = harness({ refresh: () => [] });
  await h.panel.refresh();
  assert.equal(h.feedback.textContent, t("status.refreshedUsageWithIssues", "ko"));
  assert.equal(h.card.querySelector("[data-tool-retry]").hidden, false);
});

test("force timeout unblocks controls without clearing visible values", async () => {
  const reply = deferred();
  const h = harness({health: "ready", refresh: () => reply.promise});
  h.panel.seed([sample()]);
  const pending = h.panel.refresh();
  await h.timeout(46_000);
  await pending;
  assert.equal(h.card.querySelector(".hero-num").textContent, "60");
  assert.equal(h.button.disabled, false);
  reply.resolve([sample(99)]);
  await settle();
  assert.equal(h.card.querySelector(".hero-num").textContent, "60");
});

test("new native snapshots win over the manual reply", async () => {
  const reply = deferred();
  const h = harness({refresh: () => reply.promise});
  const pending = h.panel.refresh();
  h.panel.native([sample(12)], {codex: "ready"});
  reply.resolve([sample(90)]);
  await pending;
  assert.equal(h.card.querySelector(".hero-num").textContent, "88");
  assert.equal(h.feedback.textContent, t("status.refreshedUsage", "ko"));
});

test("provider cooldown without a health error is not reported as a successful refresh", async () => {
  const reply = deferred();
  const h = harness({health: "ready", refresh: () => reply.promise});
  const pending = h.panel.refresh();
  h.panel.native([sample(12)], {codex: "ready"});
  reply.reject("usage_refresh_throttled");
  await pending;
  assert.equal(h.card.querySelector(".hero-num").textContent, "88");
  assert.equal(h.feedback.textContent, t("status.refreshUsageRateLimited", "ko"));
});

test("a failed manual refresh recovers missing health without clearing the error", async () => {
  for (const eventFirst of [false, true]) {
    const reply = deferred();
    const h = harness({refresh: () => reply.promise});
    const pending = h.panel.refresh();
    h.health("ready");
    if (eventFirst) h.panel.nativeStatusOnly([sample(12)]);
    reply.reject("A different provider failed");
    await pending;
    if (!eventFirst) h.panel.nativeStatusOnly([sample(12)]);
    assert.deepEqual(h.commands, ["refresh_status", "get_collection_health"]);
    assert.equal(h.card.querySelector(".hero-num").textContent, "88");
    assert.equal(h.feedback.textContent, t("status.refreshUsageFailed", "ko"));
  }
});

test("manual errors are independent of native snapshots in both arrival orders", async () => {
  for (const [timeout, eventFirst] of [[false, true], [false, false], [true, true], [true, false]]) {
    const reply = deferred();
    const h = harness({refresh: () => reply.promise});
    const pending = h.panel.refresh();
    if (eventFirst) h.panel.native([sample(12)], {codex: "ready"});
    if (timeout) await h.timeout(46_000);
    else reply.reject(new Error("late failure"));
    await pending;
    if (!eventFirst) h.panel.native([sample(12)], {codex: "ready"});
    assert.equal(h.card.querySelector(".hero-num").textContent, "88");
    assert.equal(h.feedback.hidden, false);
    assert.equal(h.feedback.textContent, t("status.refreshUsageFailed", "ko"));
    assert.equal(h.button.disabled, false);
    if (timeout) reply.resolve([sample(90)]);
    await settle();
  }
});

test("disabled collections cannot be enabled by refresh controls", async () => {
  const h = harness({disabled: true, refresh: () => []});
  assert.equal(h.button.disabled, true);
  h.click();
  await h.panel.refresh();
  assert.deepEqual(h.commands, []);
});

test("refresh layout keeps bounded controls and wrapping feedback without extra animation", () => {
  const css = readFileSync(new URL("./panel.css", import.meta.url), "utf8");
  assert.match(css, /\.overview-actions\s*\{[^}]*min-width: 0;[^}]*flex-wrap: wrap;/);
  assert.match(css, /\.panel-window \.usage-refresh-button\s*\{[^}]*width: 30px;[^}]*height: 30px;/);
  assert.match(css, /\.tool-retry-row\s*\{[^}]*grid-column: 1 \/ -1;[^}]*grid-row: 3;/);
  assert.match(css, /\.empty-hint\s*\{[^}]*grid-row: 3;\s*\}\s*\.tool-retry-row\s*\{\s*grid-row: 4;/);
  assert.match(css, /html\[data-text-scale="enlarged"\] \.tool-retry-row\s*\{\s*grid-row: 4;/);
  assert.match(css, /\.overview-refresh-status\s*\{[^}]*overflow-wrap: anywhere;/);
  assert.doesNotMatch(css, /\.usage-refresh-icon\s*\{[^}]*animation:/);
});
