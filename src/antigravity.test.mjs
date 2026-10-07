import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { barToolViewModel, barViewModel } from './bar-state.js';
import { viewModelForTool } from './panel-state.js';
import { formStateFromSettings, payloadFromEntries } from './settings-state.js';
import { formatLocalDateTime, t } from './i18n.js';

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
  assert.match(collection, /publish_late_cursor_result\([\s\S]*?combined_collection_last_result/);
  const publication = source.match(/fn publish_late_cursor_result[\s\S]*?\n}\r?\n/)?.[0] ?? '';
  assert.match(publication, /with_taskbar_settings_read/);
  assert.match(publication, /let mut snapshot = current_snapshot\(\)/);
  assert.match(publication, /filter_enabled_statuses\(latest_per_tool\(&snapshot\), &settings\)/);
  assert.match(collection, /let _ = sender\.send\(\(\)\)/);
  assert.match(collection, /let _ = receiver\.recv_timeout\(remaining\);\s*statuses\.extend\(antigravity::cached\(\)\)/);
  const combined = source.match(/fn combined_collection_last_result[\s\S]*?\n}\r?\n/)?.[0] ?? '';
  assert.match(combined, /statuses\.extend\(antigravity::cached\(\)\)/);
  const emit = source.match(/fn emit_collection_snapshot[\s\S]*?\n}\r?\n/)?.[0] ?? '';
  assert.match(emit, /consume_current_collection_snapshot\([\s\S]*?combined_collection_last_result/);
  for (const command of ['get_status', 'refresh_status']) {
    const handler = source.match(new RegExp(`async fn ${command}\\b[\\s\\S]*?\\n}\\r?\\n`))?.[0] ?? '';
    assert.match(handler, /current_collection_snapshot\(\)\.map_err/);
  }
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
  assert.equal(panel.primary.label, t('limit.fiveHour', config));
  assert.equal(panel.secondary.label, t('limit.weekly', config));
  assert.match(card, /class="metric p5h">\s*<div class="metric-row">\s*<span>5h<\/span>/);
  assert.match(card, /class="metric pweek">\s*<div class="metric-row">\s*<span>주간<\/span>/);
  assert.doesNotMatch(card, /Claude\/GPT|geminiModels|claudeGptModels/);
  for(const language of ['ko','en']) {
    assert.match(t('help.showAntigravity',language), /Gemini/);
    assert.match(t('help.showAntigravity',language), language==='ko'?/5시간·주간/:/5h and weekly/);
    assert.match(t('field.antigravityPrimaryColor',language), /5h/);
    assert.match(t('field.antigravitySecondaryColor',language), language==='ko'?/주간/:/weekly/);
  }
});

test('missing Desktop and CLI sources and missing login have distinct localized messages', () => {
  for(const language of ['ko','en']) for(const health of ['app_required','login_required']) {
    const config={...settings,language};
    const expected=health==='app_required' ? (language==='ko'?'Antigravity Desktop/CLI 실행 필요':'Open Antigravity Desktop/CLI') : (language==='ko'?'로그인 필요':'Sign in required');
    const bar=barToolViewModel([status], 'antigravity', config, now, {collectionHealth:{antigravity:health}});
    assert.equal(bar.loginText, expected);
    assert.doesNotMatch(bar.tooltip, /75%|90%/);
    const panel=viewModelForTool([status], 'antigravity', config, now, {antigravity:health});
    assert.equal(panel.exists, false);
    assert.equal(panel.emptyHint, expected);
  }
});

test('Antigravity save effects use the committed mutation snapshot instead of preflight flags', () => {
  const source = readFileSync(new URL('../src-tauri/src/lib.rs', import.meta.url), 'utf8');
  const update = source.match(/fn try_update_taskbar_settings\b[\s\S]*?\n}\r?\n/)?.[0] ?? '';
  assert.match(update, /persist_settings_with_collection_policy\([\s\S]*?\|edit\| Settings::try_update\(edit\)/);
  assert.match(update, /Ok\(TaskbarSettingsSnapshot\s*\{[\s\S]*?collection_changes/);
  const save = source.match(/async fn save_settings\b[\s\S]*?\n}\r?\n/)?.[0] ?? '';
  assert.match(save, /Ok::<_, anyhow::Error>\(\(snapshot, autostart_changed\)\)/);
  assert.match(save, /let TaskbarSettingsSnapshot\s*\{[\s\S]*?collection_changes,[\s\S]*?\}\s*= snapshot;/);
  assert.match(save, /let antigravity_enabled_now = collection_changes.enabled_now\(&Tool::Antigravity\)/);
  assert.match(save, /if collection_changes.any\(\)/);
  assert.match(save, /if collection_changes.transition\(&Tool::Antigravity\).is_some\(\)\s*&& reconcile_antigravity_cli_off_thread\(\).await.is_err\(\)/);
  assert.doesNotMatch(save, /baseline.show_antigravity|antigravity_collection_transition/);
});

test('Desktop and CLI copy is localized and the existing activation help stays synchronized', () => {
  const html = readFileSync(new URL('./index.html', import.meta.url), 'utf8');
  const help = html.match(/data-i18n="help\.showAntigravity">([^<]+)<\/small>/)?.[1];
  assert.equal(help, t('help.showAntigravity', 'ko'));
  assert.equal((html.match(/name="show_antigravity"/g) ?? []).length, 1);
  for (const language of ['ko', 'en']) {
    for (const key of ['state.antigravityRequired', 'tooltip.antigravitySource', 'empty.antigravity', 'help.showAntigravity']) {
      assert.match(t(key, language), /Desktop/);
      assert.match(t(key, language), /CLI/);
    }
    const copy = t('help.showAntigravity', language);
    assert.match(copy, language === 'ko' ? /Desktop 우선, 미실행 시 CLI 이벤트/ : /Desktop first, CLI events when closed/);
    assert.match(copy, language === 'ko' ? /연결 변경 후 재실행/ : /Restart the CLI after connection changes/);
    assert.match(copy, /\/usage/);
    assert.match(copy, language === 'ko' ? /토큰 활동 미지원/ : /No token activity/);
  }
});

test('idle CLI snapshots retain quota values and the callback input timestamp in stale views', () => {
  const idle = {...status, session_id: 'antigravity-cli:test-producer', session: {active: false}};
  const capturedAt = idle.captured_at;
  for (const language of ['ko', 'en']) {
    const config = {...settings, language};
    for (const elapsedMinutes of [10, 20]) {
      const later = new Date(now.getTime() + elapsedMinutes * 60_000);
      const bar = barToolViewModel([idle], 'antigravity', config, later);
      const panel = viewModelForTool([idle], 'antigravity', config, later);
      assert.equal(bar.state, 'stale');
      assert.equal(panel.state, 'stale');
      assert.match(bar.primary.text, /75%/);
      assert.match(bar.secondary.text, /90%/);
      assert.equal(panel.primary.value, '75%');
      assert.equal(panel.secondary.value, '90%');
      const record = bar.tooltip.split('\n').find(line => line.startsWith(`${t('tooltip.record', language)}:`));
      assert.ok(record?.includes(formatLocalDateTime(capturedAt, language)));
      assert.ok(record?.includes(t('tooltip.ago', language)));
      assert.ok(!record?.includes(t('tooltip.justNow', language)));
      assert.ok(bar.tooltip.includes(t('state.stale', language)));
      assert.ok(panel.context.includes(t('state.stale', language)));
      assert.equal(idle.captured_at, capturedAt);
    }
  }
});

test('Korean and English documentation distinguish Desktop refresh from event-only CLI collection', () => {
  const readme = readFileSync(new URL('../README.md', import.meta.url), 'utf8');
  const sections = [...readme.matchAll(/#### Antigravity Desktop\/CLI (?:한도|quotas)\r?\n([\s\S]*?)(?=\r?\n#### )/g)];
  assert.equal(sections.length, 2);
  for (const [, section] of sections) {
    assert.match(section, /agy/);
    assert.match(section, /statusline.*JSON/);
    assert.match(section, /native helper/);
    assert.match(section, /snapshot/);
    assert.match(section, /Node.*PATH/);
    assert.match(section, /spawn polling/);
    assert.match(section, /account API/);
    assert.match(section, /custom statusline.*stdout.*flags/);
    assert.match(section, /builtin.*`stack`.*`true`/);
    assert.match(section, /callback.*(?:입력|input).*시각|callback input time/);
    assert.match(section, /3p/);
    assert.match(section, /60초|60 seconds/);
    assert.match(section, /CLI 설치는 필수가 아닙니다|Installing the CLI is optional/);
    assert.match(section, /GUI가 꺼져 있을 때만 CLI|CLI is used only while the GUI is closed/);
    assert.match(section, /활성화된 상태로 Juice를 시작|starting Juice while it is enabled/);
    assert.match(section, /CLI settings 파일이 존재할 때만|only if the CLI settings file exists/);
    assert.match(section, /표시를 끄거나 Juice를 제거|Disabling it or uninstalling Juice/);
    assert.match(section, /자기 연동만 해제하고 기존 설정을 복원|removes only its own connection and restores the previous settings/);
    assert.match(section, /CLI는 재실행해야|Restart any CLI already open/);
    assert.match(section, /\/usage/);
    assert.match(section, /CLI 서버 조회를 강제하지|nor its force refresh forces a CLI server query/);
    assert.match(section, /마지막 상태값.*오래됨|last state values.*stale/);
    assert.match(section, /새로운 서버 조회를 보증하지|does not guarantee a new server query/);
    assert.match(section, /토큰 활동 잔디는 아직 포함하지|token activity is not included yet/);
  }
  assert.match(readme, /email·text·token.*원본 이벤트 JSON을 저장하지/);
  assert.match(readme, /stores no email, text, token, or raw event JSON/);
  assert.match(readme, /account scope hash·producer lifetime/);
  assert.match(readme, /account scope hash and producer lifetime/);
  assert.doesNotMatch(readme, /Antigravity IDE와 CLI는.*대상이 아닙니다|not the separate Antigravity IDE or CLI|Antigravity cannot yet be read while its app is closed|Antigravity's two model pools/);
});
