//! Pixel tests for decorations and glyphs, plus the pre-existing icon tests.
//! Kept in their own file: enough cases here that they would crowd `mod.rs`.

use super::*;

const SIZE: u32 = 72;

/// A non-uniform tile: a solid color would hide a decoration drawn in the
/// wrong place or a frame that leaked into the interior.
fn gradient_tile(size: u32) -> RgbImage {
    RgbImage::from_fn(size, size, |x, y| {
        Rgb([
            (x * 255 / size.max(1)) as u8,
            (y * 255 / size.max(1)) as u8,
            128,
        ])
    })
}

fn same_pixels(a: &RgbImage, b: &RgbImage) -> bool {
    a.dimensions() == b.dimensions() && a.as_raw() == b.as_raw()
}

/// Lays `tiles` out in a 4-column grid on a dark background, for a contact
/// sheet a human can look at.
fn sheet(tiles: &[RgbImage], size: u32, gap: u32) -> RgbImage {
    let cols = 4;
    let rows = u32::try_from(tiles.len()).unwrap().div_ceil(cols);
    let mut canvas = RgbImage::from_pixel(
        cols * (size + gap) + gap,
        rows * (size + gap) + gap,
        Rgb([45, 45, 50]),
    );
    for (i, tile) in tiles.iter().enumerate() {
        let i = u32::try_from(i).unwrap();
        let (row, col) = (i / cols, i % cols);
        imageops::replace(
            &mut canvas,
            tile,
            i64::from(gap + col * (size + gap)),
            i64::from(gap + row * (size + gap)),
        );
    }
    canvas
}

fn diff_positions(a: &RgbImage, b: &RgbImage) -> Vec<(u32, u32)> {
    a.enumerate_pixels()
        .zip(b.pixels())
        .filter(|((.., pa), pb)| *pa != *pb)
        .map(|((x, y, _), _)| (x, y))
        .collect()
}

fn lit_dot_count(img: &RgbImage) -> usize {
    let y0 = img.height() - img.height() / 5;
    img.enumerate_pixels()
        .filter(|(_, y, p)| *y >= y0 && p[0] > 240 && p[1] > 240 && p[2] > 240)
        .count()
}

#[test]
fn every_page_fills_a_dot() {
    for pages in 1..=20 {
        for page in 0..pages {
            let (dots, filled) = page_dots(page, pages);
            assert!(filled < dots, "page {page} of {pages}");
            if pages <= 8 {
                assert_eq!((dots, filled), (pages, page));
            }
        }
        assert_eq!(page_dots(0, pages).1, 0);
        assert_eq!(
            page_dots(pages - 1, pages),
            (pages.min(8), pages.min(8) - 1)
        );
    }
}

#[test]
fn a_thumbnail_is_a_square_of_the_key_size() {
    for (w, h) in [(3000, 2000), (500, 1600), (300, 300), (50, 40)] {
        let cover = DynamicImage::new_rgb8(w, h);
        assert_eq!(thumbnail(&cover, 72).dimensions(), (72, 72), "{w}x{h}");
    }
}

#[test]
fn decorate_with_nothing_set_is_a_no_op() {
    let tile = gradient_tile(SIZE);
    assert!(same_pixels(&decorate(&tile, Decor::default()), &tile));
}

#[test]
fn decorate_current_only_equals_with_highlight() {
    let tile = gradient_tile(SIZE);
    let via_decorate = decorate(
        &tile,
        Decor {
            current: true,
            ..Decor::default()
        },
    );
    assert!(same_pixels(&via_decorate, &with_highlight(&tile)));
}

#[test]
fn badge_only_touches_the_top_left_region() {
    let tile = gradient_tile(SIZE);
    let decorated = decorate(
        &tile,
        Decor {
            badge: Some(Glyph::Note),
            ..Decor::default()
        },
    );
    let diffs = diff_positions(&tile, &decorated);
    assert!(!diffs.is_empty(), "badge drew nothing");
    let limit = f64::from(SIZE) * 0.45;
    for (x, y) in diffs {
        assert!(
            f64::from(x) <= limit && f64::from(y) <= limit,
            "badge pixel outside top-left: ({x}, {y})"
        );
    }
}

#[test]
fn progress_only_touches_the_bottom_band() {
    let tile = gradient_tile(SIZE);
    let decorated = decorate(
        &tile,
        Decor {
            progress: Some(5),
            ..Decor::default()
        },
    );
    let diffs = diff_positions(&tile, &decorated);
    assert!(!diffs.is_empty(), "progress bar drew nothing");
    let floor = f64::from(SIZE) * 0.7;
    for (_, y) in diffs {
        assert!(
            f64::from(y) >= floor,
            "progress pixel above the bottom band: y={y}"
        );
    }
}

#[test]
fn progress_fill_grows_with_step() {
    let tile = gradient_tile(SIZE);
    let bright_pixels = |step: u8| {
        decorate(
            &tile,
            Decor {
                progress: Some(step),
                ..Decor::default()
            },
        )
        .pixels()
        .filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200)
        .count()
    };
    let (none, half, full) = (bright_pixels(0), bright_pixels(5), bright_pixels(10));
    assert!(none < half, "{none} vs {half}");
    assert!(half < full, "{half} vs {full}");
}

#[test]
fn new_dot_only_touches_the_top_right_region_and_is_red() {
    let tile = gradient_tile(SIZE);
    let decorated = decorate(
        &tile,
        Decor {
            new: true,
            ..Decor::default()
        },
    );
    let diffs = diff_positions(&tile, &decorated);
    assert!(!diffs.is_empty(), "new-item dot drew nothing");
    let (low, high) = (f64::from(SIZE) * 0.55, f64::from(SIZE) * 0.45);
    let mut saw_red = false;
    for (x, y) in diffs {
        assert!(
            f64::from(x) >= low && f64::from(y) <= high,
            "dot pixel outside top-right: ({x}, {y})"
        );
        let p = decorated.get_pixel(x, y);
        if p[0] > p[1].saturating_add(40) && p[0] > p[2].saturating_add(40) {
            saw_red = true;
        }
    }
    assert!(saw_red, "no reddish pixel in the new-item dot");
}

#[test]
fn decorations_stay_inside_the_highlight_frame() {
    let tile = gradient_tile(SIZE);
    let decorated = decorate(
        &tile,
        Decor {
            current: true,
            badge: Some(Glyph::Mic),
            progress: Some(6),
            new: true,
        },
    );
    let t = inset(SIZE);
    for y in 0..t {
        for x in 0..SIZE {
            assert_eq!(
                *decorated.get_pixel(x, y),
                HIGHLIGHT,
                "top frame at ({x}, {y})"
            );
            assert_eq!(
                *decorated.get_pixel(x, SIZE - 1 - y),
                HIGHLIGHT,
                "bottom frame at ({x}, {y})"
            );
        }
    }
}

#[test]
fn every_glyph_draws_white_and_differs_from_the_others() {
    let glyphs = [
        Glyph::Note,
        Glyph::Book,
        Glyph::Star,
        Glyph::Waves,
        Glyph::Mic,
        Glyph::Spotify,
    ];
    let tiles: Vec<RgbImage> = glyphs
        .iter()
        .map(|&g| glyph_placeholder(g, Rgb([40, 40, 40]), SIZE))
        .collect();
    for (glyph, tile) in glyphs.iter().zip(&tiles) {
        let has_white = tile
            .pixels()
            .any(|p| p[0] > 240 && p[1] > 240 && p[2] > 240);
        assert!(has_white, "{glyph:?} drew no white pixel");
    }
    for i in 0..tiles.len() {
        for j in (i + 1)..tiles.len() {
            assert_ne!(
                tiles[i].as_raw(),
                tiles[j].as_raw(),
                "{:?} looks the same as {:?}",
                glyphs[i],
                glyphs[j]
            );
        }
    }
}

#[test]
fn shelf_lights_more_dots_for_a_later_position() {
    let tile = gradient_tile(SIZE);
    let counts: Vec<usize> = (0..4)
        .map(|pos| lit_dot_count(&shelf(&tile, pos, 4)))
        .collect();
    assert!(counts.windows(2).all(|w| w[0] < w[1]), "{counts:?}");
}

#[test]
fn flip_lights_more_dots_for_a_later_step() {
    let counts: Vec<usize> = (0..4)
        .map(|step| lit_dot_count(&flip(SIZE, step, 4)))
        .collect();
    assert!(counts.windows(2).all(|w| w[0] < w[1]), "{counts:?}");
}

/// Not a check, a look: with `KIDS_DECK_CONTACT_SHEET=<path>` set, renders
/// every glyph and decoration at 144 px so a human can eyeball them.
#[test]
fn contact_sheet() {
    let Ok(path) = std::env::var("KIDS_DECK_CONTACT_SHEET") else {
        return;
    };
    let size = 144;
    let gap = 12;
    let cover = gradient_tile(size);
    let glyphs = [
        Glyph::Note,
        Glyph::Book,
        Glyph::Star,
        Glyph::Waves,
        Glyph::Mic,
        Glyph::Spotify,
    ];
    let mut tiles: Vec<RgbImage> = glyphs
        .iter()
        .map(|&g| glyph_placeholder(g, name_color(&format!("{g:?}")), size))
        .collect();
    tiles.push(decorate(
        &cover,
        Decor {
            current: true,
            ..Decor::default()
        },
    ));
    tiles.push(decorate(
        &cover,
        Decor {
            badge: Some(Glyph::Mic),
            ..Decor::default()
        },
    ));
    tiles.push(decorate(
        &cover,
        Decor {
            progress: Some(3),
            ..Decor::default()
        },
    ));
    tiles.push(decorate(
        &cover,
        Decor {
            progress: Some(10),
            ..Decor::default()
        },
    ));
    tiles.push(decorate(
        &cover,
        Decor {
            new: true,
            ..Decor::default()
        },
    ));
    tiles.push(decorate(
        &cover,
        Decor {
            current: true,
            badge: Some(Glyph::Spotify),
            progress: Some(7),
            new: true,
        },
    ));
    tiles.push(shelf(&cover, 1, 4));
    tiles.push(flip(size, 2, 4));
    // A shelf's dots must clear a placeholder's glyph: Note's head and
    // Star's lower point are the two shapes that reach closest to the
    // bottom edge.
    tiles.push(shelf(
        &glyph_placeholder(Glyph::Note, name_color("Note"), size),
        0,
        3,
    ));
    tiles.push(shelf(
        &glyph_placeholder(Glyph::Star, name_color("Star"), size),
        1,
        3,
    ));

    sheet(&tiles, size, gap)
        .save(&path)
        .expect("save contact sheet");

    let small = 72;
    let small_tiles = [
        shelf(
            &glyph_placeholder(Glyph::Note, name_color("Note"), small),
            0,
            3,
        ),
        shelf(
            &glyph_placeholder(Glyph::Star, name_color("Star"), small),
            1,
            3,
        ),
    ];
    let small_path = path.replace(".png", "-72.png");
    sheet(&small_tiles, small, gap)
        .save(&small_path)
        .expect("save 72px contact sheet");
}
