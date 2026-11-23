import assert from "node:assert/strict";
import test from "node:test";
import { createStatusRequests } from "./status-requests.js";

function deferred() {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
const settle = () => new Promise(resolve => setImmediate(resolve));

function harness(invoke) {
  const commands = [], statuses = [], health = [], healthRender = [], timers = new Map();
  let statusGeneration = 0, healthGeneration = 0;
  const loader = createStatusRequests({
    invoke(command) { commands.push(command); return invoke(command); },
    getStatusGeneration: () => statusGeneration,
    getHealthGeneration: () => healthGeneration,
    acceptStatus: value => statuses.push(value),
    acceptHealth: (value, renderCurrent) => { health.push(value); healthRender.push(renderCurrent); },
    setTimer(callback, delay) { const timer = {callback, delay}; timers.set(timer, timer); return timer; },
    clearTimer(timer) { timers.delete(timer); },
  });
  return {
    loader, commands, statuses, health, healthRender, timers,
    statusEvent() { statusGeneration += 1; }, healthEvent() { healthGeneration += 1; },
    async timeout(delay) {
      const timer = [...timers.values()].find(value => value.delay === delay);
      assert.ok(timer, `timer ${delay}`); timers.delete(timer); timer.callback(); await settle();
    },
  };
}

test("newer requests retire earlier success and rejection without overwriting either payload", async () => {
  for (const fails of [false, true]) {
    const first = deferred(), second = deferred(); let count = 0;
    const h = harness(command => command === "get_status" ? (++count === 1 ? first.promise : second.promise) : {codex: "ready"});
    const old = h.loader.load(), next = h.loader.load();
    await old;
    second.resolve(["new"]); await next;
    if (fails) first.reject(new Error("old fixture failure")); else first.resolve(["old"]);
    await settle();
    assert.deepEqual(h.statuses, [["new"]]);
    assert.deepEqual(h.health, [{codex: "ready"}]);
    assert.equal(h.timers.size, 0);
  }
});

test("a new request retires old supplemental health even when status already resolved", async () => {
  const oldHealth = deferred(); let statusCalls = 0, healthCalls = 0;
  const h = harness(command => command === "get_status" ? [++statusCalls]
    : ++healthCalls === 1 ? oldHealth.promise : {codex: "new"});
  const old = h.loader.load(); await settle();
  await h.loader.load(); await old;
  oldHealth.resolve({codex: "old"}); await settle();
  assert.deepEqual(h.statuses, [[2]]);
  assert.deepEqual(h.health, [{codex: "new"}]);
});

test("normal requests and supplemental health rejection preserve valid status", async () => {
  for (const fails of [false, true]) {
    const h = harness(command => command === "get_status" ? ["valid"]
      : fails ? Promise.reject(new Error("health fixture failure")) : {codex: "ready"});
    await h.loader.load();
    assert.deepEqual(h.statuses, [["valid"]]);
    assert.equal(h.health.length, fails ? 0 : 1);
    assert.equal(h.timers.size, 0);
  }
});

test("health timeout renders valid quota and ignores late health success and rejection", async () => {
  for (const fails of [false, true]) {
    const response = deferred();
    const h = harness(command => command === "get_status" ? ["valid"] : response.promise);
    const pending = h.loader.load(); await settle();
    await h.timeout(2_000); await pending;
    assert.deepEqual(h.statuses, [["valid"]]);
    if (fails) response.reject(new Error("late health fixture")); else response.resolve({codex: "late"});
    await settle();
    assert.deepEqual(h.health, []);
    assert.equal(h.timers.size, 0);
  }
});

test("status timeout and status failure retain the existing empty snapshot contract", async () => {
  const response = deferred();
  const h = harness(() => response.promise);
  const pending = h.loader.load(); await h.timeout(20_000); await pending;
  response.resolve(["late"]); await settle();
  assert.deepEqual(h.statuses, [[]]);
  assert.deepEqual(h.commands, ["get_status"]);
  const failed = harness(() => { throw new Error("status fixture failure"); });
  await failed.loader.load();
  assert.deepEqual(failed.statuses, [[]]);
  assert.equal(failed.timers.size, 0);
});

test("native status and health event generations remain authoritative", async () => {
  const response = deferred();
  const h = harness(command => command === "get_status" ? response.promise : {cursor: "ready"});
  const pending = h.loader.load(); h.statusEvent(); response.resolve(["old"]); await pending;
  assert.deepEqual(h.statuses, []);
  assert.deepEqual(h.commands, ["get_status", "get_collection_health"]);
  assert.deepEqual(h.health, [{cursor: "ready"}]);
  assert.deepEqual(h.healthRender, [true]);
  const healthReply = deferred();
  const other = harness(command => command === "get_status" ? ["valid"] : healthReply.promise);
  const next = other.loader.load(); await settle(); other.healthEvent(); healthReply.resolve({codex: "old"}); await next;
  assert.deepEqual(other.statuses, [["valid"]]);
  assert.deepEqual(other.health, []);
});

test("disposal settles all waits and forbids new IPC, callbacks and timers", async () => {
  for (const phase of ["status", "health"]) {
    const response = deferred();
    const h = harness(command => command === "get_status" && phase === "health" ? ["valid"] : response.promise);
    const pending = h.loader.load(); await settle();
    h.loader.dispose(); h.loader.dispose(); await pending;
    const calls = h.commands.length;
    await h.loader.load(); response.resolve(phase === "status" ? ["late"] : {codex: "late"}); await settle();
    assert.equal(h.commands.length, calls);
    assert.deepEqual(h.statuses, []);
    assert.deepEqual(h.health, []);
    assert.equal(h.timers.size, 0);
  }
});

test("manual refresh shares one force request with repeated clicks and automatic reads", async () => {
  const response = deferred();
  const h = harness(command => command === "refresh_status" ? response.promise : {codex: "ready"});
  const pending = h.loader.refresh();
  assert.equal(h.loader.refresh(), pending);
  assert.equal(h.loader.load(), pending);
  assert.deepEqual(h.commands, ["refresh_status"]);
  response.resolve(["reconnected"]);
  assert.equal((await pending).ok, true);
  assert.deepEqual(h.statuses, [["reconnected"]]);
  assert.deepEqual(h.health, [{codex: "ready"}]);
  assert.equal(h.timers.size, 0);
});

test("a manual request retires a pending automatic read without accepting its late reply", async () => {
  const old = deferred();
  const h = harness(command => command === "get_status" ? old.promise : command === "refresh_status" ? ["fresh"] : {codex: "ready"});
  const automatic = h.loader.load();
  await h.loader.refresh();
  await automatic;
  old.reject(new Error("retired read"));
  await settle();
  assert.deepEqual(h.statuses, [["fresh"]]);
});

test("manual failures, invalid replies and timeout preserve the last visible quota", async () => {
  for (const kind of ["failure", "invalid", "timeout"]) {
    const response = deferred();
    const h = harness(command => command === "get_status" ? ["last-good"] : command === "get_collection_health" ? {codex: "ready"}
      : kind === "failure" ? Promise.reject(new Error("refresh failed")) : kind === "invalid" ? null : response.promise);
    await h.loader.load();
    const pending = h.loader.refresh();
    if (kind === "timeout") {
      assert.equal([...h.timers.values()].some(timer => timer.delay === 20_000), false);
      await h.timeout(46_000);
    }
    assert.equal((await pending).ok, false);
    assert.deepEqual(h.statuses, [["last-good"]]);
    response.resolve(["late"]);
    await settle();
    assert.deepEqual(h.statuses, [["last-good"]]);
    assert.equal(h.timers.size, 0);
  }
});

test("native events remain authoritative during a force refresh", async () => {
  const response = deferred();
  const h = harness(() => response.promise);
  const pending = h.loader.refresh();
  h.statusEvent();
  h.healthEvent();
  response.resolve(["old reply"]);
  assert.equal((await pending).ok, true);
  assert.deepEqual(h.statuses, []);
  assert.deepEqual(h.commands, ["refresh_status"]);
});

test("disposed force requests settle without callbacks or more IPC", async () => {
  const response = deferred();
  const h = harness(() => response.promise);
  const pending = h.loader.refresh();
  h.loader.dispose();
  assert.equal((await pending).retired, true);
  assert.equal((await h.loader.refresh()).retired, true);
  response.reject(new Error("late refresh"));
  await settle();
  assert.deepEqual(h.commands, ["refresh_status"]);
  assert.deepEqual(h.statuses, []);
  assert.equal(h.timers.size, 0);
});
