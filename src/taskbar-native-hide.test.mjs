import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const libSource = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8").replace(
  /\r\n?/g,
  "\n",
);
const taskbarSource = readFileSync(
  new URL("../src-tauri/src/taskbar.rs", import.meta.url),
  "utf8",
).replace(/\r\n?/g, "\n");

test("Windows taskbar bars are hidden with native ShowWindow", () => {
  const hideFn = libSource.match(/fn hide_taskbar_bar[\s\S]*?\n}\n\n#\[cfg\(windows\)\]/)?.[0] ?? "";

  assert.match(taskbarSource, /pub fn hide_window\(/);
  assert.match(taskbarSource, /ShowWindow/);
  assert.match(taskbarSource, /SW_HIDE/);
  assert.match(hideFn, /taskbar::hide_window/);
});
