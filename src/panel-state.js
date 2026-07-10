import { formatDuration, formatLocalDateTime, resolveLanguage, t } from "./i18n.js";

export const DEFAULT_SETTINGS = {
  warn_threshold: 70,
  danger_threshold: 90,
  display_basis: "remaining",
  poll_interval_secs: 60,
  palette: "Traffic",
  theme: "system",
  language: "system",
  font_mode: "system",
  taskbar_offset_ratio: 0.5,
  claude_taskbar_offset_ratio: 0.5,
  codex_taskbar_offset_ratio: 0.5,
  show_claude: true,
  show_codex: true,
  claude_account_auto_collect_on: true,
  ring_numbers_on: true,
  ring_number_outline_on: true,
  ring_number_outline_width_px: 1.2,
  fullscreen_hide_on: true,
  indicator_style: "ring",
  indicator_effect_style: "flat",
  ring_size_px: 36,
  ring_thickness_px: 4,
  ring_gap_px: 6,
  ring_center_size_px: 16,
  ring_number_font_size_px: 9,
  ring_number_font_weight: 600,
  bar_text_font_size_px: 11,
  bar_text_font_weight: 500,
  update_check_on: true,
};

const RAMP = {
  Traffic: ["#22c55e", "#f59e0b", "#ef4444"],
  Signal: ["#22c55e", "#f59e0b", "#ef4444"],
  Cvd: ["#0072b2", "#e69f00", "#cc79a7"],
  Cool: ["#14b8a6", "#6366f1", "#ec4899"],
  Ocean: ["#0f9fb5", "#377bd3", "#6d5bd0"],
  Forest: ["#4f8a64", "#b18432", "#c6535d"],
  Sunset: ["#d9823d", "#d2576f", "#9658b3"],
};

const SECONDARY_RAMP = {
  Traffic: ["#2563eb", "#7c3aed", "#db2777"],
  Signal: ["#14b8a6", "#f97316", "#e11d48"],
  Cvd: ["#56b4e9", "#d55e00", "#882255"],
  Cool: ["#38bdf8", "#8b5cf6", "#f43f5e"],
  Ocean: ["#2db8a8", "#4f68c7", "#9256b5"],
  Forest: ["#75974b", "#c26f3f", "#a94e75"],
  Sunset: ["#e2a03d", "#c94b54", "#6f59b6"],
};

const UNKNOWN_COLOR = "#9ca3af";
const TOOL_SAFE = {
  claude: ["#d79a32", "#d36b86"],
  codex: ["#2fac7d", "#4d86d6"],
};

function rgbColor(value, fallback) {
  if (!Array.isArray(value) || value.length !== 3) return fallback;
  return `#${hexByte(value[0])}${hexByte(value[1])}${hexByte(value[2])}`;
}

function configuredToolColor(settings, tool, secondary) {
  const fallback = TOOL_SAFE[tool]?.[secondary ? 1 : 0] ?? UNKNOWN_COLOR;
  const key = `${tool}_${secondary ? "secondary" : "primary"}`;
  return rgbColor(settings?.tool_colors?.[key], fallback);
}

export function toolBrandColor(tool, settings = DEFAULT_SETTINGS) {
  return configuredToolColor(settings ?? DEFAULT_SETTINGS, tool, false);
}

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
    if (normalized === "signal") return "Signal";
    if (normalized === "ocean") return "Ocean";
    if (normalized === "forest") return "Forest";
    if (normalized === "sunset") return "Sunset";
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

function monoRamp(value) {
  const mono = value && typeof value === "object" ? value.Mono : null;
  if (!Array.isArray(mono) || mono.length !== 3) return null;
  const base = `#${hexByte(mono[0])}${hexByte(mono[1])}${hexByte(mono[2])}`;
  return [base, "#f59e0b", "#ef4444"];
}

function paletteRamps(value) {
  const mono = monoRamp(value);
  if (mono) return { primary: mono, secondary: mono, unified: true };

  const custom = customRamp(value);
  if (custom) return { primary: custom, secondary: custom, unified: true };

  const name = paletteName(value);
  return {
    primary: RAMP[name] ?? RAMP.Traffic,
    secondary: SECONDARY_RAMP[name] ?? SECONDARY_RAMP.Traffic,
    unified: false,
    name,
  };
}

export function colorForPercent(percent, settings = DEFAULT_SETTINGS) {
  const set = settings ?? DEFAULT_SETTINGS;
  const value = finiteNumber(percent);
  if (value == null) return UNKNOWN_COLOR;

  const warn = finiteNumber(set.warn_threshold) ?? DEFAULT_SETTINGS.warn_threshold;
  const danger = finiteNumber(set.danger_threshold) ?? DEFAULT_SETTINGS.danger_threshold;
  const ramp = paletteRamps(set.palette).primary;
  const index = value >= danger ? 2 : value >= warn ? 1 : 0;
  return ramp[index];
}

export function normalizeDisplayBasis(value) {
  return String(value || "remaining").toLowerCase() === "used" ? "used" : "remaining";
}

export function displayPercentFromUsed(usedPercent, settings = DEFAULT_SETTINGS) {
  const used = clampPercent(usedPercent);
  if (used == null) return null;
  return normalizeDisplayBasis(settings?.display_basis) === "used" ? used : 100 - used;
}

export function colorForToolPercent(
  percent,
  tool,
  settings = DEFAULT_SETTINGS,
  secondary = false,
) {
  const set = settings ?? DEFAULT_SETTINGS;
  const value = finiteNumber(percent);
  if (value == null) return UNKNOWN_COLOR;

  const warn = finiteNumber(set.warn_threshold) ?? DEFAULT_SETTINGS.warn_threshold;
  const danger = finiteNumber(set.danger_threshold) ?? DEFAULT_SETTINGS.danger_threshold;
  const ramps = paletteRamps(set.palette);
  const index = value >= danger ? 2 : value >= warn ? 1 : 0;

  if (index === 0 && ramps.name === "Traffic" && TOOL_SAFE[tool]) {
    return configuredToolColor(set, tool, secondary);
  }
  return (secondary ? ramps.secondary : ramps.primary)[index];
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

function limitModel(limit, settings, now, language, tool, secondary = false) {
  const used = finiteNumber(limit?.used_percent);
  const displayed = displayPercentFromUsed(used, settings);

  return {
    value: percentText(displayed),
    width: `${displayed ?? 0}%`,
    color: colorForToolPercent(used, tool, settings, secondary),
    reset: formatReset(limit?.resets_at, now, language),
  };
}

function emptyHintForTool(tool, settings, language) {
  if (
    tool === "claude" &&
    (settings?.claude_account_auto_collect_on === true ||
      settings?.claude_usage_auto_refresh_lab_on === true)
  ) {
    return t("empty.claudeCollect", language);
  }
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
      brandColor: toolBrandColor(tool, settings),
      primary: limitModel(null, settings, now, language, tool),
      secondary: limitModel(null, settings, now, language, tool, true),
      context: `${t("context.label", language)} –`,
      pcId: "",
      meta: "",
      emptyHint: emptyHintForTool(tool, settings, language),
    };
  }

  const active = status.session?.active === true;
  const context = percentText(status.session?.context_used_percent);
  const meta = status.approx === false ? "" : t("meta.approx", language);

  return {
    active,
    exists: true,
    brandColor: toolBrandColor(tool, settings),
    primary: limitModel(status.primary, settings, now, language, tool),
    secondary: limitModel(status.secondary, settings, now, language, tool, true),
    context: `${t("context.label", language)} ${context}${active ? "" : ` · ${t("state.stale", language)}`}`,
    pcId: status.pc_id ?? "",
    meta,
    emptyHint: "",
  };
}
