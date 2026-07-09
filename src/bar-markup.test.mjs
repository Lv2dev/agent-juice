import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const markup = readFileSync(resolve(here, "bar.html"), "utf8").replace(/\r\n?/g, "\n");

test("bar markup stays presentation-only without logo or button semantics", () => {
  assert.doesNotMatch(markup, /class="bar-brand"/);
  assert.doesNotMatch(markup, />\s*Juice\s*</);
  assert.doesNotMatch(markup, /role="button"/);
  assert.doesNotMatch(markup, /tabindex="0"/);
  assert.doesNotMatch(markup, /aria-label="Juice 패널 열기"/);
});

test("bar markup includes quad mode single-ring slots for each tool limit", () => {
  const quadSlots = markup.match(/class="bar-quad"/g) ?? [];
  const primarySlots = markup.match(/class="quad-ring quad-primary"/g) ?? [];
  const secondarySlots = markup.match(/class="quad-ring quad-secondary"/g) ?? [];

  assert.equal(quadSlots.length, 2);
  assert.equal(primarySlots.length, 2);
  assert.equal(secondarySlots.length, 2);
});
