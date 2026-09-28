import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { barToolViewModel, barViewModel } from './bar-state.js';
import { viewModelForTool } from './panel-state.js';
import { formStateFromSettings, payloadFromEntries } from './settings-state.js';

const now = new Date('2026-09-27T09:00:00Z');
const status = {
  tool: 'antigravity', captured_at: now.toISOString(), approx: false, session: { active: true },
  primary: { label: 'gemini_models', used_percent: 25, resets_at: '2026-09-27T10:00:00Z' },
  secondary: { label: 'claude_gpt_models', used_percent: 10, resets_at: '2026-09-27T11:00:00Z' },
};
const settings = { show_antigravity: true, show_claude: false, show_codex: false, language: 'en', display_basis: 'remaining' };

test('late and queued provider completions publish only the current Antigravity cache', () => {
  const source = readFileSync(new URL('../src-tauri/src/lib.rs', import.meta.url), 'utf8');
  const collection = source.match(/fn collect_representatives_with_options_and_late_app[\s\S]*?\n}\r?\n/)?.[0] ?? '';
  assert.match(collection, /let mut snapshot = combined_collection_last_result\(\)/);
  assert.match(collection, /let _ = sender\.send\(\(\)\)/);
  assert.match(collection, /let _ = receiver\.recv_timeout\(remaining\);\s*statuses\.extend\(antigravity::cached\(\)\)/);
  const combined = source.match(/fn combined_collection_last_result[\s\S]*?\n}\r?\n/)?.[0] ?? '';
  assert.match(combined, /statuses\.extend\(antigravity::cached\(\)\)/);
});

test('Antigravity is opt-in and settings preserve independent colors and position', () => {
  assert.equal(formStateFromSettings({}).showAntigravity, false);
  const form = formStateFromSettings({ ...settings, antigravity_taskbar_offset_ratio: 0.7,
    tool_colors: { antigravity_primary: [1,2,3], antigravity_secondary: [4,5,6] },
    taskbar_text_colors: { antigravity: [7,8,9], antigravity_on: true } });
  assert.equal(form.antigravityPrimaryColor, '#010203');
  assert.equal(form.antigravitySecondaryColor, '#040506');
  assert.equal(form.antigravityTextColor, '#070809');
  assert.equal(form.antigravityTextColorOn, true);
  assert.equal(form.antigravityTaskbarOffsetRatio, 0.7);
  const payload = payloadFromEntries({ show_antigravity: 'on', antigravity_primary_color: '#010203', antigravity_secondary_color: '#040506', antigravity_text_color: '#070809', antigravity_text_color_on: 'on', antigravity_taskbar_offset_ratio: '0.7' });
  assert.equal(payload.show_antigravity, true);
  assert.equal(payload.antigravity_primary_color, '#010203');
  assert.equal(payload.antigravity_secondary_color, '#040506');
  assert.equal(payload.antigravity_text_color_on, true);
  assert.equal(payload.antigravity_taskbar_offset_ratio, 0.7);
});

test('Antigravity pools retain their meaning across all modes, indicators and display bases', () => {
  for (const bar_mode of ['full','compact','dual','quad']) for (const indicator_style of ['ring','bar']) for (const display_basis of ['used','remaining']) {
    const config = { ...settings, bar_mode, indicator_style, display_basis };
    const vm = barViewModel([status], config, now);
    assert.equal(vm.tools.length, 1);
    const tool = vm.tools[0];
    assert.equal(tool.label, 'Antigravity');
    assert.equal(tool.primary.labelKey, 'limit.geminiModels');
    assert.equal(tool.secondary.labelKey, 'limit.claudeGptModels');
    assert.match(tool.primary.text, display_basis==='used' ? /25%/ : /75%/);
    assert.match(tool.tooltip, /Quota reported by the running Antigravity app/);
    if(bar_mode==='compact') { assert.equal(tool.primary.text, display_basis==='used'?'25%':'75%'); }
    assert.doesNotMatch(tool.tooltip, /5h|Weekly/);
  }
  const single = barToolViewModel([{...status,secondary:null}], 'antigravity', settings, now);
  assert.equal(single.secondary.visible, false);
  assert.equal(viewModelForTool([{...status,primary:null}], 'antigravity', settings, now).primary.visible, false);
});

test('closed GUI and missing login hide old quota and have distinct localized messages', () => {
  for(const language of ['ko','en']) for(const health of ['app_required','login_required']) {
    const config={...settings,language};
    const expected=health==='app_required' ? (language==='ko'?'Antigravity 실행 필요':'Open Antigravity') : (language==='ko'?'로그인 필요':'Sign in required');
    const bar=barToolViewModel([status], 'antigravity', config, now, {collectionHealth:{antigravity:health}});
    assert.equal(bar.loginText, expected);
    assert.doesNotMatch(bar.tooltip, /75%|90%/);
    const panel=viewModelForTool([status], 'antigravity', config, now, {antigravity:health});
    assert.equal(panel.exists, false);
    assert.equal(panel.emptyHint, expected);
  }
});
