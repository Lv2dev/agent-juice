import assert from 'node:assert/strict';
import test from 'node:test';
import {readFileSync} from 'node:fs';
import {viewModelForTool} from './panel-state.js';
const html=readFileSync(new URL('./index.html',import.meta.url),'utf8');
const css=readFileSync(new URL('./panel.css',import.meta.url),'utf8');

test('save status is outside view-specific panels and supports dismissal',()=>{
  const at=html.indexOf('id="settings-status"');
  assert.ok(at>0 && at<html.indexOf('id="panel-view-overview"'));
  assert.match(html,/data-dismiss-settings-error/);
});

test('overview hero chooses the limiting known period for both display bases',()=>{
  const status={tool:'claude',session:{active:true},primary:{used_percent:5},secondary:{used_percent:96}};
  for(const language of ['ko','en'])for(const display_basis of ['used','remaining']){
    const vm=viewModelForTool([status],'claude',{language,display_basis});
    assert.equal(vm.hero,vm.secondary);
    assert.equal(vm.hero.value,display_basis==='used'?'96%':'4%');
    const missing=viewModelForTool([{...status,secondary:{used_percent:null}}],'claude',{language,display_basis});
    assert.equal(missing.hero,missing.primary);
    const zero=viewModelForTool([{...status,primary:{used_percent:0},secondary:null}],'claude',{language,display_basis});
    assert.equal(zero.hero,zero.primary);
  }
});

test('heatmap preserves minimum cells while reset text can wrap',()=>{
  assert.match(css,/--activity-cell-min:/);
  assert.match(css,/--activity-cell-size:\s*clamp\(/);
  const reset=css.match(/\.metric \.reset\s*\{([^}]+)\}/)?.[1]??'';
  assert.match(reset,/white-space:\s*normal/);
  assert.match(reset,/grid-column:\s*1 \/ -1/);
});
