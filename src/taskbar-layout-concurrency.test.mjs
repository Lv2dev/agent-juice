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
  const plan = body('plan_taskbar_dock');
  const apply = body('apply_taskbar_dock_with_snapshot');
  assert.match(plan, /let content_revisions = TASKBAR_TOOLS[\s\S]*?taskbar_content_layout_revision[\s\S]*?let mut actions/);
  assert.match(apply, /content_revisions,[\s\S]*?plan_taskbar_dock\(manager, settings, snapshot\)\?[\s\S]*?let _layout_guard/);
  assert.match(apply, /let _layout_guard = try_taskbar_layout_gate[\s\S]*?validate_taskbar_content_layout_plan[\s\S]*?for action in actions/);
});

test('drag owns global placement through native commit and width admission', () => {
  const width = body('set_taskbar_content_width');
  assert.match(width, /taskbar_drag_active\(&app\)[\s\S]*?return Err[\s\S]*?begin_taskbar_content_layout/);
  assert.match(body('set_taskbar_drag_active'), /if active[\s\S]*?try_taskbar_layout_gate\(&TASKBAR_LAYOUT_GATE\)[\s\S]*?return false[\s\S]*?store\(active, Ordering::Release\)/);
  assert.match(body('spawn_taskbar_drag_loop'), /if !set_taskbar_drag_active\(&app, drag_tool.is_some\(\)\)[\s\S]*?drag_tool = None/);
  assert.match(body('apply_taskbar_dock_with_snapshot'), /let _layout_guard[\s\S]*?!taskbar_drag_active\(manager\)[\s\S]*?for action in actions/);
  assert.match(body('save_taskbar_drag_target'), /if !taskbar_drag_active\(app\)[\s\S]*?apply_taskbar_dock_for_generation/);
});

test('foreign shell input cannot fall back to bar rectangles or trigger a drop layout', () => {
  assert.match(body('current_bar_drag_start'), /let tool = taskbar_tool_at_point\(app, point\)\?;[\s\S]*?current_bar_drag_start_for_tool/);
  assert.doesNotMatch(body('current_bar_drag_start'), /for tool in TASKBAR_TOOLS/);
  assert.doesNotMatch(body('current_bar_at_point'), /current_bar_rect|point_inside_rect/);
  assert.match(body('spawn_taskbar_drag_loop'), /set_taskbar_drag_active\(&app, false\);\s*if drag_tool.is_some\(\)[\s\S]*?apply_taskbar_dock_for_generation/);
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

test('active drag policy commit and drop recheck share the settings admission gate', () => {
  assert.match(body('position_taskbar_drag_bar'), /with_taskbar_settings_read[\s\S]*?Settings::try_load[\s\S]*?taskbar_drag_policy_allows[\s\S]*?position_taskbar_bar_on_taskbar/);
  const drop = body('save_taskbar_drag_target');
  assert.match(drop, /try_update_taskbar_settings[\s\S]*?taskbar_drag_policy_allows[\s\S]*?TASKBAR_SETTINGS_GENERATION.load[\s\S]*?set_taskbar_target/);
  assert.doesNotMatch(drop, /\.or_else\(/);
  assert.match(drop, /!monitor_key.is_empty\(\) && taskbar_matches_monitor_key/);
  const loop = body('spawn_taskbar_drag_loop');
  assert.match(loop, /drag_settings_generation[\s\S]*?if !drag_cancelled[\s\S]*?position_taskbar_drag_bar[\s\S]*?Ok\(false\) => drag_cancelled = true/);
  assert.match(loop, /if \(was_down && !down\) \|\| drag_cancelled[\s\S]*?if !drag_cancelled[\s\S]*?save_taskbar_drag_target[\s\S]*?set_taskbar_drag_active\(&app, false\)/);
  assert.match(body('plan_taskbar_dock'), /let taskbar_paused = settings.taskbar_bars_paused \|\| taskbar_bars_paused/);
  assert.match(body('taskbar_dock_signature'), /let taskbar_paused = settings.taskbar_bars_paused \|\| taskbar_bars_paused/);
});

test('common native plan keeps the policy read gate through every native action', () => {
  const apply = body('apply_taskbar_dock_with_snapshot');
  assert.match(apply, /with_taskbar_settings_read\(\|generation\|[\s\S]*?generation == expected_generation[\s\S]*?plan_taskbar_dock[\s\S]*?let _layout_guard[\s\S]*?for action in actions[\s\S]*?apply_taskbar_overlay/);
});

test('successful moves advance the preferred monitor and successful saves always record the confirmed profile', () => {
  assert.match(body('spawn_taskbar_drag_loop'), /Ok\(true\) => \{[\s\S]*?drag_monitor_key = Some\(taskbar.key.clone\(\)\);[\s\S]*?last_monitor_key = Some\(taskbar.key\)/);
  assert.match(body('save_settings'), /\*current = requested;\s*if !profile_topology.is_empty\(\) \{\s*record_taskbar_layout_profile/);
});
