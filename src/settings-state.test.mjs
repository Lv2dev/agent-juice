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
      display_basis: "used",
      poll_interval_secs: 4,
      stale_after_secs: 100,
      activity_weeks: 26,
      activity_scale_mode: "fixed",
      activity_tokens_per_level: 750000,
      bar_mode: "compact",
      full_reset_time_on: true,
      limit_order: "secondary_first",
      fullscreen_hide_on: false,
      maximized_hide_on: true,
      indicator_style: "bar",
      indicator_effect_style: "glow",
      indicator_track_color_auto: false,
      indicator_track_color: [0x12, 0x34, 0x56],
      indicator_track_opacity_percent: 37.5,
      ring_on: false,
      ring_numbers_on: false,
      ring_number_outline_on: true,
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
      autostart_on: false,
      update_check_on: false,
      language: "en",
      theme: "dark",
      font_mode: "pretendard",
      taskbar_offset_ratio: 0.25,
      claude_taskbar_offset_ratio: 0.15,
      codex_taskbar_offset_ratio: 0.85,
      show_claude: false,
      show_codex: true,
      claude_account_auto_collect_on: true,
      tool_colors: {
        claude_primary: [0x10, 0x20, 0x30],
        claude_secondary: [0x40, 0x50, 0x60],
        codex_primary: [0x70, 0x80, 0x90],
        codex_secondary: [0xa0, 0xb0, 0xc0],
        warning: [0xb0, 0xc0, 0xd0],
        danger: [0xd0, 0xc0, 0xb0],
        warning_on: false,
        danger_on: true,
      },
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
    }),
    {
      palette: "cvd",
      displayBasis: "used",
      warnThreshold: 60,
      dangerThreshold: 92,
      pollIntervalSecs: 4,
      staleAfterSecs: 100,
      activityWeeks: 26,
      activityScaleMode: "fixed",
      activityTokensPerLevel: 750000,
      barMode: "compact",
      fullResetTimeOn: true,
      limitOrder: "secondary_first",
      fullscreenHideOn: false,
      maximizedHideOn: true,
      indicatorStyle: "bar",
      indicatorEffectStyle: "glow",
      indicatorTrackColorAuto: false,
      indicatorTrackColor: "#123456",
      indicatorTrackOpacityPercent: 37.5,
      ringOn: false,
      ringNumbersOn: false,
      ringNumberOutlineOn: true,
      ringNumberOutlineWidthPx: 1.4,
      ringSizePx: 34.5,
      ringThicknessPx: 6.5,
      ringGapPx: 8.5,
      ringCenterSizePx: 18.5,
      ringNumberFontSizePx: 10.5,
      ringNumberFontWeight: 650,
      barTextFontSizePx: 12.5,
      barTextFontWeight: 550,
      barContentGapPx: 3.5,
      autostartOn: false,
      updateCheckOn: false,
      language: "en",
      theme: "dark",
      fontMode: "pretendard",
      claudeTaskbarOffsetRatio: 0.15,
      codexTaskbarOffsetRatio: 0.85,
      showClaude: false,
      showCodex: true,
      claudeAccountAutoCollectOn: true,
      monoColor: "#4f8a73",
      customSafe: "#22c55e",
      customWarn: "#f59e0b",
      customDanger: "#ef4444",
      claudePrimaryColor: "#102030",
      claudeSecondaryColor: "#405060",
      codexPrimaryColor: "#708090",
      codexSecondaryColor: "#a0b0c0",
      toolWarningColor: "#b0c0d0",
      toolDangerColor: "#d0c0b0",
      toolWarningColorOn: false,
      toolDangerColorOn: true,
      claudeTextColor: "#112244",
      claudeTextColorOn: true,
      codexTextColor: "#335577",
      codexTextColorOn: false,
      infoTextColor: "#446688",
      infoTextColorOn: true,
      ringTextColor: "#557799",
      ringTextColorOn: true,
    },
  );

  const custom = formStateFromSettings({
    palette: { Custom: [[1, 2, 3], [4, 5, 6], [7, 8, 9]] },
  });

  assert.equal(custom.palette, "custom");
  assert.equal(custom.customSafe, "#010203");
  assert.equal(custom.customWarn, "#040506");
  assert.equal(custom.customDanger, "#070809");

  const mono = formStateFromSettings({ palette: { Mono: [0x34, 0x56, 0x78] } });
  assert.equal(mono.palette, "mono");
  assert.equal(mono.monoColor, "#345678");
});

test("payloadFromEntries creates save_settings input payload", () => {
  const payload = payloadFromEntries({
    palette: "custom",
    display_basis: "remaining",
    warn_threshold: "28",
    danger_threshold: "9",
    poll_interval_secs: "5",
    stale_after_secs: "80",
    activity_weeks: "26",
    activity_scale_mode: "fixed",
    activity_tokens_per_level: "750000",
    bar_mode: "quad",
    full_reset_time_on: "on",
    limit_order: "secondary_first",
    fullscreen_hide_on: "on",
    maximized_hide_on: "on",
    indicator_style: "bar",
    indicator_effect_style: "depth",
    indicator_track_color_auto: "on",
    indicator_track_color: "#123456",
    indicator_track_opacity_percent: "37.5",
    ring_on: "on",
    ring_numbers_on: "on",
    ring_number_outline_on: "on",
    ring_number_outline_width_px: "1.4",
    ring_size_px: "34.5",
    ring_thickness_px: "6.5",
    ring_gap_px: "8.5",
    ring_center_size_px: "18.5",
    ring_number_font_size_px: "10.5",
    ring_number_font_weight: "650",
    bar_text_font_size_px: "12.5",
    bar_text_font_weight: "550",
    bar_content_gap_px: "3.5",
    autostart_on: "on",
    update_check_on: "on",
    language: "en",
    theme: "light",
    font_mode: "pretendard",
    claude_taskbar_offset_ratio: "0.25",
    codex_taskbar_offset_ratio: "0.75",
    show_claude: "on",
    show_codex: "on",
    claude_account_auto_collect_on: "on",
    mono_color: "#345678",
    custom_safe: "#112233",
    custom_warn: "#445566",
    custom_danger: "#778899",
    claude_primary_color: "#102030",
    claude_secondary_color: "#405060",
    codex_primary_color: "#708090",
    codex_secondary_color: "#a0b0c0",
    tool_warning_color: "#b0c0d0",
    tool_danger_color: "#d0c0b0",
    tool_warning_color_on: "on",
    claude_text_color: "#112244",
    claude_text_color_on: "on",
    codex_text_color: "#335577",
    info_text_color: "#446688",
    info_text_color_on: "on",
    ring_text_color: "#557799",
    ring_text_color_on: "on",
  });

  assert.deepEqual(payload, {
    palette: "custom",
    display_basis: "remaining",
    warn_threshold: 72,
    danger_threshold: 91,
    poll_interval_secs: 5,
    stale_after_secs: 80,
    activity_weeks: 26,
    activity_scale_mode: "fixed",
    activity_tokens_per_level: 750000,
    bar_mode: "quad",
    full_reset_time_on: true,
    limit_order: "secondary_first",
    fullscreen_hide_on: true,
    maximized_hide_on: true,
    indicator_style: "bar",
    indicator_effect_style: "depth",
    indicator_track_color_auto: true,
    indicator_track_color: "#123456",
    indicator_track_opacity_percent: 37.5,
    ring_on: true,
    ring_numbers_on: true,
    ring_number_outline_on: true,
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
    autostart_on: true,
    update_check_on: true,
    language: "en",
    theme: "light",
    font_mode: "pretendard",
    claude_taskbar_offset_ratio: 0.25,
    codex_taskbar_offset_ratio: 0.75,
    show_claude: true,
    show_codex: true,
    claude_account_auto_collect_on: true,
    mono_color: "#345678",
    custom_safe: "#112233",
    custom_warn: "#445566",
    custom_danger: "#778899",
    claude_primary_color: "#102030",
    claude_secondary_color: "#405060",
    codex_primary_color: "#708090",
    codex_secondary_color: "#a0b0c0",
    tool_warning_color: "#b0c0d0",
    tool_danger_color: "#d0c0b0",
    tool_warning_color_on: true,
    tool_danger_color_on: false,
    claude_text_color: "#112244",
    claude_text_color_on: true,
    codex_text_color: "#335577",
    codex_text_color_on: false,
    info_text_color: "#446688",
    info_text_color_on: true,
    ring_text_color: "#557799",
    ring_text_color_on: true,
  });
});

test("theme defaults to system and taskbar offset is clamped", () => {
  assert.equal(formStateFromSettings({}).theme, "system");
  assert.equal(formStateFromSettings({}).displayBasis, "remaining");
  assert.equal(formStateFromSettings({}).warnThreshold, 30);
  assert.equal(formStateFromSettings({}).dangerThreshold, 10);
  assert.equal(formStateFromSettings({}).pollIntervalSecs, 60);
  assert.equal(formStateFromSettings({}).activityWeeks, 52);
  assert.equal(formStateFromSettings({}).activityScaleMode, "auto");
  assert.equal(formStateFromSettings({}).activityTokensPerLevel, 250000);
  assert.equal(formStateFromSettings({}).language, "system");
  assert.equal(formStateFromSettings({ language: "ko" }).language, "ko");
  assert.equal(formStateFromSettings({ language: "en" }).language, "en");
  assert.equal(formStateFromSettings({ language: "unexpected" }).language, "system");
  assert.equal(formStateFromSettings({}).fontMode, "system");
  assert.equal(formStateFromSettings({}).fullscreenHideOn, false);
  assert.equal(formStateFromSettings({}).fullResetTimeOn, true);
  assert.equal(
    formStateFromSettings({ full_reset_time_on: false }).fullResetTimeOn,
    false,
  );
  assert.equal(formStateFromSettings({}).maximizedHideOn, false);
  assert.equal(formStateFromSettings({}).indicatorStyle, "ring");
  assert.equal(formStateFromSettings({}).indicatorEffectStyle, "flat");
  assert.equal(formStateFromSettings({}).indicatorTrackColorAuto, true);
  assert.equal(formStateFromSettings({}).indicatorTrackColor, "#6b7280");
  assert.equal(formStateFromSettings({}).indicatorTrackOpacityPercent, 11);
  assert.equal(formStateFromSettings({ indicator_effect_style: "breathe" }).indicatorEffectStyle, "breathe");
  assert.equal(formStateFromSettings({ indicator_effect_style: "unexpected" }).indicatorEffectStyle, "flat");
  assert.equal(formStateFromSettings({}).limitOrder, "primary_first");
  assert.equal(formStateFromSettings({ limit_order: "secondary_first" }).limitOrder, "secondary_first");
  assert.equal(formStateFromSettings({ limit_order: "unexpected" }).limitOrder, "primary_first");
  assert.equal(formStateFromSettings({}).ringNumbersOn, true);
  assert.equal(formStateFromSettings({}).ringNumberOutlineOn, true);
  assert.equal(formStateFromSettings({}).ringNumberOutlineWidthPx, 1.2);
  assert.equal(formStateFromSettings({}).ringSizePx, 36);
  assert.equal(formStateFromSettings({}).ringThicknessPx, 4);
  assert.equal(formStateFromSettings({}).ringGapPx, 6);
  assert.equal(formStateFromSettings({}).ringCenterSizePx, 16);
  assert.equal(formStateFromSettings({}).ringNumberFontSizePx, 9);
  assert.equal(formStateFromSettings({}).ringNumberFontWeight, 600);
  assert.equal(formStateFromSettings({}).barTextFontSizePx, 11);
  assert.equal(formStateFromSettings({}).barTextFontWeight, 500);
  assert.equal(formStateFromSettings({}).barContentGapPx, 14);
  assert.equal(formStateFromSettings({ indicator_style: "unexpected" }).indicatorStyle, "ring");
  assert.equal(formStateFromSettings({ ring_size_px: 99 }).ringSizePx, 44);
  assert.equal(formStateFromSettings({ ring_thickness_px: 99 }).ringThicknessPx, 10);
  assert.equal(formStateFromSettings({ ring_gap_px: -1 }).ringGapPx, 2);
  assert.equal(formStateFromSettings({ ring_center_size_px: 99 }).ringCenterSizePx, 32);
  assert.equal(formStateFromSettings({ ring_center_size_px: -1 }).ringCenterSizePx, 4);
  assert.equal(formStateFromSettings({ ring_number_outline_width_px: 99 }).ringNumberOutlineWidthPx, 4);
  assert.equal(formStateFromSettings({ ring_number_outline_width_px: -1 }).ringNumberOutlineWidthPx, 0);
  assert.equal(formStateFromSettings({ ring_number_font_size_px: 99 }).ringNumberFontSizePx, 16);
  assert.equal(formStateFromSettings({ ring_number_font_weight: 999 }).ringNumberFontWeight, 900);
  assert.equal(formStateFromSettings({ bar_text_font_size_px: -1 }).barTextFontSizePx, 8);
  assert.equal(formStateFromSettings({ bar_text_font_weight: 999 }).barTextFontWeight, 900);
  assert.equal(formStateFromSettings({ bar_content_gap_px: 99 }).barContentGapPx, 24);
  assert.equal(formStateFromSettings({ bar_content_gap_px: -1 }).barContentGapPx, 0);
  assert.equal(formStateFromSettings({}).claudeTaskbarOffsetRatio, 0);
  assert.equal(formStateFromSettings({}).codexTaskbarOffsetRatio, 0);
  assert.equal(formStateFromSettings({ taskbar_offset_ratio: 0.2 }).claudeTaskbarOffsetRatio, 0.2);
  assert.equal(formStateFromSettings({ taskbar_offset_ratio: 0.2 }).codexTaskbarOffsetRatio, 0.2);
  assert.equal(formStateFromSettings({ claude_taskbar_offset_ratio: 2 }).claudeTaskbarOffsetRatio, 1);
  assert.equal(formStateFromSettings({ codex_taskbar_offset_ratio: -1 }).codexTaskbarOffsetRatio, 0);
  assert.equal(formStateFromSettings({}).showClaude, true);
  assert.equal(formStateFromSettings({}).showCodex, true);
  assert.equal(formStateFromSettings({}).claudeAccountAutoCollectOn, true);
  assert.equal(formStateFromSettings({}).updateCheckOn, true);
  assert.equal(formStateFromSettings({}).claudePrimaryColor, "#d79a32");
  assert.equal(formStateFromSettings({}).claudeSecondaryColor, "#d36b86");
  assert.equal(formStateFromSettings({}).codexPrimaryColor, "#2fac7d");
  assert.equal(formStateFromSettings({}).codexSecondaryColor, "#4d86d6");
  assert.equal(formStateFromSettings({}).toolWarningColor, "#f59e0b");
  assert.equal(formStateFromSettings({}).toolDangerColor, "#ef4444");
  assert.equal(formStateFromSettings({}).toolWarningColorOn, true);
  assert.equal(formStateFromSettings({}).toolDangerColorOn, true);
  assert.equal(formStateFromSettings({}).claudeTextColor, "#d79a32");
  assert.equal(formStateFromSettings({}).claudeTextColorOn, false);
  assert.equal(formStateFromSettings({}).codexTextColor, "#2fac7d");
  assert.equal(formStateFromSettings({}).codexTextColorOn, false);
  assert.equal(formStateFromSettings({}).infoTextColor, "#6b7280");
  assert.equal(formStateFromSettings({}).infoTextColorOn, false);
  assert.equal(formStateFromSettings({}).ringTextColor, "#6b7280");
  assert.equal(formStateFromSettings({}).ringTextColorOn, false);
  assert.equal(formStateFromSettings({ update_check_on: false }).updateCheckOn, false);
  assert.equal(
    formStateFromSettings({ claude_usage_auto_refresh_lab_on: false }).claudeAccountAutoCollectOn,
    false,
  );

  const payload = payloadFromEntries({
    display_basis: "remaining",
    warn_threshold: "30",
    danger_threshold: "10",
    theme: "unexpected",
    language: "unexpected",
    font_mode: "unexpected",
    indicator_style: "unexpected",
    indicator_effect_style: "unexpected",
    indicator_track_opacity_percent: "999",
    limit_order: "unexpected",
    ring_size_px: "99",
    ring_thickness_px: "99",
    ring_gap_px: "-1",
    ring_center_size_px: "99",
    ring_number_outline_width_px: "99",
    ring_number_font_size_px: "99",
    ring_number_font_weight: "999",
    bar_text_font_size_px: "-1",
    bar_text_font_weight: "999",
    bar_content_gap_px: "99",
    claude_taskbar_offset_ratio: "-1",
    codex_taskbar_offset_ratio: "2",
    activity_weeks: "99",
    activity_scale_mode: "unexpected",
    activity_tokens_per_level: "0",
  });

  assert.equal(payload.theme, "system");
  assert.equal(payload.display_basis, "remaining");
  assert.equal(payload.warn_threshold, 70);
  assert.equal(payload.danger_threshold, 90);
  assert.equal(payload.language, "system");
  assert.equal(payload.font_mode, "system");
  assert.equal(payload.fullscreen_hide_on, false);
  assert.equal(payload.full_reset_time_on, false);
  assert.equal(payload.maximized_hide_on, false);
  assert.equal(payload.indicator_style, "ring");
  assert.equal(payload.indicator_effect_style, "flat");
  assert.equal(payload.indicator_track_color_auto, false);
  assert.equal(payload.indicator_track_color, "#6b7280");
  assert.equal(payload.indicator_track_opacity_percent, 100);
  assert.equal(payload.limit_order, "primary_first");
  assert.equal(payload.ring_numbers_on, false);
  assert.equal(payload.ring_number_outline_on, false);
  assert.equal(payload.ring_size_px, 44);
  assert.equal(payload.ring_thickness_px, 10);
  assert.equal(payload.ring_gap_px, 2);
  assert.equal(payload.ring_center_size_px, 32);
  assert.equal(payload.ring_number_outline_width_px, 4);
  assert.equal(payload.ring_number_font_size_px, 16);
  assert.equal(payload.ring_number_font_weight, 900);
  assert.equal(payload.bar_text_font_size_px, 8);
  assert.equal(payload.bar_text_font_weight, 900);
  assert.equal(payload.bar_content_gap_px, 24);
  assert.equal(payload.claude_taskbar_offset_ratio, 0);
  assert.equal(payload.codex_taskbar_offset_ratio, 1);
  assert.equal(payload.activity_weeks, 52);
  assert.equal(payload.activity_scale_mode, "auto");
  assert.equal(payload.activity_tokens_per_level, 1);
  assert.equal(payload.show_claude, false);
  assert.equal(payload.show_codex, false);
  assert.equal(payload.claude_account_auto_collect_on, false);
  assert.equal(payload.update_check_on, false);
  assert.equal("tool_gap_px" in payload, false);
});

test("usage display basis keeps canonical used thresholds without inversion", () => {
  const state = formStateFromSettings({
    display_basis: "used",
    warn_threshold: 70,
    danger_threshold: 90,
  });
  assert.equal(state.displayBasis, "used");
  assert.equal(state.warnThreshold, 70);
  assert.equal(state.dangerThreshold, 90);

  const payload = payloadFromEntries({
    display_basis: "used",
    warn_threshold: "72",
    danger_threshold: "91",
  });
  assert.equal(payload.display_basis, "used");
  assert.equal(payload.warn_threshold, 72);
  assert.equal(payload.danger_threshold, 91);
});
