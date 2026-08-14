import assert from "node:assert/strict";
import test from "node:test";

import {
  activityTooltipPosition,
  buildActivityView,
  formatActivityDate,
  formatActivityTokens,
} from "./activity-state.js";

const NOW = new Date(2026, 6, 19, 12, 0, 0);

test("activity view clamps weeks and creates complete Sunday-first columns", () => {
  const minimum = buildActivityView({ days: [] }, { activity_weeks: 1 }, "all", NOW);
  assert.equal(minimum.weeks, 4);
  assert.equal(minimum.cells.length, 28);
  assert.equal(minimum.cells[0].date.getDay(), 0);
  assert.equal(minimum.cells.at(-1).date.getDay(), 6);

  const maximum = buildActivityView({ days: [] }, { activity_weeks: 99 }, "all", NOW);
  assert.equal(maximum.weeks, 52);
  assert.equal(maximum.cells.length, 364);
  assert.ok(maximum.cells.some((cell) => cell.future));
});

test("fixed scale uses the configured token unit without changing totals", () => {
  const days = [1, 100, 101, 400].map((tokens, index) => ({
    date: `2026-07-${String(15 + index).padStart(2, "0")}`,
    claude_tokens: tokens,
    codex_tokens: 0,
  }));
  const view = buildActivityView(
    { days },
    {
      activity_weeks: 4,
      activity_scale_mode: "fixed",
      activity_tokens_per_level: 100,
    },
    "claude",
    NOW,
  );
  const active = view.cells.filter((cell) => cell.tokens > 0);
  assert.deepEqual(active.map((cell) => cell.level), [1, 1, 2, 4]);
  assert.equal(view.totalTokens, 602);
  assert.equal(view.activeDays, 4);
});

test("automatic logarithmic scale preserves visible low-activity days", () => {
  const view = buildActivityView(
    {
      days: [
        { date: "2026-07-17", claude_tokens: 10, codex_tokens: 0 },
        { date: "2026-07-18", claude_tokens: 1_000_000, codex_tokens: 0 },
      ],
    },
    { activity_weeks: 4, activity_scale_mode: "auto" },
    "claude",
    NOW,
  );
  const active = view.cells.filter((cell) => cell.tokens > 0);
  assert.ok(active[0].level >= 1);
  assert.equal(active[1].level, 4);
});

test("filters respect disabled tools while preserving raw tooltip totals", () => {
  const snapshot = {
    partial: true,
    backfill_pending: true,
    days: [{ date: "2026-07-18", claude_tokens: 100, codex_tokens: 300 }],
  };
  const all = buildActivityView(snapshot, { activity_weeks: 4 }, "all", NOW);
  assert.equal(all.totalTokens, 400);
  assert.equal(all.partial, true);
  assert.equal(all.backfillPending, true);

  const codexDisabled = buildActivityView(
    snapshot,
    { activity_weeks: 4, show_codex: false },
    "codex",
    NOW,
  );
  assert.equal(codexDisabled.filter, "all");
  assert.equal(codexDisabled.totalTokens, 100);
  const source = codexDisabled.cells.find((cell) => cell.key === "2026-07-18");
  assert.equal(source.claudeTokens, 100);
  assert.equal(source.codexTokens, 300);
});

test("activity totals and filters include Grok only when it is enabled", () => {
  const snapshot = {
    days: [{
      date: "2026-07-18",
      claude_tokens: 100,
      codex_tokens: 300,
      grok_tokens: 500,
    }],
  };
  const enabled = buildActivityView(
    snapshot,
    { activity_weeks: 4, show_grok: true },
    "grok",
    NOW,
  );
  assert.equal(enabled.filter, "grok");
  assert.equal(enabled.totalTokens, 500);
  assert.equal(enabled.cells.find((cell) => cell.key === "2026-07-18").grokTokens, 500);

  const disabled = buildActivityView(snapshot, { activity_weeks: 4 }, "all", NOW);
  assert.equal(disabled.totalTokens, 400);
});

test("activity formatters return localized compact and exact labels", () => {
  assert.match(formatActivityTokens(1_250_000, "en", true), /1\.3M/);
  assert.equal(formatActivityTokens(1250, "en"), "1,250");
  assert.match(formatActivityDate(new Date(2026, 6, 19), "ko"), /2026/);
});

test("tooltip positioning stays inside the card from edge cells", () => {
  const card = { bottom: 300 };
  const chart = { left: 20, top: 100, width: 260 };
  const tooltip = { width: 100, height: 40 };
  const nearTopRight = activityTooltipPosition(
    card,
    chart,
    { left: 270, top: 110, bottom: 118, width: 8 },
    tooltip,
  );
  const nearBottom = activityTooltipPosition(
    card,
    chart,
    { left: 80, top: 270, bottom: 278, width: 8 },
    tooltip,
  );

  assert.deepEqual(nearTopRight, { left: 152, top: 25 });
  assert.ok(nearBottom.top + tooltip.height <= card.bottom - chart.top - 8);
});
