import { formatDuration, formatLocalDateTime, resolveLanguage, t } from "./i18n.js";

export const DEFAULT_SETTINGS = {
  warn_threshold: 70,
  danger_threshold: 90,
  palette: "Traffic",
  theme: "system",
  language: "system",
  font_mode: "system",
  taskbar_offset_ratio: 0.5,
  claude_taskbar_offset_ratio: 0.5,
  codex_taskbar_offset_ratio: 0.5,
  show_claude: true,
  show_codex: true,
  ring_numbers_on: true,
  ring_number_outline_on: true,
  fullscreen_hide_on: true,
  indicator_style: "ring",
  ring_size_px: 36,
  ring_thickness_px: 4,
  ring_gap_px: 6,
  ring_center_gap_px: 0,
  ring_number_font_size_px: 9,
  ring_number_font_weight: 600,
  bar_text_font_size_px: 11,
  bar_text_font_weight: 500,
};

const RAMP = {
  Traffic: ["#22c55e", "#f59e0b", "#ef4444"],
  Cvd: ["#0072b2", "#e69f00", "#cc79a7"],
  Cool: ["#14b8a6", "#6366f1", "#ec4899"],
};

const UNKNOWN_COLOR = "#9ca3af";

function finiteNumber(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function clampPercent(value) {
  const number = finiteNumber(value);
  return number == null ? null : Math.min(100, Math.max(0, number));
}

function paletteName(value) {
  if (typeof value === "string") {
    const normalized = value.toLowerCase();
    if (normalized === "cvd") return "Cvd";
    if (normalized === "cool") return "Cool";
    return "Traffic";
  }
  return "Traffic";
}

function hexByte(value) {
  const byte = Math.min(255, Math.max(0, Math.round(value)));
  return byte.toString(16).padStart(2, "0");
}

function customRamp(value) {
  const custom = value && typeof value === "object" ? value.Custom : null;
  if (!Array.isArray(custom) || custom.length !== 3) return null;
  return custom.map((rgb) => {
    if (!Array.isArray(rgb) || rgb.length !== 3) return UNKNOWN_COLOR;
    return `#${hexByte(rgb[0])}${hexByte(rgb[1])}${hexByte(rgb[2])}`;
  });
}

export function colorForPercent(percent, settings = DEFAULT_SETTINGS) {
  const set = settings ?? DEFAULT_SETTINGS;
  const value = finiteNumber(percent);
  if (value == null) return UNKNOWN_COLOR;

  const warn = finiteNumber(set.warn_threshold) ?? DEFAULT_SETTINGS.warn_threshold;
  const danger = finiteNumber(set.danger_threshold) ?? DEFAULT_SETTINGS.danger_threshold;
  const ramp =
    customRamp(set.palette) ?? RAMP[paletteName(set.palette)] ?? RAMP.Traffic;
  const index = value >= danger ? 2 : value >= warn ? 1 : 0;
  return ramp[index];
}

function parseTime(value) {
  const time = Date.parse(value);
  return Number.isFinite(time) ? time : null;
}

function isNewer(next, current) {
  const nextTime = parseTime(next?.captured_at);
  const currentTime = parseTime(current?.captured_at);

  if (nextTime != null && currentTime != null) return nextTime > currentTime;
  if (nextTime != null && currentTime == null) return true;
  if (nextTime == null && currentTime != null) return false;
  return String(next?.captured_at ?? "") > String(current?.captured_at ?? "");
}

export function representativeByTool(statuses = []) {
  const representatives = {};

  for (const status of Array.isArray(statuses) ? statuses : []) {
    const tool = typeof status?.tool === "string" ? status.tool.toLowerCase() : "";
    if (!tool) continue;
    if (!representatives[tool] || isNewer(status, representatives[tool])) {
      representatives[tool] = status;
    }
  }

  return representatives;
}

function percentText(value) {
  const percent = finiteNumber(value);
  return percent == null ? "–" : `${Math.round(percent)}%`;
}

function formatReset(iso, now, language) {
  if (!iso) return "";
  const resetAt = parseTime(iso);
  if (resetAt == null) return "";

  const minutes = Math.round((resetAt - now.getTime()) / 60000);
  if (minutes <= 0) return t("reset.past", language);

  return `${t("reset.prefix", language)} ${formatDuration(minutes, language)} (${formatLocalDateTime(resetAt, language)})`;
}

function limitModel(limit, settings, now, language) {
  const value = finiteNumber(limit?.used_percent);
  const clamped = clampPercent(value);

  return {
    value: percentText(value),
    width: `${clamped ?? 0}%`,
    color: colorForPercent(value, settings),
    reset: formatReset(limit?.resets_at, now, language),
  };
}

function emptyHintForTool(tool, language) {
  return tool === "claude" ? t("empty.claude", language) : t("empty.codex", language);
}

export function viewModelForTool(
  statuses,
  tool,
  settings = DEFAULT_SETTINGS,
  now = new Date(),
) {
  const language = resolveLanguage(settings);
  const status = representativeByTool(statuses)[tool];

  if (!status) {
    return {
      active: false,
      exists: false,
      primary: limitModel(null, settings, now, language),
      secondary: limitModel(null, settings, now, language),
      context: `${t("context.label", language)} –`,
      pcId: "",
      meta: "",
      emptyHint: emptyHintForTool(tool, language),
    };
  }

  const active = status.session?.active === true;
  const context = percentText(status.session?.context_used_percent);
  const meta = status.approx === false ? "" : t("meta.approx", language);

  return {
    active,
    exists: true,
    primary: limitModel(status.primary, settings, now, language),
    secondary: limitModel(status.secondary, settings, now, language),
    context: `${t("context.label", language)} ${context}${active ? "" : ` · ${t("state.stale", language)}`}`,
    pcId: status.pc_id ?? "",
    meta,
    emptyHint: "",
  };
}
