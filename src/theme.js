export function normalizeTheme(value) {
  const theme = String(value || "system").toLowerCase();
  return theme === "light" || theme === "dark" ? theme : "system";
}

export function applyTheme(settings = {}, root = globalThis.document?.documentElement) {
  const theme = normalizeTheme(settings.theme);
  if (!root) return theme;

  if (theme === "system") {
    root.removeAttribute("data-theme");
    return theme;
  }

  root.dataset.theme = theme;
  return theme;
}

export function normalizePanelSkin(value) {
  return String(value || "fluent").toLowerCase() === "paper" ? "paper" : "fluent";
}

// Paper is a light-only panel look: dark themes (explicit or system) always keep the dark panel.
export function applyPanelSkin(
  settings = {},
  root = globalThis.document?.documentElement,
  prefersDark = globalThis.matchMedia?.("(prefers-color-scheme: dark)")?.matches === true,
) {
  const skin = normalizePanelSkin(settings.panel_skin);
  const theme = normalizeTheme(settings.theme);
  const dark = theme === "dark" || (theme === "system" && prefersDark);
  const look = skin === "paper" && !dark ? "paper" : "default";
  if (!root) return look;
  if (look === "paper") root.dataset.panelLook = "paper";
  else root.removeAttribute?.("data-panel-look");
  return look;
}
