import assert from 'node:assert/strict';
import test from 'node:test';
import {readFileSync} from 'node:fs';

const source = readFileSync(new URL('../src-tauri/src/lib.rs', import.meta.url), 'utf8');
const body = name => source.match(new RegExp(`(?:async )?fn ${name}\\b[\\s\\S]*?\\n}\\r?\\n`))?.[0] ?? '';

test('menu publications version every value and all getters use slot snapshots', () => {
  assert.match(source, /struct TaskbarMenuSlot\s*\{\s*revision: u64,\s*layout: TaskbarMenuLayout/);
  const write = body('set_taskbar_menu_layout_in_slot');
  assert.match(write, /slot.lock\(\)[\s\S]*?current.revision = current.revision.wrapping_add\(1\);[\s\S]*?current.layout =/);
  assert.match(body('set_taskbar_menu_state'), /set_taskbar_menu_layout\(/);
  assert.match(body('set_taskbar_menu_layout'), /set_taskbar_menu_layout_in_slot\(/);
  for (const name of ['taskbar_menu_is_open', 'taskbar_layout_ratio']) {
    assert.match(body(name), /taskbar_menu_layout_snapshot/);
  }
});

test('menu generation admission precedes mutation and IPC errors clean only their token', () => {
  const publish = body('set_taskbar_menu_layout_for_generation');
  assert.match(publish, /with_taskbar_settings_read[\s\S]*?generation == expected_generation[\s\S]*?set_taskbar_menu_layout_in_slot/);
  const command = body('set_taskbar_menu_open');
  assert.match(command, /let write =\s*set_taskbar_menu_layout_for_generation[\s\S]*?apply_taskbar_dock_for_generation[\s\S]*?close_taskbar_menu_layout_if_current\(&app, &tool, write\)/);
  assert.doesNotMatch(command, /set_taskbar_menu_state\(|\.lock\(\)/);
});

test('menu cleanup rejects retired revisions before changing the slot', () => {
  assert.match(body('close_taskbar_menu_layout_if_current'), /close_taskbar_menu_layout_in_slot_if_current/);
  const cleanup = body('close_taskbar_menu_layout_in_slot_if_current');
  assert.match(cleanup, /current.revision != write.revision[\s\S]*?return false;[\s\S]*?current.revision = current.revision.wrapping_add\(1\);[\s\S]*?current.layout = TaskbarMenuLayout::default\(\)/);
});

test('native planning defers menu cleanup until successful revision-guarded commit', () => {
  const apply = body('apply_taskbar_dock_with_snapshot');
  const planning = apply.split('let _layout_guard =')[0];
  assert.doesNotMatch(planning, /set_taskbar_menu_state\(|close_taskbar_menu_layout_if_current\(/);
  assert.equal(planning.match(/queue_menu_cleanup\(tool\)/g)?.length, 3);
  assert.match(planning, /taskbar_menu_layout_snapshot[\s\S]*?TaskbarMenuWrite \{ revision \}/);
  assert.match(apply, /validate_taskbar_content_layout_plan[\s\S]*?for action in actions[\s\S]*?apply_taskbar_overlay[\s\S]*?for \(tool, write\) in menu_cleanup[\s\S]*?close_taskbar_menu_layout_if_current/);
});
