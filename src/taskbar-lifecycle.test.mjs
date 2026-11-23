import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
const functionBody = (name) => {
  const body = source.match(new RegExp(`fn ${name}\\b[\\s\\S]*?\\n}\\r?\\n`))?.[0];
  assert.ok(body, name);
  return body;
};

test("retiring tooltip registrations cannot close the replacement window menu", () => {
  const sync = functionBody("sync_taskbar_tooltips");
  assert.match(sync, /remove_window_tooltip\(old\)/);
  assert.doesNotMatch(sync, /set_taskbar_menu_state/);
});

test("window retirement, hiding and shutdown still close their menus", () => {
  assert.match(functionBody("request_taskbar_bar_recovery"), /set_taskbar_menu_state\(&recovery_app, tool, false\)/);
  assert.match(functionBody("hide_taskbar_bar"), /set_taskbar_menu_state\(manager, tool, false\)/);
  assert.match(functionBody("spawn_taskbar_drag_loop"), /TaskbarShutdownState[\s\S]*set_taskbar_menu_state\(&app, tool, false\)/);
});

test("an equal-width settings edit preserves its effective content ratio before continuing", () => {
  assert.match(functionBody("preserve_taskbar_leading_edges"), /current_length == requested_length \{[\s\S]*unchanged_taskbar_content_ratio\([\s\S]*set_taskbar_offset_ratio\(requested, tool, ratio\);[\s\S]*continue;/);
});

test("manual and automatic update checks share an authoritative result rather than a busy-cache success", () => {
  const check = source.match(/async fn run_update_check\b[\s\S]*?\n}\r?\n/)?.[0];
  assert.ok(check);
  assert.match(check, /UPDATE_CHECK_COORDINATOR[\s\S]*\.check\(\|\| async/);
  assert.match(check, /try_lock\(\)[\s\S]*map_err/);
  assert.doesNotMatch(check, /try_lock\(\) else[\s\S]*return Ok\(update::cached_result/);
  assert.match(check, /tokio::time::timeout/);
});

test("settings commit and content measurements serialize the complete position transaction", () => {
  const save = functionBody("save_settings");
  const worker = save.slice(save.indexOf("spawn_blocking(move ||"));
  assert.match(worker, /observed_taskbars = taskbar::shell_taskbar_windows\(\)[\s\S]*TASKBAR_PROFILE_GATE[\s\S]*TASKBAR_CONTENT_EDIT_GATE[\s\S]*try_update_taskbar_settings\([\s\S]*preserve_taskbar_leading_edges\([\s\S]*sync_taskbar_content_layout_ratios[\s\S]*Ok::<_, anyhow::Error>/);
  const measurement = functionBody("set_taskbar_content_width");
  assert.match(measurement, /try_taskbar_layout_gate\(&TASKBAR_CONTENT_EDIT_GATE\)[\s\S]*load_settings_with_generation[\s\S]*begin_taskbar_content_layout[\s\S]*apply_taskbar_dock_for_generation[\s\S]*rollback_taskbar_content_layout/);
  assert.doesNotMatch(measurement, /TASKBAR_CONTENT_EDIT_GATE\s*\.lock\(/);
  assert.doesNotMatch(functionBody("preserve_taskbar_leading_edges"), /shell_taskbar_window_for_key|GetDpiForWindow/);
});
