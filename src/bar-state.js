import {
  colorForPercent,
  DEFAULT_SETTINGS,
  representativeByTool,
} from "./panel-state.js";
import { formatDuration, resolveLanguage, t } from "./i18n.js";

const TOOL_LABELS = {
  claude: "Claude",
  codex: "Codex",
};

const SECONDARY_RAMP = ["#2563eb", "#7c3aed", "#db2777"];
const MODES = new Set(["full", "compact", "dual", "quad"]);
const TOOLS = ["claude", "codex"];
const INDICATOR_STYLES = new Set(["ring", "bar"]);
const LIMIT_ORDERS = new Set(["primary_first", "secondary_first"]);

function finiteNumber(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function toolEnabled(settings, tool) {
  if (tool === "claude") return settings.show_claude !== false;
  if (tool === "codex") return settings.show_codex !== false;
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

function remainingPercent(limit) {
  const used = finiteNumber(limit?.used_percent);
  return used == null ? null : Math.max(0, Math.min(100, 100 - used));
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

function secondaryColorForPercent(percent, settings) {
  const value = finiteNumber(percent);
  if (value == null) return colorForPercent(percent, settings);

  const warn = finiteNumber(settings.warn_threshold) ?? DEFAULT_SETTINGS.warn_threshold;
  const danger = finiteNumber(settings.danger_threshold) ?? DEFAULT_SETTINGS.danger_threshold;
  if (value >= danger) return SECONDARY_RAMP[2];
  if (value >= warn) return SECONDARY_RAMP[1];
  return SECONDARY_RAMP[0];
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

function ringSvgGeometry(sizePx, thicknessPx, gapPx, centerGapPx) {
  const size = Math.max(1, sizePx);
  const visibleThickness = Math.max(1, thicknessPx - centerGapPx);
  const scale = 100 / size;
  const requestedStroke = visibleThickness * scale;
  const requestedOuterRadius = Math.max(requestedStroke / 2, 50 - requestedStroke / 2);
  const requestedInnerRadius = Math.max(
    requestedStroke / 2,
    50 - gapPx * scale - requestedStroke / 2,
  );
  const roundedStroke = geometryNumber(requestedStroke);
  const roundedOuterRadius = geometryNumber(requestedOuterRadius);
  const roundedInnerRadius = geometryNumber(requestedInnerRadius);
  const requestedGeometryIsValid =
    roundedOuterRadius > roundedInnerRadius &&
    roundedOuterRadius + roundedStroke / 2 <= 50 &&
    roundedInnerRadius - roundedStroke / 2 >= 0 &&
    roundedOuterRadius - roundedInnerRadius >= roundedStroke;

  if (requestedGeometryIsValid) {
    return {
      ringSvgStroke: geometryText(requestedStroke),
      outerRadius: geometryText(requestedOuterRadius),
      innerRadius: geometryText(requestedInnerRadius),
    };
  }

  const stroke = Math.min(24.9, roundedStroke);
  const outerRadius = Math.floor((50 - stroke / 2) * 10) / 10;
  const innerRadius = Math.max(
    Math.ceil((stroke / 2) * 10) / 10,
    Math.min(
      geometryNumber(outerRadius - stroke),
      geometryNumber(50 - gapPx * scale - stroke / 2),
    ),
  );

  return {
    ringSvgStroke: geometryText(stroke),
    outerRadius: geometryText(outerRadius),
    innerRadius: geometryText(innerRadius),
  };
}

function shortReset(iso, now, language) {
  if (!iso) return "";
  const resetAt = Date.parse(iso);
  if (!Number.isFinite(resetAt)) return "";

  const minutes = Math.round((resetAt - now.getTime()) / 60000);
  if (minutes <= 0) return t("reset.past", language);
  return formatDuration(minutes, language);
}

function limitModel(labelKey, limit, settings, now, language, colorForLimit = colorForPercent) {
  const used = finiteNumber(limit?.used_percent);
  const remaining = remainingPercent(limit);
  return {
    text: `${t(labelKey, language)} ${percentText(remaining)}`,
    number: numberText(remaining),
    percent: remaining,
    reset: shortReset(limit?.resets_at, now, language),
    color: colorForLimit(used, settings),
    arc: arcText(remaining),
    dash: dashText(remaining),
  };
}

function worstText(primary, secondary) {
  const values = [remainingPercent(primary), remainingPercent(secondary)].filter(
    (value) => value != null,
  );
  return values.length === 0 ? "–" : String(Math.round(Math.min(...values)));
}

export function barToolViewModel(
  statuses,
  tool,
  settings = DEFAULT_SETTINGS,
  now = new Date(),
) {
  const language = resolveLanguage(settings);
  const status = representativeByTool(statuses)[tool];
  const base = {
    tool,
    label: TOOL_LABELS[tool] ?? tool,
  };

  if (!status) {
    return {
      ...base,
      state: "empty",
      severity: "empty",
      primary: limitModel("limit.fiveHour", null, settings, now, language),
      secondary: limitModel("limit.weekly", null, settings, now, language, secondaryColorForPercent),
      worst: "–",
    };
  }

  return {
    ...base,
    state: status.session?.active === true ? "live" : "stale",
    severity: severityForStatus(status, settings),
    primary: limitModel("limit.fiveHour", status.primary, settings, now, language),
    secondary: limitModel("limit.weekly", status.secondary, settings, now, language, secondaryColorForPercent),
    worst: worstText(status.primary, status.secondary),
  };
}

export function normalizeBarMode(value) {
  return MODES.has(value) ? value : "full";
}

function normalizeIndicatorStyle(value) {
  return INDICATOR_STYLES.has(value) ? value : "ring";
}

function normalizeLimitOrder(value) {
  return LIMIT_ORDERS.has(value) ? value : "primary_first";
}

export function barViewModel(statuses, settings = DEFAULT_SETTINGS, now = new Date()) {
  const merged = { ...DEFAULT_SETTINGS, ...settings };
  const ringSizePx = numberRangeSetting(merged.ring_size_px, 36, 20, 44);
  const ringThicknessPx = numberRangeSetting(merged.ring_thickness_px, 4, 1, 10);
  const ringGapPx = numberRangeSetting(merged.ring_gap_px, 6, 2, 14);
  const ringCenterGapPx = numberRangeSetting(merged.ring_center_gap_px, 0, 0, 8);
  const svgGeometry = ringSvgGeometry(
    ringSizePx,
    ringThicknessPx,
    ringGapPx,
    ringCenterGapPx,
  );

  return {
    mode: normalizeBarMode(merged.bar_mode),
    limitOrder: normalizeLimitOrder(merged.limit_order),
    indicatorStyle: normalizeIndicatorStyle(merged.indicator_style),
    ringOn: merged.ring_on !== false,
    ringNumbersOn: boolSetting(merged.ring_numbers_on, true),
    ringNumberOutlineOn: boolSetting(merged.ring_number_outline_on, true),
    ringNumberOutlineWidthPx: numberRangeSetting(merged.ring_number_outline_width_px, 1.2, 0, 4),
    ringSizePx,
    ringThicknessPx,
    ringGapPx,
    ringCenterGapPx,
    ...svgGeometry,
    ringNumberFontSizePx: numberRangeSetting(merged.ring_number_font_size_px, 9, 6, 16),
    ringNumberFontWeight: intRangeSetting(merged.ring_number_font_weight, 600, 100, 900),
    barTextFontSizePx: numberRangeSetting(merged.bar_text_font_size_px, 11, 8, 16),
    barTextFontWeight: intRangeSetting(merged.bar_text_font_weight, 500, 100, 900),
    tools: TOOLS.filter((tool) => toolEnabled(merged, tool)).map((tool) =>
      barToolViewModel(statuses, tool, merged, now),
    ),
  };
}
