import assert from 'node:assert/strict';
import test from 'node:test';
import {readFileSync} from 'node:fs';

const source = readFileSync(new URL('../src-tauri/src/lib.rs', import.meta.url), 'utf8');
const body = name => source.match(new RegExp(`(?:async )?fn ${name}\\b[\\s\\S]*?\\n}\\r?\\n`))?.[0] ?? '';

test('content-width rollback owns a cache revision without holding a lock during native placement', () => {
  const update = body('set_taskbar_content_width');
  assert.match(update, /load_settings_with_generation\(\)[\s\S]*?TASKBAR_SETTINGS_GENERATION.load\(Ordering::Acquire\) != settings_generation[\s\S]*?let \(layout_revision, previous\) = taskbar_content_layout_snapshot/);
  assert.match(update, /begin_taskbar_content_layout\(&app, tool, layout_revision, next\)[\s\S]*?apply_taskbar_dock_for_generation[\s\S]*?rollback_taskbar_content_layout\(&app, tool, write\)/);
  assert.match(update, /TaskbarContentWidthDecision::AlreadyApplied\s*=>\s*\{[\s\S]*?acknowledge_taskbar_content_layout\(&app, tool, layout_revision\)[\s\S]*?return Ok\(false\)/);
  assert.doesNotMatch(update, /TASKBAR_CONTENT_LAYOUT_WRITE_GATE|\.lock\(\)/);
});

test('profile and drag ratio writes invalidate old rollback revisions including identical values', () => {
  assert.match(body('set_taskbar_content_layout_ratio'), /set_taskbar_content_layout_ratio_in_slot\(slot, ratio\)/);
  const ratio = body('set_taskbar_content_layout_ratio_in_slot');
  assert.match(ratio, /slot.lock\(\)[\s\S]*?current.revision = current.revision.wrapping_add\(1\);[\s\S]*?update_taskbar_content_layout_ratio/);
  assert.match(body('begin_taskbar_content_layout_in_slot'), /current.revision == expected_revision[\s\S]*?current.revision = current.revision.wrapping_add\(1\)/);
  assert.match(body('rollback_taskbar_content_layout_in_slot'), /current.revision != write.revision[\s\S]*?return false;[\s\S]*?current.layout = write.previous/);
  assert.match(body('sync_taskbar_content_layout_ratios'), /set_taskbar_content_layout_ratio/);
});

test('deferred settings saves cannot overwrite a newer layout ratio snapshot', () => {
  const save = body('save_settings');
  assert.match(save, /with_taskbar_settings_read\(\|current_generation\|\s*\{[\s\S]*?current_generation == generation[\s\S]*?sync_taskbar_content_layout_ratios\(&app, &settings\);[\s\S]*?Ok\(\(\)\)/);
});

test('native layout commits validate content revisions after acquiring the native gate', () => {
  const apply = body('apply_taskbar_dock_with_snapshot');
  assert.match(apply, /let content_revisions = TASKBAR_TOOLS[\s\S]*?taskbar_content_layout_revision[\s\S]*?let mut actions/);
  assert.match(apply, /let _layout_guard = try_taskbar_layout_gate[\s\S]*?validate_taskbar_content_layout_plan[\s\S]*?for action in actions/);
});

test('drag and move cache publications validate the current settings generation first', () => {
  const publish = body('set_taskbar_content_layout_ratio_for_generation');
  assert.match(publish, /with_taskbar_settings_read[\s\S]*?generation == expected_generation[\s\S]*?set_taskbar_content_layout_ratio/);
  for (const name of ['save_taskbar_drag_target','move_taskbar_bar']) {
    const command = body(name);
    assert.match(command, /set_taskbar_content_layout_ratio_for_generation/);
    assert.doesNotMatch(command, /\bset_taskbar_content_layout_ratio\(/);
  }
});
