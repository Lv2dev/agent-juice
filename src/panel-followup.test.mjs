import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {viewModelForTool} from './panel-state.js';
import {buildActivityView} from './activity-state.js';
import {collectionIssue,panelCollectionIssue} from './collection-state.js';

test('overview keeps exact quota readable when the local Codex session record is stale',()=>{
  const vm=viewModelForTool([{tool:'codex',session_id:'rollout-session',approx:false,captured_at:'2026-09-01T00:00:00Z',session:{active:false},primary:null,secondary:{used_percent:69}}],'codex',{language:'ko'});
  assert.equal(vm.secondary.value,'31%');
  assert.equal(vm.meta,'로컬 세션 기록 오래됨');
  const css=readFileSync(new URL('./panel.css',import.meta.url),'utf8');
  assert.doesNotMatch(css,/\.tool-card[^}]*data-state="stale"[^}]*opacity:\s*0\.62/);
});

test('Codex account-only snapshots distinguish request failure from old local sessions',()=>{
  const status={tool:'codex',session_id:'app-server-account',approx:false,captured_at:'2026-09-01T00:00:00Z',session:{active:false},primary:null,secondary:{used_percent:69}};
  for(const language of ['ko','en']) {
    const vm=viewModelForTool([status],'codex',{language},undefined,{codex:'transient_error'});
    assert.equal(vm.secondary.value,'31%');
    assert.equal(vm.state,'stale');
    assert.match(vm.meta,language==='ko'?/네트워크/:/network/);
    assert.match(vm.meta,language==='ko'?/마지막 성공/:/last successful/);
    assert.doesNotMatch(vm.meta,/로컬|Local session|Claude/);
    for(const health of ['ready','unavailable','rate_limited','parse_error']) {
      const result=viewModelForTool([status],'codex',{language},undefined,{codex:health});
      assert.equal(result.secondary.value,'31%');
      assert.doesNotMatch(result.meta,/로컬|Local session|Claude/);
      assert.ok(result.meta.length>0);
    }
  }
  const missingSource=viewModelForTool([{...status,session_id:undefined}],'codex',{language:'ko'});
  assert.equal(missingSource.meta,'마지막 성공 값 표시 중');
  const freshButFailed=viewModelForTool([{...status,session:{active:true}}],'codex',{language:'ko'},undefined,{codex:'transient_error'});
  assert.equal(freshButFailed.state,'stale');
  const empty=viewModelForTool([],'codex',{language:'ko'},undefined,{codex:'transient_error'});
  assert.match(empty.emptyHint,/네트워크/);
  const loggedOut=viewModelForTool([status],'codex',{language:'ko'},undefined,{codex:'login_required'});
  assert.equal(loggedOut.exists,false);
  assert.equal(loggedOut.state,'login_required');
  const recovered=viewModelForTool([{...status,session:{active:true}}],'codex',{language:'ko'},undefined,{codex:'ready'});
  assert.equal(recovered.state,'live');
  assert.equal(recovered.meta,'');
  assert.equal(collectionIssue('codex','transient_error','ko'),null,'panel health messages must not change the taskbar contract');
  assert.deepEqual(panelCollectionIssue('claude','credentials_error','ko'),collectionIssue('claude','credentials_error','ko'));
});

test('Cursor activity failures remain partial even before account scope was established',()=>{
  const view=buildActivityView({days:[],local_partial:false,codex_account_scope:true,codex_partial:false,cursor_account_scope:false,cursor_partial:true,cursor_backfill_pending:false},{show_claude:false,show_codex:true,show_cursor:true},'all');
  assert.equal(view.partial,true);
});

test('update status shares the action row instead of a separate border-tight row',()=>{
  const html=readFileSync(new URL('./index.html',import.meta.url),'utf8');
  assert.match(html,/<div class="button-row update-actions">(?:(?!<\/div>)[\s\S])*id="update-check-status"/);
});
