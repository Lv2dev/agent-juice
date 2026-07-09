import assert from "node:assert/strict";
import test from "node:test";

import {
  colorForPercent,
  representativeByTool,
  viewModelForTool,
} from "./panel-state.js";

const settings = {
  warn_threshold: 70,
  danger_threshold: 90,
  palette: "Traffic",
  language: "ko",
};

test("colorForPercent uses thresholds and palette from settings", () => {
  assert.equal(colorForPercent(50, settings), "#22c55e");
  assert.equal(colorForPercent(80, settings), "#f59e0b");
  assert.equal(colorForPercent(95, settings), "#ef4444");
  assert.equal(colorForPercent(null, settings), "#9ca3af");
});

test("representativeByTool picks the newest captured_at per tool", () => {
  const statuses = [
    { tool: "claude", captured_at: "2026-07-07T00:00:00Z" },
    { tool: "claude", captured_at: "2026-07-07T00:02:00Z" },
    { tool: "codex", captured_at: "2026-07-07T00:01:00Z" },
  ];

  const rep = representativeByTool(statuses);

  assert.equal(rep.claude.captured_at, "2026-07-07T00:02:00Z");
  assert.equal(rep.codex.captured_at, "2026-07-07T00:01:00Z");
});

test("representativeByTool prefers valid timestamps over invalid strings", () => {
  const rep = representativeByTool([
    { tool: "codex", captured_at: "2026-07-07T00:01:00Z" },
    { tool: "codex", captured_at: "zzzz-invalid" },
  ]);

  assert.equal(rep.codex.captured_at, "2026-07-07T00:01:00Z");
});

test("viewModelForTool renders live values, approximate flag, and raw pc id text", () => {
  const status = {
    tool: "claude",
    pc_id: "<b>DESKTOP</b>",
    captured_at: "2026-07-07T00:00:00Z",
    primary: { used_percent: 88, resets_at: "2026-07-07T01:05:00Z" },
    secondary: { used_percent: 41, resets_at: null },
    session: { active: true, context_used_percent: 63 },
    cost_estimate_usd: 0.12,
    approx: true,
  };

  const vm = viewModelForTool(
    [status],
    "claude",
    settings,
    new Date("2026-07-07T00:00:00Z"),
  );

  assert.equal(vm.active, true);
  assert.equal(vm.pcId, "<b>DESKTOP</b>");
  assert.equal(vm.primary.value, "88%");
  assert.equal(vm.primary.width, "88%");
  assert.equal(vm.primary.color, "#22c55e");
  assert.match(vm.primary.reset, /^리셋 1시간 5분 \(/);
  assert.equal(vm.secondary.value, "41%");
  assert.equal(vm.secondary.color, "#22c55e");
  assert.equal(vm.context, "컨텍스트 63%");
  assert.equal(vm.meta, "근사치");
  assert.equal(vm.emptyHint, "");
});

test("viewModelForTool is null-safe and marks stale sessions", () => {
  const status = {
    tool: "codex",
    pc_id: "PC",
    captured_at: "2026-07-07T00:00:00Z",
    primary: null,
    secondary: { used_percent: null, resets_at: "2026-07-06T23:59:00Z" },
    session: { active: false, context_used_percent: null },
    cost_estimate_usd: null,
    approx: true,
  };

  const vm = viewModelForTool(
    [status],
    "codex",
    settings,
    new Date("2026-07-07T00:00:00Z"),
  );

  assert.equal(vm.active, false);
  assert.equal(vm.primary.value, "–");
  assert.equal(vm.primary.width, "0%");
  assert.equal(vm.primary.color, "#9ca3af");
  assert.equal(vm.secondary.reset, "리셋 지남");
  assert.equal(vm.context, "컨텍스트 – · 오래됨");
  assert.equal(vm.meta, "근사치");
});

test("viewModelForTool explains empty Claude state in the selected language", () => {
  const ko = viewModelForTool([], "claude", settings);
  assert.equal(ko.emptyHint, "Claude 연결 후 Claude를 한 번 사용하면 표시됩니다");

  const en = viewModelForTool([], "claude", { ...settings, language: "en" });
  assert.equal(en.emptyHint, "Connect Claude, then use Claude once on this PC");
});
