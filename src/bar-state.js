import {
  colorForToolPercent,
  DEFAULT_SETTINGS,
  displayPercentFromUsed,
  normalizeDisplayBasis,
  representativeByTool,
  toolBrandColor,
} from "./panel-state.js";
import { formatDuration, resolveLanguage, t } from "./i18n.js";

const TOOL_LABELS = {
  claude: "Claude",
  codex: "Codex",
  grok: "Grok",
  cursor: "Cursor",
};

const MODES = new Set(["full", "compact", "dual", "quad"]);
const TOOLS = ["claude", "codex", "grok", "cursor"];
const INDICATOR_STYLES = new Set(["ring", "bar"]);
const INDICATOR_EFFECT_STYLES = new Set(["flat", "soft", "depth", "glow", "breathe"]);
const LIMIT_ORDERS = new Set(["primary_first", "secondary_first"]);

function finiteNumber(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function toolEnabled(settings, tool) {
  if (tool === "claude") return settings.show_claude !== false;
  if (tool === "codex") return settings.show_codex !== false;
  if (tool === "grok") return settings.show_grok === true;
  if (tool === "cursor") return settings.show_cursor === true;
  return true;
}

function boolSetting(value, fallback) {
  if (value == null) return fallback;
  if (value === false || value === "false" || value === "off") return false;
  if (value === true || value === "true" || value === "on") return true;
  return fallback;
}

function intRangeSetting(value, fallback, min, max) {
  const number = Math.round(Number(value));
  if (!Number.isFinite(number)) return fallback;
  return Math.min(max, Math.max(min, number));
}

function numberRangeSetting(value, fallback, min, max) {
  const number = Number(value);
  if (!Number.isFinite(number)) return fallback;
  return Math.min(max, Math.max(min, number));
}

function worstLimitPercent(primary, secondary) {
  const values = [primary?.used_percent, secondary?.used_percent]
    .map(finiteNumber)
    .filter((value) => value != null);
  return values.length === 0 ? null : Math.max(...values);
}

function severityForStatus(status, settings) {
  if (!status) return "empty";
  if (status.session?.active !== true) return "stale";

  const worst = worstLimitPercent(status.primary, status.secondary);
  if (worst == null) return "live";

  const warn = finiteNumber(settings.warn_threshold) ?? DEFAULT_SETTINGS.warn_threshold;
  const danger = finiteNumber(settings.danger_threshold) ?? DEFAULT_SETTINGS.danger_threshold;
  if (worst >= danger) return "danger";
  if (worst >= warn) return "warn";
  return "live";
}

function percentText(value) {
  const number = finiteNumber(value);
  return number == null ? "–" : `${Math.round(number)}%`;
}

function numberText(value) {
  const number = finiteNumber(value);
  return number == null ? "–" : String(Math.round(number));
}

function arcText(value) {
  const number = finiteNumber(value);
  if (number == null) return "0deg";
  return `${(Math.min(100, Math.max(0, number)) * 3.6).toFixed(1)}deg`;
}

function dashText(value) {
  const number = finiteNumber(value);
  if (number == null) return "0";
  const clamped = Math.min(100, Math.max(0, number));
  return Number.isInteger(clamped) ? String(clamped) : clamped.toFixed(1);
}

function geometryText(value) {
  const rounded = geometryNumber(value);
  return Number.isInteger(rounded) ? rounded.toFixed(0) : rounded.toFixed(1);
}

function geometryNumber(value) {
  return Math.round(value * 10) / 10;
}

function ringSvgGeometry(sizePx, thicknessPx, gapPx, centerSizePx) {
  const size = Math.max(1, sizePx);
  const centerSize = Math.min(Math.max(4, centerSizePx), Math.max(4, size - 4));
  const visibleThickness = Math.max(1, Math.min(thicknessPx, (size - centerSize) / 4));
  const maxCenterGap = Math.max(visibleThickness, (size - centerSize) / 2 - visibleThickness);
  const centerGap = Math.min(Math.max(gapPx, visibleThickness), maxCenterGap);
  const quadThickness = Math.max(1, Math.min(thicknessPx, (size - centerSize) / 2));
  const scale = 100 / size;
  const requestedStroke = visibleThickness * scale;
  const requestedInnerRadius = (centerSize / 2 + visibleThickness / 2) * scale;
  const requestedOuterRadius = requestedInnerRadius + centerGap * scale;
  const requestedQuadStroke = quadThickness * scale;
  const requestedQuadRadius = (centerSize / 2 + quadThickness / 2) * scale;
  const stroke = geometryNumber(requestedStroke);
  const quadStroke = geometryNumber(requestedQuadStroke);
  const outerBound = Math.floor((50 - stroke / 2) * 10) / 10;
  const quadOuterBound = Math.floor((50 - quadStroke / 2) * 10) / 10;
  const outerRadius = Math.min(geometryNumber(requestedOuterRadius), outerBound);
  const innerFloor = Math.ceil((stroke / 2) * 10) / 10;
  const innerRadius = Math.max(
    innerFloor,
    Math.min(geometryNumber(requestedInnerRadius), geometryNumber(outerRadius - stroke)),
  );
  const quadRadius = Math.max(
    Math.ceil((quadStroke / 2) * 10) / 10,
    Math.min(geometryNumber(requestedQuadRadius), quadOuterBound),
  );

  return {
    ringCenterSizePx: geometryNumber(centerSize),
    ringSvgStroke: geometryText(stroke),
    outerRadius: geometryText(outerRadius),
    innerRadius: geometryText(innerRadius),
    quadSvgStroke: geometryText(quadStroke),
    quadRadius: geometryText(quadRadius),
  };
}

function shortReset(iso, now, language) {
  if (!iso) return "";
  const fullDate = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso);
  const monthDay = /^(\d{2})-(\d{2})$/.exec(iso);
  if (fullDate || monthDay) {
    const year = fullDate ? Number(fullDate[1]) : 2000;
    const month = Number((fullDate || monthDay)[fullDate ? 2 : 1]);
    const day = Number((fullDate || monthDay)[fullDate ? 3 : 2]);
    const date = new Date(Date.UTC(year, month - 1, day));
    if (date.getUTCFullYear() !== year || date.getUTCMonth() !== month - 1 || date.getUTCDate() !== day) {
      return "";
    }
    const options = {
      month: "short",
      day: "numeric",
      timeZone: "UTC",
    };
    if (fullDate) options.year = "numeric";
    return new Intl.DateTimeFormat(language === "ko" ? "ko-KR" : "en-US", options).format(date);
  }
  const resetAt = Date.parse(iso);
  if (!Number.isFinite(resetAt)) return "";

  const minutes = Math.round((resetAt - now.getTime()) / 60000);
  if (minutes <= 0) return t("reset.awaitingRefresh", language);
  return formatDuration(minutes, language);
}

function limitModel(labelKey, limit, settings, now, language, tool, secondary = false) {
  const used = finiteNumber(limit?.used_percent);
  const displayed = displayPercentFromUsed(used, settings);
  const compactCursor = tool === "cursor" && normalizeBarMode(settings?.bar_mode) === "compact";
  const displayLabel = !compactCursor && labelKey ? t(labelKey, language) : "";
  return {
    text: labelKey ? [displayLabel, percentText(displayed)].filter(Boolean).join(" ") : "",
    number: numberText(displayed),
    percent: displayed,
    reset: shortReset(limit?.resets_at, now, language),
    resetDateOnly: /^(?:\d{4}-\d{2}-\d{2}|\d{2}-\d{2})$/.test(String(limit?.resets_at ?? "")),
    color: colorForToolPercent(used, tool, settings, secondary),
    arc: arcText(displayed),
    dash: dashText(displayed),
    labelKey,
    visible: labelKey != null,
  };
}

function grokLimitLabel(limit) {
  const label = String(limit?.label ?? "").toLowerCase();
  if (label === "month" || label === "monthly") return "limit.monthly";
  if (label === "week" || label === "weekly") return "limit.weekly";
  return "limit.usage";
}

function limitLabelKeys(tool, status) {
  if (tool === "grok") return [grokLimitLabel(status?.primary), null];
  if (tool === "cursor") return ["limit.cursorModels", "limit.otherModels"];
  if (tool === "codex" && status) {
    return [
      status.primary == null ? null : "limit.fiveHour",
      status.secondary == null ? null : "limit.weekly",
    ];
  }
  return ["limit.fiveHour", "limit.weekly"];
}

function rgbSetting(value, fallback) {
  if (!Array.isArray(value) || value.length !== 3) return fallback;
  const bytes = value.map((part) => Math.round(Number(part)));
  if (bytes.some((part) => !Number.isFinite(part))) return fallback;
  return `#${bytes
    .map((part) => Math.min(255, Math.max(0, part)).toString(16).padStart(2, "0"))
    .join("")}`;
}

function taskbarTextColor(settings, key, fallback) {
  return rgbSetting(settings?.taskbar_text_colors?.[key], fallback);
}

function taskbarTextColorOn(settings, key) {
  return boolSetting(settings?.taskbar_text_colors?.[`${key}_on`], false);
}

function tooltipResetLine(labelKey, reset, language, dateOnly = false) {
  const label = t(labelKey, language);
  if (!reset) return `${label} –`;
  if (reset === t("reset.awaitingRefresh", language)) return `${label} ${reset}`;
  if (dateOnly) return `${label} · ${t("reset.datePrefix", language)} ${reset}`;
  return `${label} ${t("reset.prefix", language)} ${reset}`;
}

function toolTooltip(label, primary, secondary, language) {
  return [label, primary, secondary]
    .filter((limit) => typeof limit === "string" || limit?.visible)
    .map((limit) => typeof limit === "string"
      ? limit
      : tooltipResetLine(limit.labelKey, limit.reset, language, limit.resetDateOnly))
    .join("\n");
}

function toolAriaLabel(label, primary, secondary, state, language) {
  const parts = [label];
  if (primary.visible) parts.push(primary.text);
  if (secondary.visible) parts.push(secondary.text);
  if (state === "stale") parts.push(t("state.stale", language));
  return parts.join(", ");
}

function worstText(primary, secondary, settings) {
  const values = [primary, secondary]
    .map((limit) => displayPercentFromUsed(limit?.used_percent, settings))
    .filter((value) => value != null);
  if (values.length === 0) return "–";
  const pick = normalizeDisplayBasis(settings?.display_basis) === "used" ? Math.max : Math.min;
  return String(Math.round(pick(...values)));
}

export function barToolViewModel(
  statuses,
  tool,
  settings = DEFAULT_SETTINGS,
  now = new Date(),
  options = {},
) {
  const language = resolveLanguage(settings);
  const status = representativeByTool(statuses)[tool];
  const [primaryLabel, secondaryLabel] = limitLabelKeys(tool, status);
  const primaryUsesSecondaryColor = tool === "grok" && primaryLabel === "limit.monthly";
  const base = {
    tool,
    label: TOOL_LABELS[tool] ?? tool,
    brandColor: toolBrandColor(tool, settings),
  };

  if (options.collectionHealth?.[tool] === "login_required") {
    const primary = limitModel(primaryLabel, null, settings, now, language, tool, false);
    const secondary = limitModel(secondaryLabel, null, settings, now, language, tool, true);
    primary.visible = true;
    secondary.visible = false;
    const loginText = t("state.loginRequired", language);
    return {
      ...base,
      state: "login_required",
      severity: "empty",
      primary,
      secondary,
      worst: "-",
      loginText,
      tooltip: `${base.label}\n${loginText}`,
      ariaLabel: `${base.label}, ${loginText}`,
    };
  }

  if (options.startupLoading === true) {
    const primary = limitModel(
      primaryLabel,
      status?.primary,
      settings,
      now,
      language,
      tool,
      primaryUsesSecondaryColor,
    );
    const secondary = limitModel(
      secondaryLabel,
      status?.secondary,
      settings,
      now,
      language,
      tool,
      true,
    );
    const loadingText = t("state.loading", language);
    return {
      ...base,
      state: "loading",
      severity: "loading",
      primary,
      secondary,
      worst: "…",
      loadingText,
      tooltip: `${base.label}\n${loadingText}`,
      ariaLabel: `${base.label}, ${loadingText}`,
    };
  }

  if (!status) {
    const primary = limitModel(
      primaryLabel,
      null,
      settings,
      now,
      language,
      tool,
      primaryUsesSecondaryColor,
    );
    const secondary = limitModel(secondaryLabel, null, settings, now, language, tool, true);
    const state = "empty";
    return {
      ...base,
      state,
      severity: "empty",
      primary,
      secondary,
      worst: "–",
      tooltip: toolTooltip(base.label, primary, secondary, language),
      ariaLabel: toolAriaLabel(base.label, primary, secondary, state, language),
    };
  }

  const primary = limitModel(
    primaryLabel,
    status.primary,
    settings,
    now,
    language,
    tool,
    primaryUsesSecondaryColor,
  );
  const secondary = limitModel(secondaryLabel, status.secondary, settings, now, language, tool, true);
  const state = status.session?.active === true ? "live" : "stale";
  return {
    ...base,
    state,
    severity: severityForStatus(status, settings),
    primary,
    secondary,
    worst: worstText(status.primary, status.secondary, settings),
    tooltip: toolTooltip(base.label, primary, secondary, language),
    ariaLabel: toolAriaLabel(base.label, primary, secondary, state, language),
  };
}

export function normalizeBarMode(value) {
  return MODES.has(value) ? value : "full";
}

function normalizeIndicatorStyle(value) {
  return INDICATOR_STYLES.has(value) ? value : "ring";
}

function normalizeIndicatorEffectStyle(value) {
  return INDICATOR_EFFECT_STYLES.has(value) ? value : "flat";
}

function normalizeLimitOrder(value) {
  return LIMIT_ORDERS.has(value) ? value : "primary_first";
}

export function barViewModel(
  statuses,
  settings = DEFAULT_SETTINGS,
  now = new Date(),
  options = {},
) {
  const merged = { ...DEFAULT_SETTINGS, ...settings };
  const ringSizePx = numberRangeSetting(merged.ring_size_px, 36, 20, 44);
  const ringThicknessPx = numberRangeSetting(merged.ring_thickness_px, 4, 1, 10);
  const ringGapPx = numberRangeSetting(merged.ring_gap_px, 6, 2, 14);
  const ringCenterSizePx = numberRangeSetting(merged.ring_center_size_px, 16, 4, 32);
  const svgGeometry = ringSvgGeometry(
    ringSizePx,
    ringThicknessPx,
    ringGapPx,
    ringCenterSizePx,
  );

  return {
    mode: normalizeBarMode(merged.bar_mode),
    fullResetTimeOn: boolSetting(merged.full_reset_time_on, true),
    displayBasis: normalizeDisplayBasis(merged.display_basis),
    limitOrder: normalizeLimitOrder(merged.limit_order),
    indicatorStyle: normalizeIndicatorStyle(merged.indicator_style),
    indicatorEffectStyle: normalizeIndicatorEffectStyle(merged.indicator_effect_style),
    indicatorTrackColorAuto: boolSetting(merged.indicator_track_color_auto, true),
    indicatorTrackColor: rgbSetting(merged.indicator_track_color, "#6b7280"),
    indicatorTrackOpacityPercent: numberRangeSetting(
      merged.indicator_track_opacity_percent,
      11,
      0,
      100,
    ),
    claudeTextColor: taskbarTextColor(merged, "claude", "#d79a32"),
    claudeTextColorOn: taskbarTextColorOn(merged, "claude"),
    codexTextColor: taskbarTextColor(merged, "codex", "#2fac7d"),
    codexTextColorOn: taskbarTextColorOn(merged, "codex"),
    grokTextColor: taskbarTextColor(merged, "grok", "#d9578b"),
    grokTextColorOn: taskbarTextColorOn(merged, "grok"),
    cursorTextColor: taskbarTextColor(merged, "cursor", "#72716d"),
    cursorTextColorOn: taskbarTextColorOn(merged, "cursor"),
    infoTextColor: taskbarTextColor(merged, "info", "#6b7280"),
    infoTextColorOn: taskbarTextColorOn(merged, "info"),
    ringTextColor: taskbarTextColor(merged, "ring", "#6b7280"),
    ringTextColorOn: taskbarTextColorOn(merged, "ring"),
    ringOn: merged.ring_on !== false,
    ringNumbersOn: boolSetting(merged.ring_numbers_on, true),
    ringNumberOutlineOn: boolSetting(merged.ring_number_outline_on, true),
    ringNumberOutlineWidthPx: numberRangeSetting(merged.ring_number_outline_width_px, 1.2, 0, 4),
    ringSizePx,
    ringThicknessPx,
    ringGapPx,
    ...svgGeometry,
    ringNumberFontSizePx: numberRangeSetting(merged.ring_number_font_size_px, 9, 6, 16),
    ringNumberFontWeight: intRangeSetting(merged.ring_number_font_weight, 600, 100, 900),
    barTextFontSizePx: numberRangeSetting(merged.bar_text_font_size_px, 11, 8, 16),
    barTextFontWeight: intRangeSetting(merged.bar_text_font_weight, 500, 100, 900),
    barContentGapPx: numberRangeSetting(merged.bar_content_gap_px, 14, 0, 24),
    tools: TOOLS.filter((tool) => toolEnabled(merged, tool)).map((tool) =>
      barToolViewModel(statuses, tool, merged, now, options),
    ),
  };
}
