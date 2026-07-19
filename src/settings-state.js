const DEFAULT_CUSTOM = {
  customSafe: "#22c55e",
  customWarn: "#f59e0b",
  customDanger: "#ef4444",
};

const DEFAULT_TOOL_COLORS = {
  claudePrimary: "#d79a32",
  claudeSecondary: "#d36b86",
  codexPrimary: "#2fac7d",
  codexSecondary: "#4d86d6",
  warning: "#f59e0b",
  danger: "#ef4444",
};

const DEFAULT_TASKBAR_TEXT_COLORS = {
  claude: "#d79a32",
  codex: "#2fac7d",
  info: "#6b7280",
  ring: "#6b7280",
};

const DEFAULT_MONO_COLOR = "#4f8a73";
const DEFAULT_INDICATOR_TRACK_COLOR = "#6b7280";

const DEFAULT_RING = {
  sizePx: 36,
  thicknessPx: 4,
  gapPx: 6,
  centerSizePx: 16,
  numberOutlineWidthPx: 1.2,
  numberFontSizePx: 9,
  numberFontWeight: 600,
  textFontSizePx: 11,
  textFontWeight: 500,
};

function numberOr(value, fallback) {
  const number = Number(value);
  return Number.isFinite(number) ? number : fallback;
}

function percentOr(value, fallback) {
  return Math.min(100, Math.max(0, numberOr(value, fallback)));
}

function usedToRemainingThreshold(value, fallbackUsed) {
  return 100 - percentOr(value, fallbackUsed);
}

function remainingToUsedThreshold(value, fallbackRemaining) {
  return 100 - percentOr(value, fallbackRemaining);
}

function intOr(value, fallback) {
  const number = Math.round(numberOr(value, fallback));
  return Math.max(1, number);
}

function intRangeOr(value, fallback, min, max) {
  const number = Math.round(numberOr(value, fallback));
  return Math.min(max, Math.max(min, number));
}

function pxRangeOr(value, fallback, min, max) {
  const number = numberOr(value, fallback);
  return Math.min(max, Math.max(min, number));
}

function weightRangeOr(value, fallback) {
  return intRangeOr(value, fallback, 100, 900);
}

function ratioOr(value, fallback) {
  return Math.min(1, Math.max(0, numberOr(value, fallback)));
}

function isChecked(value) {
  return value === true || value === "on" || value === "true";
}

function boolOr(value, fallback) {
  if (value == null) return fallback;
  if (value === false || value === "false" || value === "off") return false;
  if (value === true || value === "true" || value === "on") return true;
  return fallback;
}

function themeOr(value) {
  const theme = String(value || "system").toLowerCase();
  return theme === "light" || theme === "dark" ? theme : "system";
}

function fontModeOr(value) {
  const mode = String(value || "system").toLowerCase();
  return mode === "pretendard" ? mode : "system";
}

function languageOr(value) {
  const language = String(value || "system").toLowerCase();
  return language === "ko" || language === "en" ? language : "system";
}

function displayBasisOr(value) {
  return String(value || "remaining").toLowerCase() === "used" ? "used" : "remaining";
}

function activityScaleModeOr(value) {
  return String(value || "auto").toLowerCase() === "fixed" ? "fixed" : "auto";
}

function indicatorStyleOr(value) {
  const style = String(value || "ring").toLowerCase();
  return style === "bar" ? "bar" : "ring";
}

function indicatorEffectStyleOr(value) {
  const style = String(value || "flat").toLowerCase();
  return ["soft", "depth", "glow", "breathe"].includes(style) ? style : "flat";
}

function limitOrderOr(value) {
  const order = String(value || "primary_first").toLowerCase();
  return order === "secondary_first" ? "secondary_first" : "primary_first";
}

function hexByte(value) {
  return Math.min(255, Math.max(0, Math.round(value)))
    .toString(16)
    .padStart(2, "0");
}

function rgbToHex(rgb, fallback) {
  if (!Array.isArray(rgb) || rgb.length !== 3) return fallback;
  return `#${hexByte(rgb[0])}${hexByte(rgb[1])}${hexByte(rgb[2])}`;
}

function paletteState(palette) {
  if (palette && typeof palette === "object" && Array.isArray(palette.Custom)) {
    return {
      palette: "custom",
      monoColor: DEFAULT_MONO_COLOR,
      customSafe: rgbToHex(palette.Custom[0], DEFAULT_CUSTOM.customSafe),
      customWarn: rgbToHex(palette.Custom[1], DEFAULT_CUSTOM.customWarn),
      customDanger: rgbToHex(palette.Custom[2], DEFAULT_CUSTOM.customDanger),
    };
  }


  if (palette && typeof palette === "object" && Array.isArray(palette.Mono)) {
    return {
      palette: "mono",
      monoColor: rgbToHex(palette.Mono, DEFAULT_MONO_COLOR),
      ...DEFAULT_CUSTOM,
    };
  }

  const value = typeof palette === "string" ? palette.toLowerCase() : "traffic";
  if (["signal", "cvd", "cool", "ocean", "forest", "sunset"].includes(value)) {
    return { palette: value, monoColor: DEFAULT_MONO_COLOR, ...DEFAULT_CUSTOM };
  }
  return { palette: "traffic", monoColor: DEFAULT_MONO_COLOR, ...DEFAULT_CUSTOM };
}

function toolColorState(toolColors) {
  const colors = toolColors && typeof toolColors === "object" ? toolColors : {};
  return {
    claudePrimaryColor: rgbToHex(colors.claude_primary, DEFAULT_TOOL_COLORS.claudePrimary),
    claudeSecondaryColor: rgbToHex(colors.claude_secondary, DEFAULT_TOOL_COLORS.claudeSecondary),
    codexPrimaryColor: rgbToHex(colors.codex_primary, DEFAULT_TOOL_COLORS.codexPrimary),
    codexSecondaryColor: rgbToHex(colors.codex_secondary, DEFAULT_TOOL_COLORS.codexSecondary),
    toolWarningColor: rgbToHex(colors.warning, DEFAULT_TOOL_COLORS.warning),
    toolDangerColor: rgbToHex(colors.danger, DEFAULT_TOOL_COLORS.danger),
    toolWarningColorOn: boolOr(colors.warning_on, true),
    toolDangerColorOn: boolOr(colors.danger_on, true),
  };
}

function taskbarTextColorState(taskbarTextColors) {
  const colors = taskbarTextColors && typeof taskbarTextColors === "object"
    ? taskbarTextColors
    : {};
  return {
    claudeTextColor: rgbToHex(colors.claude, DEFAULT_TASKBAR_TEXT_COLORS.claude),
    claudeTextColorOn: boolOr(colors.claude_on, false),
    codexTextColor: rgbToHex(colors.codex, DEFAULT_TASKBAR_TEXT_COLORS.codex),
    codexTextColorOn: boolOr(colors.codex_on, false),
    infoTextColor: rgbToHex(colors.info, DEFAULT_TASKBAR_TEXT_COLORS.info),
    infoTextColorOn: boolOr(colors.info_on, false),
    ringTextColor: rgbToHex(colors.ring, DEFAULT_TASKBAR_TEXT_COLORS.ring),
    ringTextColorOn: boolOr(colors.ring_on, false),
  };
}

export function formStateFromSettings(settings = {}) {
  const palette = paletteState(settings.palette);
  const toolColors = toolColorState(settings.tool_colors);
  const taskbarTextColors = taskbarTextColorState(settings.taskbar_text_colors);
  const legacyOffset = ratioOr(settings.taskbar_offset_ratio, 0);
  const displayBasis = displayBasisOr(settings.display_basis);

  return {
    palette: palette.palette,
    displayBasis,
    warnThreshold: displayBasis === "used"
      ? percentOr(settings.warn_threshold, 70)
      : usedToRemainingThreshold(settings.warn_threshold, 70),
    dangerThreshold: displayBasis === "used"
      ? percentOr(settings.danger_threshold, 90)
      : usedToRemainingThreshold(settings.danger_threshold, 90),
    pollIntervalSecs: intOr(settings.poll_interval_secs, 60),
    staleAfterSecs: intOr(settings.stale_after_secs, 90),
    activityWeeks: intRangeOr(settings.activity_weeks, 52, 4, 52),
    activityScaleMode: activityScaleModeOr(settings.activity_scale_mode),
    activityTokensPerLevel: intRangeOr(
      settings.activity_tokens_per_level,
      250_000,
      1,
      1_000_000_000_000,
    ),
    barMode: settings.bar_mode || "full",
    fullResetTimeOn: boolOr(settings.full_reset_time_on, true),
    limitOrder: limitOrderOr(settings.limit_order),
    fullscreenHideOn: boolOr(settings.fullscreen_hide_on, false),
    maximizedHideOn: boolOr(settings.maximized_hide_on, false),
    indicatorStyle: indicatorStyleOr(settings.indicator_style),
    indicatorEffectStyle: indicatorEffectStyleOr(settings.indicator_effect_style),
    indicatorTrackColorAuto: boolOr(settings.indicator_track_color_auto, true),
    indicatorTrackColor: rgbToHex(
      settings.indicator_track_color,
      DEFAULT_INDICATOR_TRACK_COLOR,
    ),
    indicatorTrackOpacityPercent: percentOr(settings.indicator_track_opacity_percent, 11),
    ringOn: settings.ring_on !== false,
    ringNumbersOn: boolOr(settings.ring_numbers_on, true),
    ringNumberOutlineOn: boolOr(settings.ring_number_outline_on, true),
    ringNumberOutlineWidthPx: pxRangeOr(
      settings.ring_number_outline_width_px,
      DEFAULT_RING.numberOutlineWidthPx,
      0,
      4,
    ),
    ringSizePx: pxRangeOr(settings.ring_size_px, DEFAULT_RING.sizePx, 20, 44),
    ringThicknessPx: pxRangeOr(settings.ring_thickness_px, DEFAULT_RING.thicknessPx, 1, 10),
    ringGapPx: pxRangeOr(settings.ring_gap_px, DEFAULT_RING.gapPx, 2, 14),
    ringCenterSizePx: pxRangeOr(settings.ring_center_size_px, DEFAULT_RING.centerSizePx, 4, 32),
    ringNumberFontSizePx: pxRangeOr(
      settings.ring_number_font_size_px,
      DEFAULT_RING.numberFontSizePx,
      6,
      16,
    ),
    ringNumberFontWeight: weightRangeOr(
      settings.ring_number_font_weight,
      DEFAULT_RING.numberFontWeight,
    ),
    barTextFontSizePx: pxRangeOr(
      settings.bar_text_font_size_px,
      DEFAULT_RING.textFontSizePx,
      8,
      16,
    ),
    barTextFontWeight: weightRangeOr(
      settings.bar_text_font_weight,
      DEFAULT_RING.textFontWeight,
    ),
    barContentGapPx: pxRangeOr(settings.bar_content_gap_px, 14, 0, 24),
    autostartOn: settings.autostart_on !== false,
    updateCheckOn: settings.update_check_on !== false,
    language: languageOr(settings.language),
    theme: themeOr(settings.theme),
    fontMode: fontModeOr(settings.font_mode),
    claudeTaskbarOffsetRatio: ratioOr(
      settings.claude_taskbar_offset_ratio,
      legacyOffset,
    ),
    codexTaskbarOffsetRatio: ratioOr(
      settings.codex_taskbar_offset_ratio,
      legacyOffset,
    ),
    showClaude: settings.show_claude !== false,
    showCodex: settings.show_codex !== false,
    claudeAccountAutoCollectOn: boolOr(
      settings.claude_account_auto_collect_on ?? settings.claude_usage_auto_refresh_lab_on,
      true,
    ),
    monoColor: palette.monoColor,
    customSafe: palette.customSafe,
    customWarn: palette.customWarn,
    customDanger: palette.customDanger,
    ...toolColors,
    ...taskbarTextColors,
  };
}

function entrySource(entries) {
  if (entries && typeof entries.get === "function") return entries;
  return {
    get(name) {
      return entries?.[name] ?? null;
    },
  };
}

export function payloadFromEntries(entries) {
  const source = entrySource(entries);
  const displayBasis = displayBasisOr(source.get("display_basis"));
  const warnUsedThreshold = displayBasis === "used"
    ? percentOr(source.get("warn_threshold"), 70)
    : remainingToUsedThreshold(source.get("warn_threshold"), 30);
  const dangerUsedThreshold = Math.max(
    displayBasis === "used"
      ? percentOr(source.get("danger_threshold"), 90)
      : remainingToUsedThreshold(source.get("danger_threshold"), 10),
    warnUsedThreshold,
  );

  return {
    palette: String(source.get("palette") || "traffic"),
    display_basis: displayBasis,
    warn_threshold: warnUsedThreshold,
    danger_threshold: dangerUsedThreshold,
    poll_interval_secs: intOr(source.get("poll_interval_secs"), 60),
    stale_after_secs: intOr(source.get("stale_after_secs"), 90),
    activity_weeks: intRangeOr(source.get("activity_weeks"), 52, 4, 52),
    activity_scale_mode: activityScaleModeOr(source.get("activity_scale_mode")),
    activity_tokens_per_level: intRangeOr(
      source.get("activity_tokens_per_level"),
      250_000,
      1,
      1_000_000_000_000,
    ),
    bar_mode: String(source.get("bar_mode") || "full"),
    full_reset_time_on: isChecked(source.get("full_reset_time_on")),
    limit_order: limitOrderOr(source.get("limit_order")),
    fullscreen_hide_on: isChecked(source.get("fullscreen_hide_on")),
    maximized_hide_on: isChecked(source.get("maximized_hide_on")),
    indicator_style: indicatorStyleOr(source.get("indicator_style")),
    indicator_effect_style: indicatorEffectStyleOr(source.get("indicator_effect_style")),
    indicator_track_color_auto: isChecked(source.get("indicator_track_color_auto")),
    indicator_track_color: String(
      source.get("indicator_track_color") || DEFAULT_INDICATOR_TRACK_COLOR,
    ),
    indicator_track_opacity_percent: percentOr(
      source.get("indicator_track_opacity_percent"),
      11,
    ),
    ring_on: isChecked(source.get("ring_on")),
    ring_numbers_on: isChecked(source.get("ring_numbers_on")),
    ring_number_outline_on: isChecked(source.get("ring_number_outline_on")),
    ring_number_outline_width_px: pxRangeOr(
      source.get("ring_number_outline_width_px"),
      DEFAULT_RING.numberOutlineWidthPx,
      0,
      4,
    ),
    ring_size_px: pxRangeOr(source.get("ring_size_px"), DEFAULT_RING.sizePx, 20, 44),
    ring_thickness_px: pxRangeOr(source.get("ring_thickness_px"), DEFAULT_RING.thicknessPx, 1, 10),
    ring_gap_px: pxRangeOr(source.get("ring_gap_px"), DEFAULT_RING.gapPx, 2, 14),
    ring_center_size_px: pxRangeOr(source.get("ring_center_size_px"), DEFAULT_RING.centerSizePx, 4, 32),
    ring_number_font_size_px: pxRangeOr(
      source.get("ring_number_font_size_px"),
      DEFAULT_RING.numberFontSizePx,
      6,
      16,
    ),
    ring_number_font_weight: weightRangeOr(
      source.get("ring_number_font_weight"),
      DEFAULT_RING.numberFontWeight,
    ),
    bar_text_font_size_px: pxRangeOr(
      source.get("bar_text_font_size_px"),
      DEFAULT_RING.textFontSizePx,
      8,
      16,
    ),
    bar_text_font_weight: weightRangeOr(
      source.get("bar_text_font_weight"),
      DEFAULT_RING.textFontWeight,
    ),
    bar_content_gap_px: pxRangeOr(source.get("bar_content_gap_px"), 14, 0, 24),
    autostart_on: isChecked(source.get("autostart_on")),
    update_check_on: isChecked(source.get("update_check_on")),
    language: languageOr(source.get("language")),
    theme: themeOr(source.get("theme")),
    font_mode: fontModeOr(source.get("font_mode")),
    claude_taskbar_offset_ratio: ratioOr(
      source.get("claude_taskbar_offset_ratio"),
      0,
    ),
    codex_taskbar_offset_ratio: ratioOr(
      source.get("codex_taskbar_offset_ratio"),
      0,
    ),
    show_claude: isChecked(source.get("show_claude")),
    show_codex: isChecked(source.get("show_codex")),
    claude_account_auto_collect_on: isChecked(
      source.get("claude_account_auto_collect_on"),
    ),
    mono_color: String(source.get("mono_color") || DEFAULT_MONO_COLOR),
    custom_safe: String(source.get("custom_safe") || DEFAULT_CUSTOM.customSafe),
    custom_warn: String(source.get("custom_warn") || DEFAULT_CUSTOM.customWarn),
    custom_danger: String(source.get("custom_danger") || DEFAULT_CUSTOM.customDanger),
    claude_primary_color: String(source.get("claude_primary_color") || DEFAULT_TOOL_COLORS.claudePrimary),
    claude_secondary_color: String(source.get("claude_secondary_color") || DEFAULT_TOOL_COLORS.claudeSecondary),
    codex_primary_color: String(source.get("codex_primary_color") || DEFAULT_TOOL_COLORS.codexPrimary),
    codex_secondary_color: String(source.get("codex_secondary_color") || DEFAULT_TOOL_COLORS.codexSecondary),
    tool_warning_color: String(source.get("tool_warning_color") || DEFAULT_TOOL_COLORS.warning),
    tool_danger_color: String(source.get("tool_danger_color") || DEFAULT_TOOL_COLORS.danger),
    tool_warning_color_on: isChecked(source.get("tool_warning_color_on")),
    tool_danger_color_on: isChecked(source.get("tool_danger_color_on")),
    claude_text_color: String(
      source.get("claude_text_color") || DEFAULT_TASKBAR_TEXT_COLORS.claude,
    ),
    claude_text_color_on: isChecked(source.get("claude_text_color_on")),
    codex_text_color: String(
      source.get("codex_text_color") || DEFAULT_TASKBAR_TEXT_COLORS.codex,
    ),
    codex_text_color_on: isChecked(source.get("codex_text_color_on")),
    info_text_color: String(
      source.get("info_text_color") || DEFAULT_TASKBAR_TEXT_COLORS.info,
    ),
    info_text_color_on: isChecked(source.get("info_text_color_on")),
    ring_text_color: String(
      source.get("ring_text_color") || DEFAULT_TASKBAR_TEXT_COLORS.ring,
    ),
    ring_text_color_on: isChecked(source.get("ring_text_color_on")),
  };
}
