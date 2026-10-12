import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { barViewModel } from './bar-state.js';
import { viewModelForTool } from './panel-state.js';
import { formStateFromSettings, payloadFromEntries } from './settings-state.js';
import { previewSnapshot } from './bar-preview.js';
import { t } from './i18n.js';

const tools = ['claude', 'codex', 'grok', 'cursor', 'antigravity'];
const now = new Date('2026-10-10T00:00:00Z');
const enabled = Object.fromEntries(tools.map(tool => [`show_${tool}`, true]));
const statuses = tools.map(tool => ({
  tool, captured_at: now.toISOString(), session: { active: true },
  primary: { label: '5h', used_percent: 25 },
  secondary: { label: 'week', used_percent: 10 },
}));

test('taskbar visibility defaults preserve every enabled tool in legacy settings', () => {
  assert.deepEqual(barViewModel(statuses, enabled, now).tools.map(tool => tool.tool), tools);
  const form = formStateFromSettings(enabled);
  for (const tool of tools) {
    const suffix = tool[0].toUpperCase() + tool.slice(1);
    assert.equal(form[`taskbarShow${suffix}`], true);
  }
});

test('taskbar-hidden tools keep panel usage while disappearing from every bar mode', () => {
  for (const tool of tools) for (const bar_mode of ['full', 'compact', 'dual', 'quad']) {
    const settings = { ...enabled, bar_mode, [`taskbar_show_${tool}`]: false };
    assert.deepEqual(barViewModel(statuses, settings, now).tools.map(item => item.tool), tools.filter(item => item !== tool));
    assert.equal(viewModelForTool(statuses, tool, settings, now).exists, true);
    assert.equal(settings[`show_${tool}`], true);
  }
});

test('a taskbar preference cannot enable collection and is preserved by form conversion', () => {
  for (const tool of tools) {
    const settings = { ...enabled, [`show_${tool}`]: false, [`taskbar_show_${tool}`]: true };
    assert(!barViewModel(statuses, settings, now).tools.some(item => item.tool === tool));
    const suffix = tool[0].toUpperCase() + tool.slice(1);
    const form = formStateFromSettings({ ...settings, [`taskbar_show_${tool}`]: false });
    assert.equal(form[`taskbarShow${suffix}`], false);
    const input = payloadFromEntries({ [`show_${tool}`]: 'on', [`taskbar_show_${tool}`]: '' });
    assert.equal(input[`show_${tool}`], true);
    assert.equal(input[`taskbar_show_${tool}`], false);
  }
});

test('hiding all taskbar tools produces no bars without changing collection switches', () => {
  const settings = { ...enabled, ...Object.fromEntries(tools.map(tool => [`taskbar_show_${tool}`, false])) };
  assert.equal(barViewModel(statuses, settings, now).tools.length, 0);
  for (const tool of tools) assert.equal(viewModelForTool(statuses, tool, settings, now).exists, true);
});

test('preview remains visible for taskbar-hidden tools without changing either saved switch', () => {
  for (const tool of tools) {
    const settings = { ...enabled, [`show_${tool}`]: false, [`taskbar_show_${tool}`]: false };
    const snapshot = previewSnapshot(settings, statuses, tool);
    assert.equal(settings[`show_${tool}`], false);
    assert.equal(settings[`taskbar_show_${tool}`], false);
    assert(barViewModel(snapshot.statuses, snapshot.settings, now).tools.some(item => item.tool === tool));
  }
});

test('tool settings provide separate accessible activation and taskbar controls in both languages', () => {
  const html = readFileSync(new URL('./index.html', import.meta.url), 'utf8');
  for (const tool of tools) {
    for (const name of [`show_${tool}`, `taskbar_show_${tool}`]) {
      const matches = [...html.matchAll(new RegExp(`<input\\b[^>]*name="${name}"[^>]*>`, 'g'))];
      assert.equal(matches.length, 1);
      const key = matches[0][0].match(/data-i18n-aria-label="([^"]+)"/)?.[1];
      assert(key);
      for (const language of ['ko', 'en']) assert(t(key, language).toLowerCase().includes(tool));
    }
  }
});

test('collection policy and panel filters remain independent of taskbar visibility', () => {
  const lib = readFileSync(new URL('../src-tauri/src/lib.rs', import.meta.url), 'utf8');
  const policy = lib.match(/fn collection_plan\b[\s\S]*?\n}\r?\n/)?.[0] ?? '';
  const enabledStatus = lib.match(/fn filter_enabled_statuses\b[\s\S]*?\n}\r?\n/)?.[0] ?? '';
  assert(enabledStatus.includes('show_claude'));
  assert.doesNotMatch(enabledStatus, /taskbar_show_/);
  assert(policy.includes('show_codex'));
  assert.doesNotMatch(policy, /taskbar_show_/);
  const panel = readFileSync(new URL('./panel.js', import.meta.url), 'utf8');
  assert.doesNotMatch(panel, /taskbar_show_/);
});
