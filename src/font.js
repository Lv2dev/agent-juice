export function normalizeFontMode(value) {
  const mode = String(value || "system").toLowerCase();
  return mode === "pretendard" ? "pretendard" : "system";
}

export function applyFont(settings = {}, root = globalThis.document?.documentElement) {
  const mode = normalizeFontMode(settings.font_mode);
  if (!root) return mode;

  if (mode === "system") {
    root.removeAttribute("data-font-mode");
    return mode;
  }

  root.dataset.fontMode = mode;
  return mode;
}
