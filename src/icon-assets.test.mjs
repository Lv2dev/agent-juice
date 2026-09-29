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
  assert.match(readme, /Grok Build 사용량 자동 수집/);
  assert.match(readme, /Grok은 기존 사용자에게[^\n]+기본값이 \*\*꺼짐\*\*/);
  assert.match(readme, /한 번 `initialize`한 뒤 persistent connection으로 `_x\.ai\/billing`만 재사용합니다/);
  assert.match(readme, /주간이면 `주간`, 월간이면 `월간` 한도 하나만 표시합니다/);
  assert.match(readme, /Automatic Grok Build usage collection/);
  assert.match(readme, /Grok defaults to \*\*off\*\*/);
  assert.match(readme, /initializes the logged-in Grok Build official ACP once and reuses one persistent connection only for `_x\.ai\/billing`/);
  assert.match(readme, /weekly ACP period appears as one `Weekly` limit and a monthly period as one `Monthly` limit/);
  assert.match(readme, /Cursor 사용량과 토큰 활동 자동 수집/);
  assert.match(readme, /Cursor는 기존 사용자에게 새 네 번째 바가 갑자기 생기지 않도록 기본값이 \*\*꺼짐\*\*/);
  assert.match(readme, /Dashboard의 `Auto`는 \*\*Cursor Models\*\*, `API`는 \*\*Other Models\*\*/);
  assert.match(readme, /Automatic Cursor usage and token activity collection/);
  assert.match(readme, /Cursor defaults to \*\*off\*\*/);
  assert.match(readme, /Dashboard Auto maps to \*\*Cursor Models\*\* and API maps to \*\*Other Models\*\*/);
  assert.match(readme, /Cursor Agent CLI의 bounded `auth\.json` access token과 `cli-config\.json` userId/);
  assert.match(readme, /Cursor 계정 사용량/);
  assert.match(readme, /Cursor Agent CLI `auth\.json` access token and `cli-config\.json` userId/);
  assert.match(readme, /Cursor account usage/);
  assert.match(readme, /4가지 바 모드/);
  assert.match(readme, /four bar modes/);
  assert.match(readme, /원·바 표현 스타일/);
  assert.match(readme, /Ring and bar visual styles/);
  assert.match(readme, /업데이트 확인과 알림/);
  assert.match(readme, /Update checks and notifications/);
  assert.match(readme, /테마·팔레트·도구별 색상/);
  assert.match(readme, /Theme, palettes, and per-tool colors/);
  assert.match(readme, /경고 시 색상 변경/);
  assert.match(readme, /위험 시 색상 변경/);
  assert.match(readme, /Recolor on warning/);
  assert.match(readme, /Recolor on danger/);
  assert.match(readme, /정보와 로컬 처리 원칙/);
  assert.match(readme, /About and local processing/);
  assert.match(readme, /보조 모니터로 이동/);
  assert.match(readme, /Move to another monitor/);
  assert.match(readme, /합성 데모/);
  assert.match(readme, /synthetic demo/);
  assert.match(readme, /Claude 계정 자동 조회는 요청을 줄이기 위해 최소 5분 간격/);
  assert.match(readme, /automatic Claude account queries run no more often than every five minutes/);
});

test("README uses localized current UI assets with bounded motion", () => {
  const readme = readFileSync(resolve(projectRoot, "README.md"), "utf8").replace(/\r\n?/g, "\n");
  // Panel shots are 2x renders of the 620px panel; composites are 1280px pages at 2x.
  // Heights follow the localized content, so they are bounded rather than exact.
  const dimensions = {
    "hero.png": [2560, 1900, 2400],
    "panel-skins.png": [2560, 1100, 1400],
    "panel-overview.png": [1240, 1400, 1700],
    "panel-activity.png": [1240, 950, 1250],
    "panel-appearance.png": [1240, 1750, 2150],
    "panel-taskbar.png": [1240, 1950, 2450],
    "panel-collection.png": [1240, 1600, 2000],
    "panel-effects.png": [1240, 2900, 3400],
    "panel-update.png": [1240, 850, 1100],
  };
  for (const language of ["ko", "en"]) {
    for (const [suffix, [width, minHeight, maxHeight]] of Object.entries(dimensions)) {
      const name = `juice-v028-${language}-${suffix}`;
      const file = resolve(projectRoot, "docs/assets", name);
      assert.ok(readme.includes(`docs/assets/${name}`), name);
      const [actualWidth, actualHeight] = pngDimensions(file);
      assert.equal(actualWidth, width, name);
      assert.ok(actualHeight >= minHeight && actualHeight <= maxHeight, `${name} height ${actualHeight}`);
      assert.ok(readFileSync(file).length > 10000);
    }
    assert.doesNotMatch(readme, new RegExp(`docs/assets/juice-v021-${language}-(?:hero|panel-)`));
    for (const style of ["modes", "bars"]) {
      const file = resolve(projectRoot, `docs/assets/juice-v021-${language}-taskbar-${style}.png`);
      const [width, height] = pngDimensions(file);
      assert.ok(width >= 2200 && width <= 2500);
      assert.ok(height >= 1150 && height <= 1300);
    }
    const gif = resolve(projectRoot, `docs/assets/juice-v021-${language}-multi-monitor.gif`);
    assert.deepEqual(gifDimensions(gif), [1280, 660]);
    assert.ok(readFileSync(gif).length < 1500000);
    const stats = JSON.parse(execFileSync("python", ["-c", `
import json, sys
from pathlib import Path
from PIL import Image
im = Image.open(sys.argv[1])
durations = []
for i in range(im.n_frames):
    im.seek(i)
    durations.append(im.info.get("duration", 0))
boards = []
colors = [(215,154,50), (47,172,125), (217,87,139), (133,132,127)]
widths = [180,177,133,249] if sys.argv[2] == "ko" else [194,191,147,285]
for style in ["modes", "bars"]:
    image = Image.open(Path(sys.argv[1]).with_name(f"juice-v021-{sys.argv[2]}-taskbar-{style}.png")).convert("RGB")
    left = (40 + 146) * 2
    for width, color in zip(widths, colors):
        ys = [y for y in range(420, image.height - 90) if any(
            sum((image.getpixel((x,y))[i]-color[i])**2 for i in range(3)) < 300
            for x in range(left, left + 73))]
        groups = []
        for y in ys:
            if not groups or y - groups[-1][-1] > 40:
                groups.append([])
            groups[-1].append(y)
        boards.append({"style": style, "heights": [max(g)-min(g)+1 for g in groups]})
        left += (width + 40) * 2
print(json.dumps({"frames": im.n_frames, "duration": sum(durations), "boards": boards}))
`, gif, language], { encoding: "utf8", windowsHide: true }));
    assert.equal(stats.frames, 150);
    assert.equal(stats.duration, 7500);
    for (const board of stats.boards) {
      assert.equal(board.heights.length, 4, `missing mode indicator: ${JSON.stringify(board)}`);
      for (const [row, height] of board.heights.entries()) {
        if (board.style === "modes") {
          const minimum = row === 3 ? 38 : 68;
          assert.ok(height >= minimum && height <= 74, `ring raster size: ${height}`);
        } else assert.ok(height >= 6 && height <= 36, `horizontal bar raster size: ${height}`);
      }
    }
  }
  assert.doesNotMatch(readme, /docs\/assets\/juice-v014/);
  assert.match(readme, /같은 4개 모드를 원 대신 위아래 두 줄의 가로 바로 표시합니다/);
  assert.match(readme, /The same four modes use two stacked horizontal bars instead of rings/);

  assert.doesNotMatch(readme, /docs\/assets\/juice-panel-/);
  assert.doesNotMatch(readme, /docs\/assets\/juice-taskbar-modes\.png/);
  assert.doesNotMatch(readme, /docs\/assets\/juice-v014-panel-about\.png/);
  assert.doesNotMatch(readme, /docs\/assets\/juice-v014-panel-settings-full\.png/);
  assert.match(readme, /\| 기본 \|[\s\S]*\| 수집 \|[\s\S]*\| 표시줄 \|[\s\S]*\| 색상 \|[\s\S]*\| 표시기·글자 \|/);
  assert.match(readme, /\| General \|[\s\S]*\| Collection \|[\s\S]*\| Taskbar \|[\s\S]*\| Colors \|[\s\S]*\| Indicators & text \|/);
  assert.doesNotMatch(readme, /\| 외형 \||\| 표시·수집 \||\| 원·바 세부 \|/);
  assert.match(readme, /최초 실행에서는 표시 중인 바를 작업표시줄 왼쪽부터 서로 겹치지 않게 배치합니다/);
  assert.match(readme, /숨겨 둔 도구를 나중에 켜면 기존 바를 움직이지 않고 작업표시줄의 첫 빈 위치에 배치합니다/);
  assert.match(readme, /바에 마우스를 올리면 도구명과 실제로 존재하는 5h·주간·월간 한도의 초기화까지 남은 시간을 보여줍니다/);
  assert.match(readme, /On first launch, visible bars are placed from the left edge of the taskbar without overlapping/);
  assert.match(readme, /Enabling a previously hidden tool places it in the first free taskbar position without moving existing bars/);
  assert.match(readme, /Hovering a bar shows the tool name and time remaining until each available 5-hour, weekly, or monthly limit resets/);
  assert.match(readme, /전체화면 숨김:[^\n]*신규 설치 기본값은 꺼짐입니다/);
  assert.match(readme, /Fullscreen hiding:[^\n]*Off by default on a new installation/);
  assert.doesNotMatch(readme, /\b(?:RAII|HWND|AppHang)\b|z-order/i);


});
