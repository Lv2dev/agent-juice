import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const css = readFileSync(resolve(here, "styles.css"), "utf8").replace(/\r\n?/g, "\n");
const iconDir = resolve(here, "../src-tauri/icons");
const projectRoot = resolve(here, "..");

function cssToken(name) {
  const match = css.match(new RegExp(`${name}:\\s*(#[0-9a-fA-F]{6})`));
  return match?.[1] ?? "";
}

function pngDimensions(path) {
  const buffer = readFileSync(path);
  assert.equal(buffer.subarray(1, 4).toString("ascii"), "PNG", `${path} is not a PNG`);
  return [buffer.readUInt32BE(16), buffer.readUInt32BE(20)];
}

test("app and tray icons use the same vertical capsule mark as the settings logo", () => {
  const script = String.raw`
import json
import sys
from pathlib import Path
from PIL import Image

def rgb(hex_color):
    hex_color = hex_color.lstrip("#")
    return tuple(int(hex_color[i:i+2], 16) for i in (0, 2, 4))

def distance(a, b):
    return sum((int(x) - int(y)) ** 2 for x, y in zip(a, b)) ** 0.5

def average_patch(image, x, y, radius=3):
    pixels = []
    for yy in range(max(0, y - radius), min(image.height, y + radius + 1)):
        for xx in range(max(0, x - radius), min(image.width, x + radius + 1)):
            r, g, b, a = image.getpixel((xx, yy))
            if a > 48:
                pixels.append((r, g, b))
    if not pixels:
        return (0, 0, 0)
    return tuple(round(sum(channel) / len(pixels)) for channel in zip(*pixels))

def mark_stats(path):
    image = Image.open(path).convert("RGBA")
    opaque = [
        (x, y)
        for y in range(image.height)
        for x in range(image.width)
        if image.getpixel((x, y))[3] > 64
    ]
    if not opaque:
        raise AssertionError(f"{path.name} has no opaque mark")
    xs = [point[0] for point in opaque]
    ys = [point[1] for point in opaque]
    left, right = min(xs), max(xs)
    top, bottom = min(ys), max(ys)
    mark_w = right - left + 1
    mark_h = bottom - top + 1
    center_x = round((left + right) / 2)
    top_y = top + max(1, round(mark_h * 0.10))
    bottom_y = bottom - max(1, round(mark_h * 0.10))
    corners = [
        image.getpixel((0, 0))[3],
        image.getpixel((image.width - 1, 0))[3],
        image.getpixel((0, image.height - 1))[3],
        image.getpixel((image.width - 1, image.height - 1))[3],
    ]
    return {
        "size": [image.width, image.height],
        "ratio": mark_w / mark_h,
        "topColor": average_patch(image, center_x, top_y),
        "bottomColor": average_patch(image, center_x, bottom_y),
        "cornerAlphaMax": max(corners),
    }

root = Path(sys.argv[1])
warm = rgb(sys.argv[2])
accent = rgb(sys.argv[3])
required = [
    "icon.png",
    "32x32.png",
    "128x128.png",
    "128x128@2x.png",
    "icon.ico",
]
stats = {name: mark_stats(root / name) for name in required}
base = stats["icon.png"]
checks = {
    "required": sorted(path.name for path in root.iterdir() if path.name in required),
    "baseSize": base["size"],
    "baseRatio": base["ratio"],
    "topDistance": distance(base["topColor"], warm),
    "bottomDistance": distance(base["bottomColor"], accent),
    "cornerAlphaMax": base["cornerAlphaMax"],
    "smallRatio": stats["32x32.png"]["ratio"],
    "icoRatio": stats["icon.ico"]["ratio"],
}
print(json.dumps(checks))
`;

  const result = JSON.parse(
    execFileSync("python", ["-c", script, iconDir, cssToken("--accent-warm"), cssToken("--accent")], {
      encoding: "utf8",
    }),
  );

  assert.deepEqual(result.required, [
    "128x128.png",
    "128x128@2x.png",
    "32x32.png",
    "icon.ico",
    "icon.png",
  ]);
  assert.deepEqual(result.baseSize, [512, 512]);
  assert.ok(result.baseRatio > 0.26 && result.baseRatio < 0.52, `ratio ${result.baseRatio}`);
  assert.ok(result.smallRatio > 0.22 && result.smallRatio < 0.58, `small ratio ${result.smallRatio}`);
  assert.ok(result.icoRatio > 0.22 && result.icoRatio < 0.58, `ico ratio ${result.icoRatio}`);
  assert.ok(result.topDistance < 42, `top distance ${result.topDistance}`);
  assert.ok(result.bottomDistance < 42, `bottom distance ${result.bottomDistance}`);
  assert.ok(result.cornerAlphaMax < 8, `corner alpha ${result.cornerAlphaMax}`);
});

test("README is product-focused and opens with the Juice brand lockup", () => {
  const readme = readFileSync(resolve(projectRoot, "README.md"), "utf8").replace(/\r\n?/g, "\n");
  const brandPath = resolve(projectRoot, "docs/assets/juice-brand.svg");

  assert.ok(existsSync(brandPath), "README brand lockup asset is missing");
  assert.match(
    readme,
    /^<p align="center">\n\s*<img src="docs\/assets\/juice-brand\.svg" alt="Juice" width="260">\n<\/p>/,
  );
  assert.doesNotMatch(readme, /src-tauri\/icons\/icon\.png/);

  const brand = readFileSync(brandPath, "utf8");
  assert.match(brand, /<title id="title">Juice<\/title>/);
  assert.match(brand, /<text[^>]*>Juice<\/text>/);

  for (const forbidden of [
    "npm install",
    "npm run tauri",
    "node --test",
    "cargo test",
    "릴리즈 전에는 installer SHA256",
    "Before every release",
    "docs/assets/juice-preview.svg",
  ]) {
    assert.ok(!readme.includes(forbidden), `README still contains internal copy: ${forbidden}`);
  }

  assert.match(readme, /Claude `\/usage` 자동 새로고침/);
  assert.match(readme, /기본값은 \*\*꺼짐\*\*/);
  assert.match(readme, /statusline의 5시간\/주간 값이 있으면 덮어쓰지 않습니다/);
  assert.match(readme, /`Claude \/usage` auto-refresh/);
  assert.match(readme, /off by default/);
  assert.match(readme, /never overwrites existing statusline limits/);
  assert.match(readme, /4가지 바 모드/);
  assert.match(readme, /four bar modes/);
});

test("README uses current Tauri panel and taskbar capture assets", () => {
  const readme = readFileSync(resolve(projectRoot, "README.md"), "utf8").replace(/\r\n?/g, "\n");
  const assets = new Map([
    ["juice-panel-overview.png", [620, 720]],
    ["juice-panel-settings-full.png", [620, 1473]],
    ["juice-panel-taskbar.png", [620, 720]],
    ["juice-panel-lab.png", [620, 720]],
    ["juice-taskbar-modes.png", [760, 292]],
  ]);

  for (const [name, dimensions] of assets) {
    const path = resolve(projectRoot, `docs/assets/${name}`);
    assert.ok(existsSync(path), `${name} is missing`);
    assert.match(readme, new RegExp(`docs/assets/${name.replaceAll(".", "\\.")}`));
    assert.deepEqual(pngDimensions(path), dimensions, `${name} dimensions changed`);
    assert.ok(readFileSync(path).length > 10_000, `${name} is unexpectedly small`);
  }
});
