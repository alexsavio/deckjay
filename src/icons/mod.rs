//! Draws key images without any font or icon files: simple shapes in
//! 0.0–1.0 coordinates, rendered 4x oversized and scaled down for smooth edges.

mod glyphs;

use image::imageops::{self, FilterType};
use image::{DynamicImage, GenericImageView, Rgb, RgbImage};

pub use glyphs::Glyph;

const SUPERSAMPLE: u32 = 4;

const WHITE: Rgb<u8> = Rgb([255, 255, 255]);
const HIGHLIGHT: Rgb<u8> = Rgb([255, 205, 0]);
const BG_PLAY: Rgb<u8> = Rgb([22, 150, 70]);
const BG_SKIP: Rgb<u8> = Rgb([90, 70, 190]);
const BG_VOLUME: Rgb<u8> = Rgb([30, 110, 210]);
const BG_MORE: Rgb<u8> = Rgb([235, 120, 20]);
const BG_BLANK: Rgb<u8> = Rgb([0, 0, 0]);
/// Background for a badge chip and a progress track: dark enough to read on
/// any cover.
const DECOR_DARK: Rgb<u8> = Rgb([20, 20, 24]);
const NEW_DOT: Rgb<u8> = Rgb([214, 40, 40]);

struct Canvas {
    img: RgbImage,
    /// Oversampled canvas width in pixels; scales 0.0–1.0 coordinates into pixel space.
    scale: f32,
}

impl Canvas {
    fn new(size: u32, background: Rgb<u8>) -> Self {
        let big = size * SUPERSAMPLE;
        Canvas {
            img: RgbImage::from_pixel(big, big, background),
            scale: big as f32,
        }
    }

    /// Fills a convex polygon given in 0.0–1.0 coordinates.
    fn polygon(&mut self, points: &[(f32, f32)], color: Rgb<u8>) {
        let pts: Vec<(f32, f32)> = points
            .iter()
            .map(|&(x, y)| (x * self.scale, y * self.scale))
            .collect();
        let (min_x, max_x, min_y, max_y) = bounds(&pts, self.img.width());
        for y in min_y..max_y {
            for x in min_x..max_x {
                let p = (x as f32 + 0.5, y as f32 + 0.5);
                if inside_convex(&pts, p) {
                    self.img.put_pixel(x, y, color);
                }
            }
        }
    }

    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: Rgb<u8>) {
        self.polygon(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)], color);
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Rgb<u8>) {
        let (cx, cy, r) = (cx * self.scale, cy * self.scale, r * self.scale);
        let size = self.img.width() as f32;
        let (x0, x1) = ((cx - r).max(0.0) as u32, (cx + r).min(size).ceil() as u32);
        let (y0, y1) = ((cy - r).max(0.0) as u32, (cy + r).min(size).ceil() as u32);
        for y in y0..y1 {
            for x in x0..x1 {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                if dx * dx + dy * dy <= r * r {
                    self.img.put_pixel(x, y, color);
                }
            }
        }
    }

    fn finish(self, size: u32) -> RgbImage {
        imageops::resize(&self.img, size, size, FilterType::Triangle)
    }
}

fn bounds(pts: &[(f32, f32)], size: u32) -> (u32, u32, u32, u32) {
    let clamp = |v: f32| v.clamp(0.0, size as f32) as u32;
    let min_x = pts.iter().map(|p| p.0).fold(f32::MAX, f32::min);
    let max_x = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max);
    let min_y = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min);
    let max_y = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max);
    (
        clamp(min_x.floor()),
        clamp(max_x.ceil()),
        clamp(min_y.floor()),
        clamp(max_y.ceil()),
    )
}

#[expect(
    clippy::float_cmp,
    reason = "an exact zero cross product means the point is on the edge"
)]
fn inside_convex(pts: &[(f32, f32)], p: (f32, f32)) -> bool {
    let mut sign = 0.0f32;
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        let cross = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        if cross != 0.0 {
            if sign == 0.0 {
                sign = cross.signum();
            } else if cross.signum() != sign {
                return false;
            }
        }
    }
    true
}

// ---------------------------------------------------------------- controls

pub fn play(size: u32) -> RgbImage {
    let mut c = Canvas::new(size, BG_PLAY);
    c.polygon(&[(0.36, 0.26), (0.36, 0.74), (0.76, 0.5)], WHITE);
    c.finish(size)
}

pub fn pause(size: u32) -> RgbImage {
    let mut c = Canvas::new(size, BG_PLAY);
    c.rect(0.30, 0.27, 0.43, 0.73, WHITE);
    c.rect(0.57, 0.27, 0.70, 0.73, WHITE);
    c.finish(size)
}

pub fn next(size: u32) -> RgbImage {
    let mut c = Canvas::new(size, BG_SKIP);
    c.polygon(&[(0.26, 0.28), (0.26, 0.72), (0.60, 0.5)], WHITE);
    c.rect(0.62, 0.28, 0.72, 0.72, WHITE);
    c.finish(size)
}

pub fn prev(size: u32) -> RgbImage {
    let mut c = Canvas::new(size, BG_SKIP);
    c.polygon(&[(0.74, 0.28), (0.74, 0.72), (0.40, 0.5)], WHITE);
    c.rect(0.28, 0.28, 0.38, 0.72, WHITE);
    c.finish(size)
}

/// Volume key with a level bar along the bottom (`level` 0.0–1.0 of the cap).
pub fn volume(size: u32, up: bool, level: f32) -> RgbImage {
    let mut c = Canvas::new(size, BG_VOLUME);
    // speaker
    c.rect(0.14, 0.36, 0.26, 0.58, WHITE);
    c.polygon(
        &[(0.26, 0.36), (0.42, 0.22), (0.42, 0.72), (0.26, 0.58)],
        WHITE,
    );
    // minus / plus
    c.rect(0.52, 0.44, 0.84, 0.50, WHITE);
    if up {
        c.rect(0.65, 0.31, 0.71, 0.63, WHITE);
    }
    // level bar
    let dim = Rgb([15, 60, 120]);
    c.rect(0.12, 0.82, 0.88, 0.90, dim);
    if level > 0.0 {
        c.rect(0.12, 0.82, 0.12 + 0.76 * level.min(1.0), 0.90, WHITE);
    }
    c.finish(size)
}

/// "More albums" arrow, with one dot per page and the current page filled in.
pub fn more(size: u32, page: usize, pages: usize) -> RgbImage {
    let mut c = Canvas::new(size, BG_MORE);
    c.rect(0.20, 0.36, 0.56, 0.52, WHITE);
    c.polygon(&[(0.54, 0.20), (0.54, 0.68), (0.82, 0.44)], WHITE);
    let (dots, filled) = page_dots(page, pages);
    let spacing = 0.11;
    let start = 0.5 - spacing * (dots as f32 - 1.0) / 2.0;
    for i in 0..dots {
        let r = if i == filled { 0.045 } else { 0.025 };
        c.circle(start + spacing * i as f32, 0.84, r, WHITE);
    }
    c.finish(size)
}

/// (dots drawn, index of the filled dot). At most 8 dots fit on a key, so
/// with more pages neighbouring pages share a dot.
fn page_dots(page: usize, pages: usize) -> (usize, usize) {
    let dots = pages.clamp(1, 8);
    (dots, page * dots / pages.max(1))
}

pub fn blank(size: u32) -> RgbImage {
    RgbImage::from_pixel(size, size, BG_BLANK)
}

// ------------------------------------------------------------------ albums

/// Center-cropped square thumbnail of a cover image.
pub fn thumbnail(cover: &DynamicImage, size: u32) -> RgbImage {
    let (w, h) = cover.dimensions();
    let short = w.min(h);
    let target = size * 4;
    if short > target {
        // Lanczos3's kernel grows with the shrink ratio, so a box filter does
        // the bulk first: about 4x faster on a 3000 px cover, no visible change.
        let cover = cover.thumbnail_exact(w * target / short, h * target / short);
        return cover
            .resize_to_fill(size, size, FilterType::Lanczos3)
            .into_rgb8();
    }
    cover
        .resize_to_fill(size, size, FilterType::Lanczos3)
        .into_rgb8()
}

/// Deterministic color for `name` (FNV-1a hash to hue), so a placeholder
/// without a configured color still gets a stable, distinct one.
pub fn name_color(name: &str) -> Rgb<u8> {
    let hash = name.bytes().fold(2_166_136_261_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(16_777_619)
    });
    hue_to_rgb((hash % 360) as f32)
}

/// Colored tile with a music note, for albums without a cover.
pub fn placeholder(name: &str, size: u32) -> RgbImage {
    let mut c = Canvas::new(size, name_color(name));
    c.circle(0.40, 0.66, 0.12, WHITE);
    c.rect(0.47, 0.22, 0.53, 0.66, WHITE);
    c.polygon(
        &[(0.47, 0.22), (0.72, 0.32), (0.72, 0.42), (0.47, 0.32)],
        WHITE,
    );
    c.finish(size)
}

/// Colored tile with a large white glyph, for an item without a picture.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used once keys show kinds, progress and shelves")
)]
pub fn glyph_placeholder(glyph: Glyph, color: Rgb<u8>, size: u32) -> RgbImage {
    let mut c = Canvas::new(size, color);
    glyphs::draw(&mut c, glyph, 0.5, 0.5, 0.30, WHITE);
    c.finish(size)
}

/// Draws the "now playing" frame on an album tile.
pub fn with_highlight(tile: &RgbImage) -> RgbImage {
    decorate(
        tile,
        Decor {
            current: true,
            ..Decor::default()
        },
    )
}

// ------------------------------------------------------------- decorations

/// What to draw on top of a tile (a cover or a placeholder). `progress` is
/// steps out of 10; values above 10 clamp to 10.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decor {
    pub current: bool,
    pub badge: Option<Glyph>,
    pub progress: Option<u8>,
    pub new: bool,
}

/// Draws `decor` onto `tile`. Every decoration keeps `inset(size)` clear of
/// the edge, so the highlight frame never cuts into one, whatever order they
/// are drawn in; the frame is drawn last regardless.
pub fn decorate(tile: &RgbImage, decor: Decor) -> RgbImage {
    let mut img = tile.clone();
    let size = img.width();
    let margin = inset(size) as f32 / size as f32;
    if let Some(glyph) = decor.badge {
        badge(&mut img, glyph, margin);
    }
    if let Some(step) = decor.progress {
        progress(&mut img, step.min(10), margin);
    }
    if decor.new {
        new_dot(&mut img, margin);
    }
    if decor.current {
        frame(&mut img, inset(size), HIGHLIGHT);
    }
    img
}

/// Highlight-frame thickness for a `size`-px key, also the margin every
/// decoration keeps from the edge.
fn inset(size: u32) -> u32 {
    (size as f32 * 0.08).round().max(2.0) as u32
}

/// Solid frame, `t` px thick, around the edge of `img`.
fn frame(img: &mut RgbImage, t: u32, color: Rgb<u8>) {
    let (w, h) = img.dimensions();
    for y in 0..h {
        for x in 0..w {
            if x < t || y < t || x >= w - t || y >= h - t {
                img.put_pixel(x, y, color);
            }
        }
    }
}

/// Renders `draw` (in `WHITE`, on a black background) as a `size`-square
/// coverage mask: `Canvas` has no notion of an existing image to draw onto,
/// so a decoration is rendered on its own, then blended over the tile.
fn mask(size: u32, draw: impl FnOnce(&mut Canvas)) -> RgbImage {
    let mut c = Canvas::new(size, BG_BLANK);
    draw(&mut c);
    c.finish(size)
}

/// Blends `color` onto `img`, weighted by `mask`'s brightness (0 none, 255
/// full) times `opacity`. `mask` and `img` must be the same size.
fn blend(img: &mut RgbImage, mask: &RgbImage, color: Rgb<u8>, opacity: f32) {
    for (px, mp) in img.pixels_mut().zip(mask.pixels()) {
        let a = f32::from(mp[0]) / 255.0 * opacity;
        if a <= 0.0 {
            continue;
        }
        for i in 0..3 {
            px[i] = (f32::from(px[i]) * (1.0 - a) + f32::from(color[i]) * a).round() as u8;
        }
    }
}

/// Small chip in the top-left corner naming the item's kind.
fn badge(img: &mut RgbImage, glyph: Glyph, margin: f32) {
    let size = img.width();
    let chip = 0.30;
    let center = margin + chip / 2.0;
    let chip_mask = mask(size, |c| {
        c.rect(margin, margin, margin + chip, margin + chip, WHITE);
    });
    blend(img, &chip_mask, DECOR_DARK, 0.78);
    let glyph_mask = mask(size, |c| {
        glyphs::draw(c, glyph, center, center, chip * 0.38, WHITE);
    });
    blend(img, &glyph_mask, WHITE, 1.0);
}

/// Playback progress bar along the bottom edge, `step` of 10 filled.
fn progress(img: &mut RgbImage, step: u8, margin: f32) {
    let size = img.width();
    let bar_h = 0.14;
    let (y0, y1) = (1.0 - margin - bar_h, 1.0 - margin);
    let track_mask = mask(size, |c| c.rect(margin, y0, 1.0 - margin, y1, WHITE));
    blend(img, &track_mask, DECOR_DARK, 0.65);
    if step > 0 {
        let x1 = margin + (1.0 - 2.0 * margin) * (f32::from(step) / 10.0);
        let fill_mask = mask(size, |c| c.rect(margin, y0, x1, y1, WHITE));
        blend(img, &fill_mask, WHITE, 1.0);
    }
}

/// Red dot with a thin white ring in the top-right corner, marking a new item.
fn new_dot(img: &mut RgbImage, margin: f32) {
    let size = img.width();
    let (cx, cy, r) = (1.0 - margin - 0.10, margin + 0.10, 0.10);
    let ring_mask = mask(size, |c| c.circle(cx, cy, r, WHITE));
    blend(img, &ring_mask, WHITE, 1.0);
    let dot_mask = mask(size, |c| c.circle(cx, cy, r * 0.62, WHITE));
    blend(img, &dot_mask, NEW_DOT, 1.0);
}

// -------------------------------------------------------------------- shelf

/// A shelf key: the shelf's own picture (or a glyph placeholder), framed
/// orange, with one dot per shelf and `position + 1` of them lit.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used once keys show kinds, progress and shelves")
)]
pub fn shelf(tile: &RgbImage, position: usize, count: usize) -> RgbImage {
    let mut img = tile.clone();
    let t = inset(img.width());
    frame(&mut img, t, BG_MORE);
    fill_dots(&mut img, position, count, 0.84, WHITE);
    img
}

/// Flip key for small decks: pages the current shelf, then moves to the next
/// one. Orange background like `more`, a double-chevron in place of its
/// single arrow, plus one dot per step.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used once keys show kinds, progress and shelves")
)]
pub fn flip(size: u32, step: usize, steps: usize) -> RgbImage {
    let mut c = Canvas::new(size, BG_MORE);
    for dx in [0.0_f32, 0.22] {
        c.polygon(
            &[(0.30 + dx, 0.20), (0.30 + dx, 0.68), (0.58 + dx, 0.44)],
            WHITE,
        );
    }
    let mut img = c.finish(size);
    fill_dots(&mut img, step, steps, 0.84, WHITE);
    img
}

/// Row of dots along the bottom: `position + 1` of `count` (capped at 8) lit,
/// bigger than the rest, at height `y` (0.0-1.0 of the key).
fn fill_dots(img: &mut RgbImage, position: usize, count: usize, y: f32, color: Rgb<u8>) {
    let size = img.width();
    let dots = count.clamp(1, 8);
    let filled = ((position + 1) * dots / count.max(1)).clamp(1, dots);
    let spacing = 0.11;
    let start = 0.5 - spacing * (dots as f32 - 1.0) / 2.0;
    let dots_mask = mask(size, |c| {
        for i in 0..dots {
            let r = if i < filled { 0.045 } else { 0.025 };
            c.circle(start + spacing * i as f32, y, r, WHITE);
        }
    });
    blend(img, &dots_mask, color, 1.0);
}

#[expect(
    clippy::many_single_char_names,
    reason = "the usual names in the HSV formula"
)]
fn hue_to_rgb(hue: f32) -> Rgb<u8> {
    // Saturated but soft colors (HSV with s=0.6, v=0.85).
    let (s, v) = (0.6f32, 0.85f32);
    let c = v * s;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match hue as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |f: f32| ((f + m) * 255.0).round() as u8;
    Rgb([to(r), to(g), to(b)])
}

#[cfg(test)]
mod tests;
