import assert from "node:assert/strict";
import test from "node:test";
import { barToolViewModel } from "./bar-state.js";
import { viewModelForTool } from "./panel-state.js";
import { collectionIssue } from "./collection-state.js";
import { t } from "./i18n.js";

const now = new Date("2026-09-28T03:00:00Z");
const status = { tool: "claude", session_id: "claude-desktop-usage", captured_at: now.toISOString(), approx: false,
  session: { active: true }, primary: { used_percent: 30 }, secondary: { used_percent: 22 } };

test("Claude errors are distinct and visible with or without last-good values in every mode", () => {
  const failures = ["rate_limited", "parse_error", "credentials_error", "source_changed", "transient_error", "unavailable"];
  for (const language of ["ko", "en"]) {
    assert.equal(new Set(failures.map(h => collectionIssue("claude", h, language).short)).size, failures.length);
    for (const health of failures) for (const bar_mode of ["full", "compact", "dual", "quad"])
      for (const display_basis of ["used", "remaining"]) for (const hasValue of [false, true]) {
        const settings = { language, bar_mode, display_basis };
        const statuses = hasValue ? [status] : [];
        const collectionHealth = { claude: health };
        const bar = barToolViewModel(statuses, "claude", settings, now, { collectionHealth });
        const panel = viewModelForTool(statuses, "claude", settings, now, collectionHealth);
        const issue = collectionIssue("claude", health, language);
        assert.equal(bar.collectionIssue, issue.short);
        assert.ok(bar.tooltip.includes(issue.detail));
        assert.ok(bar.ariaLabel.includes(issue.short));
        assert.equal(bar.primary.number, hasValue ? display_basis === "used" ? "30" : "70" : "–");
        assert.equal(bar.secondary.number, hasValue ? display_basis === "used" ? "22" : "78" : "–");
        assert.ok((hasValue ? panel.meta : panel.emptyHint).includes(issue.detail));
        if (hasValue) {
          assert.equal(bar.state, "stale");
          assert.ok(panel.meta.includes(t("collection.lastGood", language)));
          assert.ok(bar.tooltip.includes(t("collection.lastGood", language)));
        }
      }
  }
});

test("Claude recovery clears notices, zero stays numeric, and auth failure hides cached quotas", () => {
  const zero = { ...status, primary: { used_percent: 0 } };
  for (const language of ["ko", "en"]) {
    const settings = { language, display_basis: "used" };
    const bar = barToolViewModel([zero], "claude", settings, now, { collectionHealth: { claude: "ready" } });
    assert.equal(bar.primary.number, "0");
    assert.equal(bar.collectionIssue, "");
    assert.equal(bar.state, "live");
    const auth = barToolViewModel([status], "claude", settings, now, { collectionHealth: { claude: "login_required" } });
    assert.equal(auth.state, "login_required");
    assert.equal(auth.primary.number, "–");
    assert.equal(auth.loginText, t("state.loginRequired", language));
    const panel = viewModelForTool([status], "claude", settings, now, { claude: "login_required" });
    assert.equal(panel.exists, false);
  }
  for (const tool of ["codex", "cursor", "grok", "antigravity"]) assert.equal(collectionIssue(tool, "unavailable", "ko"), null);
  assert.equal(collectionIssue("claude", "idle", "ko"), null);
});
