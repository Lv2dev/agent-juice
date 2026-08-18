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

test("barToolViewModel prioritizes login-required health for every provider", () => {
  for (const tool of ["claude", "codex", "grok"]) {
    const status = {
      tool,
      captured_at: "2026-08-14T00:00:00Z",
      primary: { used_percent: 10, resets_at: null },
      secondary: { used_percent: 20, resets_at: null },
      session: { active: true },
    };
    const vm = barToolViewModel([status], tool, settings, new Date(), {
      startupLoading: true,
      collectionHealth: { [tool]: "login_required" },
    });

    assert.equal(vm.state, "login_required");
    assert.equal(vm.loginText, "로그인 필요");
    assert.equal(vm.secondary.visible, false);
    assert.match(vm.ariaLabel, /로그인 필요/);
  }
});

test("barToolViewModel clears login-required copy when collection health recovers", () => {
  const vm = barToolViewModel([], "codex", settings, new Date(), {
    collectionHealth: { codex: "ready" },
  });

  assert.equal(vm.state, "empty");
  assert.equal(vm.loginText, undefined);
});

test("barViewModel preserves login-required state across every mode and indicator", () => {
  for (const bar_mode of ["full", "compact", "dual", "quad"]) {
    for (const indicator_style of ["ring", "bar"]) {
      const vm = barViewModel(
        [],
        {
          ...settings,
          bar_mode,
          indicator_style,
          show_claude: true,
          show_codex: false,
          show_grok: false,
        },
        new Date(),
        { collectionHealth: { claude: "login_required" } },
      );

      assert.equal(vm.mode, bar_mode);
      assert.equal(vm.indicatorStyle, indicator_style);
      assert.equal(vm.tools[0].state, "login_required");
    }
  }
});

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
  assert.equal(vm.tooltip, "Claude\n5h 리셋 1시간 5분\n주간 리셋 3일 2시간");
  assert.equal(vm.ariaLabel, "Claude, 5h 12%, 주간 59%");
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
  assert.equal(vm.tooltip, "Codex\n5h –\n주간 –");
  assert.equal(vm.ariaLabel, "Codex, 5h –, 주간 –, 오래됨");

  const empty = barToolViewModel([], "claude", settings);
  assert.equal(empty.state, "empty");
  assert.equal(empty.severity, "empty");
  assert.equal(empty.primary.text, "5h –");
  assert.equal(empty.tooltip, "Claude\n5h –\n주간 –");
  assert.equal(empty.ariaLabel, "Claude, 5h –, 주간 –");
});

test("barToolViewModel exposes startup loading while preserving a last-known gauge", () => {
  const previous = {
    tool: "codex",
    captured_at: "2026-07-06T23:00:00Z",
    primary: { used_percent: 75, resets_at: "2026-07-06T23:30:00Z" },
    secondary: { used_percent: 40, resets_at: "2026-07-10T00:00:00Z" },
    session: { active: false },
  };

  const withPrevious = barToolViewModel(
    [previous],
    "codex",
    settings,
    new Date("2026-07-07T00:00:00Z"),
    { startupLoading: true },
  );
  assert.equal(withPrevious.state, "loading");
  assert.equal(withPrevious.severity, "loading");
  assert.equal(withPrevious.loadingText, "로딩 중");
  assert.equal(withPrevious.primary.percent, 25);
  assert.equal(withPrevious.secondary.percent, 60);
  assert.equal(withPrevious.worst, "…");
  assert.equal(withPrevious.tooltip, "Codex\n로딩 중");
  assert.equal(withPrevious.ariaLabel, "Codex, 로딩 중");

  const empty = barToolViewModel([], "claude", settings, new Date(), {
    startupLoading: true,
  });
  assert.equal(empty.state, "loading");
  assert.equal(empty.primary.percent, null);
  assert.equal(empty.tooltip, "Claude\n로딩 중");
});

test("barToolViewModel keeps a weekly-only Codex limit in the weekly slot", () => {
  const weeklyOnly = {
    tool: "codex",
    captured_at: "2026-07-13T00:00:00Z",
    primary: null,
    secondary: { used_percent: 16, resets_at: "2026-07-20T00:00:00Z" },
    session: { active: true },
  };

  const vm = barToolViewModel(
    [weeklyOnly],
    "codex",
    settings,
    new Date("2026-07-13T00:00:00Z"),
  );

  assert.equal(vm.primary.text, "5h –");
  assert.equal(vm.primary.percent, null);
  assert.equal(vm.secondary.text, "주간 84%");
  assert.equal(vm.secondary.percent, 84);
  assert.equal(vm.worst, "84");
  assert.equal(vm.ariaLabel, "Codex, 5h –, 주간 84%");
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
  assert.equal(vm.ariaLabel, "Claude, 5h –, 주간 –, 오래됨");
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

  assert.equal(vm.brandColor, "#d79a32");
  assert.equal(vm.primary.color, "#d79a32");
  assert.equal(vm.secondary.color, "#d36b86");
  assert.notEqual(vm.primary.color, vm.secondary.color);
});

test("barToolViewModel distinguishes tool colors and supports usage display basis", () => {
  const statuses = ["claude", "codex"].map((tool) => ({
    tool,
    captured_at: "2026-07-07T00:00:00Z",
    primary: { used_percent: 20, resets_at: null },
    secondary: { used_percent: 25, resets_at: null },
    session: { active: true },
  }));
  const usedSettings = { ...settings, display_basis: "used" };
  const claude = barToolViewModel(statuses, "claude", usedSettings);
  const codex = barToolViewModel(statuses, "codex", usedSettings);

  assert.equal(claude.primary.text, "5h 20%");
  assert.equal(claude.primary.percent, 20);
  assert.equal(claude.worst, "25");
  assert.equal(claude.brandColor, "#d79a32");
  assert.equal(claude.primary.color, "#d79a32");
  assert.equal(claude.secondary.color, "#d36b86");
  assert.equal(codex.brandColor, "#2fac7d");
  assert.equal(codex.primary.color, "#2fac7d");
  assert.equal(codex.secondary.color, "#4d86d6");

  const customCodex = barToolViewModel(statuses, "codex", {
    ...usedSettings,
    tool_colors: {
      codex_primary: [0x12, 0x34, 0x56],
      codex_secondary: [0x65, 0x43, 0x21],
    },
  });
  assert.equal(customCodex.primary.color, "#123456");
  assert.equal(customCodex.secondary.color, "#654321");
});

test("barToolViewModel localizes the weekly limit label", () => {
  const vm = barToolViewModel([], "codex", { ...settings, language: "en" });

  assert.equal(vm.secondary.text, "Weekly –");
  assert.equal(vm.tooltip, "Codex\n5h –\nWeekly –");
  assert.equal(vm.ariaLabel, "Codex, 5h –, Weekly –");

  const past = barToolViewModel(
    [{
      tool: "codex",
      primary: { used_percent: 20, resets_at: "2026-07-06T23:00:00Z" },
      secondary: null,
      session: { active: true },
    }],
    "codex",
    { ...settings, language: "en" },
    new Date("2026-07-07T00:00:00Z"),
  );
  assert.equal(past.tooltip, "Codex\n5h Waiting for refresh\nWeekly –");
});

test("barViewModel normalizes mode and ring settings", () => {
  const full = barViewModel([], {
    ...settings,
    bar_mode: "compact",
    limit_order: "secondary_first",
    indicator_style: "bar",
    indicator_effect_style: "glow",
    indicator_track_color_auto: false,
    indicator_track_color: [0x12, 0x34, 0x56],
    indicator_track_opacity_percent: 37.5,
    taskbar_text_colors: {
      claude: [0x11, 0x22, 0x44],
      claude_on: true,
      codex: [0x33, 0x55, 0x77],
      codex_on: false,
      info: [0x44, 0x66, 0x88],
      info_on: true,
      ring: [0x55, 0x77, 0x99],
      ring_on: true,
    },
    ring_on: false,
    ring_numbers_on: false,
    ring_number_outline_on: false,
    ring_number_outline_width_px: 1.4,
    ring_size_px: 34.5,
    ring_thickness_px: 6.5,
    ring_gap_px: 8.5,
    ring_center_size_px: 18.5,
    ring_number_font_size_px: 10.5,
    ring_number_font_weight: 650,
    bar_text_font_size_px: 12.5,
    bar_text_font_weight: 550,
    bar_content_gap_px: 3.5,
  });
  assert.equal(full.mode, "compact");
  assert.equal(full.fullResetTimeOn, true);
  assert.equal(full.limitOrder, "secondary_first");
  assert.equal(full.indicatorStyle, "bar");
  assert.equal(full.indicatorEffectStyle, "glow");
  assert.equal(full.indicatorTrackColorAuto, false);
  assert.equal(full.indicatorTrackColor, "#123456");
  assert.equal(full.indicatorTrackOpacityPercent, 37.5);
  assert.equal(full.claudeTextColor, "#112244");
  assert.equal(full.claudeTextColorOn, true);
  assert.equal(full.codexTextColor, "#335577");
  assert.equal(full.codexTextColorOn, false);
  assert.equal(full.infoTextColor, "#446688");
  assert.equal(full.infoTextColorOn, true);
  assert.equal(full.ringTextColor, "#557799");
  assert.equal(full.ringTextColorOn, true);
  assert.equal(full.ringOn, false);
  assert.equal(full.ringNumbersOn, false);
  assert.equal(full.ringNumberOutlineOn, false);
  assert.equal(full.ringNumberOutlineWidthPx, 1.4);
  assert.equal(full.ringSizePx, 34.5);
  assert.equal(full.ringThicknessPx, 6.5);
  assert.equal(full.ringGapPx, 8.5);
  assert.equal(full.ringCenterSizePx, 18.5);
  assert.equal(full.ringNumberFontSizePx, 10.5);
  assert.equal(full.ringNumberFontWeight, 650);
  assert.equal(full.barTextFontSizePx, 12.5);
  assert.equal(full.barTextFontWeight, 550);
  assert.equal(full.barContentGapPx, 3.5);
  assert.equal(full.displayBasis, "remaining");
  assert.equal(full.tools.length, 2);

  const withResetTime = barViewModel([], { ...settings, full_reset_time_on: true });
  assert.equal(withResetTime.fullResetTimeOn, true);
  const withoutResetTime = barViewModel([], { ...settings, full_reset_time_on: false });
  assert.equal(withoutResetTime.fullResetTimeOn, false);

  const fallback = barViewModel([], {
    ...settings,
    bar_mode: "unknown",
    indicator_track_color_auto: "unexpected",
    indicator_track_color: [1, 2],
    indicator_track_opacity_percent: "unexpected",
  });
  assert.equal(fallback.mode, "full");
  assert.equal(fallback.limitOrder, "primary_first");
  assert.equal(fallback.indicatorStyle, "ring");
  assert.equal(fallback.indicatorEffectStyle, "flat");
  assert.equal(fallback.indicatorTrackColorAuto, true);
  assert.equal(fallback.indicatorTrackColor, "#6b7280");
  assert.equal(fallback.indicatorTrackOpacityPercent, 11);
  assert.equal(fallback.claudeTextColor, "#d79a32");
  assert.equal(fallback.claudeTextColorOn, false);
  assert.equal(fallback.codexTextColor, "#2fac7d");
  assert.equal(fallback.codexTextColorOn, false);
  assert.equal(fallback.infoTextColor, "#6b7280");
  assert.equal(fallback.infoTextColorOn, false);
  assert.equal(fallback.ringTextColor, "#6b7280");
  assert.equal(fallback.ringTextColorOn, false);
  assert.equal(fallback.ringNumbersOn, true);
  assert.equal(fallback.ringNumberOutlineOn, true);
  assert.equal(fallback.ringNumberOutlineWidthPx, 1.2);
  assert.equal(fallback.ringSizePx, 36);
  assert.equal(fallback.ringThicknessPx, 4);
  assert.equal(fallback.ringGapPx, 6);
  assert.equal(fallback.ringCenterSizePx, 16);
  assert.equal(fallback.ringNumberFontSizePx, 9);
  assert.equal(fallback.ringNumberFontWeight, 600);
  assert.equal(fallback.barTextFontSizePx, 11);
  assert.equal(fallback.barTextFontWeight, 500);
  assert.equal(fallback.barContentGapPx, 14);
});

test("barViewModel clamps ring geometry settings", () => {
  const vm = barViewModel([], {
    ...settings,
    ring_size_px: 99,
    ring_thickness_px: 99,
    ring_gap_px: -4,
    ring_center_size_px: 99,
    indicator_effect_style: "unknown",
    indicator_track_color: [999, -4, 12.6],
    indicator_track_opacity_percent: 999,
    ring_number_outline_width_px: 99,
    ring_number_font_size_px: 99,
    ring_number_font_weight: 999,
    bar_text_font_size_px: -1,
    bar_text_font_weight: 999,
    bar_content_gap_px: 99,
  });

  assert.equal(vm.ringSizePx, 44);
  assert.equal(vm.ringThicknessPx, 10);
  assert.equal(vm.ringGapPx, 2);
  assert.equal(vm.ringCenterSizePx, 32);
  assert.equal(vm.indicatorEffectStyle, "flat");
  assert.equal(vm.indicatorTrackColor, "#ff000d");
  assert.equal(vm.indicatorTrackOpacityPercent, 100);
  assert.equal(vm.ringNumberOutlineWidthPx, 4);
  assert.equal(vm.ringNumberFontSizePx, 16);
  assert.equal(vm.ringNumberFontWeight, 900);
  assert.equal(vm.barTextFontSizePx, 8);
  assert.equal(vm.barTextFontWeight, 900);
  assert.equal(vm.barContentGapPx, 24);
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
    ring_center_size_px: 18.5,
  });
  assert.deepEqual(
    {
      stroke: fractional.ringSvgStroke,
      outer: fractional.outerRadius,
      inner: fractional.innerRadius,
    },
    { stroke: "11.6", outer: "44.2", inner: "32.6" },
  );
});

test("barViewModel keeps dual-ring strokes disjoint across boundary and mode combinations", () => {
  const values = {
    ring_size_px: [20, 20.1, 36, 43.9, 44],
    ring_thickness_px: [1, 1.1, 4, 9.9, 10],
    ring_gap_px: [2, 2.1, 6, 13.9, 14],
    ring_center_size_px: [4, 4.1, 16, 31.9, 32],
  };
  const modes = ["full", "compact", "dual", "quad"];
  const orders = ["primary_first", "secondary_first"];
  const indicatorStyles = ["ring", "bar"];
  const oneDecimal = /^\d+(?:\.\d)?$/;
  const epsilon = 1e-9;

  for (const ring_size_px of values.ring_size_px) {
    for (const ring_thickness_px of values.ring_thickness_px) {
      for (const ring_gap_px of values.ring_gap_px) {
        for (const ring_center_size_px of values.ring_center_size_px) {
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
                  ring_center_size_px,
                });
                const stroke = Number(vm.ringSvgStroke);
                const outer = Number(vm.outerRadius);
                const inner = Number(vm.innerRadius);
                const quadStroke = Number(vm.quadSvgStroke);
                const quadRadius = Number(vm.quadRadius);
                const label = JSON.stringify({
                  bar_mode,
                  limit_order,
                  indicator_style,
                  ring_size_px,
                  ring_thickness_px,
                  ring_gap_px,
                  ring_center_size_px,
                });

                assert.match(vm.ringSvgStroke, oneDecimal, label);
                assert.match(vm.outerRadius, oneDecimal, label);
                assert.match(vm.innerRadius, oneDecimal, label);
                assert.match(vm.quadSvgStroke, oneDecimal, label);
                assert.match(vm.quadRadius, oneDecimal, label);
                assert.ok(stroke > 0, label);
                assert.ok(quadStroke > 0, label);
                assert.ok(outer > inner, label);
                assert.ok(outer + stroke / 2 <= 50 + epsilon, label);
                assert.ok(inner - stroke / 2 >= -epsilon, label);
                assert.ok(outer - stroke / 2 + epsilon >= inner + stroke / 2, label);
                assert.ok(quadRadius + quadStroke / 2 <= 50 + epsilon, label);
                assert.ok(quadRadius - quadStroke / 2 >= -epsilon, label);
                const dualCenterPx = (inner - stroke / 2) * 2 * vm.ringSizePx / 100;
                const quadCenterPx = (quadRadius - quadStroke / 2) * 2 * vm.ringSizePx / 100;
                assert.ok(Math.abs(dualCenterPx - vm.ringCenterSizePx) <= 0.11, label);
                assert.ok(Math.abs(quadCenterPx - vm.ringCenterSizePx) <= 0.11, label);
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

test("barViewModel filters hidden tools without restoring the removed tool-to-tool gap", () => {
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

test("Grok renders one dynamic weekly or monthly limit without an empty sibling", () => {
  const weekly = barViewModel(
    [{
      tool: "grok",
      captured_at: "2026-08-13T00:00:00Z",
      primary: { label: "week", used_percent: 34, resets_at: null },
      secondary: null,
      session: { active: true },
    }],
    { ...settings, language: "en", show_grok: true, limit_order: "secondary_first" },
  );

  assert.deepEqual(weekly.tools.map((tool) => tool.tool), ["claude", "codex", "grok"]);
  const grok = weekly.tools[2];
  assert.equal(grok.primary.text, "Weekly 66%");
  assert.equal(grok.primary.color, "#d9578b");
  assert.equal(grok.secondary.visible, false);
  assert.equal(grok.tooltip, "Grok\nWeekly –");
  assert.doesNotMatch(grok.ariaLabel, /Monthly/);

  const monthly = barToolViewModel(
    [{
      tool: "grok",
      captured_at: "2026-08-13T00:00:00Z",
      primary: { label: "month", used_percent: 10, resets_at: null },
      secondary: null,
      session: { active: true },
    }],
    "grok",
    { ...settings, language: "en" },
  );
  assert.equal(monthly.primary.text, "Monthly 90%");
  assert.equal(monthly.primary.color, "#8a6fd1");
});
