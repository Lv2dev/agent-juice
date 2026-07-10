import assert from "node:assert/strict";
import test from "node:test";

import { barToolViewModel, barViewModel } from "./bar-state.js";

const settings = {
  warn_threshold: 70,
  danger_threshold: 90,
  palette: "Traffic",
  bar_mode: "full",
  ring_on: true,
  language: "ko",
};

test("barToolViewModel renders remaining account limits, reset text, and lowest remaining ring", () => {
  const status = {
    tool: "claude",
    captured_at: "2026-07-07T00:00:00Z",
    primary: { used_percent: 88, resets_at: "2026-07-07T01:05:00Z" },
    secondary: { used_percent: 41, resets_at: "2026-07-10T02:00:00Z" },
    session: { active: true },
  };

  const vm = barToolViewModel(
    [status],
    "claude",
    settings,
    new Date("2026-07-07T00:00:00Z"),
  );

  assert.equal(vm.state, "live");
  assert.equal(vm.label, "Claude");
  assert.equal(vm.primary.text, "5h 12%");
  assert.equal(vm.primary.percent, 12);
  assert.equal(vm.primary.reset, "1시간 5분");
  assert.equal(vm.primary.color, "#f59e0b");
  assert.equal(vm.primary.arc, "43.2deg");
  assert.equal(vm.secondary.text, "주간 59%");
  assert.equal(vm.secondary.percent, 59);
  assert.equal(vm.secondary.reset, "3일 2시간");
  assert.equal(vm.secondary.arc, "212.4deg");
  assert.equal(vm.worst, "12");
  assert.equal(vm.severity, "warn");
});

test("barToolViewModel is null-safe and marks stale or empty tools", () => {
  const stale = {
    tool: "codex",
    captured_at: "2026-07-07T00:00:00Z",
    primary: null,
    secondary: { used_percent: null, resets_at: null },
    session: { active: false },
  };

  const vm = barToolViewModel([stale], "codex", settings);
  assert.equal(vm.state, "stale");
  assert.equal(vm.severity, "stale");
  assert.equal(vm.primary.text, "5h –");
  assert.equal(vm.primary.arc, "0deg");
  assert.equal(vm.secondary.text, "주간 –");
  assert.equal(vm.worst, "–");

  const empty = barToolViewModel([], "claude", settings);
  assert.equal(empty.state, "empty");
  assert.equal(empty.severity, "empty");
  assert.equal(empty.primary.text, "5h –");
});

test("barToolViewModel treats missing active flag as stale", () => {
  const vm = barToolViewModel(
    [
      {
        tool: "claude",
        captured_at: "2026-07-07T00:00:00Z",
        session: {},
      },
    ],
    "claude",
    settings,
  );

  assert.equal(vm.state, "stale");
  assert.equal(vm.severity, "stale");
});

test("barToolViewModel marks danger when any displayed limit crosses danger threshold", () => {
  const vm = barToolViewModel(
    [
      {
        tool: "codex",
        captured_at: "2026-07-07T00:00:00Z",
        primary: { used_percent: 45, resets_at: null },
        secondary: { used_percent: 91, resets_at: null },
        session: { active: true },
      },
    ],
    "codex",
    settings,
  );

  assert.equal(vm.state, "live");
  assert.equal(vm.severity, "danger");
});

test("barToolViewModel uses distinct colors for 5h and weekly rings in the same bucket", () => {
  const vm = barToolViewModel(
    [
      {
        tool: "claude",
        captured_at: "2026-07-07T00:00:00Z",
        primary: { used_percent: 20, resets_at: null },
        secondary: { used_percent: 25, resets_at: null },
        session: { active: true },
      },
    ],
    "claude",
    settings,
  );

  assert.equal(vm.primary.color, "#22c55e");
  assert.equal(vm.secondary.color, "#2563eb");
  assert.notEqual(vm.primary.color, vm.secondary.color);
});

test("barToolViewModel localizes the weekly limit label", () => {
  const vm = barToolViewModel([], "codex", { ...settings, language: "en" });

  assert.equal(vm.secondary.text, "Weekly –");
});

test("barViewModel normalizes mode and ring settings", () => {
  const full = barViewModel([], {
    ...settings,
    bar_mode: "compact",
    limit_order: "secondary_first",
    indicator_style: "bar",
    ring_on: false,
    ring_numbers_on: false,
    ring_number_outline_on: false,
    ring_number_outline_width_px: 1.4,
    ring_size_px: 34.5,
    ring_thickness_px: 6.5,
    ring_gap_px: 8.5,
    ring_center_gap_px: 2.5,
    ring_number_font_size_px: 10.5,
    ring_number_font_weight: 650,
    bar_text_font_size_px: 12.5,
    bar_text_font_weight: 550,
  });
  assert.equal(full.mode, "compact");
  assert.equal(full.limitOrder, "secondary_first");
  assert.equal(full.indicatorStyle, "bar");
  assert.equal(full.ringOn, false);
  assert.equal(full.ringNumbersOn, false);
  assert.equal(full.ringNumberOutlineOn, false);
  assert.equal(full.ringNumberOutlineWidthPx, 1.4);
  assert.equal(full.ringSizePx, 34.5);
  assert.equal(full.ringThicknessPx, 6.5);
  assert.equal(full.ringGapPx, 8.5);
  assert.equal(full.ringCenterGapPx, 2.5);
  assert.equal(full.ringNumberFontSizePx, 10.5);
  assert.equal(full.ringNumberFontWeight, 650);
  assert.equal(full.barTextFontSizePx, 12.5);
  assert.equal(full.barTextFontWeight, 550);
  assert.equal(full.tools.length, 2);

  const fallback = barViewModel([], { ...settings, bar_mode: "unknown" });
  assert.equal(fallback.mode, "full");
  assert.equal(fallback.limitOrder, "primary_first");
  assert.equal(fallback.indicatorStyle, "ring");
  assert.equal(fallback.ringNumbersOn, true);
  assert.equal(fallback.ringNumberOutlineOn, true);
  assert.equal(fallback.ringNumberOutlineWidthPx, 1.2);
  assert.equal(fallback.ringSizePx, 36);
  assert.equal(fallback.ringThicknessPx, 4);
  assert.equal(fallback.ringGapPx, 6);
  assert.equal(fallback.ringCenterGapPx, 0);
  assert.equal(fallback.ringNumberFontSizePx, 9);
  assert.equal(fallback.ringNumberFontWeight, 600);
  assert.equal(fallback.barTextFontSizePx, 11);
  assert.equal(fallback.barTextFontWeight, 500);
});

test("barViewModel clamps ring geometry settings", () => {
  const vm = barViewModel([], {
    ...settings,
    ring_size_px: 99,
    ring_thickness_px: 99,
    ring_gap_px: -4,
    ring_center_gap_px: 99,
    ring_number_outline_width_px: 99,
    ring_number_font_size_px: 99,
    ring_number_font_weight: 999,
    bar_text_font_size_px: -1,
    bar_text_font_weight: 999,
  });

  assert.equal(vm.ringSizePx, 44);
  assert.equal(vm.ringThicknessPx, 10);
  assert.equal(vm.ringGapPx, 2);
  assert.equal(vm.ringCenterGapPx, 8);
  assert.equal(vm.ringNumberOutlineWidthPx, 4);
  assert.equal(vm.ringNumberFontSizePx, 16);
  assert.equal(vm.ringNumberFontWeight, 900);
  assert.equal(vm.barTextFontSizePx, 8);
  assert.equal(vm.barTextFontWeight, 900);
});

test("barViewModel preserves valid ring geometry at one-decimal precision", () => {
  const defaults = barViewModel([], settings);
  assert.deepEqual(
    {
      stroke: defaults.ringSvgStroke,
      outer: defaults.outerRadius,
      inner: defaults.innerRadius,
    },
    { stroke: "11.1", outer: "44.4", inner: "27.8" },
  );

  const fractional = barViewModel([], {
    ...settings,
    ring_size_px: 34.5,
    ring_thickness_px: 6.5,
    ring_gap_px: 8.5,
    ring_center_gap_px: 2.5,
  });
  assert.deepEqual(
    {
      stroke: fractional.ringSvgStroke,
      outer: fractional.outerRadius,
      inner: fractional.innerRadius,
    },
    { stroke: "11.6", outer: "44.2", inner: "19.6" },
  );
});

test("barViewModel keeps dual-ring strokes disjoint across boundary and mode combinations", () => {
  const values = {
    ring_size_px: [20, 20.1, 36, 43.9, 44],
    ring_thickness_px: [1, 1.1, 4, 9.9, 10],
    ring_gap_px: [2, 2.1, 6, 13.9, 14],
    ring_center_gap_px: [0, 0.1, 4, 7.9, 8],
  };
  const modes = ["full", "compact", "dual", "quad"];
  const orders = ["primary_first", "secondary_first"];
  const indicatorStyles = ["ring", "bar"];
  const oneDecimal = /^\d+(?:\.\d)?$/;
  const epsilon = 1e-9;

  for (const ring_size_px of values.ring_size_px) {
    for (const ring_thickness_px of values.ring_thickness_px) {
      for (const ring_gap_px of values.ring_gap_px) {
        for (const ring_center_gap_px of values.ring_center_gap_px) {
          for (const bar_mode of modes) {
            for (const limit_order of orders) {
              for (const indicator_style of indicatorStyles) {
                const vm = barViewModel([], {
                  ...settings,
                  bar_mode,
                  limit_order,
                  indicator_style,
                  ring_size_px,
                  ring_thickness_px,
                  ring_gap_px,
                  ring_center_gap_px,
                });
                const stroke = Number(vm.ringSvgStroke);
                const outer = Number(vm.outerRadius);
                const inner = Number(vm.innerRadius);
                const label = JSON.stringify({
                  bar_mode,
                  limit_order,
                  indicator_style,
                  ring_size_px,
                  ring_thickness_px,
                  ring_gap_px,
                  ring_center_gap_px,
                });

                assert.match(vm.ringSvgStroke, oneDecimal, label);
                assert.match(vm.outerRadius, oneDecimal, label);
                assert.match(vm.innerRadius, oneDecimal, label);
                assert.ok(stroke > 0, label);
                assert.ok(outer > inner, label);
                assert.ok(outer + stroke / 2 <= 50 + epsilon, label);
                assert.ok(inner - stroke / 2 >= -epsilon, label);
                assert.ok(outer - stroke / 2 + epsilon >= inner + stroke / 2, label);
              }
            }
          }
        }
      }
    }
  }
});

test("barViewModel preserves every documented taskbar bar mode", () => {
  for (const mode of ["full", "compact", "dual", "quad"]) {
    const vm = barViewModel([], { ...settings, bar_mode: mode });
    assert.equal(vm.mode, mode);
  }
});

test("barViewModel filters hidden tools without exposing a gap setting", () => {
  const codexOnly = barViewModel([], {
    ...settings,
    show_claude: false,
    show_codex: true,
  });

  assert.equal("gapPx" in codexOnly, false);
  assert.deepEqual(
    codexOnly.tools.map((tool) => tool.tool),
    ["codex"],
  );

  const none = barViewModel([], {
    ...settings,
    show_claude: false,
    show_codex: false,
  });
  assert.deepEqual(none.tools, []);
});
