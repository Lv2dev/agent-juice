export const DEFAULT_SETTINGS = {
  warn_threshold: 70,
  danger_threshold: 90,
  palette: "Traffic",
  theme: "system",
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

function formatReset(iso, now) {
  if (!iso) return "";
  const resetAt = parseTime(iso);
  if (resetAt == null) return "";

  const minutes = Math.round((resetAt - now.getTime()) / 60000);
  if (minutes <= 0) return "리셋 지남";

  const days = Math.floor(minutes / 1440);
  const hours = Math.floor((minutes % 1440) / 60);
  const mins = minutes % 60;
  const relative =
    days > 0
      ? `${days}일 ${hours}시간`
      : `${hours > 0 ? `${hours}시간 ` : ""}${mins}분`;
  return `리셋 ${relative} (${new Date(resetAt).toLocaleString("ko-KR")})`;
}

function limitModel(limit, settings, now) {
  const value = finiteNumber(limit?.used_percent);
  const clamped = clampPercent(value);

  return {
    value: percentText(value),
    width: `${clamped ?? 0}%`,
    color: colorForPercent(value, settings),
    reset: formatReset(limit?.resets_at, now),
  };
}

function formatCost(status) {
  if (finiteNumber(status?.cost_estimate_usd) == null) return "추정 비용";
  return `추정 비용 $${status.cost_estimate_usd.toFixed(2)}`;
}

export function viewModelForTool(
  statuses,
  tool,
  settings = DEFAULT_SETTINGS,
  now = new Date(),
) {
  const status = representativeByTool(statuses)[tool];

  if (!status) {
    return {
      active: false,
      exists: false,
      primary: limitModel(null, settings, now),
      secondary: limitModel(null, settings, now),
      context: "컨텍스트 –",
      pcId: "",
      meta: "추정 비용 · 근사치",
    };
  }

  const active = status.session?.active === true;
  const context = percentText(status.session?.context_used_percent);
  const approx = status.approx === false ? "" : " · 근사치";

  return {
    active,
    exists: true,
    primary: limitModel(status.primary, settings, now),
    secondary: limitModel(status.secondary, settings, now),
    context: `컨텍스트 ${context}${active ? "" : " · 오래됨"}`,
    pcId: status.pc_id ?? "",
    meta: `${formatCost(status)}${approx}`,
  };
}
