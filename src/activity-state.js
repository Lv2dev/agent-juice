const DAY_MS = 24 * 60 * 60 * 1000;
const MIN_WEEKS = 4;
const MAX_WEEKS = 52;
const DEFAULT_WEEKS = 52;
const DEFAULT_TOKENS_PER_LEVEL = 250_000;

function finiteInteger(value, fallback = 0) {
  const number = Number(value);
  if (!Number.isFinite(number)) return fallback;
  return Math.min(Number.MAX_SAFE_INTEGER, Math.max(0, Math.round(number)));
}

function clampWeeks(value) {
  return Math.min(MAX_WEEKS, Math.max(MIN_WEEKS, finiteInteger(value, DEFAULT_WEEKS)));
}

function localDate(value = new Date()) {
  const date = value instanceof Date ? value : new Date(value);
  return new Date(date.getFullYear(), date.getMonth(), date.getDate());
}

function addDays(date, count) {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate() + count);
}

function dateKey(date) {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function tokensForFilter(day, filter, settings) {
  const claude = settings?.show_claude === false ? 0 : finiteInteger(day?.claude_tokens);
  const codex = settings?.show_codex === false ? 0 : finiteInteger(day?.codex_tokens);
  if (filter === "claude") return claude;
  if (filter === "codex") return codex;
  return Math.min(Number.MAX_SAFE_INTEGER, claude + codex);
}

function normalizedFilter(value, settings) {
  const filter = ["claude", "codex"].includes(value) ? value : "all";
  if (filter === "claude" && settings?.show_claude === false) return "all";
  if (filter === "codex" && settings?.show_codex === false) return "all";
  return filter;
}

function fixedLevel(tokens, unit) {
  if (tokens <= 0) return 0;
  return Math.min(4, Math.max(1, Math.ceil(tokens / unit)));
}

function autoLevel(tokens, maximum) {
  if (tokens <= 0 || maximum <= 0) return 0;
  const ratio = Math.log1p(tokens) / Math.log1p(maximum);
  return Math.min(4, Math.max(1, Math.ceil(ratio * 4)));
}

function monthLabels(cells, weeks) {
  const labels = [];
  let previousMonth = -1;
  for (let column = 0; column < weeks; column += 1) {
    const week = cells.slice(column * 7, column * 7 + 7);
    const firstOfMonth = week.find((cell) => cell.date.getDate() === 1);
    const representative = firstOfMonth ?? week[0];
    const month = representative.date.getMonth();
    if (column === 0 || firstOfMonth) {
      if (month !== previousMonth) labels.push({ column, date: representative.date });
      previousMonth = month;
    }
  }
  return labels;
}

export function buildActivityView(
  snapshot,
  settings = {},
  requestedFilter = "all",
  now = new Date(),
) {
  const weeks = clampWeeks(settings?.activity_weeks);
  const filter = normalizedFilter(requestedFilter, settings);
  const today = localDate(now);
  const currentWeekStart = addDays(today, -today.getDay());
  const start = addDays(currentWeekStart, -(weeks - 1) * 7);
  const byDate = new Map();

  for (const day of Array.isArray(snapshot?.days) ? snapshot.days : []) {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(String(day?.date ?? ""))) continue;
    byDate.set(day.date, day);
  }

  const cells = [];
  for (let index = 0; index < weeks * 7; index += 1) {
    const date = addDays(start, index);
    const key = dateKey(date);
    const source = byDate.get(key);
    cells.push({
      date,
      key,
      future: date > today,
      claudeTokens: finiteInteger(source?.claude_tokens),
      codexTokens: finiteInteger(source?.codex_tokens),
      tokens: tokensForFilter(source, filter, settings),
      level: 0,
    });
  }

  const visibleCells = cells.filter((cell) => !cell.future);
  const maximum = visibleCells.reduce((value, cell) => Math.max(value, cell.tokens), 0);
  const fixed = String(settings?.activity_scale_mode).toLowerCase() === "fixed";
  const unit = Math.min(
    1_000_000_000_000,
    Math.max(1, finiteInteger(settings?.activity_tokens_per_level, DEFAULT_TOKENS_PER_LEVEL)),
  );
  for (const cell of visibleCells) {
    cell.level = fixed ? fixedLevel(cell.tokens, unit) : autoLevel(cell.tokens, maximum);
  }

  return {
    weeks,
    filter,
    cells,
    monthLabels: monthLabels(cells, weeks),
    totalTokens: visibleCells.reduce(
      (total, cell) => Math.min(Number.MAX_SAFE_INTEGER, total + cell.tokens),
      0,
    ),
    activeDays: visibleCells.filter((cell) => cell.tokens > 0).length,
    partial: snapshot?.partial === true,
    backfillPending: snapshot?.backfill_pending === true,
    empty: visibleCells.every((cell) => cell.tokens === 0),
    scaleMode: fixed ? "fixed" : "auto",
    tokensPerLevel: unit,
  };
}

export function formatActivityTokens(value, language = "ko", compact = false) {
  const locale = language === "en" ? "en-US" : "ko-KR";
  return new Intl.NumberFormat(locale, compact
    ? { notation: "compact", maximumFractionDigits: 1 }
    : { maximumFractionDigits: 0 }).format(finiteInteger(value));
}

export function formatActivityDate(value, language = "ko") {
  const locale = language === "en" ? "en-US" : "ko-KR";
  return new Intl.DateTimeFormat(locale, {
    year: "numeric",
    month: "short",
    day: "numeric",
  }).format(value);
}

export function formatActivityMonth(value, language = "ko") {
  const locale = language === "en" ? "en-US" : "ko-KR";
  return new Intl.DateTimeFormat(locale, { month: "short" }).format(value);
}

export function activityTooltipPosition(cardRect, chartRect, cellRect, tooltipRect) {
  const padding = 8;
  const gap = 7;
  const preferredLeft = cellRect.left - chartRect.left + cellRect.width / 2 - tooltipRect.width / 2;
  const maxLeft = Math.max(padding, chartRect.width - tooltipRect.width - padding);
  const left = Math.min(maxLeft, Math.max(padding, preferredLeft));
  const above = cellRect.top - chartRect.top - tooltipRect.height - gap;
  const below = cellRect.bottom - chartRect.top + gap;
  const maxTop = Math.max(
    0,
    cardRect.bottom - chartRect.top - tooltipRect.height - padding,
  );
  const top = Math.min(maxTop, Math.max(0, above >= 0 ? above : below));
  return { left, top };
}
