import assert from 'node:assert/strict';
import test from 'node:test';
import {readFileSync} from 'node:fs';

const source = readFileSync(new URL('../src-tauri/src/lib.rs', import.meta.url), 'utf8');
const body = name => source.match(new RegExp(`(?:async )?fn ${name}\\b[\\s\\S]*?\\n}\\r?\\n`))?.[0] ?? '';

test('all collection side effects and immediate refresh flags use the committed snapshot', () => {
  const save = body('save_settings');
  assert.match(save, /collection_changes,[\s\S]*?\}\s*= snapshot;/);
  for (const tool of ['Claude','Codex','Grok','Cursor','Antigravity']) {
    assert.match(save, new RegExp(`collection_changes.enabled_now\\(&Tool::${tool}\\)`));
  }
  for (const tool of ['Codex','Grok']) {
    assert.match(save, new RegExp(`collection_changes.transition\\(&Tool::${tool}\\).is_some\\(\\)[\\s\\S]*?reconcile_broker_policy_off_thread\\(Tool::${tool}\\)`));
  }
  assert.match(save, /if collection_changes.any\(\)/);
  assert.match(save, /if collection_changes.cursor_activity_range_changed/);
  assert.doesNotMatch(save, /previous_show_|baseline.show_|reconcile_claude_statusline_off_thread|Claude collection rollback failed/);
});

test('startup and deferred broker/Claude work load policy under the same settings gate', () => {
  const current = body('reconcile_current_collection_policy');
  assert.match(current, /with_taskbar_settings_read/);
  assert.match(current, /apply_collection_policy_from_settings\(tool, load_settings\(\), reconcile\)/);
  const apply = body('apply_collection_policy_from_settings');
  assert.match(apply, /apply_collection_policy_from_settings_with_retirement\(tool, settings, reconcile/);
  assert.match(apply, /antigravity::suspend_for_settings_failure\(\)/);
  assert.doesNotMatch(apply, /antigravity::set_enabled\(false\)/);
  const retirement = body('apply_collection_policy_from_settings_with_retirement');
  assert.match(retirement, /reconcile\(CollectionPolicy::from_settings\(&settings\).enabled\(tool\)\)/);
  assert.match(retirement, /Tool::Codex \| Tool::Grok[\s\S]*?reconcile\(false\)/);
  assert.match(retirement, /Err\(error\) if \*tool == Tool::Antigravity[\s\S]*?retire_antigravity\(\)/);
  assert.match(body('reconcile_broker_policy_for'), /reconcile_current_collection_policy[\s\S]*?Settings::try_load/);
  assert.match(body('reconcile_claude_statusline_for_release'), /reconcile_current_collection_policy[\s\S]*?Settings::try_load/);
  assert.doesNotMatch(body('spawn_claude_statusline_reconcile'), /enabled: bool|settings.show_claude/);
});

test('settings-read retirement keeps the Antigravity external-attempt throttles', () => {
  const cache = readFileSync(new URL('../src-tauri/src/antigravity.rs', import.meta.url), 'utf8');
  const suspension = cache.match(/fn suspend_for_settings_failure\(&mut self\)[\s\S]*?\n    }/)?.[0] ?? '';
  assert.match(suspension, /self.suspended = true/);
  assert.match(suspension, /self.generation = self.generation.wrapping_add\(1\)/);
  assert.match(suspension, /self.result = None/);
  assert.doesNotMatch(suspension, /self\.(?:retry_at|last_started_at|force_retry_at) = None|self.failures = 0/);
});

test('automatic native snapshots honor shared system activity while manual refresh stays explicit', () => {
  const status = body('get_status');
  assert.match(status, /if !automatic_collection_allowed\(&app\)[\s\S]*?return current_collection_snapshot\(\)/);
  assert.ok(status.indexOf('automatic_collection_allowed') < status.indexOf('Settings::try_load'));
  const activity = body('get_activity');
  assert.match(activity, /Result<Option<activity::ActivitySnapshot>, String>/);
  assert.match(activity, /if !automatic_collection_allowed\(&app\)[\s\S]*?return Ok\(None\)/);
  assert.ok(activity.indexOf('automatic_collection_allowed') < activity.indexOf('collect_activity_off_thread'));
  assert.match(activity, /with_taskbar_settings_read[\s\S]*?Settings::try_load[\s\S]*?Ok\(Some\(current_activity_snapshot\(/);
  assert.match(activity, /&snapshot,[\s\S]*?settings.show_codex/);
  assert.match(source, /app.manage\(system_activity.clone\(\)\)/);
  assert.doesNotMatch(body('refresh_status'), /automatic_collection_allowed|system_activity_allows_automatic/);
  assert.match(body('refresh_status'), /collect_force_refresh_off_thread[\s\S]*?spawn_activity_refresh[\s\S]*?spawn_cursor_activity_refresh/);
});

test('activity results keep internal Codex provenance through final IPC and both emitters', () => {
  assert.match(source, /struct CollectedActivity\s*\{[\s\S]*?snapshot: activity::ActivitySnapshot,[\s\S]*?codex_generation: Option<u64>/);
  const collect = body('collect_activity_off_thread');
  assert.match(collect, /Result<CollectedActivity, String>/);
  assert.match(collect, /collector::codex_collection_generation\(\)[\s\S]*?codex_activity::collect/);
  assert.match(collect, /current_activity_snapshot\(&local, settings.show_codex\)/);
  assert.match(body('current_activity_snapshot_with_policy'), /result.codex_generation == current_generation\(\)/);
  assert.match(body('current_activity_snapshot_with_policy'), /merge_codex_activity\(snapshot, None, codex_enabled\)/);
  for (const name of ['spawn_activity_refresh', 'spawn_cursor_activity_refresh']) {
    assert.match(body(name), /with_taskbar_settings_read[\s\S]*?current_activity_snapshot[\s\S]*?emit\("activity-updated"/);
  }
  assert.match(body('get_activity'), /Result<Option<activity::ActivitySnapshot>, String>/);
});

test('queued Cursor activity reads current settings and their generation before dispatch', () => {
  const spawn = body('spawn_cursor_activity_refresh');
  assert.ok(spawn.indexOf('current_cursor_activity_request') < spawn.indexOf('try_begin_cursor_activity_refresh'));
  assert.ok(spawn.indexOf('current_cursor_activity_request') < spawn.indexOf('tauri::async_runtime::spawn'));
  const request = body('current_cursor_activity_request');
  assert.match(request, /with_taskbar_settings_read[\s\S]*?load_settings\(\)/);
  assert.match(request, /TASKBAR_SETTINGS_GENERATION.load\(Ordering::Acquire\) == generation/);
  assert.match(request, /settings.show_cursor.then_some\(\(settings, generation\)\)/);
});

test('automatic activity backfill rechecks inactivity between passes without blocking force', () => {
  const activity = body('spawn_activity_refresh');
  assert.match(activity, /if !force_codex && !automatic_collection_allowed\(&app\)/);
  assert.match(activity, /for pass in 0\.\.ACTIVITY_BACKFILL_MAX_PASSES[\s\S]*?if !force_codex && !automatic_collection_allowed\(&app\)[\s\S]*?break/);
  const cursor = body('spawn_cursor_activity_refresh');
  assert.match(cursor, /!force && !automatic_collection_allowed\(&app\)/);
  assert.match(cursor, /for _ in 0\.\.CURSOR_ACTIVITY_MAX_PASSES[\s\S]*?system_activity_allows_automatic\(activity_monitor.as_ref\(\)\)/);
  assert.match(cursor, /force\s*\|\|\s*system_activity_allows_automatic\(activity_monitor.as_ref\(\)\)/);
});

test('failed policies are recovered before collection without polling healthy bindings', () => {
  const collection = body('collect_representatives_with_options_and_late_app');
  assert.match(collection, /recover_pending_collection_policies\(\)[\s\S]*?if !settings.show_claude/);
  const broker = body('reconcile_broker_policy_for');
  assert.match(broker, /COLLECTION_POLICY_RECOVERY[\s\S]*?\.record\(tool, failed, std::time::Instant::now\(\)\)/);
  const recover = body('recover_pending_collection_policies');
  assert.match(recover, /Settings::try_load/);
  assert.match(recover, /Tool::Antigravity => apply_antigravity_cli_policy_for_release\(enabled\)/);
  assert.match(recover, /antigravity_cli::set_connection_failed\(failed\)/);
  assert.match(recover, /Tool::Claude\s*=>\s*\{?\s*apply_claude_statusline_for_release\(enabled\)/);
  assert.doesNotMatch(recover, /Command::|spawn\(/);
});

test('broker recovery rechecks pending under the settings gate and retains failed attempts', () => {
  const recover = body('recover_pending_collection_policies_with');
  assert.match(recover, /state.any_due\(clock\(\)\)/);
  assert.match(recover, /if !due[\s\S]*?return Ok\(\(\)\)/);
  assert.match(recover, /with_taskbar_settings_read[\s\S]*?\.due\(tool, clock\(\)\)/);
  assert.match(recover, /apply_collection_policy_from_settings\(tool, load_settings\(\)/);
  assert.match(recover, /\.record\(\s*tool,\s*result.is_err\(\),\s*clock\(\),?\s*\)/);
  assert.match(recover, /with_taskbar_settings_read[\s\S]*?publish_failure\(tool, result.is_err\(\)\)/);
});

test('CLI direct failures join recovery and no-op saves can repair pending policy', () => {
  const direct=body('reconcile_antigravity_cli_for_release');
  assert.match(direct, /COLLECTION_POLICY_RECOVERY[\s\S]*?\.record\(&Tool::Antigravity, failed, std::time::Instant::now\(\)\)/);
  assert.match(direct, /antigravity_cli::set_connection_failed\(failed\)/);
  const save=body('save_settings');
  assert.match(save, /recover_pending_collection_policies_off_thread\(\)\s*\.await/);
  const deferred=body('recover_pending_collection_policies_off_thread');
  assert.match(deferred, /\.any_due\(std::time::Instant::now\(\)\)/);
  assert.match(deferred, /if !due[\s\S]*?return Ok\(\(\)\);[\s\S]*?spawn_blocking\(recover_pending_collection_policies\)/);
  const apply=body('apply_antigravity_cli_policy_for_release');
  assert.match(apply, /antigravity::set_enabled\(enabled\)/);
  assert.match(apply, /antigravity::set_enabled\(enabled\)/);
  assert.match(apply, /antigravity_cli::binding::reconcile\(false/);
  assert.doesNotMatch(apply, /antigravity_cli::binding::reconcile\(true|antigravity_cli::binding::reconcile\(enabled/);
});

test('CLI startup reconciliation remains scheduled when the initial settings read fails', () => {
  assert.match(source, /\}\s*\/\/ Current-policy workers[^\r\n]*\r?\n\s*spawn_claude_statusline_reconcile\(\);\s*spawn_antigravity_cli_reconcile\(\);\s*let \(system_activity/);
  const current=body('reconcile_antigravity_cli_for_release');
  assert.match(current, /Settings::try_load/);
  assert.match(current, /COLLECTION_POLICY_RECOVERY/);
});

test('Claude prepare and failed-save recovery remain inside the commit gate', () => {
  const update = body('try_update_taskbar_settings');
  assert.match(update, /TASKBAR_SETTINGS_WRITE_GATE[\s\S]*?persist_settings_with_collection_policy[\s\S]*?mark_taskbar_settings_changed/);
  const transaction = body('persist_settings_with_collection_policy');
  assert.match(transaction, /mutate_settings_with_collection_changes/);
  assert.match(transaction, /changes.transition\(&Tool::Claude\)[\s\S]*?claude_attempted = true;[\s\S]*?apply_claude\(enabled\)\?/);
  assert.match(transaction, /load_current\(\).and_then\(\|current\| apply_claude\(current.show_claude\)\)/);
  assert.doesNotMatch(transaction, /baseline|with_taskbar_settings_read|reconcile_claude_statusline_off_thread/);
});

test('Claude startup failures join current-policy recovery and committed success retires retries', () => {
  const direct = body('reconcile_claude_statusline_for_release');
  assert.match(direct, /COLLECTION_POLICY_RECOVERY[\s\S]*?\.record\(&Tool::Claude, failed, std::time::Instant::now\(\)\)/);
  assert.match(body('recover_pending_collection_policies'), /Tool::Claude\s*=>\s*\{?\s*apply_claude_statusline_for_release\(enabled\)/);
  assert.match(source, /retry_settings_side_effects\(app.handle\(\).clone\(\), taskbar_retry, autostart_retry\);\s*\}\s*\/\/[^\r\n]*\r?\n\s*spawn_claude_statusline_reconcile\(\);/);
  const update = body('try_update_taskbar_settings');
  assert.match(update, /persist_settings_with_collection_policy[\s\S]*?\)\?;[\s\S]*?collection_changes.transition\(&Tool::Claude\).is_some\(\)[\s\S]*?\.record\(&Tool::Claude, false, std::time::Instant::now\(\)\)/);
});
