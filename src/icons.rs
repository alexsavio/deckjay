//! Draws key images without any font or icon files: simple shapes in
//! 0.0–1.0 coordinates, rendered 4x oversized and scaled down for smooth edges.

use image::imageops::{self, FilterType};
use image::{DynamicImage, Rgb, RgbImage};

const SUPERSAMPLE: u32 = 4;

pub const WHITE: Rgb<u8> = Rgb([255, 255, 255]);
pub const HIGHLIGHT: Rgb<u8> = Rgb([255, 205, 0]);
pub const BG_PLAY: Rgb<u8> = Rgb([22, 150, 70]);
pub const BG_SKIP: Rgb<u8> = Rgb([90, 70, 190]);
pub const BG_VOLUME: Rgb<u8> = Rgb([30, 110, 210]);
pub const BG_MORE: Rgb<u8> = Rgb([235, 120, 20]);
pub const BG_BLANK: Rgb<u8> = Rgb([0, 0, 0]);

pub struct Canvas {
    img: RgbImage,
    /// Oversampled canvas width in pixels; scales 0.0–1.0 coordinates into pixel space.
    scale: f32,
}

impl Canvas {
    pub fn new(size: u32, background: Rgb<u8>) -> Self {
        let big = size * SUPERSAMPLE;
        Canvas {
            img: RgbImage::from_pixel(big, big, background),
            scale: big as f32,
        }
    }

    /// Fills a convex polygon given in 0.0–1.0 coordinates.
    pub fn polygon(&mut self, points: &[(f32, f32)], color: Rgb<u8>) {
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

    pub fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: Rgb<u8>) {
        self.polygon(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)], color);
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Rgb<u8>) {
        let (cx, cy, r) = (cx * self.scale, cy * self.scale, r * self.scale);
        let size = self.img.width() as f32;
        let (x0, x1) = ((cx - r).max(0.0) as u32, (cx + r).min(size) as u32);
        let (y0, y1) = ((cy - r).max(0.0) as u32, (cy + r).min(size) as u32);
        for y in y0..y1 {
            for x in x0..x1 {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                if dx * dx + dy * dy <= r * r {
                    self.img.put_pixel(x, y, color);
                }
            }
        }
    }

    pub fn finish(self, size: u32) -> RgbImage {
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
    let pages = pages.clamp(1, 8);
    let spacing = 0.11;
    let start = 0.5 - spacing * (pages as f32 - 1.0) / 2.0;
    for i in 0..pages {
        let r = if i == page { 0.045 } else { 0.025 };
        c.circle(start + spacing * i as f32, 0.84, r, WHITE);
    }
    c.finish(size)
}

pub fn blank(size: u32) -> RgbImage {
    RgbImage::from_pixel(size, size, BG_BLANK)
}

// ------------------------------------------------------------------ albums

/// Center-cropped square thumbnail of a cover image.
pub fn thumbnail(cover: &DynamicImage, size: u32) -> RgbImage {
    cover
        .resize_to_fill(size, size, FilterType::Lanczos3)
        .to_rgb8()
}

/// Colored tile with a music note, for albums without a cover.
pub fn placeholder(name: &str, size: u32) -> RgbImage {
    let hash = name.bytes().fold(2_166_136_261_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(16_777_619)
    });
    let mut c = Canvas::new(size, hue_to_rgb((hash % 360) as f32));
    c.circle(0.40, 0.66, 0.12, WHITE);
    c.rect(0.47, 0.22, 0.53, 0.66, WHITE);
    c.polygon(
        &[(0.47, 0.22), (0.72, 0.32), (0.72, 0.42), (0.47, 0.32)],
        WHITE,
    );
    c.finish(size)
}

/// Draws the "now playing" frame on an album tile.
pub fn with_highlight(tile: &RgbImage) -> RgbImage {
    let mut img = tile.clone();
    let (w, h) = img.dimensions();
    let t = (w as f32 * 0.08).round().max(2.0) as u32;
    for y in 0..h {
        for x in 0..w {
            if x < t || y < t || x >= w - t || y >= h - t {
                img.put_pixel(x, y, HIGHLIGHT);
            }
        }
    }
    img
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
