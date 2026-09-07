export const TEXT_SCALE_EVENT = "system-text-scale-updated";

export function normalizeTextScale(value) {
  return typeof value === "number" && Number.isFinite(value)
    ? Math.min(2.25, Math.max(1, value)) : 1;
}

export function createTextScaleState(onChange = () => {}, getRoot = () => globalThis.document?.documentElement) {
  let factor = 1;
  let revision = -1;
  let disposed = false;
  let pendingRead = null;
  return {
    get factor() { return factor; },
    accept(snapshot) {
      if (disposed || !snapshot || !Number.isFinite(snapshot.factor)
        || !Number.isSafeInteger(snapshot.revision) || snapshot.revision < 0
        || snapshot.revision < revision) return false;
      const next = normalizeTextScale(snapshot.factor);
      const changed = next !== factor;
      revision = snapshot.revision;
      factor = next;
      const root = getRoot();
      root?.style?.setProperty("--system-text-scale", String(factor));
      if (root?.dataset) root.dataset.textScale = factor > 1 ? "enlarged" : "normal";
      if (changed) onChange(factor);
      return changed;
    },
    load(invoke) {
      if (disposed) return Promise.resolve();
      pendingRead ??= Promise.resolve()
        .then(() => invoke("get_system_text_scale"))
        .then((snapshot) => this.accept(snapshot))
        .catch(() => {})
        .finally(() => { pendingRead = null; });
      return pendingRead;
    },
    dispose() { disposed = true; },
  };
}

export function fittedRingNumberSize(fontSize, textWidth, textHeight, diameter, outline = 0) {
  const diagonal = Math.hypot(textWidth, textHeight);
  if (!(diagonal > 0) || !(diameter > 0)) return fontSize;
  const available = Math.max(1, diameter - 2 * outline);
  return Math.floor(fontSize * Math.min(1, available / diagonal) * 10) / 10;
}
