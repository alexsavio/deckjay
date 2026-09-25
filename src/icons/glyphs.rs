//! Pictograms for item kinds: kids cannot read, so a badge or placeholder
//! shows a shape instead of a label. Every shape is built from the same
//! `Canvas` primitives as the rest of `icons` (convex polygon, rect, circle).

use image::Rgb;

use super::Canvas;

/// The kind of item a key represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used once keys show kinds, progress and shelves")
)]
pub enum Glyph {
    /// Music album.
    Note,
    /// Audiobook.
    Book,
    /// Story or sound effects.
    Star,
    /// Internet radio.
    Waves,
    /// Podcast.
    Mic,
    /// Spotify playlist.
    Spotify,
}

/// Draws `glyph` centered at (`cx`, `cy`) in the canvas's 0.0-1.0 coordinates,
/// with `scale` roughly the glyph's own half-width.
pub(super) fn draw(
    canvas: &mut Canvas,
    glyph: Glyph,
    cx: f32,
    cy: f32,
    scale: f32,
    color: Rgb<u8>,
) {
    match glyph {
        Glyph::Note => note(canvas, cx, cy, scale, color),
        Glyph::Book => book(canvas, cx, cy, scale, color),
        Glyph::Star => star(canvas, cx, cy, scale, color),
        Glyph::Waves => waves(canvas, cx, cy, scale, color),
        Glyph::Mic => mic(canvas, cx, cy, scale, color),
        Glyph::Spotify => spotify(canvas, cx, cy, scale, color),
    }
}

/// One stamped arc: circles of `thickness` radius along a circular path, close
/// enough together to look like a stroked curve. `Canvas` has no stroke
/// primitive, so this is how every curved glyph (waves, mic stand, Spotify
/// bars) draws one.
fn arc(
    c: &mut Canvas,
    center: (f32, f32),
    radius: f32,
    span: (f32, f32),
    thickness: f32,
    color: Rgb<u8>,
) {
    let (cx, cy) = center;
    let (start, end) = span;
    let arc_len = radius * (end - start).to_radians().abs();
    let steps = (arc_len / (thickness * 0.7)).ceil().max(2.0) as usize;
    for i in 0..=steps {
        let t = start + (end - start) * (i as f32 / steps as f32);
        let rad = t.to_radians();
        c.circle(
            cx + rad.cos() * radius,
            cy + rad.sin() * radius,
            thickness,
            color,
        );
    }
}

/// Eighth note: round head, stem, flag.
fn note(c: &mut Canvas, cx: f32, cy: f32, s: f32, color: Rgb<u8>) {
    c.circle(cx - 0.35 * s, cy + 0.45 * s, 0.35 * s, color);
    c.rect(
        cx + 0.10 * s,
        cy - 0.85 * s,
        cx + 0.28 * s,
        cy + 0.45 * s,
        color,
    );
    c.polygon(
        &[
            (cx + 0.28 * s, cy - 0.85 * s),
            (cx + 0.85 * s, cy - 0.55 * s),
            (cx + 0.85 * s, cy - 0.15 * s),
            (cx + 0.28 * s, cy - 0.45 * s),
        ],
        color,
    );
}

/// Open book: two pages meeting at a narrow spine gap.
fn book(c: &mut Canvas, cx: f32, cy: f32, s: f32, color: Rgb<u8>) {
    c.polygon(
        &[
            (cx - 0.85 * s, cy - 0.50 * s),
            (cx - 0.05 * s, cy - 0.62 * s),
            (cx - 0.05 * s, cy + 0.62 * s),
            (cx - 0.85 * s, cy + 0.50 * s),
        ],
        color,
    );
    c.polygon(
        &[
            (cx + 0.85 * s, cy - 0.50 * s),
            (cx + 0.05 * s, cy - 0.62 * s),
            (cx + 0.05 * s, cy + 0.62 * s),
            (cx + 0.85 * s, cy + 0.50 * s),
        ],
        color,
    );
}

/// Five-pointed star: `polygon` only fills convex shapes, so this is an inner
/// pentagon plus five tip triangles, not one non-convex outline.
fn star(c: &mut Canvas, cx: f32, cy: f32, s: f32, color: Rgb<u8>) {
    let point = |radius: f32, deg: f32| {
        let rad = deg.to_radians();
        (cx + rad.sin() * radius * s, cy - rad.cos() * radius * s)
    };
    let outer: Vec<(f32, f32)> = (0..5_usize)
        .map(|k| point(1.0, -90.0 + k as f32 * 72.0))
        .collect();
    let inner: Vec<(f32, f32)> = (0..5_usize)
        .map(|k| point(0.42, -90.0 + 36.0 + k as f32 * 72.0))
        .collect();
    c.polygon(&inner, color);
    for k in 0..5_usize {
        let prev = inner[(k + 4) % 5];
        c.polygon(&[outer[k], prev, inner[k]], color);
    }
}

/// Broadcast waves: a source dot with two nested arcs fanning to the right.
fn waves(c: &mut Canvas, cx: f32, cy: f32, s: f32, color: Rgb<u8>) {
    let origin = (cx - 0.55 * s, cy);
    c.circle(origin.0, origin.1, 0.16 * s, color);
    for radius_mul in [0.48_f32, 0.82] {
        arc(c, origin, radius_mul * s, (-55.0, 55.0), 0.10 * s, color);
    }
}

/// Microphone: capsule head (circle-rect-circle) on a stand.
fn mic(c: &mut Canvas, cx: f32, cy: f32, s: f32, color: Rgb<u8>) {
    c.circle(cx, cy - 0.55 * s, 0.32 * s, color);
    c.rect(
        cx - 0.32 * s,
        cy - 0.55 * s,
        cx + 0.32 * s,
        cy + 0.05 * s,
        color,
    );
    c.circle(cx, cy + 0.05 * s, 0.32 * s, color);
    c.rect(
        cx - 0.06 * s,
        cy + 0.05 * s,
        cx + 0.06 * s,
        cy + 0.55 * s,
        color,
    );
    c.rect(
        cx - 0.35 * s,
        cy + 0.55 * s,
        cx + 0.35 * s,
        cy + 0.68 * s,
        color,
    );
}

/// Generic (non-brand) playlist mark: a ring around three curved bars.
fn spotify(c: &mut Canvas, cx: f32, cy: f32, s: f32, color: Rgb<u8>) {
    arc(c, (cx, cy), 0.92 * s, (0.0, 360.0), 0.08 * s, color);
    for (radius_mul, y_off) in [(0.55_f32, -0.24), (0.42, 0.0), (0.55, 0.24)] {
        arc(
            c,
            (cx - 0.12 * s, cy + y_off * s),
            radius_mul * s,
            (-38.0, 38.0),
            0.07 * s,
            color,
        );
    }
}
