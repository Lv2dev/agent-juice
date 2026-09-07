import assert from "node:assert/strict";
import test from "node:test";
import { createTextScaleState, fittedRingNumberSize, normalizeTextScale } from "./text-scale.js";
import { barViewModel } from "./bar-state.js";

test("system text scale is independent of DPI and preserves a newer event over a slow initial read", async () => {
  const styles = new Map();
  const root = { dataset: {}, style: { setProperty: (name, value) => styles.set(name, value) } };
  const changes = [];
  const state = createTextScaleState((factor) => changes.push(factor), () => root);
  let resolveRead;
  const loading = state.load(() => new Promise((resolve) => { resolveRead = resolve; }));
  assert.equal(state.load(() => { throw new Error("duplicate read"); }), loading);
  await Promise.resolve();
  state.accept({ factor: 1.5, revision: 2 });
  resolveRead({ factor: 1, revision: 0 });
  await loading;
  assert.equal(state.factor, 1.5);
  assert.equal(styles.get("--system-text-scale"), "1.5");
  assert.equal(root.dataset.textScale, "enlarged");
  await state.load(() => Promise.reject(new Error("unavailable")));
  assert.equal(state.factor, 1.5);
  for (const value of [null, {}, { factor: NaN, revision: 3 }, { factor: 2, revision: -1 }]) {
    assert.equal(state.accept(value), false);
  }
  state.accept({ factor: 1, revision: 3 });
  assert.equal(root.dataset.textScale, "normal");
  state.dispose();
  assert.equal(state.accept({ factor: 2.25, revision: 4 }), false);
  assert.deepEqual(changes, [1.5, 1]);
  assert.equal(normalizeTextScale(9), 2.25);
  assert.equal(normalizeTextScale(-1), 1);
  assert.equal(normalizeTextScale(Infinity), 1);
});

test("large system text expands indicators within taskbar height without changing saved settings", () => {
  const settings = { ring_size_px: 36, ring_number_font_size_px: 9, bar_text_font_size_px: 11 };
  const before = JSON.stringify(settings);
  for (const textScale of [1, 1.25, 1.5, 2, 2.25]) {
    for (const crossAxisSize of [40, 48, 64]) {
      const vm = barViewModel([], settings, new Date(), { textScale, crossAxisSize });
      assert.ok(vm.ringSizePx <= crossAxisSize);
      assert.ok(vm.barTextFontSizePx * 1.4 <= crossAxisSize);
      assert.equal(vm.ringNumberFontSizePx, 9 * textScale);
      assert.ok(vm.ringCenterSizePx >= 16);
      const size = fittedRingNumberSize(vm.ringNumberFontSizePx, 30, 22, vm.ringCenterSizePx, 1.2);
      assert.ok(size > 0 && size <= vm.ringNumberFontSizePx);
      assert.ok(Math.hypot(30, 22) * size / vm.ringNumberFontSizePx + 2.4 <= vm.ringCenterSizePx + 0.01);
    }
  }
  assert.equal(JSON.stringify(settings), before);
  const normal = barViewModel([], settings);
  assert.equal(normal.ringSizePx, 36);
  assert.equal(normal.barTextFontSizePx, 11);
  assert.equal(normal.ringNumberFontSizePx, 9);
});
