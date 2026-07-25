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

test("Windows taskbar bars are hidden with non-blocking native ShowWindowAsync", () => {
  const hideFn = libSource.match(/fn hide_taskbar_bar[\s\S]*?\n}\n\n#\[cfg\(windows\)\]/)?.[0] ?? "";

  assert.match(taskbarSource, /pub fn hide_window\(/);
  assert.match(taskbarSource, /ShowWindowAsync\(hwnd, SW_HIDE\)/);
  assert.match(taskbarSource, /SW_HIDE/);
  assert.match(hideFn, /taskbar::hide_window/);
});

test("native tracking tooltip applies its final non-activating position after Windows shows it", () => {
  const showTooltipFn =
    taskbarSource.match(
      /fn show_window_tooltip_direct[\s\S]*?\n}\n\npub fn rect_covers_monitor/,
    )?.[0] ?? "";
  const activateIndex = showTooltipFn.indexOf("TTM_TRACKACTIVATE");
  const showIndex = showTooltipFn.indexOf("ShowWindow(tooltip");
  const pumpIndex = showTooltipFn.indexOf("pump_current_thread_messages();", showIndex);
  const measureIndex = showTooltipFn.indexOf("GetWindowRect(tooltip", showIndex);
  const finalAnchorIndex = showTooltipFn.indexOf("taskbar_tooltip_anchor(", measureIndex);
  const finalTrackIndex = showTooltipFn.indexOf("TTM_TRACKPOSITION", finalAnchorIndex);
  const positionIndex = showTooltipFn.indexOf("set_tooltip_window_position(", finalAnchorIndex);

  assert.ok(activateIndex >= 0, "tracking tooltip must be activated");
  assert.ok(showIndex > activateIndex, "tooltip must be shown after tracking activation");
  assert.ok(pumpIndex > showIndex, "native tooltip placement messages must be drained after show");
  assert.ok(measureIndex > pumpIndex, "visible tooltip HWND must provide the final physical size");
  assert.ok(finalAnchorIndex > measureIndex, "final anchor must use the visible HWND size");
  assert.ok(finalTrackIndex > finalAnchorIndex, "tracking state must receive the real-size anchor");
  assert.ok(positionIndex > finalTrackIndex, "final HWND position must follow the tracking update");
  assert.match(taskbarSource, /fn packed_tooltip_track_position[\s\S]*i16::try_from\(x\)/);
  assert.match(
    taskbarSource,
    /fn set_tooltip_window_position[\s\S]*SetWindowPos\([\s\S]*SWP_NOACTIVATE[\s\S]*SWP_NOSIZE/,
  );
  assert.match(taskbarSource, /TOOLTIP_SERVICE_TIMEOUT: Duration = Duration::from_millis\(750\)/);
});

test("visible native tooltip is repositioned after text updates can change its bubble size", () => {
  const updateTooltipFn =
    taskbarSource.match(
      /fn set_window_tooltip_direct[\s\S]*?\n}\n\n#\[cfg\(windows\)\]\npub fn remove_window_tooltip/,
    )?.[0] ?? "";
  const visibilityIndex = updateTooltipFn.indexOf("IsWindowVisible(tooltip)");
  const updateIndex = updateTooltipFn.indexOf("TTM_UPDATETIPTEXTW");
  const repositionIndex = updateTooltipFn.indexOf("show_window_tooltip_direct(parent, true)");

  assert.ok(visibilityIndex >= 0, "tooltip update must preserve its current visibility state");
  assert.ok(updateIndex > visibilityIndex, "tooltip text must update after visibility is sampled");
  assert.ok(repositionIndex > updateIndex, "visible resized tooltip must be anchored again");
  assert.match(updateTooltipFn, /if was_visible/);
});
