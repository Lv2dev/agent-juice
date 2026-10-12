import assert from 'node:assert/strict';
import test from 'node:test';
import vm from 'node:vm';
import { readFileSync } from 'node:fs';
import { DEFAULT_SETTINGS } from './panel-state.js';
import { barViewModel } from './bar-state.js';

const source = readFileSync(new URL('./bar.js', import.meta.url), 'utf8');
const tools = ['claude', 'codex', 'grok', 'cursor', 'antigravity'];
const body = name => source.match(new RegExp(`(?:async )?function ${name}\\b[\\s\\S]*?\\n}\\r?\\n`))?.[0] ?? '';

function fixture(tool, { delayedOpen = false, failedClose = false } = {}) {
  let resolveOpen;
  const menu = { hidden: true, style: { setProperty() {} } };
  const root = { dataset: {}, style: { setProperty() {} } };
  const items = Object.fromEntries(tools.map(name => [name, { hidden: true }]));
  const calls = [];
  const context = vm.createContext({ DEFAULT_SETTINGS, barViewModel, Date, menu,
    document: { documentElement: { clientHeight: 48, clientWidth: 400 }, querySelector: selector => selector === '#bar' ? root : selector === '#bar-menu' ? menu : null },
    window: { innerWidth: 400, innerHeight: 48 },
    toolElement: name => items[name], renderTool: model => { items[model.tool].hidden = false; },
    cancelRefreshMenuClose() {}, syncNativeTooltip() {}, scheduleTaskbarContentWidthSync() {},
    stopTooltipClock() {}, refreshTaskbarStatus() {}, emitSafely() {},
    setNativeMenuOpen(open) {
      calls.push(open);
      if (open && delayedOpen) return new Promise(resolve => { resolveOpen = resolve; });
      return !open && failedClose ? Promise.reject(new Error('synthetic close failure')) : Promise.resolve(true);
    },
  });
  for (const name of ['refreshMenu', 'clampMenuPosition', 'showRefreshMenu', 'hideRefreshMenu', 'setRefreshMenuState', 'renderBar']) assert(body(name), name);
  vm.runInContext(`
    const CURRENT_TOOL=${JSON.stringify(tool)}, PREVIEW=false, TOOLS=${JSON.stringify(tools)};
    const MENU_WIDTH=88,MENU_HEIGHT=28,MENU_MARGIN=4,REFRESH_MENU_OPENED_EVENT='opened';
    const MENU_STATES={CLOSED:'closed',OPENING:'opening',OPEN:'open',CLOSING:'closing'};
    let refreshMenuState='closed',refreshMenuRequest=0,refreshMenuDesiredOpen=false;
    let currentBarView=null,pendingNativeTooltip=null,lastNativeTooltip='';
    let statuses=[],collectionHealth={},startupStatusLoading=false;
    const systemTextScale={factor:1};
    let settings={...DEFAULT_SETTINGS,${tools.map(name => `show_${name}:true`).join(',')}};
    ${body('currentBarIsVisible')}
    ${['refreshMenu', 'clampMenuPosition', 'showRefreshMenu', 'hideRefreshMenu', 'setRefreshMenuState', 'renderBar'].map(body).join('\n')}
    globalThis.show=()=>showRefreshMenu({clientX:10,clientY:10});
    globalThis.visibility=(name,value)=>{settings['taskbar_show_'+name]=value;renderBar();};
    globalThis.inspect=()=>({state:refreshMenuState,desired:refreshMenuDesiredOpen,hidden:menu.hidden});
    renderBar();
  `, context);
  return { context, calls, items, resolveOpen: () => resolveOpen(true) };
}

test('hidden taskbar tools invalidate delayed menu opens and remain closed after re-enabling', async () => {
  for (const tool of tools) {
    const f = fixture(tool, { delayedOpen: true });
    const pending = f.context.show();
    f.context.visibility(tool, false);
    assert(f.items[tool].hidden);
    f.resolveOpen();
    await pending;
    await Promise.resolve();
    f.context.visibility(tool, true);
    assert.equal(f.context.inspect().hidden, true, tool);
    assert.equal(f.context.inspect().state, 'closed', tool);
    assert.equal(f.context.inspect().desired, false, tool);
  }
});

test('already-open menus close locally on hiding even if native close fails', async () => {
  for (const tool of tools) {
    const f = fixture(tool, { failedClose: true });
    await f.context.show();
    f.context.visibility(tool, false);
    await Promise.resolve();
    assert.equal(f.context.inspect().state, 'closed', tool);
    assert.equal(f.context.inspect().hidden, true, tool);
    const opens = f.calls.filter(Boolean).length;
    await f.context.show();
    assert.equal(f.calls.filter(Boolean).length, opens, 'hidden tools must not open native menus');
  }
});

test('hiding another tool does not dismiss the current tool menu', async () => {
  for (const tool of tools) {
    const f = fixture(tool);
    await f.context.show();
    const other = tools[(tools.indexOf(tool) + 1) % tools.length];
    f.context.visibility(other, false);
    assert.equal(f.context.inspect().hidden, false, tool);
    assert.equal(f.context.inspect().state, 'open', tool);
    assert.deepEqual(f.calls, [true]);
  }
});
