import assert from 'node:assert/strict';
import test from 'node:test';
import {readFileSync} from 'node:fs';
import {barToolViewModel} from './bar-state.js';
import {viewModelForTool} from './panel-state.js';
import {t} from './i18n.js';

test('desktop Claude keeps quota periods and identifies its source without a second login',()=>{
  const now=new Date('2026-09-28T03:00:00Z');
  const status={tool:'claude',session_id:'claude-desktop-usage',captured_at:now.toISOString(),approx:false,session:{active:true},
    primary:{label:'5h',used_percent:39},secondary:{label:'week',used_percent:7}};
  for(const language of ['ko','en'])for(const bar_mode of ['full','compact','dual','quad'])for(const display_basis of ['used','remaining']){
    const config={language,bar_mode,display_basis,show_claude:true};
    const vm=barToolViewModel([status],'claude',config,now,{collectionHealth:{claude:'ready'}});
    assert.equal(vm.primary.number,display_basis==='used'?'39':'61');
    assert.equal(vm.secondary.number,display_basis==='used'?'7':'93');
    assert.match(vm.tooltip,language==='ko'?/Claude 데스크톱 로그인/:/Claude desktop login/);
    const panel=viewModelForTool([status],'claude',config,now,{claude:'ready'});
    assert.equal(panel.exists,true);
    assert.match(t('help.claudeUsageAutoRefresh',language),language==='ko'?/추가 로그인이나 채팅 없이/:/without another sign-in/);
  }
});

test('desktop collection has no CLI, token refresh, or secret output path',()=>{
  const source=readFileSync(new URL('../src-tauri/src/claude_desktop.rs',import.meta.url),'utf8');
  const production=source.split('#[cfg(test)]')[0];
  assert.doesNotMatch(production,/Command::|\.spawn\(|println!|eprintln!|refresh_token|\/v1\/oauth\/token/);
  assert.match(production,/PROFILE_URL/);assert.match(production,/credentials_unchanged/);
  const lib=readFileSync(new URL('../src-tauri/src/lib.rs',import.meta.url),'utf8');
  assert.match(lib,/claude_status\.filter\(\|_\| !desktop_source\)/);
  assert.match(lib,/discard_failed_desktop_status\(&CLAUDE_USAGE_CACHE, status\)/);
});
