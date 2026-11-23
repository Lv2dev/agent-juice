import assert from 'node:assert/strict';
import test from 'node:test';
import {readFileSync} from 'node:fs';

const source = readFileSync(new URL('../src-tauri/src/lib.rs', import.meta.url), 'utf8');
const body = name => source.match(new RegExp(`(?:async )?fn ${name}\\b[\\s\\S]*?\\n}\\r?\\n`))?.[0] ?? '';

test('menu publications version every value and all getters use slot snapshots', () => {
  assert.match(source, /struct TaskbarMenuSlot\s*\{\s*revision: u64,\s*layout: TaskbarMenuLayout/);
  const write = body('set_taskbar_menu_layout_in_slot');
  assert.match(write, /slot.lock\(\)[\s\S]*?write_taskbar_menu_layout\(&mut current, open, ratio\)/);
  assert.match(body('write_taskbar_menu_layout'), /current.revision = current.revision.wrapping_add\(1\);[\s\S]*?current.layout =/);
  assert.match(body('set_taskbar_menu_state'), /set_taskbar_menu_layout\(/);
  assert.match(body('set_taskbar_menu_layout'), /set_taskbar_menu_layout_in_slot\(/);
  for (const name of ['taskbar_menu_is_open', 'taskbar_layout_ratio']) {
    assert.match(body(name), /taskbar_menu_layout_snapshot/);
  }
});

test('menu generation admission precedes mutation and IPC errors clean only their token', () => {
  const publish = body('set_taskbar_menu_layout_for_generation');
  assert.match(publish, /with_taskbar_settings_read[\s\S]*?generation == expected_generation[\s\S]*?try_taskbar_layout_gate[\s\S]*?taskbar_window_handle[\s\S]*?set_taskbar_menu_layout_for_request_in_slot/);
  const command = body('set_taskbar_menu_open');
  assert.match(command, /let write =\s*set_taskbar_menu_layout_for_generation\([\s\S]*?origin,\s*sequence,[\s\S]*?apply_taskbar_dock_for_generation[\s\S]*?close_taskbar_menu_layout_if_current\(&app, &tool, write\)/);
  assert.doesNotMatch(command, /set_taskbar_menu_state\(|\.lock\(\)/);
});

test('R5 admission remembers intent before slow work and guarded slot writes reject superseded intents', () => {
  const command = body('set_taskbar_menu_open');
  assert.match(command, /intent_sequence: String[\s\S]*?parse_taskbar_menu_request_number\(&intent_sequence\)[\s\S]*?admit_taskbar_menu_request[\s\S]*?load_settings_with_generation/);
  assert.match(body('admit_taskbar_menu_request_in_slot'), /sequence == 0[\s\S]*?sequence <= current.request_sequence[\s\S]*?return Err[\s\S]*?current.request_sequence = sequence/);
  assert.match(body('set_taskbar_menu_layout_for_request_in_slot'), /taskbar_menu_request_origin_matches[\s\S]*?current.request_generation != origin.generation[\s\S]*?current.request_sequence != sequence[\s\S]*?return Err[\s\S]*?write_taskbar_menu_layout/);
});

test('R6 actual originating HWND and fixed generation are checked at admission and actual write', () => {
  const command = body('set_taskbar_menu_open');
  assert.match(command, /window_generation: String[\s\S]*?window.hwnd\(\)[\s\S]*?let origin = TaskbarWindowHandle \{ raw, generation \}[\s\S]*?admit_taskbar_menu_request/);
  assert.doesNotMatch(command, /get_webview_window/);
  assert.match(body('taskbar_menu_request_origin_matches'), /origin.raw != 0 && origin.generation != 0 && current == Some\(origin\)/);
  for (const name of ['admit_taskbar_menu_request', 'set_taskbar_menu_layout_for_generation']) {
    assert.match(body(name), /try_taskbar_layout_gate[\s\S]*?taskbar_window_handle\(manager, tool\)[\s\S]*?current_window,\s*sequence/);
  }
});

test('R6 initial and recovery generations are frozen before window creation, with no current-token IPC', () => {
  assert.match(body('taskbar_menu_context'), /config_mut\(\).app.windows[\s\S]*?menuGeneration=1/);
  assert.match(source, /\.build\(taskbar_menu_context\(\)\)/);
  assert.match(body('register_taskbar_window_handles'), /set_taskbar_window_hwnd\(app, tool, hwnd, 1\)/);
  const create = body('create_taskbar_bar_window');
  assert.match(create, /let generation = TASKBAR_WINDOW_GENERATION.fetch_add[\s\S]*?menuGeneration=\{generation\}[\s\S]*?\.build\(\)\?[\s\S]*?set_taskbar_window_hwnd\(app, tool, Some\(window.hwnd\(\)\?\), generation\)/);
  assert.doesNotMatch(source, /fn get_taskbar_menu_(?:generation|token)/);
  const recovery = body('request_taskbar_bar_recovery');
  assert.match(recovery, /try_taskbar_layout_gate[\s\S]*?set_taskbar_window_hwnd\(&recovery_app, tool, None, 0\);[\s\S]*?set_taskbar_menu_state\(&recovery_app, tool, false\);[\s\S]*?stale.destroy\(\)[\s\S]*?create_taskbar_bar_window/);
});

test('menu commit validates planned menu revisions under the existing layout gate', () => {
  const plan = body('plan_taskbar_dock'), apply = body('apply_taskbar_dock_with_snapshot');
  assert.match(plan, /let menu_revisions = TASKBAR_TOOLS[\s\S]*?taskbar_menu_layout_snapshot[\s\S]*?let mut actions/);
  assert.match(apply, /menu_revisions,[\s\S]*?plan_taskbar_dock[\s\S]*?try_taskbar_layout_gate[\s\S]*?taskbar_menu_layout_plan_is_current\(&menu_revisions[\s\S]*?for action in actions/);
  assert.doesNotMatch(body('admit_taskbar_menu_request'), /with_taskbar_settings_read|\.hwnd\(|apply_taskbar/);
  assert.match(body('set_taskbar_menu_layout_for_generation'), /with_taskbar_settings_read[\s\S]*?try_taskbar_layout_gate/);
});

test('menu cleanup rejects retired revisions before changing the slot', () => {
  assert.match(body('close_taskbar_menu_layout_if_current'), /close_taskbar_menu_layout_in_slot_if_current/);
  const cleanup = body('close_taskbar_menu_layout_in_slot_if_current');
  assert.match(cleanup, /current.revision != write.revision[\s\S]*?return false;[\s\S]*?current.revision = current.revision.wrapping_add\(1\);[\s\S]*?current.layout = TaskbarMenuLayout::default\(\)/);
});

test('native planning defers menu cleanup until successful revision-guarded commit', () => {
  const apply = body('apply_taskbar_dock_with_snapshot');
  const planning = body('plan_taskbar_dock');
  assert.doesNotMatch(planning, /set_taskbar_menu_state\(|close_taskbar_menu_layout_if_current\(/);
  assert.equal(planning.match(/queue_menu_cleanup\(tool\)/g)?.length, 3);
  assert.match(planning, /taskbar_menu_layout_snapshot[\s\S]*?TaskbarMenuWrite \{ revision \}/);
  assert.match(planning, /Ok\(TaskbarDockPlan \{[\s\S]*?menu_cleanup,/);
  const beforeCommit = apply.split('let commit =')[0];
  assert.match(beforeCommit, /menu_cleanup,[\s\S]*?plan_taskbar_dock\(manager, settings, snapshot\)\?;[\s\S]*?let _layout_guard = try_taskbar_layout_gate/);
  assert.doesNotMatch(beforeCommit, /set_taskbar_menu_state\(|close_taskbar_menu_layout_if_current\(/);
  assert.match(apply, /let commit = \|\|[\s\S]*?validate_taskbar_content_layout_plan[\s\S]*?for action in actions[\s\S]*?hide_window\(hwnd\)\?[\s\S]*?apply_taskbar_overlay\(hwnd, rect\)\?[\s\S]*?for \(tool, write\) in menu_cleanup[\s\S]*?close_taskbar_menu_layout_if_current/);
});
