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
