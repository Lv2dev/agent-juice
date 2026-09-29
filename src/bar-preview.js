import { DEFAULT_SETTINGS, representativeByTool } from "./panel-state.js";
import { normalizeTextScale } from "./text-scale.js";
import { t } from "./i18n.js";

export const PREVIEW_TOOLS = ["claude", "codex", "grok", "cursor", "antigravity"];

// Form drafts use SettingsInput's flat hex fields; renderers consume Settings' RGB groups.
export function previewDisplaySettings(input = {}) {
  const rgb = value => typeof value === "string" && /^#[0-9a-f]{6}$/i.test(value)
    ? [1, 3, 5].map(at => Number.parseInt(value.slice(at, at + 2), 16)) : null;
  const result = { ...input, tool_colors: { ...input.tool_colors },
    taskbar_text_colors: { ...input.taskbar_text_colors } };
  for (const tool of PREVIEW_TOOLS) for (const period of ["primary", "secondary"]) {
    const color = rgb(input[`${tool}_${period}_color`]);
    if (color) result.tool_colors[`${tool}_${period}`] = color;
  }
  for (const level of ["warning", "danger"]) {
    const color = rgb(input[`tool_${level}_color`]);
    if (color) result.tool_colors[level] = color;
    if (typeof input[`tool_${level}_color_on`] === "boolean") result.tool_colors[`${level}_on`] = input[`tool_${level}_color_on`];
  }
  for (const key of [...PREVIEW_TOOLS, "info", "ring"]) {
    const color = rgb(input[`${key}_text_color`]);
    if (color) result.taskbar_text_colors[key] = color;
    if (typeof input[`${key}_text_color_on`] === "boolean") result.taskbar_text_colors[`${key}_on`] = input[`${key}_text_color_on`];
  }
  const track = rgb(input.indicator_track_color);
  if (track) result.indicator_track_color = track;
  if (input.palette === "mono" && rgb(input.mono_color)) result.palette = { Mono: rgb(input.mono_color) };
  if (input.palette === "custom") {
    const colors = [input.custom_safe, input.custom_warn, input.custom_danger].map(rgb);
    if (colors.every(Boolean)) result.palette = { Custom: colors };
  }
  return result;
}

export function previewSnapshot(settings, statuses, tool = "claude", textScale = 1, active = true) {
  const selected = PREVIEW_TOOLS.includes(tool) ? tool : "claude";
  const current = representativeByTool(statuses)[selected];
  const sample = !current || ![current.primary, current.secondary].some(limit =>
    typeof limit?.used_percent === "number" && Number.isFinite(limit.used_percent));
  const reset = hours => new Date(Date.now() + hours * 3_600_000).toISOString();
  const status = sample ? {
    tool: selected, approx: true, session: { active: true }, captured_at: new Date().toISOString(),
    primary: { label: selected === "cursor" ? "cursor_models" : selected === "grok" ? "week" : "5h", used_percent: 30,
      resets_at: selected === "cursor" ? reset(168).slice(0,10) : reset(selected === "grok" ? 144 : 2) },
    secondary: selected === "grok" ? null : { label: selected === "cursor" ? "other_models" : "week", used_percent: 65,
      resets_at: selected === "cursor" ? reset(168).slice(0,10) : reset(144) },
  } : current;
  return { type: "juice-bar-preview", tool: selected, settings: { ...DEFAULT_SETTINGS, ...previewDisplaySettings(settings), [`show_${selected}`]: true },
    statuses: [status], textScale: normalizeTextScale(textScale), active, sample };
}

export function createBarPreview(doc = document, host = window) {
  const frame = doc.querySelector("[data-bar-preview]");
  const picker = doc.querySelector("[data-preview-tool]");
  const source = doc.querySelector("[data-preview-source]");
  if (!frame || !picker) return { update() {}, setActive() {} };
  let state = { settings: DEFAULT_SETTINGS, statuses: [], textScale: 1, active: false };
  let loaded = false;
  const origin = host.location.origin;
  const send = () => {
    if (!loaded || !frame.contentWindow) return;
    const payload = previewSnapshot(state.settings, state.statuses, picker.value, state.textScale, state.active);
    if (source) source.textContent = t(payload.sample ? "preview.sample" : "preview.live", state.settings);
    frame.title = t("preview.title", state.settings);
    frame.contentWindow.postMessage(payload, origin === "null" ? "*" : origin);
  };
  frame.addEventListener("load", send);
  picker.addEventListener("change", send);
  const receive = event => {
    if (event.source !== frame.contentWindow || event.origin !== origin) return;
    if (event.data?.type !== "juice-bar-preview-size") return;
    const width = event.data.width;
    if (typeof width === "number" && Number.isFinite(width) && width >= 1 && width <= 2304) {
      frame.style.width = `${Math.ceil(width)}px`;
    }
  };
  host.addEventListener("message", receive);
  host.addEventListener("pagehide", () => host.removeEventListener("message", receive), { once: true });
  return {
    update(next) { state = { ...state, ...next }; send(); },
    setActive(active) {
      state.active = active;
      if (active && !loaded) {
        loaded = true;
        frame.src = "bar.html?preview=1";
      }
      send();
    },
  };
}
