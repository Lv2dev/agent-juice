import assert from "node:assert/strict";
import test from "node:test";
import { barToolViewModel } from "./bar-state.js";
import { formatLocalDateTime } from "./i18n.js";

const now = new Date("2026-09-14T03:00:00Z");
const sample = {
  tool: "codex", pc_id: "DESKTOP", captured_at: "2026-09-14T02:58:00Z", approx: false,
  primary: { used_percent: 28, resets_at: "2026-09-14T05:15:00Z" },
  secondary: { used_percent: 54, resets_at: "2026-09-18T03:00:00Z" },
  session: { active: true, context_used_percent: 13 },
  session_id: "private-session", cost_estimate_usd: 123.45,
};
const view = (status = sample, settings = {}, at = now, options = {}) => barToolViewModel(
  status ? [status] : [], status?.tool ?? "codex", { language: "en", display_basis: "remaining", ...settings }, at, options,
);

test("tooltip contains paired percentages, reset timestamps and record metadata", () => {
  assert.equal(view().tooltip, [
    "Codex · Ready", "5h · Remaining 72% · Used 28%",
    `Resets: 2h 15m (${formatLocalDateTime(sample.primary.resets_at, "en")})`,
    "Weekly · Remaining 46% · Used 54%",
    `Resets: 4d 0h (${formatLocalDateTime(sample.secondary.resets_at, "en")})`,
    `Last record: ${formatLocalDateTime(sample.captured_at, "en")} (2m ago)`,
    "Data: Exact", "PC: DESKTOP",
  ].join("\n"));
  assert.doesNotMatch(view().tooltip, /private-session|123\.45|context|Context/);
});

test("tooltip detail is independent of mode and indicator for all tools and languages", () => {
  for (const language of ["ko", "en"]) for (const tool of ["claude", "codex", "grok", "cursor"]) {
    const status = { ...sample, tool, primary: { ...sample.primary, label: tool === "grok" ? "month" : "5h" }, secondary: tool === "grok" ? null : sample.secondary };
    const reference = view(status, { language }).tooltip;
    for (const bar_mode of ["full", "compact", "dual", "quad"]) for (const indicator_style of ["ring", "bar"]) {
      assert.equal(view(status, { language, bar_mode, indicator_style }).tooltip, reference);
    }
    if (tool === "grok") assert.match(reference, language === "ko" ? /월간/ : /Monthly/);
    if (tool === "cursor") assert.match(reference, language === "ko" ? /Cursor 모델[\s\S]*기타 모델/ : /Cursor Models[\s\S]*Other Models/);
  }
});

test("tooltip leading percentage matches the selected gauge and complements to 100", () => {
  for (const used_percent of [0, 12.5, 50.5, 99.5, 100, null, NaN]) for (const display_basis of ["remaining", "used"]) {
    const vm = view({ ...sample, primary: { ...sample.primary, used_percent }, secondary: null }, { display_basis });
    const line = vm.tooltip.split("\n")[1];
    const first = display_basis === "used" ? "Used" : "Remaining";
    const expected = vm.primary.percent == null ? "–" : `${Math.round(vm.primary.percent)}%`;
    assert.ok(line.startsWith(`5h · ${first} ${expected}`));
    const numbers = [...line.matchAll(/(\d+)%/g)].map(match => Number(match[1]));
    if (numbers.length) assert.equal(numbers[0] + numbers[1], 100);
  }
});

test("tooltip omits missing periods and private data while authentication is unavailable", () => {
  const weekly = view({ ...sample, primary: null });
  assert.doesNotMatch(weekly.tooltip, /5h/);
  assert.match(weekly.tooltip, /Weekly · Remaining/);
  assert.doesNotMatch(view({ ...sample, primary: null, secondary: null }).tooltip, /5h|Weekly|Resets/);
  assert.equal(view(null).tooltip, "Codex · No records");
  for (const options of [{ startupLoading: true }, { collectionHealth: { codex: "login_required" } }]) {
    const hidden = view(sample, {}, now, options).tooltip;
    assert.doesNotMatch(hidden, /72%|DESKTOP|Last record|Exact/);
    assert.equal(hidden.split("\n").length, 2);
  }
  assert.match(view().tooltip, /72%/);
});

test("tooltip keeps date-only precision and refuses invalid reset dates", () => {
  const firstLine = resets_at => view({ ...sample, primary: { used_percent: 28, resets_at }, secondary: null }).tooltip.split("\n")[2];
  assert.equal(firstLine("09-21"), "Resets: Sep 21");
  assert.equal(firstLine("2026-09-21"), "Resets: Sep 21, 2026");
  for (const date of [null, "garbage", "02-30", "2026-02-30", "2026-02-30T00:00:00Z", "2026-09-21T25:00:00Z"]) {
    assert.equal(firstLine(date), "Resets: –");
  }
  assert.match(firstLine("2026-09-14T02:59:59Z"), /^Resets: Waiting for refresh \(/);
  assert.match(firstLine("2026-09-14T03:00:01Z"), /^Resets: 1m \(/);
  assert.equal(view({ ...sample, primary: { ...sample.primary, resets_at: "2026-09-14T03:00:01Z" } }).primary.reset, "1m");
  assert.equal(firstLine("2026-09-14T05:15:00Z"), firstLine("2026-09-14T14:15:00+09:00"));
});

test("tooltip marks record age and approximation without claiming a new collection", () => {
  assert.match(view({ ...sample, session: { active: false }, approx: true }).tooltip, /^Codex · stale[\s\S]*Data: approximate/);
  assert.match(view(sample, {}, new Date(now.getTime() + 120000)).tooltip, /\(4m ago\)/);
  assert.match(view({ ...sample, captured_at: now.toISOString() }).tooltip, /\(just now\)/);
  assert.match(view({ ...sample, captured_at: "2026-09-15T00:00:00Z" }).tooltip, /check device clock/);
  assert.match(view({ ...sample, captured_at: "invalid" }).tooltip, /Last record: –/);
  assert.match(view({ ...sample, primary: { used_percent: 91 }, secondary: null }).tooltip, /^Codex · Danger/);
});

test("stale stays in tooltip and accessibility copy for every tool and mode", () => {
  for (const tool of ["claude", "codex", "grok", "cursor", "antigravity"]) {
    for (const language of ["ko", "en"]) for (const bar_mode of ["full", "compact", "dual", "quad"]) {
      const vm = view({ ...sample, tool, session: { active: false } }, { language, bar_mode });
      assert.equal(vm.state, "stale");
      assert.match(vm.tooltip, language === "ko" ? /오래됨/ : /stale/);
      assert.match(vm.ariaLabel, language === "ko" ? /오래됨/ : /stale/);
      assert.match(vm.tooltip, language === "ko" ? /마지막 기록/ : /Last record/);
    }
  }
});

test("tooltip sanitizes and bounds display metadata within the native limit", () => {
  for (const language of ["ko", "en"]) {
    const text = view({ ...sample, pc_id: "PC\nInjected\0\u202e" + "🧪".repeat(10000) }, { language }).tooltip;
    assert.ok(Array.from(text).length <= 512);
    assert.doesNotMatch(text, /[\0\u202e]/);
    assert.equal(text.split("\n").length, 8);
    assert.ok(Array.from(text.split("PC: ")[1]).length <= 40);
    assert.match(text, language === "ko" ? /마지막 기록/ : /Last record/);
  }
});
