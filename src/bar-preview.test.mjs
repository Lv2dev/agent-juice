import assert from "node:assert/strict";
import test from "node:test";
import {previewSnapshot, createBarPreview, previewDisplaySettings} from "./bar-preview.js";
import {barViewModel} from "./bar-state.js";

test("unsaved colors use the renderer contract without waiting for a settings save",()=>{
  const draft={palette:'traffic',claude_primary_color:'#123456',claude_secondary_color:'#abcdef',
    tool_warning_color:'#112233',tool_warning_color_on:false,tool_danger_color:'#445566',tool_danger_color_on:false,
    info_text_color:'#667788',info_text_color_on:true,indicator_track_color:'#778899',indicator_track_color_auto:false};
  const snapshot=previewSnapshot(draft,[]);
  const vm=barViewModel(snapshot.statuses,snapshot.settings);
  assert.equal(vm.tools[0].primary.color,'#123456');assert.equal(vm.tools[0].secondary.color,'#abcdef');
  assert.equal(vm.infoTextColor,'#667788');assert.equal(vm.infoTextColorOn,true);
  assert.equal(vm.indicatorTrackColor,'#778899');assert.equal(vm.indicatorTrackColorAuto,false);
  assert.deepEqual(previewDisplaySettings({palette:'mono',mono_color:'#123456'}).palette,{Mono:[18,52,86]});
  assert.deepEqual(previewDisplaySettings({palette:'custom',custom_safe:'#123456',custom_warn:'#112233',custom_danger:'#445566'}).palette,{Custom:[[18,52,86],[17,34,51],[68,85,102]]});
  assert.equal(draft.tool_colors,undefined);
});

test("preview uses existing values, labels sample data, and never mutates saved visibility",()=>{
  const settings={show_cursor:false,bar_mode:'compact'};
  const status={tool:'cursor',primary:{used_percent:0},secondary:null,session:{active:true}};
  const live=previewSnapshot(settings,[status],'cursor',9);
  assert.equal(live.sample,false);
  assert.equal(live.statuses[0],status);
  assert.equal(live.settings.show_cursor,true);
  assert.equal(settings.show_cursor,false);
  assert.equal(live.textScale,2.25);
  const missing=previewSnapshot(settings,[],'grok');
  assert.equal(missing.sample,true);
  assert.equal(missing.statuses[0].secondary,null);
  assert.equal(previewSnapshot(settings,[],'unknown').tool,'claude');
});

test("preview controller loads lazily and accepts size only from its own frame",()=>{
  const messages=[],events={};
  const child={postMessage:(...args)=>messages.push(args)};
  const frame={style:{},contentWindow:child,addEventListener(){}};
  const picker={value:'claude',addEventListener(){}};
  const source={};
  const doc={querySelector:s=>({'[data-bar-preview]':frame,'[data-preview-tool]':picker,'[data-preview-source]':source}[s])};
  const host={location:{origin:'http://localhost'},addEventListener:(n,f)=>events[n]=f,removeEventListener(){}};
  const preview=createBarPreview(doc,host);
  preview.update({settings:{language:'en'},statuses:[]});
  assert.equal(frame.src,undefined);
  preview.setActive(true);
  assert.equal(frame.src,'bar.html?preview=1');
  assert.equal(messages.at(-1)[1],'http://localhost');
  assert.equal(source.textContent,'Sample');
  const send=(sender,origin,width)=>events.message({source:sender,origin,data:{type:'juice-bar-preview-size',width}});
  send({},'http://localhost',100);send(child,'https://invalid.test',100);send(child,'http://localhost',Infinity);
  assert.equal(frame.style.width,undefined);
  send(child,'http://localhost',123.5);assert.equal(frame.style.width,'124px');
  preview.setActive(false);assert.equal(messages.at(-1)[0].active,false);
});

test("embedded bar reuses real renderer without native calls or account collection",async()=>{
  const handlers={},docEvents={},calls=[],posts=[];
  const style={setProperty(){},removeProperty(){}};
  const root={dataset:{},style};const nodes=new Map();
  const tool={dataset:{},style,scrollWidth:180,setAttribute(){},removeAttribute(){},querySelector(s){
    if(!nodes.has(s))nodes.set(s,{style,textContent:''});return nodes.get(s);
  }};
  const parent={postMessage:(v)=>posts.push(v)};
  global.window={location:{search:'?preview=1',origin:'http://localhost'},parent,
    addEventListener:(n,f)=>handlers[n]=f,__TAURI__:{core:{invoke:(...args)=>calls.push(args)}}};
  global.document={documentElement:{dataset:{},style,removeAttribute(){}},addEventListener:(n,f)=>docEvents[n]=f,
    querySelector(s){if(s==='#bar')return root;if(s.includes('data-tool="claude"'))return tool;return null;}};
  try{
    await import(`./bar.js?preview-test=${Date.now()}`);
    assert.equal(calls.length,0);
    const payload=previewSnapshot({bar_mode:'dual'},[],'claude');
    handlers.message({source:{},origin:'http://localhost',data:payload});
    assert.equal(root.dataset.mode,undefined);
    handlers.message({source:parent,origin:'http://localhost',data:payload});
    docEvents.contextmenu({preventDefault(){}});docEvents.mouseover();
    await new Promise(r=>setTimeout(r,100));
    assert.equal(root.dataset.mode,'dual');assert.equal(nodes.get('.bar-worst').textContent,'35');
    assert.equal(posts.at(-1).width,180);
    assert.deepEqual(calls,[]);
    handlers.pagehide();
  }finally{delete global.window;delete global.document;}
});
