import assert from "node:assert/strict";
import test from "node:test";

import {
  colorForPercent,
  colorForToolPercent,
  representativeByTool,
  toolBrandColor,
  viewModelForTool,
} from "./panel-state.js";

const settings = {
  warn_threshold: 70,
  danger_threshold: 90,
  palette: "Traffic",
  language: "ko",
};

function luminance(hex) {
  const channels = hex.match(/[0-9a-f]{2}/gi).map((value) => Number.parseInt(value, 16) / 255);
  const linear = channels.map((value) => value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

test("default tool colors are brighter than the legacy muted set", () => {
  const current = ["#d79a32", "#d36b86", "#2fac7d", "#4d86d6"];
  const legacy = ["#b7833a", "#a65f72", "#4f8a73", "#4f76a6"];

  current.forEach((color, index) => {
    assert.ok(luminance(color) > luminance(legacy[index]), `${color} must be brighter`);
  });
});

test("colorForPercent uses thresholds and palette from settings", () => {
  assert.equal(colorForPercent(50, settings), "#22c55e");
  assert.equal(colorForPercent(80, settings), "#f59e0b");
  assert.equal(colorForPercent(95, settings), "#ef4444");
  assert.equal(colorForPercent(null, settings), "#9ca3af");
});

test("extended palettes distinguish limits while monochrome unifies normal values", () => {
  assert.equal(colorForToolPercent(50, "claude", { ...settings, palette: "Ocean" }), "#0f9fb5");
  assert.equal(
    colorForToolPercent(50, "claude", { ...settings, palette: "Ocean" }, true),
    "#2db8a8",
  );
  assert.equal(colorForPercent(80, { ...settings, palette: "Forest" }), "#b18432");
  assert.equal(colorForPercent(95, { ...settings, palette: "Sunset" }), "#9658b3");

  const mono = { ...settings, palette: { Mono: [0x34, 0x56, 0x78] } };
  assert.equal(colorForToolPercent(50, "claude", mono), "#345678");
  assert.equal(colorForToolPercent(50, "codex", mono, true), "#345678");
  assert.equal(colorForToolPercent(95, "claude", mono), "#ef4444");
});

test("tool palette accepts four persisted live colors without replacing warning semantics", () => {
  const customized = {
    ...settings,
    tool_colors: {
      claude_primary: [0x10, 0x20, 0x30],
      claude_secondary: [0x40, 0x50, 0x60],
      codex_primary: [0x70, 0x80, 0x90],
      codex_secondary: [0xa0, 0xb0, 0xc0],
    },
  };

  assert.equal(colorForToolPercent(50, "claude", customized), "#102030");
  assert.equal(colorForToolPercent(50, "claude", customized, true), "#405060");
  assert.equal(colorForToolPercent(50, "codex", customized), "#708090");
  assert.equal(colorForToolPercent(50, "codex", customized, true), "#a0b0c0");
  assert.equal(colorForToolPercent(80, "claude", customized), "#f59e0b");
  assert.equal(colorForToolPercent(95, "codex", customized, true), "#db2777");
  assert.equal(toolBrandColor("claude", customized), "#102030");
  assert.equal(toolBrandColor("codex", customized), "#708090");
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

test("viewModelForTool renders remaining values by default", () => {
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
  assert.equal(vm.primary.value, "12%");
  assert.equal(vm.primary.width, "12%");
  assert.equal(vm.primary.color, "#f59e0b");
  assert.match(vm.primary.reset, /^리셋 1시간 5분 \(/);
  assert.equal(vm.secondary.value, "59%");
  assert.equal(vm.brandColor, "#d79a32");
  assert.equal(vm.secondary.color, "#d36b86");
  assert.equal(vm.context, "컨텍스트 63%");
  assert.equal(vm.meta, "근사치");
  assert.equal(vm.emptyHint, "");
});

test("viewModelForTool can render canonical usage values", () => {
  const status = {
    tool: "codex",
    captured_at: "2026-07-07T00:00:00Z",
    primary: { used_percent: 23, resets_at: null },
    secondary: { used_percent: 41, resets_at: null },
    session: { active: true },
  };

  const vm = viewModelForTool([status], "codex", {
    ...settings,
    display_basis: "used",
  });

  assert.equal(vm.primary.value, "23%");
  assert.equal(vm.primary.width, "23%");
  assert.equal(vm.brandColor, "#2fac7d");
  assert.equal(vm.primary.color, "#2fac7d");
  assert.equal(vm.secondary.value, "41%");
  assert.equal(vm.secondary.width, "41%");
  assert.equal(vm.secondary.color, "#4d86d6");
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
  assert.equal(ko.emptyHint, "Juice 실행 후 Claude를 한 번 사용하면 표시됩니다");

  const en = viewModelForTool([], "claude", { ...settings, language: "en" });
  assert.equal(en.emptyHint, "Run Juice, then use Claude once on this PC");

  const autoCollect = viewModelForTool([], "claude", {
    ...settings,
    claude_account_auto_collect_on: true,
  });
  assert.equal(autoCollect.emptyHint, "Claude 계정 사용량을 수집 중입니다. 채팅을 보낼 필요가 없습니다.");
});
