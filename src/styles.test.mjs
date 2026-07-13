import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const css = readFileSync(resolve(here, "styles.css"), "utf8").replace(/\r\n?/g, "\n");
const panelMarkup = readFileSync(resolve(here, "index.html"), "utf8").replace(/\r\n?/g, "\n");
const panelJs = readFileSync(resolve(here, "panel.js"), "utf8").replace(/\r\n?/g, "\n");
const barJs = readFileSync(resolve(here, "bar.js"), "utf8").replace(/\r\n?/g, "\n");
const settingsJs = readFileSync(resolve(here, "settings.js"), "utf8").replace(/\r\n?/g, "\n");
const i18nJsPath = resolve(here, "i18n.js");
const i18nJs = existsSync(i18nJsPath)
  ? readFileSync(i18nJsPath, "utf8").replace(/\r\n?/g, "\n")
  : "";
const tauriConfig = JSON.parse(
  readFileSync(resolve(here, "../src-tauri/tauri.conf.json"), "utf8"),
);
const packageJson = JSON.parse(readFileSync(resolve(here, "../package.json"), "utf8"));
const packageLock = JSON.parse(readFileSync(resolve(here, "../package-lock.json"), "utf8"));
const cargoToml = readFileSync(resolve(here, "../src-tauri/Cargo.toml"), "utf8");
const cargoLock = readFileSync(resolve(here, "../src-tauri/Cargo.lock"), "utf8");
const capabilitiesDir = resolve(here, "../src-tauri/capabilities");
const capabilities = readdirSync(capabilitiesDir)
  .filter((name) => name.endsWith(".json"))
  .map((name) => JSON.parse(readFileSync(resolve(capabilitiesDir, name), "utf8")));
const rustLib = readFileSync(resolve(here, "../src-tauri/src/lib.rs"), "utf8").replace(
  /\r\n?/g,
  "\n",
);
const rustConfig = readFileSync(resolve(here, "../src-tauri/src/config.rs"), "utf8").replace(
  /\r\n?/g,
  "\n",
);
const gitignore = readFileSync(resolve(here, "../.gitignore"), "utf8").replace(/\r\n?/g, "\n");
function readOptional(path) {
  return existsSync(path) ? readFileSync(path, "utf8").replace(/\r\n?/g, "\n") : "";
}

const readme = readOptional(resolve(here, "../README.md"));
const pushAllowlist = readOptional(resolve(here, "../.ai/scripts/verify-git-push-allowlist.ps1"));
const releaseVerifier = readOptional(resolve(here, "../.ai/scripts/verify-release-installer.ps1"));
const taskbarMoveVerifier = readOptional(resolve(here, "../.ai/scripts/verify-taskbar-native-move.ps1"));
const statuslineVerifier = readOptional(resolve(here, "../.ai/scripts/verify-statusline-bridge.ps1"));
const runtimeVerifier = readOptional(resolve(here, "../.ai/scripts/verify-g3.86-runtime.ps1"));
const cargoConfig = readOptional(resolve(here, "../.cargo/config.toml"));
const windowsCi = readOptional(resolve(here, "../.github/workflows/windows-ci.yml"));
const runtimeSmoke = readOptional(resolve(here, "../.github/scripts/runtime-smoke.ps1"));
const barMarkup = readFileSync(resolve(here, "bar.html"), "utf8").replace(/\r\n?/g, "\n");

function cssBlock(selector) {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = css.match(new RegExp(`(?:^|\\n)${escaped}\\s*\\{(?<body>[^}]+)\\}`));
  return match?.groups?.body ?? "";
}

function hexToken(block, name) {
  const value = block.match(new RegExp(`${name}:\\s*(#[0-9a-fA-F]{6})`))?.[1];
  assert.ok(value, `${name} must be a six-digit hex color`);
  return value;
}

function relativeLuminance(hex) {
  const channels = hex.slice(1).match(/../g).map((part) => Number.parseInt(part, 16) / 255);
  const linear = channels.map((value) =>
    value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4,
  );
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

function contrastRatio(left, right) {
  const values = [relativeLuminance(left), relativeLuminance(right)].sort((a, b) => b - a);
  return (values[0] + 0.05) / (values[1] + 0.05);
}

function markupSection(name) {
  for (const tag of ["fieldset", "section"]) {
    const match = panelMarkup.match(
      new RegExp(
        `<${tag} class="[^"]*" data-settings-section="${name}">(?<body>[\\s\\S]*?)</${tag}>`,
      ),
    );
    if (match) return match.groups?.body ?? "";
  }
  return "";
}

test("styles define the restrained Quiet Glass surface tokens used by Juice", () => {
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
  assert.match(card, /backdrop-filter: blur\(12px\)/);
  assert.doesNotMatch(card, /inset 0 1px 0 var\(--hi\)/);
  assert.match(card, /border-radius: var\(--radius\)/);
});

test("usage cards share the same surface tint regardless of tool", () => {
  const cardTint = cssBlock(".tool-card::before");
  const hover = cssBlock(".tool-card:hover");
  const dot = cssBlock(".tool-dot");
  const dotFill = cssBlock(".claude-dot,\n.codex-dot");
  const claudeCard = cssBlock('.tool-card[data-tool="claude"]');
  const codexCard = cssBlock('.tool-card[data-tool="codex"]');

  assert.match(cardTint, /display: none/);
  assert.doesNotMatch(cardTint, /var\(--tool-glow\)/);
  assert.match(hover, /var\(--accent\)/);
  assert.doesNotMatch(hover, /var\(--tool-color\)/);
  assert.doesNotMatch(hover, /var\(--tool-glow\)/);
  assert.match(dot, /var\(--accent\)/);
  assert.match(dotFill, /background: var\(--tool-brand\)/);
  assert.match(claudeCard, /--tool-brand: #d79a32/);
  assert.match(codexCard, /--tool-brand: #2fac7d/);
  assert.doesNotMatch(claudeCard, /--tool-glow/);
  assert.doesNotMatch(codexCard, /--tool-glow/);
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

test("panel reopen path restores minimized settings window before focusing it", () => {
  const showPanel = rustLib.match(/fn show_panel[\s\S]*?\n}/)?.[0] ?? "";

  assert.match(showPanel, /window\.show\(\)/);
  assert.match(showPanel, /window\.unminimize\(\)/);
  assert.match(showPanel, /window\.set_focus\(\)/);
  assert.ok(showPanel.indexOf("window.unminimize()") < showPanel.indexOf("window.set_focus()"));
});

test("language selection is wired through settings and renderer translations", () => {
  assert.match(panelMarkup, /name="language"/);
  assert.match(panelMarkup, /<option value="system"[^>]*>시스템<\/option>/);
  assert.match(panelMarkup, /<option value="ko"[^>]*>한국어<\/option>/);
  assert.match(panelMarkup, /<option value="en"[^>]*>English<\/option>/);
  assert.match(rustConfig, /language/);
  assert.match(settingsJs, /applyTranslations/);
  assert.match(panelJs, /applyTranslations/);
  assert.match(i18nJs, /resolveLanguage/);
  assert.match(i18nJs, /Run Juice, then use Claude once on this PC/);
  assert.doesNotMatch(i18nJs, /Connect Claude, then use Claude once on this PC/);
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
  assert.equal(panelWindowConfig?.skipTaskbar, false);
  for (const barWindow of tauriConfig.app.windows.filter((item) => item.label.startsWith("bar-"))) {
    assert.equal(barWindow.skipTaskbar, true);
  }
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

test("stale taskbar state stays legible while looking distinct from live data", () => {
  const staleIndicator = cssBlock(
    '.bar-tool[data-state="stale"] .ring-arc,\n.bar-tool[data-state="stale"] .limit-bar::before',
  );
  const staleArc = cssBlock('.bar-tool[data-state="stale"] .ring-arc');
  const staleText = cssBlock(
    '.bar-tool[data-state="stale"] .bar-tool-name,\n.bar-tool[data-state="stale"] .bar-line,\n.bar-tool[data-state="stale"] .bar-worst,\n.bar-tool[data-state="stale"] .quad-number,\n.bar-tool[data-state="stale"] .primary-reset,\n.bar-tool[data-state="stale"] .secondary-reset',
  );

  assert.match(staleIndicator, /opacity: 0\.62/);
  assert.match(staleArc, /stroke-linecap: butt/);
  assert.match(staleText, /color: var\(--text-muted\)/);
});

test("muted text and focus tokens meet WCAG contrast thresholds in both themes", () => {
  const light = cssBlock('html[data-theme="light"]');
  const dark = cssBlock('html[data-theme="dark"]');
  const lightMuted = hexToken(light, "--text-muted");
  const darkMuted = hexToken(dark, "--text-muted");
  const lightFocus = hexToken(light, "--focus-ring");
  const darkFocus = hexToken(dark, "--focus-ring");

  assert.ok(contrastRatio(lightMuted, "#ffffff") >= 4.5);
  assert.ok(contrastRatio(darkMuted, "#1f2223") >= 4.5);
  assert.ok(contrastRatio(lightFocus, "#f1f4f3") >= 3);
  assert.ok(contrastRatio(darkFocus, "#0d0f10") >= 3);
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
  const fullResets = cssBlock('.bar-shell[data-mode="full"][data-full-reset-time="on"] .primary-reset,\n.bar-shell[data-mode="full"][data-full-reset-time="on"] .secondary-reset');
  const compactResets = cssBlock('.bar-shell[data-mode="compact"] .primary-reset,\n.bar-shell[data-mode="compact"] .secondary-reset');

  assert.match(copy, /grid-template-columns: max-content max-content max-content/);
  for (const block of [toolName, line, primaryText, secondaryText]) {
    assert.doesNotMatch(block, /text-overflow:\s*ellipsis/);
  }
  assert.match(reset, /display: none/);
  assert.match(reset, /font-size: calc\(var\(--bar-text-font-size\) \* 0\.88\)/);
  assert.match(reset, /font-weight: var\(--bar-text-font-weight\)/);
  assert.match(fullResets, /display: inline/);
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
  const textModeTool = cssBlock('.bar-shell[data-mode="full"] .bar-tool,\n.bar-shell[data-mode="compact"] .bar-tool');
  const horizontalTextModeTool = cssBlock('.bar-shell[data-taskbar-orientation="horizontal"][data-mode="full"] .bar-tool,\n.bar-shell[data-taskbar-orientation="horizontal"][data-mode="compact"] .bar-tool');
  const fullCopy = cssBlock('.bar-shell[data-mode="full"] .bar-copy');
  const compactCopy = cssBlock('.bar-shell[data-mode="compact"] .bar-copy');
  const dualTool = cssBlock('.bar-shell[data-mode="dual"] .bar-tool');
  const quadTool = cssBlock('.bar-shell[data-mode="quad"] .bar-tool');
  const indicatorOnlyShell = cssBlock('.bar-shell[data-mode="dual"],\n.bar-shell[data-mode="quad"]');

  assert.match(compactToolName, /display: none/);
  assert.equal(compactLimitText, "");
  assert.match(dualCopy, /display: none/);
  assert.match(quad, /display: none/);
  assert.doesNotMatch(quad, /background:/);
  assert.match(quadMode, /display: flex/);
  assert.match(quadCopy, /display: none/);
  assert.match(quadPrimary, /--quad-color: var\(--primary-color\)/);
  assert.match(quadSecondary, /--quad-color: var\(--secondary-color\)/);
  assert.match(ringOff, /display: none/);
  assert.equal(cssBlock('.bar-shell[data-ring="off"] .bar-ring'), "");
  assert.match(textModeTool, /padding-inline: 0/);
  assert.match(textModeTool, /justify-content: flex-start/);
  assert.match(fullCopy, /flex: 0 0 auto/);
  assert.match(fullCopy, /grid-template-columns: max-content max-content max-content/);
  assert.match(fullCopy, /column-gap: 6px/);
  assert.match(compactCopy, /flex: 0 0 auto/);
  assert.match(compactCopy, /grid-template-columns: max-content max-content/);
  assert.match(compactCopy, /column-gap: 6px/);
  assert.match(horizontalTextModeTool, /width: max-content/);
  assert.match(horizontalTextModeTool, /overflow: visible/);
  for (const block of [dualTool, quadTool]) {
    assert.match(block, /justify-content: flex-start/);
  }
  assert.match(cssBlock(".bar-shell"), /padding: 0;/);
  assert.match(cssBlock(".bar-shell"), /--bar-content-gap: 14px/);
  assert.match(cssBlock(".bar-tool"), /padding: 0 0\.5px/);
  assert.match(cssBlock(".bar-tool"), /gap: var\(--bar-content-gap\)/);
  assert.equal(indicatorOnlyShell, "");
  assert.doesNotMatch(dualTool, /padding:/);
  assert.doesNotMatch(quadTool, /padding:/);
});

test("taskbar orientation is explicit so short horizontal bars stay vertically centered", () => {
  const verticalShell = cssBlock('.bar-shell[data-taskbar-orientation="vertical"]');
  const verticalTool = cssBlock('.bar-shell[data-taskbar-orientation="vertical"] .bar-tool');
  const verticalQuad = cssBlock('.bar-shell[data-taskbar-orientation="vertical"] .bar-quad');

  assert.match(barMarkup, /data-taskbar-orientation="horizontal"/);
  assert.doesNotMatch(css, /@media\s*\(orientation:\s*portrait\)/);
  assert.match(verticalShell, /grid-auto-flow: row/);
  assert.match(verticalTool, /flex-direction: column/);
  assert.match(verticalQuad, /flex-direction: column/);
});

test("dual ring center number stays inside the ring hole", () => {
  const ring = cssBlock('.bar-shell[data-mode="dual"] .bar-ring');
  const worst = cssBlock(".bar-worst");
  const svg = cssBlock(".ring-svg");
  const track = cssBlock(".ring-track");
  const arc = cssBlock(".ring-arc");
  const outerArc = cssBlock(".outer-arc");
  const innerArc = cssBlock(".inner-arc");
  const quadRing = cssBlock(".quad-ring");
  const quadSvg = cssBlock(".quad-svg");
  const quadNumber = cssBlock(".quad-number");
  const shell = cssBlock(".bar-shell");

  assert.match(shell, /--ring-svg-stroke:/);
  assert.match(shell, /--outer-radius:/);
  assert.match(shell, /--inner-radius:/);
  assert.match(shell, /--quad-svg-stroke:/);
  assert.match(shell, /--quad-radius:/);
  assert.match(ring, /width: var\(--ring-size\)/);
  assert.match(ring, /height: var\(--ring-size\)/);
  assert.match(ring, /flex-basis: var\(--ring-size\)/);
  assert.match(svg, /position: absolute/);
  assert.match(svg, /overflow: visible/);
  assert.match(svg, /shape-rendering: geometricPrecision/);
  assert.match(track, /stroke-width: var\(--ring-svg-stroke\)/);
  assert.match(arc, /stroke-linecap: round/);
  assert.match(arc, /stroke-dasharray: var\(--ring-dash\) 100/);
  assert.match(outerArc, /--ring-radius: var\(--outer-radius\)/);
  assert.match(innerArc, /--ring-radius: var\(--inner-radius\)/);
  assert.match(quadRing, /width: var\(--ring-size\)/);
  assert.match(quadRing, /height: var\(--ring-size\)/);
  assert.match(quadRing, /position: relative/);
  assert.match(quadSvg, /position: absolute/);
  assert.match(quadNumber, /place-items: center/);
  assert.match(quadNumber, /inset: 0/);
  assert.doesNotMatch(quadNumber, /transform:/);
  assert.match(quadNumber, /font-variant-numeric: tabular-nums/);
  assert.match(quadNumber, /font-size: var\(--ring-number-font-size\)/);
  assert.match(quadNumber, /font-weight: var\(--ring-number-font-weight\)/);
  assert.match(worst, /font-size: var\(--ring-number-font-size\)/);
  assert.match(worst, /font-weight: var\(--ring-number-font-weight\)/);
  assert.match(worst, /inset: 0/);
  assert.match(worst, /display: grid/);
  assert.match(worst, /place-items: center/);
  assert.doesNotMatch(worst, /transform:/);
  assert.match(worst, /font-variant-numeric: tabular-nums/);
  assert.match(worst, /text-align: center/);
});

test("taskbar ring markup uses SVG strokes instead of masked conic gradients", () => {
  assert.match(barMarkup, /class="ring-svg"/);
  assert.match(barMarkup, /class="ring-track outer-track"/);
  assert.match(barMarkup, /class="ring-arc outer-arc"/);
  assert.match(barMarkup, /class="ring-effect ring-effect-shadow outer-effect"/);
  assert.match(barMarkup, /class="ring-effect ring-effect-highlight inner-effect"/);
  assert.match(barMarkup, /class="quad-svg"/);
  assert.match(barMarkup, /class="ring-arc quad-arc"/);
  assert.doesNotMatch(cssBlock(".outer-ring"), /conic-gradient/);
  assert.doesNotMatch(cssBlock(".inner-ring"), /conic-gradient/);
  assert.equal(cssBlock(".quad-ring::before"), "");
  assert.doesNotMatch(css, /-webkit-mask: radial-gradient/);
  assert.doesNotMatch(css, /mask: radial-gradient/);
});

test("taskbar SVG ring strokes scale with the viewBox instead of screen pixels", () => {
  const track = cssBlock(".ring-track");
  const arc = cssBlock(".ring-arc");

  assert.match(track, /stroke-width: var\(--ring-svg-stroke\)/);
  assert.match(arc, /stroke-width: var\(--ring-svg-stroke\)/);
  assert.doesNotMatch(`${track}\n${arc}`, /vector-effect:\s*non-scaling-stroke/);
});

test("taskbar ring number visibility and outline are configurable", () => {
  const numbersOff = cssBlock('.bar-shell[data-ring-numbers="off"] .bar-worst,\n.bar-shell[data-ring-numbers="off"] .quad-number');
  const outlineOn = cssBlock('.bar-shell[data-number-outline="on"] .bar-worst,\n.bar-shell[data-number-outline="on"] .quad-number');

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
  assert.match(panelMarkup, /name="ring_number_outline_width_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="ring_size_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="ring_thickness_px"/);
  assert.match(panelMarkup, /name="ring_thickness_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="ring_gap_px"/);
  assert.match(panelMarkup, /name="ring_gap_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="ring_center_size_px"/);
  assert.match(panelMarkup, /name="ring_center_size_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="ring_number_font_size_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="bar_text_font_size_px"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="bar_content_gap_px"[\s\S]*?min="0"[\s\S]*?max="24"[\s\S]*?step="0\.1"/);
  assert.match(panelMarkup, /name="bar_content_gap_px"[\s\S]*?value="14"/);
  assert.equal((panelMarkup.match(/data-range-number-for=/g) ?? []).length, 10);
  for (const name of [
    "ring_center_size_px",
    "ring_number_font_size_px",
    "ring_number_font_weight",
    "bar_text_font_size_px",
    "bar_text_font_weight",
    "bar_content_gap_px",
    "ring_number_outline_width_px",
    "ring_size_px",
    "ring_thickness_px",
    "ring_gap_px",
  ]) {
    assert.match(panelMarkup, new RegExp(`data-range-number-for="${name}"`));
  }
  assert.match(panelMarkup, /name="ring_number_font_weight"/);
  assert.match(panelMarkup, /name="bar_text_font_weight"/);
  assert.match(rustConfig, /ring_center_size_px/);
  assert.match(rustConfig, /ring_number_outline_width_px/);
  assert.match(rustConfig, /bar_content_gap_px/);
  assert.match(numbersOff, /display: none/);
  assert.match(outlineOn, /text-shadow:/);
  assert.match(outlineOn, /var\(--ring-number-outline-width\)/);
  assert.doesNotMatch(outlineOn, /-webkit-text-stroke:/);
  assert.doesNotMatch(outlineOn, /paint-order:/);
});

test("taskbar indicator can switch from rings to stacked horizontal bars", () => {
  const bars = cssBlock(".bar-bars");
  const limitBar = cssBlock(".limit-bar");
  const limitLayers = cssBlock(".limit-bar::before,\n.limit-bar::after");
  const indicatorBars = cssBlock('.bar-shell[data-indicator="bar"] .bar-bars');
  const hiddenRings = cssBlock('.bar-shell[data-indicator="bar"] .bar-ring,\n.bar-shell[data-indicator="bar"] .bar-quad');
  const primary = cssBlock(".limit-bar.primary-limit");
  const secondary = cssBlock(".limit-bar.secondary-limit");

  assert.match(barMarkup, /class="bar-bars"/);
  assert.match(bars, /display: none/);
  assert.match(bars, /width: var\(--ring-size\)/);
  assert.match(bars, /gap: var\(--ring-gap\)/);
  assert.match(limitBar, /height: var\(--ring-thickness\)/);
  assert.match(limitBar, /background: color-mix/);
  assert.match(limitLayers, /background: var\(--limit-color\)/);
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

test("taskbar overlay base has no blur, filter, text shadow, transition, or animation effects", () => {
  const windowBlock = cssBlock(".bar-window");
  const shell = cssBlock(".bar-shell");
  const tool = cssBlock(".bar-tool");
  const liveRing = cssBlock('.bar-tool[data-state="live"] .bar-ring');
  const ring = cssBlock(".bar-ring");
  const quad = cssBlock(".bar-quad");
  const quadRing = cssBlock(".quad-ring");
  const ringSvg = cssBlock(".ring-svg");
  const ringTrack = cssBlock(".ring-track");
  const ringArc = cssBlock(".ring-arc");
  const quadSvg = cssBlock(".quad-svg");
  const quadNumber = cssBlock(".quad-number");
  const worst = cssBlock(".bar-worst");
  const outlineOn = cssBlock('.bar-shell[data-number-outline="on"] .bar-worst,\n.bar-shell[data-number-outline="on"] .quad-number');
  const barBlocks = [
    windowBlock,
    shell,
    tool,
    liveRing,
    ring,
    quad,
    quadRing,
    ringSvg,
    ringTrack,
    ringArc,
    quadSvg,
    quadNumber,
    worst,
  ].join("\n");

  assert.doesNotMatch(barBlocks, /backdrop-filter:/);
  assert.doesNotMatch(barBlocks, /-webkit-backdrop-filter:/);
  assert.doesNotMatch(barBlocks, /filter:/);
  assert.doesNotMatch(barBlocks, /text-shadow:/);
  assert.match(outlineOn, /text-shadow:/);
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

test("settings controls keep glass at the card level and use quiet flat rows", () => {
  const card = cssBlock(".settings-card,\n.settings-utility-card");
  const cardTint = cssBlock(".settings-card::before");
  const toolTint = cssBlock(".tool-card::before");
  const form = cssBlock(".settings-form");
  const section = cssBlock(".settings-section");
  const legend = cssBlock(".settings-section legend");
  const summaryAccent = cssBlock(".settings-card summary .settings-title::before");
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
  const toggleSpan = cssBlock(".toggle-row > span:not(.toggle-copy)");
  const toggleCopy = cssBlock(".toggle-copy");
  const toggleTitle = cssBlock(".toggle-title");
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
  const rangeFocus = cssBlock('input[type="range"]:focus-visible');
  const button = cssBlock("button");
  const buttonHover = cssBlock("button:hover:not(:disabled)");

  assert.match(card, /backdrop-filter: blur\(12px\) saturate\(1\.04\)/);
  assert.match(card, /var\(--surface\)/);
  assert.match(card, /var\(--shadow-soft\)/);
  assert.doesNotMatch(card, /inset 0 1px 0 var\(--hi\)/);
  assert.doesNotMatch(card, /var\(--glass\) 88%/);
  assert.doesNotMatch(card, /blur\(32px\)/);
  assert.match(toolTint, /display: none/);
  assert.match(cardTint, /display: none/);
  assert.match(summaryAccent, /background: var\(--accent\)/);
  assert.doesNotMatch(summaryAccent, /var\(--accent-warm\)/);
  assert.match(form, /grid-template-columns: minmax\(0,\s*1fr\)/);
  assert.match(section, /border-top: 1px solid/);
  assert.doesNotMatch(section, /box-shadow:/);
  assert.match(legend, /border-left: 3px solid/);
  assert.match(legend, /background: color-mix/);
  assert.doesNotMatch(legend, /position: sticky/);
  assert.doesNotMatch(legend, /z-index:/);
  assert.match(rowSurface, /background: transparent/);
  assert.match(rowSurface, /border-bottom: 1px solid/);
  assert.match(rowSurface, /box-shadow: none/);
  assert.doesNotMatch(rowSurface, /backdrop-filter/);
  assert.doesNotMatch(rowSurface, /inset 0 1px/);
  assert.match(select, /appearance: none/);
  assert.doesNotMatch(select, /backdrop-filter/);
  assert.match(select, /box-shadow: none/);
  assert.doesNotMatch(select, /inset 0/);
  assert.match(range, /appearance: none/);
  assert.match(range, /--range-progress:/);
  assert.match(range, /background:\s*transparent/);
  assert.match(range, /height: 24px/);
  assert.match(rangeTrack, /linear-gradient\(90deg/);
  assert.match(rangeTrack, /var\(--range-progress\)/);
  assert.doesNotMatch(rangeTrack, /backdrop-filter/);
  assert.match(rangeTrack, /box-shadow: none/);
  assert.doesNotMatch(rangeTrack, /inset 0/);
  assert.doesNotMatch(thumb, /backdrop-filter/);
  assert.match(thumb, /box-shadow: none/);
  assert.doesNotMatch(thumb, /radial-gradient/);
  assert.doesNotMatch(thumb, /linear-gradient/);
  assert.match(rangeFocus, /outline: 2px solid var\(--focus-ring\)/);
  assert.match(rangeFocus, /outline-offset: 2px/);
  assert.match(focusThumb, /box-shadow: 0 0 0 3px/);
  assert.match(checkbox, /border-radius: 999px/);
  assert.match(checkbox, /--toggle-track-off:/);
  assert.match(checkbox, /--toggle-knob-off:/);
  assert.doesNotMatch(checkbox, /backdrop-filter/);
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
  assert.match(toggleCopy, /order: 1/);
  assert.match(toggleCopy, /flex-direction: column/);
  assert.match(toggleCopy, /align-items: flex-start/);
  assert.match(toggleTitle, /font-weight: 740/);
  assert.match(advanced, /grid-column:\s*1 \/ -1/);
  assert.match(advanced, /border-top:/);
  assert.doesNotMatch(advanced, /background:/);
  assert.doesNotMatch(advanced, /box-shadow:/);
  assert.match(advancedSummary, /cursor:\s*pointer/);
  assert.match(advancedGrid, /display:\s*grid/);
  assert.doesNotMatch(button, /backdrop-filter/);
  assert.match(button, /box-shadow: none/);
  assert.doesNotMatch(button, /linear-gradient/);
  assert.doesNotMatch(buttonHover, /transform:/);
  assert.doesNotMatch(panelMarkup, /type="submit"/);
  assert.doesNotMatch(panelMarkup, />저장</);
});

test("autosave completion uses a centered transient toast outside the scrolling shell", () => {
  const layer = cssBlock(".settings-toast-layer");
  const toast = cssBlock(".settings-toast");
  const previewBars = cssBlock(".effect-preview-bars");
  const depthBar = cssBlock('.bar-shell[data-effect="depth"] .limit-bar::before');

  assert.match(panelMarkup, /<\/main>\s*<div class="settings-toast-layer" data-settings-toast hidden>/);
  assert.match(panelMarkup, /data-settings-toast-text/);
  assert.match(panelMarkup, /data-settings-save-state data-state="ready" hidden/);
  assert.match(layer, /position: fixed/);
  assert.match(layer, /left: 0/);
  assert.match(layer, /right: 0/);
  assert.match(layer, /place-items: center/);
  assert.match(layer, /bottom: 16px/);
  assert.match(toast, /min-height: 34px/);
  assert.match(toast, /max-width: min\(360px, 100%\)/);
  assert.match(toast, /text-align: center/);
  assert.doesNotMatch(previewBars, /color-mix/);
  assert.match(previewBars, /border-top: 3px solid currentColor/);
  assert.match(previewBars, /border-bottom: 3px solid currentColor/);
  assert.match(depthBar, /background: var\(--limit-color\)/);
  assert.match(i18nJs, /"status\.saved": "적용 완료"/);
  assert.match(i18nJs, /"status\.saved": "Applied"/);
});

test("palette picker exposes stable swatches and a clear selected state", () => {
  const field = cssBlock(".palette-field");
  const picker = cssBlock(".palette-picker");
  const option = cssBlock(".palette-option");
  const selected = cssBlock('.palette-option[aria-checked="true"]');
  const sample = cssBlock(".palette-sample");

  assert.match(field, /grid-column:\s*1 \/ -1/);
  assert.match(picker, /grid-template-columns: repeat\(3, minmax\(0, 1fr\)\)/);
  assert.match(option, /min-height: 42px/);
  assert.match(option, /background: transparent/);
  assert.match(selected, /var\(--accent\)/);
  assert.match(sample, /grid-template-columns: repeat\(3, 1fr\)/);
  assert.match(css, /data-palette-value="mono"/);
  assert.match(css, /--mono-swatch/);
  assert.match(css, /data-palette-value="traffic"[\s\S]*--tool-claude-primary-swatch/);
  assert.match(cssBlock('.palette-option[data-palette-value="traffic"] .palette-sample'), /repeat\(4, 1fr\)/);
});

test("settings form groups controls into logical sections without changing field names", () => {
  const sections = [
    ...panelMarkup.matchAll(/<(?:fieldset|section) class="[^"]*" data-settings-section="([^"]+)">/g),
  ].map((match) => match[1]);
  const indicatorSection = markupSection("indicator");

  assert.deepEqual(sections, ["appearance", "limits", "taskbar", "indicator", "system", "update", "about"]);
  assert.match(panelMarkup, /<details class="settings-card" open>/);

  assert.match(markupSection("appearance"), /name="theme"[\s\S]*name="font_mode"[\s\S]*name="palette"/);
  assert.match(markupSection("limits"), /name="display_basis"[\s\S]*name="warn_threshold"[\s\S]*name="danger_threshold"[\s\S]*name="poll_interval_secs"[\s\S]*name="stale_after_secs"[\s\S]*name="claude_account_auto_collect_on"/);
  assert.match(markupSection("limits"), /data-display-basis-copy="warning"[\s\S]*data-display-basis-copy="danger"[\s\S]*data-display-basis-copy="help"/);
  assert.match(markupSection("taskbar"), /name="bar_mode"[\s\S]*name="full_reset_time_on"[\s\S]*name="limit_order"[\s\S]*name="indicator_style"[\s\S]*name="show_claude"[\s\S]*name="show_codex"/);
  assert.match(indicatorSection, /name="ring_on"[\s\S]*name="ring_numbers_on"[\s\S]*name="ring_number_outline_on"/);
  assert.match(indicatorSection, /<details class="settings-advanced" data-settings-advanced="typography" open>[\s\S]*name="ring_number_font_size_px"[\s\S]*name="bar_text_font_weight"[\s\S]*<\/details>/);
  assert.match(indicatorSection, /<details class="settings-advanced" data-settings-advanced="indicator">/);
  assert.match(indicatorSection, /<summary data-i18n="advanced\.indicator">고급 조정<\/summary>/);
  assert.match(indicatorSection, /name="indicator_effect_style"[\s\S]*data-effect-value="flat"[\s\S]*data-effect-value="breathe"/);
  assert.match(indicatorSection, /name="ring_center_size_px"[\s\S]*<details class="settings-advanced" data-settings-advanced="typography"/);
  assert.match(indicatorSection, /<details class="settings-advanced" data-settings-advanced="indicator">[\s\S]*name="ring_number_outline_width_px"[\s\S]*name="ring_size_px"[\s\S]*name="ring_gap_px"[\s\S]*<\/details>/);
  assert.match(markupSection("system"), /name="autostart_on"[\s\S]*data-action="restore-statusline"/);
  assert.doesNotMatch(markupSection("system"), /id="settings-status"/);
  assert.match(markupSection("update"), /name="update_check_on"[\s\S]*data-action="check-updates"[\s\S]*data-action="open-releases"[\s\S]*id="update-check-status"/);
  assert.doesNotMatch(markupSection("about"), /name="update_check_on"|data-action="check-updates"/);
  assert.match(panelMarkup, /<form id="settings-form" class="settings-layout">[\s\S]*<section class="settings-utility-card update-card" data-settings-section="update">[\s\S]*<\/section>\s*<\/form>/);
  assert.match(panelMarkup, /<summary>[\s\S]*data-settings-save-state[\s\S]*id="settings-status"[\s\S]*<\/summary>/);
  assert.match(panelMarkup, /<\/form>\s*<section class="settings-utility-card about-card" data-settings-section="about">[\s\S]*<\/section>\s*<\/main>/);
  assert.doesNotMatch(panelMarkup, /settings-footer/);
  assert.doesNotMatch(cssBlock(".settings-save-state"), /position:\s*fixed/);
  assert.doesNotMatch(cssBlock(".settings-save-state"), /background:|border:/);
  assert.equal(sections.at(-1), "about");
  assert.match(markupSection("limits"), /name="claude_account_auto_collect_on"[\s\S]*class="toggle-copy"[\s\S]*class="toggle-title" data-i18n="field\.claudeUsageAutoRefresh"/);
  assert.doesNotMatch(panelMarkup, /data-settings-section="lab"/);
  assert.doesNotMatch(panelMarkup, /claude_usage_auto_refresh_lab_on/);

  const paletteOptions = [...markupSection("appearance").matchAll(/data-palette-value="([^"]+)"/g)]
    .map((match) => match[1]);
  assert.deepEqual(paletteOptions, [
    "traffic", "signal", "ocean", "forest", "sunset", "cvd", "cool", "mono", "custom",
  ]);
  assert.match(markupSection("appearance"), /role="radiogroup"[\s\S]*name="mono_color"[\s\S]*name="custom_safe"/);
  assert.match(markupSection("appearance"), /data-tool-palette[\s\S]*name="claude_primary_color"[\s\S]*name="claude_secondary_color"[\s\S]*name="codex_primary_color"[\s\S]*name="codex_secondary_color"/);

  assert.match(panelMarkup, /name="claude_taskbar_offset_ratio"/);
  assert.match(panelMarkup, /name="codex_taskbar_offset_ratio"/);
  assert.match(settingsJs, /setField\("claude_account_auto_collect_on", state\.claudeAccountAutoCollectOn\)/);
  assert.match(settingsJs, /setField\("update_check_on", state\.updateCheckOn\)/);
});

test("indicator effect presets are previewed, opt-in, and motion-safe", () => {
  const indicatorSection = markupSection("indicator");
  const effectValues = [...indicatorSection.matchAll(/data-effect-value="([^"]+)"/g)]
    .map((match) => match[1]);
  const flat = cssBlock('.bar-shell[data-effect="flat"] .ring-effect-shadow');
  const soft = cssBlock('.bar-shell[data-effect="soft"] .ring-effect-shadow');
  const depth = cssBlock('.bar-shell[data-effect="depth"] .ring-effect-shadow');
  const glow = cssBlock('.bar-shell[data-effect="glow"] .ring-effect-shadow');
  const breathe = cssBlock('.bar-shell[data-effect="breathe"] .bar-tool[data-state="live"] .ring-effect-shadow');

  assert.deepEqual(effectValues, ["flat", "soft", "depth", "glow", "breathe"]);
  assert.match(panelMarkup, /name="indicator_effect_style" value="flat"/);
  assert.match(barMarkup, /data-effect="flat"/);
  assert.equal(flat, "");
  assert.match(soft, /opacity: 0\.24/);
  assert.match(depth, /opacity: 0\.42/);
  assert.match(glow, /stroke-width: calc\(var\(--effect-stroke\) \+ 7\)/);
  assert.match(breathe, /animation: indicator-breathe 2\.8s/);
  assert.doesNotMatch(`${soft}\n${depth}\n${glow}\n${breathe}`, /filter:/);
  assert.match(css, /@media \(prefers-reduced-motion: reduce\)[\s\S]*data-effect="breathe"[\s\S]*animation: none !important/);
  assert.doesNotMatch(css, /data-state="stale"[^}]*indicator-breathe/);
  assert.doesNotMatch(css, /data-state="empty"[^}]*indicator-breathe/);
});

test("about and update sections keep product copy separate from guarded update controls", () => {
  const about = markupSection("about");
  const update = markupSection("update");

  assert.match(panelMarkup, /id="update-band"[\s\S]*data-action="open-available-release"/);
  assert.match(about, /Claude Code와 Codex의 5시간·주간 사용량/);
  assert.match(about, /별도 Juice 서버에 저장하지 않습니다/);
  assert.match(update, /name="update_check_on" checked/);
  assert.match(update, /data-action="check-updates"/);
  assert.match(update, /data-action="open-releases"/);
  assert.doesNotMatch(about, /name="update_check_on"|data-action="check-updates"/);
  assert.match(settingsJs, /invoke\("get_update_status"\)/);
  assert.match(settingsJs, /invoke\("check_for_updates"\)/);
  assert.match(settingsJs, /invoke\("open_release_page", \{ url:/);
  assert.match(rustLib, /plugin\(tauri_plugin_notification::init\(\)\)/);
  assert.match(rustLib, /spawn_update_check\(app\.handle\(\)\.clone\(\)\)/);
  assert.match(rustLib, /ensure_panel_command\(window\.label\(\)\)\?/);
  assert.doesNotMatch(tauriConfig.app.security.csp, /github\.com/);
  assert.match(i18nJs, /"status\.updateFailed": "업데이트를 확인하지 못했습니다/);
  assert.match(i18nJs, /"status\.updateFailed": "Could not check for updates/);
});

test("public README and release template are the only tracked markdown exceptions", (t) => {
  if (!pushAllowlist) return t.skip("private .ai push allowlist is unavailable");

  assert.match(gitignore, /\n!README\.md\n/);
  assert.match(gitignore, /\n!\.github\/\n/);
  assert.match(gitignore, /\n!\.github\/RELEASE_TEMPLATE\.md\n/);
  assert.match(pushAllowlist, /README\.md/);
  assert.match(pushAllowlist, /\.github\/RELEASE_TEMPLATE\.md/);
  assert.match(pushAllowlist, /'\\\.md\$'/);
  assert.match(pushAllowlist, /\^\\.ai\//);
  assert.doesNotMatch(readme, /Claude 연결/);
  assert.doesNotMatch(readme, /Connect Claude/);
  assert.match(readme, /Juice 설치본은 시작할 때 Claude statusline 연결을 비파괴·멱등으로 시도합니다/);
  assert.match(readme, /installed app attempts a non-destructive, idempotent Claude statusline connection/);
});

test("settings copy uses accurate collection timing labels and hides obsolete Claude connect action", () => {
  const limitsSection = markupSection("limits");

  assert.match(panelMarkup, />수집주기</);
  assert.match(panelMarkup, />오래됨</);
  assert.doesNotMatch(panelMarkup, />오래됨 기준</);
  assert.match(panelMarkup, />잔여량 경고</);
  assert.match(panelMarkup, />잔여량 위험</);
  assert.match(panelMarkup, /name="display_basis"/);
  assert.match(panelMarkup, /data-display-basis-copy="help"/);
  assert.match(i18nJs, /작업표시줄은 남은 사용량을 표시하므로/);
  assert.match(i18nJs, /표시 사용량이 이 값 이상이 되면/);
  assert.match(limitsSection, /name="poll-output">60<\/output>\s*<span data-i18n="unit\.seconds">초<\/span>/);
  assert.match(limitsSection, /name="stale-output">90<\/output>\s*<span data-i18n="unit\.seconds">초<\/span>/);
  const timingFields = [...limitsSection.matchAll(/<label class="field-with-help">/g)];
  assert.equal(timingFields.length, 2);
  assert.match(limitsSection, /<label class="field-with-help">[\s\S]*data-i18n="field\.pollInterval"[\s\S]*data-i18n="help\.pollInterval"/);
  assert.match(limitsSection, /<label class="field-with-help">[\s\S]*data-i18n="field\.staleAfter"[\s\S]*data-i18n="help\.staleAfter"/);
  assert.match(cssBlock(".field-grid label.field-with-help"), /grid-template-columns: minmax\(72px, 1fr\) 58px auto/);
  assert.match(cssBlock(".field-grid label.field-with-help > span:first-child"), /white-space: nowrap/);
  assert.match(panelMarkup, /data-i18n="help.staleAfter"/);
  assert.match(i18nJs, /마지막 기록 후 오래됨 표시까지/);
  assert.match(i18nJs, /로컬 상태를 다시 읽는 간격/);
  assert.doesNotMatch(i18nJs, /마지막 기록이 몇 초 지나면 오래됨으로 표시할지/);
  assert.match(i18nJs, /"unit\.seconds": "초"/);
  assert.doesNotMatch(panelMarkup, />신선도</);
  assert.doesNotMatch(panelMarkup, /data-action="install-statusline"/);
  assert.doesNotMatch(panelMarkup, /data-action="connect-statusline"/);
  assert.doesNotMatch(panelMarkup, />Claude 연결</);
  assert.doesNotMatch(i18nJs, /action\.connectClaude/);
  assert.match(panelMarkup, /data-action="restore-statusline"/);
  assert.doesNotMatch(settingsJs, /install_statusline/);
  assert.doesNotMatch(panelMarkup, /data-settings-section="lab"/);
  assert.doesNotMatch(i18nJs, /"section\.lab": "실험실"/);
  assert.match(i18nJs, /Claude 계정 사용량 자동 수집/);
});

test("taskbar bar right click exposes a visible refresh action", () => {
  const menu = cssBlock(".bar-context-menu");
  const button = cssBlock(".bar-context-menu button");

  assert.match(barMarkup, /id="bar-menu"/);
  assert.match(barMarkup, /data-bar-action="refresh"/);
  assert.match(barMarkup, /data-i18n="action.refresh"/);
  assert.match(menu, /position: absolute/);
  assert.match(menu, /left: var\(--menu-x\)/);
  assert.match(menu, /top: var\(--menu-y\)/);
  assert.match(button, /cursor: pointer/);
  assert.match(i18nJs, /"action.refresh": "새로고침"/);
  assert.match(barJs, /set_taskbar_menu_open/);
  assert.match(barJs, /setNativeMenuOpen\(true\)/);
  assert.match(barJs, /setNativeMenuOpen\(false\)/);
  assert.match(rustLib, /fn set_taskbar_menu_open/);
  assert.match(rustLib, /struct TaskbarMenuState/);
  assert.match(rustLib, /taskbar_width_with_menu\(width, taskbar_menu_is_open\(manager, tool\)\)/);
  assert.match(rustLib, /taskbar_physical_length_for_window\(width, taskbar\.hwnd\)/);
});

test("panel meta removes estimated cost copy and stays as a full-width card footer", () => {
  const meta = cssBlock(".meta");

  assert.doesNotMatch(panelMarkup, /추정 비용/);
  assert.doesNotMatch(i18nJs, /meta\.cost/);
  assert.match(meta, /width:\s*100%/);
  assert.match(meta, /justify-self:\s*stretch/);
  assert.match(meta, /text-align:\s*right/);
  assert.doesNotMatch(meta, /max-width:/);
});

test("release startup attempts a non-fatal Claude statusline auto-connect", () => {
  assert.match(rustLib, /fn auto_connect_statusline_for_release\(\)/);
  assert.match(rustLib, /fn auto_connect_statusline_for_release\(\)[\s\S]*cfg!\(debug_assertions\)/);
  assert.match(rustLib, /fn auto_connect_statusline_for_release\(\)[\s\S]*statusline_bridge_path\(\)/);
  assert.match(rustLib, /fn auto_connect_statusline_for_release\(\)[\s\S]*Settings::install_statusline_wrap/);
  assert.match(rustLib, /fn auto_connect_statusline_for_release\(\)[\s\S]*eprintln!\("\[statusline\] auto-connect failed/);
  assert.match(rustLib, /auto_connect_statusline_for_release\(\);[\s\S]*spawn_status_loop/);
});

test("styles avoid decorative one-off effects and viewport font scaling", () => {
  assert.doesNotMatch(css, /letter-spacing:\s*-/);
  assert.doesNotMatch(css, /font-size:\s*[^;]*vw/);
  assert.doesNotMatch(css, /\b(orb|blob|bokeh)\b/i);
});

test("release installer verifier stops temporary app processes before cleanup", (t) => {
  if (!releaseVerifier) return t.skip("private .ai release verifier is unavailable");

  assert.match(releaseVerifier, /agent-juice-verify-install/);
  assert.match(releaseVerifier, /Stop-Process/);
  assert.match(releaseVerifier, /agent-juice\.exe/);
});

test("release installer verifier selects the newest generated Juice installer", (t) => {
  if (!releaseVerifier) return t.skip("private .ai release verifier is unavailable");

  assert.match(releaseVerifier, /Get-ChildItem/);
  assert.match(releaseVerifier, /Juice_\*_x64-setup\.exe/);
  assert.match(releaseVerifier, /Sort-Object\s+LastWriteTime\s+-Descending/);
  assert.doesNotMatch(releaseVerifier, /Juice_0\.1\.\d+_x64-setup\.exe/);
});

test("release installer verifier restores installer registry state after temp install checks", (t) => {
  if (!releaseVerifier) return t.skip("private .ai release verifier is unavailable");

  assert.match(releaseVerifier, /HKCU:\\Software\\pointi\\Juice/);
  assert.match(releaseVerifier, /HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Juice/);
  assert.match(releaseVerifier, /Backup-InstallerRegistryState/);
  assert.match(releaseVerifier, /Restore-InstallerRegistryState/);
  assert.doesNotMatch(releaseVerifier, /DeleteSubKeyTree/);
});

test("release installer verifier protects real app processes, Run key, and shortcuts", (t) => {
  if (!releaseVerifier) return t.skip("private .ai release verifier is unavailable");

  assert.match(releaseVerifier, /Assert-NoNonTempAppProcess/);
  assert.match(releaseVerifier, /not under temp install dir/i);
  assert.match(releaseVerifier, /CurrentVersion\\Run/);
  assert.match(releaseVerifier, /Backup-RegistryNamedValues/);
  assert.match(releaseVerifier, /Restore-RegistryNamedValues/);
  assert.match(releaseVerifier, /"\/NS"/);
  assert.match(releaseVerifier, /"\/UPDATE"/);
});

test("taskbar native move verifier restores user settings after debug probes", (t) => {
  if (!taskbarMoveVerifier) return t.skip("private .ai taskbar verifier is unavailable");

  assert.match(taskbarMoveVerifier, /settings\.json/);
  assert.match(taskbarMoveVerifier, /Backup-UserSettings/);
  assert.match(taskbarMoveVerifier, /Restore-UserSettings/);
});

test("statusline bridge verifier uses an isolated data directory", (t) => {
  if (!statuslineVerifier) return t.skip("private .ai statusline verifier is unavailable");

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
  assert.match(rustLib, /ensure_status_refresh_command/);
  assert.match(rustLib, /window\.label\(\)/);
});

test("status refresh is available from panel and taskbar bars and periodic loop always emits", () => {
  const statusLoop = rustLib.match(/fn spawn_status_loop[\s\S]*?^}/m)?.[0] ?? "";

  assert.match(
    rustLib,
    /fn refresh_status\(\s*window: tauri::Window,\s*app: tauri::AppHandle,\s*\)/,
  );
  assert.match(rustLib, /refresh_status,/);
  assert.match(statusLoop, /handle\.emit\("status-updated"/);
  assert.doesNotMatch(statusLoop, /last_payload_signature/);
  assert.doesNotMatch(statusLoop, /payload_signature !=/);
  assert.match(capabilities.map((item) => item.identifier).join(","), /taskbar-bars/);
  assert.match(capabilities.map((item) => item.identifier).join(","), /panel/);
});

test("taskbar movement is persisted only by the native drag loop final save", () => {
  assert.doesNotMatch(rustLib, /WindowEvent::Moved/);
  assert.match(rustLib, /save_taskbar_drag_target\(&app, tool, &monitor_key, rect\)/);
  assert.match(rustLib, /taskbar_physical_length_for_window\(logical_length, taskbar\.hwnd\)/);
});

test("local build and test concurrency is capped at four", () => {
  const packageJson = JSON.parse(readFileSync(resolve(here, "../package.json"), "utf8"));
  assert.match(cargoConfig, /jobs\s*=\s*4/);
  assert.match(packageJson.scripts.test, /--test-concurrency=4/);
});

test("Windows CI pins external actions to full commit SHAs", () => {
  const uses = [...windowsCi.matchAll(/^\s*uses:\s*([^\s#]+)/gm)].map((match) => match[1]);
  assert.ok(uses.length >= 5);
  for (const action of uses) {
    assert.match(action, /^[^@\s]+@[0-9a-f]{40}$/);
  }
  assert.match(windowsCi, /run:\s*npm test/);
  assert.match(windowsCi, /runtime-smoke\.ps1/);
  assert.match(runtimeSmoke, /PrivateMemorySize64/);
  assert.match(runtimeSmoke, /WorkingSet64/);
});

test("taskbar idle and drag paths reuse snapshots instead of repeated disk and shell scans", () => {
  const dragLoop = rustLib.match(/fn spawn_taskbar_drag_loop[\s\S]*?\n}\n\nfn spawn_taskbar_visibility_loop/)?.[0] ?? "";
  const visibilityLoop = rustLib.match(/fn spawn_taskbar_visibility_loop[\s\S]*?\n}\n\n#\[tauri::command\]/)?.[0] ?? "";
  assert.equal((dragLoop.match(/Settings::load\(\)/g) ?? []).length, 1);
  assert.match(dragLoop, /drag_monitor_key/);
  assert.match(visibilityLoop, /taskbar_dock_snapshot/);
  assert.match(visibilityLoop, /apply_taskbar_dock_with_snapshot/);
  assert.match(rustLib, /struct TaskbarMenuLayout/);
  assert.match(rustLib, /static TASKBAR_LAYOUT_GATE/);
  assert.doesNotMatch(rustLib, /claude_ratio:\s*Mutex/);
});

test("tray quit flushes pending settings before native cleanup and exit", () => {
  assert.match(rustLib, /app\.emit\("app-quit-requested"/);
  assert.match(rustLib, /fn complete_app_quit/);
  assert.match(rustLib, /TaskbarShutdownState/);
  assert.match(settingsJs, /enqueueLatestSettingsSave/);
  assert.match(settingsJs, /flushSettingsAndQuit/);
  assert.match(settingsJs, /register\(\s*"app-quit-requested"/);
  assert.doesNotMatch(rustLib, /tray_quit_menu_id\(\)\s*=>\s*app\.exit/);
});

test("release links use the Windows shell API instead of PATH executable lookup", () => {
  assert.match(rustLib, /ShellExecuteW/);
  assert.doesNotMatch(rustLib, /Command::new\("explorer\.exe"\)/);
});

test("all application version sources stay synchronized", () => {
  const cargoTomlVersion = cargoToml.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const cargoLockVersion = cargoLock.match(
    /\[\[package\]\]\s*name\s*=\s*"agent-juice"\s*version\s*=\s*"([^"]+)"/,
  )?.[1];
  const versions = [
    packageJson.version,
    packageLock.version,
    packageLock.packages[""].version,
    cargoTomlVersion,
    cargoLockVersion,
    tauriConfig.version,
  ];
  assert.deepEqual(new Set(versions), new Set(["0.1.6"]));
});

test("runtime verifier enforces deterministic hang and resource budgets", (t) => {
  assert.match(
    rustLib,
    /if forced_hover\.is_some\(\)\s*\{\s*forced_hover\s*\}\s*else if down/,
  );
  if (!runtimeVerifier) return t.skip("private .ai runtime verifier is unavailable");
  assert.match(runtimeVerifier, /none -> claude -> none -> codex/);
  assert.match(runtimeVerifier, /SendMessageTimeout/);
  assert.match(runtimeVerifier, /IsHungAppWindow/);
  assert.match(runtimeVerifier, /new_application_hang_or_wer_events/);
  assert.match(runtimeVerifier, /CPU .* exceeded/);
  assert.match(runtimeVerifier, /handle growth .* exceeded/);
  assert.match(runtimeVerifier, /thread growth .* exceeded/);
  assert.match(runtimeVerifier, /working set growth .* exceeded/);
  assert.match(runtimeVerifier, /private memory growth .* exceeded/);
});
