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

function gifDimensions(path) {
  const buffer = readFileSync(path);
  assert.match(buffer.subarray(0, 6).toString("ascii"), /^GIF8[79]a$/, `${path} is not a GIF`);
  return [buffer.readUInt16LE(6), buffer.readUInt16LE(8)];
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
  assert.match(brand, /width="330" height="150" viewBox="0 0 330 150"/);

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

  assert.match(readme, /Claude 계정 사용량 자동 수집/);
  assert.match(readme, /기본값은 \*\*켜짐\*\*/);
  assert.match(readme, /정확 OAuth 계정 한도는 statusline의 오래된 계정 값보다 우선합니다/);
  assert.match(readme, /Automatic Claude account usage collection/);
  assert.match(readme, /on by default/);
  assert.match(readme, /Exact OAuth account limits take priority over stale statusline account values/);
  assert.match(readme, /4가지 바 모드/);
  assert.match(readme, /four bar modes/);
  assert.match(readme, /원·바 표현 스타일/);
  assert.match(readme, /Ring and bar visual styles/);
  assert.match(readme, /업데이트 확인과 알림/);
  assert.match(readme, /Update checks and notifications/);
  assert.match(readme, /테마·팔레트·도구별 색상/);
  assert.match(readme, /Theme, palettes, and per-tool colors/);
  assert.match(readme, /정보와 로컬 처리 원칙/);
  assert.match(readme, /About and local processing/);
  assert.match(readme, /보조 모니터로 이동/);
  assert.match(readme, /Move to another monitor/);
  assert.match(readme, /합성 데모/);
  assert.match(readme, /synthetic demo/);
  assert.match(readme, /기본 수집주기와 Claude 계정 조회 캐시는 모두 60초입니다/);
  assert.match(readme, /default collection interval and Claude account cache are both 60 seconds/);
});

test("README uses current Tauri panel and taskbar capture assets", () => {
  const readme = readFileSync(resolve(projectRoot, "README.md"), "utf8").replace(/\r\n?/g, "\n");
  const assets = new Map([
    ["juice-v013-panel-overview.png", [1240, 1032]],
    ["juice-v013-panel-appearance.png", [1240, 1280]],
    ["juice-v013-panel-settings-full.png", [1240, 4584]],
    ["juice-v013-panel-taskbar.png", [1240, 1652]],
    ["juice-v013-panel-collection.png", [1240, 1036]],
    ["juice-v013-panel-effects.png", [1240, 1182]],
    ["juice-v013-panel-update.png", [1240, 726]],
    ["juice-v013-panel-about.png", [1240, 628]],
    ["juice-v013-taskbar-modes.png", [1520, 584]],
  ]);

  for (const [name, dimensions] of assets) {
    const path = resolve(projectRoot, `docs/assets/${name}`);
    assert.ok(existsSync(path), `${name} is missing`);
    assert.match(readme, new RegExp(`docs/assets/${name.replaceAll(".", "\\.")}`));
    assert.deepEqual(pngDimensions(path), dimensions, `${name} dimensions changed`);
    assert.ok(readFileSync(path).length > 10_000, `${name} is unexpectedly small`);
  }

  const gifName = "juice-v013-multi-monitor.gif";
  const gifPath = resolve(projectRoot, `docs/assets/${gifName}`);
  assert.ok(existsSync(gifPath), `${gifName} is missing`);
  assert.match(readme, new RegExp(`docs/assets/${gifName.replaceAll(".", "\\.")}`));
  assert.deepEqual(gifDimensions(gifPath), [960, 420]);
  assert.ok(readFileSync(gifPath).length > 100_000, `${gifName} is unexpectedly small`);
  assert.ok(readFileSync(gifPath).length < 1_500_000, `${gifName} is too large for the README`);
  const gifStats = JSON.parse(
    execFileSync(
      "python",
      [
        "-c",
        "import json,sys; from PIL import Image; im=Image.open(sys.argv[1]); durations=[]; [(im.seek(i), durations.append(im.info.get('duration', 0))) for i in range(im.n_frames)]; print(json.dumps({'frames': im.n_frames, 'duration': sum(durations)}))",
        gifPath,
      ],
      { encoding: "utf8" },
    ),
  );
  assert.ok(gifStats.frames >= 20, `${gifName} does not have enough motion frames`);
  assert.ok(gifStats.duration >= 3_000 && gifStats.duration <= 8_000, `${gifName} has an unsuitable duration`);

  assert.doesNotMatch(readme, /docs\/assets\/juice-panel-/);
  assert.doesNotMatch(readme, /docs\/assets\/juice-taskbar-modes\.png/);
});
