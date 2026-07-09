import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const css = readFileSync(resolve(here, "styles.css"), "utf8").replace(/\r\n?/g, "\n");
const panelMarkup = readFileSync(resolve(here, "index.html"), "utf8").replace(/\r\n?/g, "\n");
const panelJs = readFileSync(resolve(here, "panel.js"), "utf8").replace(/\r\n?/g, "\n");
const settingsJs = readFileSync(resolve(here, "settings.js"), "utf8").replace(/\r\n?/g, "\n");
const tauriConfig = JSON.parse(
  readFileSync(resolve(here, "../src-tauri/tauri.conf.json"), "utf8"),
);
const capabilitiesDir = resolve(here, "../src-tauri/capabilities");
const capabilities = readdirSync(capabilitiesDir)
  .filter((name) => name.endsWith(".json"))
  .map((name) => JSON.parse(readFileSync(resolve(capabilitiesDir, name), "utf8")));
const rustLib = readFileSync(resolve(here, "../src-tauri/src/lib.rs"), "utf8");
const rustConfig = readFileSync(resolve(here, "../src-tauri/src/config.rs"), "utf8");
const releaseVerifier = readFileSync(
  resolve(here, "../.ai/scripts/verify-release-installer.ps1"),
  "utf8",
);
const taskbarMoveVerifier = readFileSync(
  resolve(here, "../.ai/scripts/verify-taskbar-native-move.ps1"),
  "utf8",
);
const statuslineVerifier = readFileSync(
  resolve(here, "../.ai/scripts/verify-statusline-bridge.ps1"),
  "utf8",
);
const barMarkup = readFileSync(resolve(here, "bar.html"), "utf8").replace(/\r\n?/g, "\n");

function cssBlock(selector) {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = css.match(new RegExp(`(?:^|\\n)${escaped}\\s*\\{(?<body>[^}]+)\\}`));
  return match?.groups?.body ?? "";
}

function markupSection(name) {
  const match = panelMarkup.match(
    new RegExp(
      `<fieldset class="settings-section" data-settings-section="${name}">(?<body>[\\s\\S]*?)</fieldset>`,
    ),
  );
  return match?.groups?.body ?? "";
}

test("styles define the BDO-lite surface tokens used by Juice", () => {
  for (const token of [
    "--glass",
    "--surface",
    "--surface-2",
    "--field",
    "--line",
    "--hi",
    "--accent-warm",
  ]) {
    assert.match(css, new RegExp(`${token}:`));
  }

  const card = cssBlock(".tool-card");
  assert.match(card, /backdrop-filter: blur\(18px\)/);
  assert.match(card, /inset 0 1px 0 var\(--hi\)/);
  assert.match(card, /border-radius: var\(--radius\)/);
});

test("styles default to system theme and allow explicit light or dark overrides", () => {
  const root = cssBlock(":root");
  const light = cssBlock('html[data-theme="light"]');
  const dark = cssBlock('html[data-theme="dark"]');

  assert.match(root, /color-scheme: light dark/);
  assert.match(css, /@media \(prefers-color-scheme: dark\)/);
  assert.match(css, /html\[data-theme="light"\]/);
  assert.match(css, /html\[data-theme="dark"\]/);
  assert.match(root, /--accent: #667a62/);
  assert.match(root, /--accent-strong: #aab8a5/);
  assert.match(light, /--accent: #667a62/);
  assert.match(dark, /--accent: #9fb39b/);
  assert.match(dark, /--accent-strong: #cad7c5/);
  assert.doesNotMatch(css, /--accent:\s*#60a5fa/);
  assert.doesNotMatch(css, /--accent:\s*#7cc7ff/);
  assert.doesNotMatch(css, /--accent:\s*#4f7ea8/);
  assert.doesNotMatch(css, /--accent:\s*#79a8cf/);
  assert.doesNotMatch(css, /--accent:\s*#5f8580/);
  assert.doesNotMatch(css, /--accent:\s*#91bdb8/);
  assert.doesNotMatch(css, /--accent-strong:\s*#91b3ae/);
  assert.doesNotMatch(css, /--accent-strong:\s*#c0d8d3/);
});

test("font mode defaults to the Windows status area font with a Pretendard override", () => {
  const root = cssBlock(":root");
  const pretendard = cssBlock('html[data-font-mode="pretendard"]');

  assert.match(root, /--ui-font:/);
  assert.match(root, /Segoe UI Variable Text/);
  assert.match(root, /font-family:\s*var\(--ui-font\)/);
  assert.match(pretendard, /Pretendard/);
  assert.match(panelMarkup, /name="font_mode"/);
  assert.match(rustConfig, /font_mode/);
  assert.match(rustConfig, /default_font_mode/);
});

test("styles keep the AppBar contract stable", () => {
  const root = cssBlock("html.bar-root");
  const windowBlock = cssBlock(".bar-window");
  const shell = cssBlock(".bar-shell");
  const tool = cssBlock(".bar-tool");
  const barWindows = tauriConfig.app.windows.filter((item) => item.label?.startsWith("bar-"));

  assert.match(root, /background: transparent/);
  assert.match(root, /overflow: hidden/);
  assert.match(windowBlock, /width: 100vw/);
  assert.match(windowBlock, /height: 100%/);
  assert.match(windowBlock, /background: transparent/);
  assert.match(shell, /height: 100%/);
  assert.match(shell, /background: transparent/);
  assert.match(tool, /height: 100%/);
  assert.match(tool, /min-width: 0/);
  assert.deepEqual(
    barWindows.map((item) => item.label).sort(),
    ["bar-claude", "bar-codex"],
  );
  for (const barWindowConfig of barWindows) {
    assert.equal(barWindowConfig?.title, "Juice Bar");
    assert.equal(barWindowConfig?.transparent, true);
    assert.equal(barWindowConfig?.shadow, false);
    assert.match(barWindowConfig?.url ?? "", /bar\.html\?tool=(claude|codex)/);
  }
  const capabilityWindows = capabilities.flatMap((item) => item.windows ?? []);
  assert.ok(capabilityWindows.includes("bar-claude"));
  assert.ok(capabilityWindows.includes("bar-codex"));
  assert.ok(!capabilityWindows.includes("bar"));
});

test("panel window uses integrated custom chrome and balanced scrolling", () => {
  const windowBlock = cssBlock(".panel-window");
  const frame = cssBlock(".panel-frame");
  const chrome = cssBlock(".panel-chrome");
  const controls = cssBlock(".window-controls");
  const dot = cssBlock(".chrome-dot");
  const shell = cssBlock(".panel-shell");
  const scrollbar = cssBlock(".panel-shell::-webkit-scrollbar");
  const track = cssBlock(".panel-shell::-webkit-scrollbar-track");
  const thumb = cssBlock(".panel-shell::-webkit-scrollbar-thumb");
  const panelWindowConfig = tauriConfig.app.windows.find((item) => item.label === "panel");

  assert.equal(panelWindowConfig?.title, "Juice");
  assert.ok(panelWindowConfig?.width >= 560);
  assert.ok(panelWindowConfig?.height >= 680);
  assert.ok(panelWindowConfig?.minWidth >= 480);
  assert.ok(panelWindowConfig?.minHeight >= 560);
  assert.equal(panelWindowConfig?.decorations, false);
  assert.equal(panelWindowConfig?.resizable, true);
  assert.equal(panelWindowConfig?.visible, false);
  assert.match(rustLib, /CloseRequested/);
  assert.match(rustLib, /prevent_close\(\)/);
  assert.match(rustLib, /\.hide\(\)/);
  assert.match(rustLib, /fn minimize_panel\(window: tauri::Window\)/);
  assert.match(rustLib, /fn toggle_panel_maximized\(window: tauri::Window\)/);
  assert.match(rustLib, /fn hide_panel_window\(window: tauri::Window\)/);
  assert.match(rustLib, /window\.minimize\(\)/);
  assert.match(rustLib, /window\.is_maximized\(\)/);
  assert.match(rustLib, /window\.maximize\(\)/);
  assert.match(rustLib, /window\.unmaximize\(\)/);

  assert.match(panelMarkup, /class="panel-frame"/);
  assert.match(panelMarkup, /class="panel-chrome"[^>]*data-tauri-drag-region/);
  assert.match(
    panelMarkup,
    /class="chrome-spacer"[\s\S]*class="chrome-title"[\s\S]*class="window-controls"/,
  );
  assert.match(panelMarkup, /data-window-action="close"/);
  assert.match(panelMarkup, /data-window-action="minimize"/);
  assert.match(panelMarkup, /data-window-action="toggle-maximize"/);
  assert.match(
    panelMarkup,
    /data-window-action="minimize"[\s\S]*data-window-action="toggle-maximize"[\s\S]*data-window-action="close"/,
  );
  assert.match(panelJs, /data-window-action/);
  assert.match(panelJs, /start_panel_drag/);
  assert.match(panelJs, /minimize_panel/);
  assert.match(panelJs, /toggle_panel_maximized/);
  assert.match(panelJs, /hide_panel_window/);
  const panelCapability = capabilities.find((item) => item.windows?.includes("panel"));
  assert.ok(panelCapability?.permissions.includes("core:window:allow-start-dragging"));
  assert.match(rustLib, /fn start_panel_drag\(window: tauri::Window\)/);
  assert.match(rustLib, /window\.start_dragging\(\)/);
  assert.match(rustLib, /start_panel_drag,/);
  assert.match(windowBlock, /width: 100vw/);
  assert.match(windowBlock, /height: 100vh/);
  assert.match(frame, /height: 100vh/);
  assert.match(frame, /display: flex/);
  assert.match(frame, /flex-direction: column/);
  assert.match(frame, /overflow: hidden/);
  assert.match(chrome, /height: 36px/);
  assert.match(chrome, /display: grid/);
  assert.match(chrome, /grid-template-columns: 96px minmax\(0,\s*1fr\) 96px/);
  assert.match(controls, /display: flex/);
  assert.match(controls, /justify-self: end/);
  assert.match(dot, /border-radius: 999px/);
  assert.doesNotMatch(windowBlock, /width: 360px/);
  assert.doesNotMatch(windowBlock, /height: 480px/);
  assert.match(shell, /flex: 1 1 auto/);
  assert.match(shell, /overflow-x: hidden/);
  assert.match(shell, /overflow-y: auto/);
  assert.match(shell, /scrollbar-gutter: stable both-edges/);
  assert.match(scrollbar, /width: 12px/);
  assert.match(track, /background: transparent/);
  assert.match(thumb, /background:/);
  assert.match(thumb, /background-clip: padding-box/);
  assert.match(css, /@media \(min-width: 640px\)/);
});

test("panel removes session-specific context from the settings window", () => {
  assert.doesNotMatch(panelMarkup, /class="ctx"/);
  assert.doesNotMatch(panelMarkup, /컨텍스트/);
});

test("taskbar overlay has no logo, permanent dimming, card border, card background, or hover lift", () => {
  const tool = cssBlock(".bar-tool");
  const stale = cssBlock('.bar-tool[data-state="stale"],\n.bar-tool[data-state="empty"]');
  const hover = cssBlock(".bar-tool:hover");
  const warn = cssBlock('.bar-tool[data-severity="warn"]');
  const danger = cssBlock('.bar-tool[data-severity="danger"]');

  assert.equal(cssBlock(".bar-brand"), "");
  assert.match(tool, /border: 0/);
  assert.match(tool, /background: transparent/);
  assert.match(tool, /box-shadow: none/);
  assert.doesNotMatch(stale, /opacity:\s*0\.[0-8]/);
  assert.doesNotMatch(stale, /filter:/);
  assert.doesNotMatch(hover, /background:/);
  assert.doesNotMatch(hover, /transform:/);
  assert.doesNotMatch(warn, /border-color:/);
  assert.doesNotMatch(danger, /border-color:/);
});

test("taskbar overlay lays out tools inside the dock width without horizontal clipping", () => {
  const shell = cssBlock(".bar-shell");
  const tool = cssBlock(".bar-tool");
  const copy = cssBlock(".bar-copy");
  const fullTools = cssBlock('.bar-shell[data-mode="full"] .bar-tool,\n.bar-shell[data-mode="compact"] .bar-tool');

  assert.match(shell, /display: grid/);
  assert.match(shell, /grid-auto-flow: column/);
  assert.match(shell, /grid-auto-columns: minmax\(0,\s*1fr\)/);
  assert.match(tool, /overflow: hidden/);
  assert.match(copy, /overflow: hidden/);
  assert.doesNotMatch(fullTools, /320px/);
  assert.doesNotMatch(css, /--tool-gap/);
  assert.doesNotMatch(panelMarkup, /tool_gap_px/);
  assert.doesNotMatch(panelMarkup, /tool-gap-output/);
});

test("taskbar overlay preserves core status text instead of ellipsizing reset text", () => {
  const copy = cssBlock(".bar-copy");
  const toolName = cssBlock(".bar-tool-name");
  const line = cssBlock(".bar-line");
  const primaryText = cssBlock(".primary-text");
  const secondaryText = cssBlock(".secondary-text");
  const reset = cssBlock(".primary-reset,\n.secondary-reset");
  const fullResets = cssBlock('.bar-shell[data-mode="full"] .primary-reset,\n.bar-shell[data-mode="full"] .secondary-reset');
  const compactResets = cssBlock('.bar-shell[data-mode="compact"] .primary-reset,\n.bar-shell[data-mode="compact"] .secondary-reset');

  assert.match(copy, /grid-template-columns: max-content max-content max-content/);
  for (const block of [toolName, line, primaryText, secondaryText]) {
    assert.doesNotMatch(block, /text-overflow:\s*ellipsis/);
  }
  assert.match(reset, /display: none/);
  assert.equal(fullResets, "");
  assert.equal(compactResets, "");
});

test("taskbar overlay implements the four documented bar display modes", () => {
  const quad = cssBlock(".bar-quad");
  const quadMode = cssBlock('.bar-shell[data-mode="quad"] .bar-quad');
  const quadPrimary = cssBlock(".quad-primary");
  const quadSecondary = cssBlock(".quad-secondary");
  const compactToolName = cssBlock('.bar-shell[data-mode="compact"] .bar-tool-name');
  const compactLimitText = cssBlock('.bar-shell[data-mode="compact"] .primary-text,\n.bar-shell[data-mode="compact"] .secondary-text');
  const dualCopy = cssBlock('.bar-shell[data-mode="dual"] .bar-copy');
  const quadCopy = cssBlock('.bar-shell[data-mode="quad"] .bar-copy,\n.bar-shell[data-mode="quad"] .bar-ring');
  const ringOff = cssBlock('.bar-shell[data-ring="off"][data-mode="full"] .bar-ring,\n.bar-shell[data-ring="off"][data-mode="compact"] .bar-ring');
  const compactTool = cssBlock('.bar-shell[data-mode="compact"] .bar-tool');
  const dualTool = cssBlock('.bar-shell[data-mode="dual"] .bar-tool');
  const quadTool = cssBlock('.bar-shell[data-mode="quad"] .bar-tool');

  assert.match(compactToolName, /display: none/);
  assert.equal(compactLimitText, "");
  assert.match(dualCopy, /display: none/);
  assert.match(quad, /display: none/);
  assert.match(quadMode, /display: flex/);
  assert.match(quadCopy, /display: none/);
  assert.match(quadPrimary, /--quad-color: var\(--primary-color\)/);
  assert.match(quadSecondary, /--quad-color: var\(--secondary-color\)/);
  assert.match(ringOff, /display: none/);
  assert.equal(cssBlock('.bar-shell[data-ring="off"] .bar-ring'), "");
  for (const block of [compactTool, dualTool, quadTool]) {
    assert.match(block, /justify-content: flex-start/);
  }
});

test("dual ring center number stays inside the ring hole", () => {
  const ring = cssBlock('.bar-shell[data-mode="dual"] .bar-ring');
  const worst = cssBlock(".bar-worst");
  const outer = cssBlock(".outer-ring");
  const inner = cssBlock(".inner-ring");
  const quadRing = cssBlock(".quad-ring");
  const shell = cssBlock(".bar-shell");

  assert.match(shell, /--ring-center-gap: 0px/);
  assert.match(shell, /--ring-visible-thickness:/);
  assert.match(ring, /width: var\(--ring-size\)/);
  assert.match(ring, /height: var\(--ring-size\)/);
  assert.match(ring, /flex-basis: var\(--ring-size\)/);
  assert.match(outer, /var\(--ring-visible-thickness\)/);
  assert.match(inner, /inset: var\(--ring-gap\)/);
  assert.match(inner, /var\(--ring-visible-thickness\)/);
  assert.match(quadRing, /width: var\(--ring-size\)/);
  assert.match(quadRing, /height: var\(--ring-size\)/);
  assert.match(quadRing, /var\(--ring-visible-thickness\)/);
  assert.match(worst, /font-size: var\(--ring-number-font-size\)/);
  assert.match(worst, /font-weight: var\(--ring-number-font-weight\)/);
  assert.match(worst, /min-width: 18px/);
  assert.match(worst, /text-align: center/);
});

test("taskbar ring number visibility and outline are configurable", () => {
  const numbersOff = cssBlock('.bar-shell[data-ring-numbers="off"] .bar-worst');
  const outlineOn = cssBlock('.bar-shell[data-number-outline="on"] .bar-worst');

  assert.match(panelMarkup, /name="fullscreen_hide_on"/);
  assert.match(panelMarkup, /name="maximized_hide_on"/);
  assert.match(panelMarkup, />전체창 숨김</);
  assert.match(rustConfig, /maximized_hide_on/);
  assert.match(rustLib, /visible_windows_coverage/);
  assert.match(panelMarkup, /name="indicator_style"/);
  assert.match(panelMarkup, /name="limit_order"/);
  assert.match(panelMarkup, />한도 순서</);
  assert.match(rustConfig, /limit_order/);
  assert.match(panelMarkup, /name="ring_numbers_on"/);
  assert.match(panelMarkup, /name="ring_number_outline_on"/);
  assert.match(panelMarkup, /name="ring_size_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="ring_thickness_px"/);
  assert.match(panelMarkup, /name="ring_thickness_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="ring_gap_px"/);
  assert.match(panelMarkup, /name="ring_gap_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="ring_center_gap_px"/);
  assert.match(panelMarkup, /name="ring_center_gap_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="ring_number_font_size_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="bar_text_font_size_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="ring_number_font_weight"/);
  assert.match(panelMarkup, /name="bar_text_font_weight"/);
  assert.match(rustConfig, /ring_center_gap_px/);
  assert.match(numbersOff, /display: none/);
  assert.match(outlineOn, /-webkit-text-stroke:/);
  assert.match(outlineOn, /paint-order: stroke fill/);
  assert.doesNotMatch(outlineOn, /text-shadow:/);
});

test("taskbar indicator can switch from rings to stacked horizontal bars", () => {
  const bars = cssBlock(".bar-bars");
  const limitBar = cssBlock(".limit-bar");
  const indicatorBars = cssBlock('.bar-shell[data-indicator="bar"] .bar-bars');
  const hiddenRings = cssBlock('.bar-shell[data-indicator="bar"] .bar-ring,\n.bar-shell[data-indicator="bar"] .bar-quad');
  const primary = cssBlock(".limit-bar.primary-limit");
  const secondary = cssBlock(".limit-bar.secondary-limit");

  assert.match(barMarkup, /class="bar-bars"/);
  assert.match(bars, /display: none/);
  assert.match(bars, /width: var\(--ring-size\)/);
  assert.match(bars, /gap: var\(--ring-gap\)/);
  assert.match(limitBar, /height: var\(--ring-thickness\)/);
  assert.match(limitBar, /background: linear-gradient/);
  assert.match(primary, /--limit-color: var\(--primary-color\)/);
  assert.match(primary, /--limit-percent: var\(--primary-percent\)/);
  assert.match(secondary, /--limit-color: var\(--secondary-color\)/);
  assert.match(secondary, /--limit-percent: var\(--secondary-percent\)/);
  assert.match(indicatorBars, /display: flex/);
  assert.match(hiddenRings, /display: none/);
});

test("taskbar text weight stays close to Windows status text", () => {
  for (const selector of [".bar-tool-name", ".bar-line", ".bar-worst"]) {
    const block = cssBlock(selector);
    assert.doesNotMatch(block, /font-weight:\s*(7|8|9)\d\d/);
  }
});

test("taskbar overlay only shows a move outline while dragging", () => {
  const shell = cssBlock(".bar-shell");
  const dragging = cssBlock('.bar-shell[data-dragging="true"]');
  const rootDragging = cssBlock('html.bar-root .bar-shell[data-dragging="true"]');

  assert.doesNotMatch(shell, /outline:/);
  assert.match(dragging || rootDragging, /outline:/);
});

test("taskbar overlay has no blur, filter, text shadow, transition, or animation effects", () => {
  const windowBlock = cssBlock(".bar-window");
  const shell = cssBlock(".bar-shell");
  const tool = cssBlock(".bar-tool");
  const liveRing = cssBlock('.bar-tool[data-state="live"] .bar-ring');
  const ring = cssBlock(".bar-ring");
  const quad = cssBlock(".bar-quad");
  const quadRing = cssBlock(".quad-ring");
  const worst = cssBlock(".bar-worst");
  const barBlocks = [windowBlock, shell, tool, liveRing, ring, quad, quadRing, worst].join("\n");

  assert.doesNotMatch(barBlocks, /backdrop-filter:/);
  assert.doesNotMatch(barBlocks, /-webkit-backdrop-filter:/);
  assert.doesNotMatch(barBlocks, /filter:/);
  assert.doesNotMatch(barBlocks, /text-shadow:/);
  assert.doesNotMatch(shell, /animation:/);
  assert.doesNotMatch(tool, /transition:/);
  assert.doesNotMatch(liveRing, /animation:/);
  assert.doesNotMatch(css, /@keyframes bar-in/);
});

test("styles include restrained panel motion with a reduced-motion escape hatch", () => {
  const settingsCardOpen = cssBlock(".settings-card[open] .settings-form");
  const settingsAdvancedOpen = cssBlock(".settings-advanced[open] .settings-advanced-grid");

  assert.match(css, /@keyframes panel-in/);
  assert.match(css, /@keyframes live-breathe/);
  assert.match(css, /@keyframes settings-expand/);
  assert.match(settingsCardOpen, /animation:\s*settings-expand/);
  assert.match(settingsAdvancedOpen, /animation:\s*settings-expand/);
  assert.match(css, /@media \(prefers-reduced-motion: reduce\)/);
});

test("settings controls have modern glassy native-control replacements", () => {
  const card = cssBlock(".settings-card");
  const cardTint = cssBlock(".settings-card::before");
  const toolTint = cssBlock(".tool-card::before");
  const form = cssBlock(".settings-form");
  const section = cssBlock(".settings-section");
  const legend = cssBlock(".settings-section legend");
  const summaryAccent = cssBlock(".settings-card summary span::before");
  const rowSurface = cssBlock(".field-row,\n.range-row,\n.toggle-row,\n.swatch-row,\n.field-grid label");
  const select =
    [...css.matchAll(/(?:^|\n)select\s*\{(?<body>[^}]+)\}/g)].map((match) => match.groups.body)
      .at(-1) ?? "";
  const range = cssBlock('input[type="range"]');
  const checkbox = cssBlock('input[type="checkbox"]');
  const checkboxKnob = cssBlock('input[type="checkbox"]::before');
  const checkedToggle = cssBlock('input[type="checkbox"]:checked');
  const checkedToggleKnob = cssBlock('input[type="checkbox"]:checked::before');
  const toggleInput = cssBlock('.toggle-row input[type="checkbox"]');
  const toggleSpan = cssBlock(".toggle-row span");
  const advanced = cssBlock(".settings-advanced");
  const advancedSummary = cssBlock(".settings-advanced summary");
  const advancedGrid = cssBlock(".settings-advanced-grid");
  const rangeTrack = css.match(
    /input\[type="range"\]::-webkit-slider-runnable-track\s*\{(?<body>[^}]+)\}/,
  )?.groups?.body ?? "";
  const thumb = css.match(/input\[type="range"\]::-webkit-slider-thumb\s*\{(?<body>[^}]+)\}/)
    ?.groups?.body ?? "";
  const focusThumb = css.match(
    /input\[type="range"\]:focus-visible::-webkit-slider-thumb\s*\{(?<body>[^}]+)\}/,
  )?.groups?.body ?? "";
  const button = cssBlock("button");
  const buttonHover = cssBlock("button:hover:not(:disabled)");

  assert.match(card, /backdrop-filter: blur\(18px\) saturate\(1\.08\)/);
  assert.match(card, /var\(--surface\) 94%/);
  assert.match(card, /var\(--shadow-soft\)/);
  assert.match(card, /inset 0 1px 0 var\(--hi\)/);
  assert.doesNotMatch(card, /var\(--glass\) 88%/);
  assert.doesNotMatch(card, /blur\(32px\)/);
  assert.match(toolTint, /background: linear-gradient\(90deg/);
  assert.match(toolTint, /opacity: 0\.34/);
  assert.match(cardTint, /background: linear-gradient\(90deg/);
  assert.match(cardTint, /color-mix\(in srgb, var\(--accent\) 14%, transparent\)/);
  assert.match(cardTint, /opacity: 0\.34/);
  assert.doesNotMatch(cardTint, /display: none/);
  assert.match(summaryAccent, /background: var\(--accent\)/);
  assert.doesNotMatch(summaryAccent, /var\(--accent-warm\)/);
  assert.match(form, /grid-template-columns: minmax\(0,\s*1fr\)/);
  assert.match(section, /border-top: 1px solid/);
  assert.doesNotMatch(section, /box-shadow:/);
  assert.doesNotMatch(legend, /position: sticky/);
  assert.doesNotMatch(legend, /z-index:/);
  assert.match(rowSurface, /background: color-mix\(in srgb, var\(--surface\) 24%, transparent\)/);
  assert.match(rowSurface, /border: 1px solid color-mix\(in srgb, var\(--line\) 42%, transparent\)/);
  assert.match(rowSurface, /box-shadow: none/);
  assert.doesNotMatch(rowSurface, /backdrop-filter/);
  assert.doesNotMatch(rowSurface, /inset 0 1px/);
  assert.match(select, /appearance: none/);
  assert.match(select, /backdrop-filter: blur\(1[0-9]px\)/);
  assert.match(select, /box-shadow: none/);
  assert.doesNotMatch(select, /inset 0/);
  assert.match(range, /appearance: none/);
  assert.match(range, /--range-progress:/);
  assert.match(range, /background:\s*transparent/);
  assert.match(range, /height: 24px/);
  assert.match(rangeTrack, /linear-gradient\(90deg/);
  assert.match(rangeTrack, /var\(--range-progress\)/);
  assert.match(rangeTrack, /backdrop-filter: blur\(1[0-9]px\)/);
  assert.match(rangeTrack, /box-shadow: none/);
  assert.doesNotMatch(rangeTrack, /inset 0/);
  assert.match(thumb, /backdrop-filter: blur\(1[0-9]px\)/);
  assert.match(thumb, /box-shadow: none/);
  assert.doesNotMatch(thumb, /radial-gradient/);
  assert.doesNotMatch(thumb, /linear-gradient/);
  assert.match(focusThumb, /box-shadow: none/);
  assert.match(checkbox, /border-radius: 999px/);
  assert.match(checkbox, /--toggle-track-off:/);
  assert.match(checkbox, /--toggle-knob-off:/);
  assert.match(checkbox, /backdrop-filter: blur\(1[0-9]px\)/);
  assert.match(checkbox, /box-shadow: none/);
  assert.match(checkbox, /background: var\(--toggle-track-off\)/);
  assert.doesNotMatch(checkbox, /var\(--ok\)/);
  assert.match(checkboxKnob, /background: var\(--toggle-knob-off\)/);
  assert.match(checkedToggle, /--toggle-knob-on:/);
  assert.match(checkedToggle, /var\(--ok\)/);
  assert.doesNotMatch(checkedToggle, /var\(--accent\)/);
  assert.doesNotMatch(checkedToggle, /linear-gradient/);
  assert.match(checkedToggleKnob, /background: var\(--toggle-knob-on\)/);
  assert.match(toggleInput, /order: 2/);
  assert.match(toggleSpan, /order: 1/);
  assert.match(advanced, /grid-column:\s*1 \/ -1/);
  assert.match(advanced, /border-top:/);
  assert.doesNotMatch(advanced, /background:/);
  assert.doesNotMatch(advanced, /box-shadow:/);
  assert.match(advancedSummary, /cursor:\s*pointer/);
  assert.match(advancedGrid, /display:\s*grid/);
  assert.match(button, /backdrop-filter: blur\(1[0-9]px\)/);
  assert.match(button, /box-shadow: none/);
  assert.doesNotMatch(button, /linear-gradient/);
  assert.doesNotMatch(buttonHover, /transform:/);
  assert.doesNotMatch(panelMarkup, /type="submit"/);
  assert.doesNotMatch(panelMarkup, />저장</);
});

test("settings form groups controls into logical sections without changing field names", () => {
  const sections = [
    ...panelMarkup.matchAll(/<fieldset class="settings-section" data-settings-section="([^"]+)">/g),
  ].map((match) => match[1]);
  const indicatorSection = markupSection("indicator");

  assert.deepEqual(sections, ["appearance", "limits", "taskbar", "indicator", "system"]);
  assert.match(panelMarkup, /<details class="settings-card" open>/);

  assert.match(markupSection("appearance"), /name="theme"[\s\S]*name="font_mode"[\s\S]*name="palette"/);
  assert.match(markupSection("limits"), /name="warn_threshold"[\s\S]*name="danger_threshold"[\s\S]*name="poll_interval_secs"[\s\S]*name="stale_after_secs"/);
  assert.match(markupSection("taskbar"), /name="bar_mode"[\s\S]*name="limit_order"[\s\S]*name="indicator_style"[\s\S]*name="show_claude"[\s\S]*name="show_codex"/);
  assert.match(indicatorSection, /name="ring_on"[\s\S]*name="ring_numbers_on"[\s\S]*name="ring_number_outline_on"[\s\S]*<details class="settings-advanced" data-settings-advanced="indicator">/);
  assert.doesNotMatch(indicatorSection, /<details class="settings-advanced"[^>]*open/);
  assert.match(indicatorSection, /<summary>고급 조정<\/summary>/);
  assert.match(indicatorSection, /<details class="settings-advanced" data-settings-advanced="indicator">[\s\S]*name="ring_size_px"[\s\S]*name="bar_text_font_weight"[\s\S]*<\/details>/);
  assert.match(markupSection("system"), /name="autostart_on"[\s\S]*data-action="restore-statusline"[\s\S]*id="settings-status"/);

  assert.match(panelMarkup, /name="claude_taskbar_offset_ratio"/);
  assert.match(panelMarkup, /name="codex_taskbar_offset_ratio"/);
});

test("settings copy uses accurate collection timing labels and exposes Claude connect action", () => {
  assert.match(panelMarkup, />수집주기</);
  assert.match(panelMarkup, />오래됨 기준</);
  assert.doesNotMatch(panelMarkup, />신선도</);
  assert.doesNotMatch(panelMarkup, /data-action="install-statusline"/);
  assert.match(panelMarkup, /data-action="connect-statusline"/);
  assert.match(panelMarkup, />Claude 연결</);
  assert.match(panelMarkup, /data-action="restore-statusline"/);
  assert.match(settingsJs, /install_statusline/);
});

test("panel cost estimate meta aligns as a full-width card footer", () => {
  const meta = cssBlock(".meta");

  assert.match(meta, /width:\s*100%/);
  assert.match(meta, /justify-self:\s*stretch/);
  assert.match(meta, /text-align:\s*right/);
  assert.doesNotMatch(meta, /max-width:/);
});

test("styles avoid decorative one-off effects and viewport font scaling", () => {
  assert.doesNotMatch(css, /letter-spacing:\s*-/);
  assert.doesNotMatch(css, /font-size:\s*[^;]*vw/);
  assert.doesNotMatch(css, /\b(orb|blob|bokeh)\b/i);
});

test("release installer verifier stops temporary app processes before cleanup", () => {
  assert.match(releaseVerifier, /agent-juice-verify-install/);
  assert.match(releaseVerifier, /Stop-Process/);
  assert.match(releaseVerifier, /agent-juice\.exe/);
});

test("release installer verifier restores installer registry state after temp install checks", () => {
  assert.match(releaseVerifier, /HKCU:\\Software\\pointi\\Juice/);
  assert.match(releaseVerifier, /HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Juice/);
  assert.match(releaseVerifier, /Backup-InstallerRegistryState/);
  assert.match(releaseVerifier, /Restore-InstallerRegistryState/);
  assert.doesNotMatch(releaseVerifier, /DeleteSubKeyTree/);
});

test("release installer verifier protects real app processes, Run key, and shortcuts", () => {
  assert.match(releaseVerifier, /Assert-NoNonTempAppProcess/);
  assert.match(releaseVerifier, /not under temp install dir/i);
  assert.match(releaseVerifier, /CurrentVersion\\Run/);
  assert.match(releaseVerifier, /Backup-RegistryNamedValues/);
  assert.match(releaseVerifier, /Restore-RegistryNamedValues/);
  assert.match(releaseVerifier, /"\/NS"/);
  assert.match(releaseVerifier, /"\/UPDATE"/);
});

test("taskbar native move verifier restores user settings after debug probes", () => {
  assert.match(taskbarMoveVerifier, /settings\.json/);
  assert.match(taskbarMoveVerifier, /Backup-UserSettings/);
  assert.match(taskbarMoveVerifier, /Restore-UserSettings/);
});

test("statusline bridge verifier uses an isolated data directory", () => {
  assert.match(statuslineVerifier, /AGENT_JUICE_DATA_DIR/);
  assert.doesNotMatch(statuslineVerifier, /\$env:LOCALAPPDATA/);
});

test("taskbar bar initial paint hides tool sections and placeholder values", () => {
  const hiddenTools = barMarkup.match(/<section class="bar-tool"[^>]*hidden/g) ?? [];

  assert.equal(hiddenTools.length, 2);
  assert.doesNotMatch(barMarkup, /<strong class="bar-worst">[–-]<\/strong>/);
  assert.doesNotMatch(barMarkup, /5h\s*[–-]/);
  assert.doesNotMatch(barMarkup, /주간\s*[–-]/);
});

test("panel and bar IPC capabilities are split and sensitive commands are label guarded", () => {
  const panelCapability = capabilities.find((item) => item.windows?.includes("panel"));
  const barCapability = capabilities.find((item) => item.windows?.includes("bar-claude"));

  assert.ok(panelCapability);
  assert.ok(barCapability);
  assert.deepEqual(panelCapability.windows, ["panel"]);
  assert.deepEqual(barCapability.windows.sort(), ["bar-claude", "bar-codex"]);
  assert.ok(barCapability.permissions.includes("core:default"));
  assert.notEqual(panelCapability.identifier, barCapability.identifier);
  assert.match(rustLib, /ensure_panel_command/);
  assert.match(rustLib, /ensure_matching_bar_command/);
  assert.match(rustLib, /window\.label\(\)/);
});

test("taskbar movement is persisted only by the native drag loop final save", () => {
  assert.doesNotMatch(rustLib, /WindowEvent::Moved/);
  assert.match(rustLib, /save_taskbar_offset_ratio\(&app, tool, ratio\)/);
});
