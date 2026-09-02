use anyhow::Result;
use once_cell::sync::OnceCell;
use resvg::{tiny_skia, usvg};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Palette {
    Traffic,
    Signal,
    Cvd,
    Cool,
    Ocean,
    Forest,
    Sunset,
    Mono([u8; 3]),
    Custom([u8; 3], [u8; 3], [u8; 3]),
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

pub fn worst(a: Option<f32>, b: Option<f32>) -> Option<f32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        _ => None,
    }
}

pub fn color_for(pct: f32, warn: f32, danger: f32, p: Palette) -> String {
    let idx = if pct >= danger {
        2
    } else if pct >= warn {
        1
    } else {
        0
    };

    match p {
        Palette::Traffic => ["#22c55e", "#f59e0b", "#ef4444"][idx].into(),
        Palette::Signal => ["#22c55e", "#f59e0b", "#ef4444"][idx].into(),
        Palette::Cvd => ["#0072b2", "#e69f00", "#cc79a7"][idx].into(),
        Palette::Cool => ["#14b8a6", "#6366f1", "#ec4899"][idx].into(),
        Palette::Ocean => ["#0f9fb5", "#377bd3", "#6d5bd0"][idx].into(),
        Palette::Forest => ["#4f8a64", "#b18432", "#c6535d"][idx].into(),
        Palette::Sunset => ["#d9823d", "#d2576f", "#9658b3"][idx].into(),
        Palette::Mono(base) => [hex(base), "#f59e0b".into(), "#ef4444".into()][idx].clone(),
        Palette::Custom(s, w, d) => hex([s, w, d][idx]),
    }
}

fn dash(pct: Option<f32>, c: f32) -> (f32, f32) {
    let p = pct.unwrap_or(0.0).clamp(0.0, 100.0);
    let arc = p / 100.0 * c;
    (arc, c - arc)
}

pub fn ring_svg(
    outer: Option<f32>,
    inner: Option<f32>,
    center: Option<f32>,
    outer_color: &str,
    inner_color: &str,
) -> String {
    let (outer_circumference, inner_circumference) = (251.327_f32, 169.646_f32);
    let (outer_arc, outer_rest) = dash(outer, outer_circumference);
    let (inner_arc, inner_rest) = dash(inner, inner_circumference);
    let label = center
        .map(|value| format!("{}", value.round() as i32))
        .unwrap_or_else(|| "–".into());

    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100">
<circle cx="50" cy="50" r="40" fill="none" stroke="#3a3a3a" stroke-width="9"/>
<circle cx="50" cy="50" r="40" fill="none" stroke="{outer_color}" stroke-width="9" stroke-linecap="round" stroke-dasharray="{outer_arc} {outer_rest}" transform="rotate(-90 50 50)"/>
<circle cx="50" cy="50" r="27" fill="none" stroke="#2a2a2a" stroke-width="8"/>
<circle cx="50" cy="50" r="27" fill="none" stroke="{inner_color}" stroke-width="8" stroke-linecap="round" stroke-dasharray="{inner_arc} {inner_rest}" transform="rotate(-90 50 50)"/>
<text x="50" y="50" text-anchor="middle" dominant-baseline="central" font-family="sans-serif" font-size="26" font-weight="700" fill="#f9fafb">{label}</text>
</svg>"##
    )
}

fn fontdb() -> &'static usvg::fontdb::Database {
    static DB: OnceCell<usvg::fontdb::Database> = OnceCell::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        db
    })
}

pub fn svg_to_png(svg: &str, size: u32) -> Result<Vec<u8>> {
    let opt = usvg::Options {
        fontdb: std::sync::Arc::new(fontdb().clone()),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg, &opt)?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or_else(|| anyhow::anyhow!("pixmap"))?;
    let scale = size as f32 / 100.0;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap.encode_png()?)
}
