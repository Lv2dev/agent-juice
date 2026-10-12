import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { formStateFromSettings, payloadFromEntries } from "./settings-state.js";
import { viewModelForTool } from "./panel-state.js";

const retiredFields = [
  "claude_account_auto_collect_on",
  "claude_usage_auto_refresh_lab_on",
];

test("Claude collection has no separate checkbox or serialized preference", () => {
  const markup = readFileSync(new URL("./index.html", import.meta.url), "utf8");
  for (const field of retiredFields) {
    assert.ok(!markup.includes(`name="${field}"`));
    const state = formStateFromSettings({ show_claude: true, [field]: false });
    assert.ok(!Object.hasOwn(state, "claudeAccountAutoCollectOn"));
    const payload = payloadFromEntries({ show_claude: "on", [field]: "on" });
    assert.equal(payload.show_claude, true);
    assert.ok(!Object.hasOwn(payload, field));
  }
  assert.equal(payloadFromEntries({}).show_claude, false);
});

test("Claude empty state always describes automatic collection, including legacy opt-outs", () => {
  for (const language of ["ko", "en"]) {
    for (const legacy of [{}, ...retiredFields.map((field) => ({ [field]: false }))]) {
      const model = viewModelForTool([], "claude", { language, ...legacy });
      assert.equal(model.emptyHint, language === "ko"
        ? "Claude 계정 사용량을 수집 중입니다. 채팅을 보낼 필요가 없습니다."
        : "Collecting Claude account usage. No chat message is required.");
      const unauthorized = viewModelForTool([], "claude", { language, ...legacy },
        new Date(), { claude: "login_required" });
      assert.equal(unauthorized.state, "login_required");
      assert.notEqual(unauthorized.emptyHint, model.emptyHint);
    }
  }
});
