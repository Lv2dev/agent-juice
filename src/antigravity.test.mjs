import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { barToolViewModel, barViewModel } from './bar-state.js';
import { viewModelForTool } from './panel-state.js';
import { formStateFromSettings, payloadFromEntries } from './settings-state.js';
import { t } from './i18n.js';

const now = new Date('2026-09-27T09:00:00Z');
const status = {
  tool: 'antigravity', captured_at: now.toISOString(), approx: false, session: { active: true },
  primary: { label: '5h', used_percent: 25, resets_at: '2026-09-27T10:00:00Z' },
  secondary: { label: 'week', used_percent: 10, resets_at: '2026-10-04T11:00:00Z' },
};
const settings = { show_antigravity: true, show_claude: false, show_codex: false, language: 'en', display_basis: 'remaining' };

test('Gemini period quota uses five-hour and weekly labels in compact and panel', () => {
  const period = {...status, primary:{label:'5h',used_percent:58}, secondary:{label:'week',used_percent:10}};
  const bar = barToolViewModel([period], 'antigravity', {...settings,bar_mode:'compact'}, now);
  assert.equal(bar.primary.text, '5h 42%');
  assert.equal(bar.secondary.labelKey, 'limit.weekly');
  const panel = viewModelForTool([period], 'antigravity', settings, now);
  assert.equal(panel.primary.label, '5h');
});

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

test('Antigravity periods retain their meaning across all modes, indicators, orders and display bases', () => {
  for (const language of ['ko','en']) for (const limit_order of ['primary_first','secondary_first'])
  for (const bar_mode of ['full','compact','dual','quad']) for (const indicator_style of ['ring','bar']) for (const display_basis of ['used','remaining']) {
    const config = { ...settings, bar_mode, indicator_style, display_basis, language, limit_order };
    const vm = barViewModel([status], config, now);
    assert.equal(vm.tools.length, 1);
    const tool = vm.tools[0];
    assert.equal(tool.label, 'Antigravity');
    assert.equal(tool.primary.labelKey, 'limit.fiveHour');
    assert.equal(tool.secondary.labelKey, 'limit.weekly');
    assert.match(tool.primary.text, display_basis==='used' ? /25%/ : /75%/);
    assert.match(tool.tooltip, /Gemini/);
    assert.match(tool.tooltip, /5h/);
    assert.match(tool.tooltip, language === 'ko' ? /주간/ : /Weekly/);
    assert.doesNotMatch(tool.tooltip, /Claude|GPT/);
    if(bar_mode==='compact') { assert.equal(tool.primary.text, display_basis==='used'?'5h 25%':'5h 75%'); }
    const panel = viewModelForTool([status], 'antigravity', config, now);
    assert.equal(panel.primary.label, '5h');
    assert.equal(panel.secondary.label, language==='ko'?'주간':'Weekly');
    assert.equal(panel.primary.value, display_basis==='used'?'25%':'75%');
  }
  const single = barToolViewModel([{...status,secondary:null}], 'antigravity', settings, now);
  assert.equal(single.secondary.visible, false);
  assert.equal(viewModelForTool([{...status,primary:null}], 'antigravity', settings, now).primary.visible, false);
  const weekly = barToolViewModel([{...status,primary:null}], 'antigravity', settings, now);
  assert.equal(weekly.primary.visible, false);
  assert.equal(weekly.secondary.visible, true);
  assert.doesNotMatch(weekly.tooltip, /5h/);
});

test('Antigravity period colors preserve user settings and labels stay synchronized', () => {
  const config = {...settings, tool_colors:{antigravity_primary:[1,2,3],antigravity_secondary:[4,5,6]}};
  const bar = barToolViewModel([status], 'antigravity', config, now);
  const panel = viewModelForTool([status], 'antigravity', config, now);
  assert.equal(bar.primary.color, panel.primary.color);
  assert.equal(bar.secondary.color, panel.secondary.color);
  assert.notEqual(bar.primary.color, bar.secondary.color);
  const html = readFileSync(new URL('./index.html', import.meta.url), 'utf8');
  const card = html.match(/<section class="tool-card" data-tool="antigravity"[\s\S]*?<\/section>/)?.[0];
  assert.match(card, /data-i18n="limit.fiveHour"/);
  assert.match(card, /data-i18n="limit.weekly"/);
  assert.doesNotMatch(card, /Claude\/GPT|geminiModels|claudeGptModels/);
  for(const language of ['ko','en']) {
    assert.match(t('help.showAntigravity',language), /Gemini/);
    assert.match(t('help.showAntigravity',language), language==='ko'?/5시간·주간/:/5h and weekly/);
    assert.match(t('field.antigravityPrimaryColor',language), /5h/);
    assert.match(t('field.antigravitySecondaryColor',language), language==='ko'?/주간/:/weekly/);
  }
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
