import assert from "node:assert/strict";
import test from "node:test";

import {
  formStateFromSettings,
  payloadFromEntries,
} from "./settings-state.js";

test("formStateFromSettings reads scalar and custom Rust palette shapes", () => {
  assert.deepEqual(
    formStateFromSettings({
      palette: "Cvd",
      warn_threshold: 60,
      danger_threshold: 92,
      poll_interval_secs: 4,
      stale_after_secs: 100,
      bar_mode: "compact",
      limit_order: "secondary_first",
      fullscreen_hide_on: false,
      maximized_hide_on: true,
      indicator_style: "bar",
      ring_on: false,
      ring_numbers_on: false,
      ring_number_outline_on: true,
      ring_number_outline_width_px: 1.4,
      ring_size_px: 34.5,
      ring_thickness_px: 6.5,
      ring_gap_px: 8.5,
      ring_center_gap_px: 2.5,
      ring_number_font_size_px: 10.5,
      ring_number_font_weight: 650,
      bar_text_font_size_px: 12.5,
      bar_text_font_weight: 550,
      autostart_on: false,
      language: "en",
      theme: "dark",
      font_mode: "pretendard",
      taskbar_offset_ratio: 0.25,
      claude_taskbar_offset_ratio: 0.15,
      codex_taskbar_offset_ratio: 0.85,
      show_claude: false,
      show_codex: true,
      claude_usage_auto_refresh_lab_on: true,
    }),
    {
      palette: "cvd",
      warnThreshold: 40,
      dangerThreshold: 8,
      pollIntervalSecs: 4,
      staleAfterSecs: 100,
      barMode: "compact",
      limitOrder: "secondary_first",
      fullscreenHideOn: false,
      maximizedHideOn: true,
      indicatorStyle: "bar",
      ringOn: false,
      ringNumbersOn: false,
      ringNumberOutlineOn: true,
      ringNumberOutlineWidthPx: 1.4,
      ringSizePx: 34.5,
      ringThicknessPx: 6.5,
      ringGapPx: 8.5,
      ringCenterGapPx: 2.5,
      ringNumberFontSizePx: 10.5,
      ringNumberFontWeight: 650,
      barTextFontSizePx: 12.5,
      barTextFontWeight: 550,
      autostartOn: false,
      language: "en",
      theme: "dark",
      fontMode: "pretendard",
      claudeTaskbarOffsetRatio: 0.15,
      codexTaskbarOffsetRatio: 0.85,
      showClaude: false,
      showCodex: true,
      claudeUsageAutoRefreshLabOn: true,
      customSafe: "#22c55e",
      customWarn: "#f59e0b",
      customDanger: "#ef4444",
    },
  );

  const custom = formStateFromSettings({
    palette: { Custom: [[1, 2, 3], [4, 5, 6], [7, 8, 9]] },
  });

  assert.equal(custom.palette, "custom");
  assert.equal(custom.customSafe, "#010203");
  assert.equal(custom.customWarn, "#040506");
  assert.equal(custom.customDanger, "#070809");
});

test("payloadFromEntries creates save_settings input payload", () => {
  const payload = payloadFromEntries({
    palette: "custom",
    warn_threshold: "28",
    danger_threshold: "9",
    poll_interval_secs: "5",
    stale_after_secs: "80",
    bar_mode: "quad",
    limit_order: "secondary_first",
    fullscreen_hide_on: "on",
    maximized_hide_on: "on",
    indicator_style: "bar",
    ring_on: "on",
    ring_numbers_on: "on",
    ring_number_outline_on: "on",
    ring_number_outline_width_px: "1.4",
    ring_size_px: "34.5",
    ring_thickness_px: "6.5",
    ring_gap_px: "8.5",
    ring_center_gap_px: "2.5",
    ring_number_font_size_px: "10.5",
    ring_number_font_weight: "650",
    bar_text_font_size_px: "12.5",
    bar_text_font_weight: "550",
    autostart_on: "on",
    language: "en",
    theme: "light",
    font_mode: "pretendard",
    claude_taskbar_offset_ratio: "0.25",
    codex_taskbar_offset_ratio: "0.75",
    show_claude: "on",
    show_codex: "on",
    claude_usage_auto_refresh_lab_on: "on",
    custom_safe: "#112233",
    custom_warn: "#445566",
    custom_danger: "#778899",
  });

  assert.deepEqual(payload, {
    palette: "custom",
    warn_threshold: 72,
    danger_threshold: 91,
    poll_interval_secs: 5,
    stale_after_secs: 80,
    bar_mode: "quad",
    limit_order: "secondary_first",
    fullscreen_hide_on: true,
    maximized_hide_on: true,
    indicator_style: "bar",
    ring_on: true,
    ring_numbers_on: true,
    ring_number_outline_on: true,
    ring_number_outline_width_px: 1.4,
    ring_size_px: 34.5,
    ring_thickness_px: 6.5,
    ring_gap_px: 8.5,
    ring_center_gap_px: 2.5,
    ring_number_font_size_px: 10.5,
    ring_number_font_weight: 650,
    bar_text_font_size_px: 12.5,
    bar_text_font_weight: 550,
    autostart_on: true,
    language: "en",
    theme: "light",
    font_mode: "pretendard",
    claude_taskbar_offset_ratio: 0.25,
    codex_taskbar_offset_ratio: 0.75,
    show_claude: true,
    show_codex: true,
    claude_usage_auto_refresh_lab_on: true,
    custom_safe: "#112233",
    custom_warn: "#445566",
    custom_danger: "#778899",
  });
});

test("theme defaults to system and taskbar offset is clamped", () => {
  assert.equal(formStateFromSettings({}).theme, "system");
  assert.equal(formStateFromSettings({}).warnThreshold, 30);
  assert.equal(formStateFromSettings({}).dangerThreshold, 10);
  assert.equal(formStateFromSettings({}).language, "system");
  assert.equal(formStateFromSettings({ language: "ko" }).language, "ko");
  assert.equal(formStateFromSettings({ language: "en" }).language, "en");
  assert.equal(formStateFromSettings({ language: "unexpected" }).language, "system");
  assert.equal(formStateFromSettings({}).fontMode, "system");
  assert.equal(formStateFromSettings({}).fullscreenHideOn, true);
  assert.equal(formStateFromSettings({}).maximizedHideOn, false);
  assert.equal(formStateFromSettings({}).indicatorStyle, "ring");
  assert.equal(formStateFromSettings({}).limitOrder, "primary_first");
  assert.equal(formStateFromSettings({ limit_order: "secondary_first" }).limitOrder, "secondary_first");
  assert.equal(formStateFromSettings({ limit_order: "unexpected" }).limitOrder, "primary_first");
  assert.equal(formStateFromSettings({}).ringNumbersOn, true);
  assert.equal(formStateFromSettings({}).ringNumberOutlineOn, true);
  assert.equal(formStateFromSettings({}).ringNumberOutlineWidthPx, 1.2);
  assert.equal(formStateFromSettings({}).ringSizePx, 36);
  assert.equal(formStateFromSettings({}).ringThicknessPx, 4);
  assert.equal(formStateFromSettings({}).ringGapPx, 6);
  assert.equal(formStateFromSettings({}).ringCenterGapPx, 0);
  assert.equal(formStateFromSettings({}).ringNumberFontSizePx, 9);
  assert.equal(formStateFromSettings({}).ringNumberFontWeight, 600);
  assert.equal(formStateFromSettings({}).barTextFontSizePx, 11);
  assert.equal(formStateFromSettings({}).barTextFontWeight, 500);
  assert.equal(formStateFromSettings({ indicator_style: "unexpected" }).indicatorStyle, "ring");
  assert.equal(formStateFromSettings({ ring_size_px: 99 }).ringSizePx, 44);
  assert.equal(formStateFromSettings({ ring_thickness_px: 99 }).ringThicknessPx, 10);
  assert.equal(formStateFromSettings({ ring_gap_px: -1 }).ringGapPx, 2);
  assert.equal(formStateFromSettings({ ring_center_gap_px: 99 }).ringCenterGapPx, 8);
  assert.equal(formStateFromSettings({ ring_center_gap_px: -1 }).ringCenterGapPx, 0);
  assert.equal(formStateFromSettings({ ring_number_outline_width_px: 99 }).ringNumberOutlineWidthPx, 4);
  assert.equal(formStateFromSettings({ ring_number_outline_width_px: -1 }).ringNumberOutlineWidthPx, 0);
  assert.equal(formStateFromSettings({ ring_number_font_size_px: 99 }).ringNumberFontSizePx, 16);
  assert.equal(formStateFromSettings({ ring_number_font_weight: 999 }).ringNumberFontWeight, 900);
  assert.equal(formStateFromSettings({ bar_text_font_size_px: -1 }).barTextFontSizePx, 8);
  assert.equal(formStateFromSettings({ bar_text_font_weight: 999 }).barTextFontWeight, 900);
  assert.equal(formStateFromSettings({ taskbar_offset_ratio: 0.2 }).claudeTaskbarOffsetRatio, 0.2);
  assert.equal(formStateFromSettings({ taskbar_offset_ratio: 0.2 }).codexTaskbarOffsetRatio, 0.2);
  assert.equal(formStateFromSettings({ claude_taskbar_offset_ratio: 2 }).claudeTaskbarOffsetRatio, 1);
  assert.equal(formStateFromSettings({ codex_taskbar_offset_ratio: -1 }).codexTaskbarOffsetRatio, 0);
  assert.equal(formStateFromSettings({}).showClaude, true);
  assert.equal(formStateFromSettings({}).showCodex, true);
  assert.equal(formStateFromSettings({}).claudeUsageAutoRefreshLabOn, false);

  const payload = payloadFromEntries({
    warn_threshold: "30",
    danger_threshold: "10",
    theme: "unexpected",
    language: "unexpected",
    font_mode: "unexpected",
    indicator_style: "unexpected",
    limit_order: "unexpected",
    ring_size_px: "99",
    ring_thickness_px: "99",
    ring_gap_px: "-1",
    ring_center_gap_px: "99",
    ring_number_outline_width_px: "99",
    ring_number_font_size_px: "99",
    ring_number_font_weight: "999",
    bar_text_font_size_px: "-1",
    bar_text_font_weight: "999",
    claude_taskbar_offset_ratio: "-1",
    codex_taskbar_offset_ratio: "2",
  });

  assert.equal(payload.theme, "system");
  assert.equal(payload.warn_threshold, 70);
  assert.equal(payload.danger_threshold, 90);
  assert.equal(payload.language, "system");
  assert.equal(payload.font_mode, "system");
  assert.equal(payload.fullscreen_hide_on, false);
  assert.equal(payload.maximized_hide_on, false);
  assert.equal(payload.indicator_style, "ring");
  assert.equal(payload.limit_order, "primary_first");
  assert.equal(payload.ring_numbers_on, false);
  assert.equal(payload.ring_number_outline_on, false);
  assert.equal(payload.ring_size_px, 44);
  assert.equal(payload.ring_thickness_px, 10);
  assert.equal(payload.ring_gap_px, 2);
  assert.equal(payload.ring_center_gap_px, 8);
  assert.equal(payload.ring_number_outline_width_px, 4);
  assert.equal(payload.ring_number_font_size_px, 16);
  assert.equal(payload.ring_number_font_weight, 900);
  assert.equal(payload.bar_text_font_size_px, 8);
  assert.equal(payload.bar_text_font_weight, 900);
  assert.equal(payload.claude_taskbar_offset_ratio, 0);
  assert.equal(payload.codex_taskbar_offset_ratio, 1);
  assert.equal(payload.show_claude, false);
  assert.equal(payload.show_codex, false);
  assert.equal(payload.claude_usage_auto_refresh_lab_on, false);
  assert.equal("tool_gap_px" in payload, false);
});
