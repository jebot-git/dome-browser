/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::RefCell;
use std::collections::HashMap;

use fonts::platform::font::PlatformFont;
use fonts::{BitmapGlyph, FontIdentifier, PlatformFontMethods};
use servo_canvas_traits::canvas::{Path, TextRun};

thread_local! {
    static FONTS: RefCell<HashMap<(FontIdentifier, u32), PlatformFont>> = RefCell::default();
}

/// Use the host strike for the bitmap build. Outline-enabled builds retain
/// their existing glyph renderer. This path is shared by both canvas backends.
pub(crate) fn text_path(runs: &[TextRun]) -> Option<Path> {
    if !fonts::bitmap_only() {
        return None;
    }
    let mut path = Path::new();
    FONTS.with(|cache| {
        let mut cache = cache.borrow_mut();
        for run in runs {
            let key = (run.font.identifier.clone(), run.pt_size.to_bits());
            if !cache.contains_key(&key) {
                let Some(font) = fonts::bitmap_font(&run.font.identifier, run.pt_size) else {
                    continue;
                };
                // Detached documents must not retain an unbounded face cache.
                if cache.len() >= 64 {
                    cache.clear();
                }
                cache.insert(key.clone(), font);
            }
            let font = &cache[&key];
            for glyph in &run.glyphs_and_positions {
                if let Some(bitmap) = font.bitmap_glyph(glyph.id) {
                    append_glyph(
                        &mut path,
                        &bitmap,
                        glyph.point.x as f64,
                        glyph.point.y as f64,
                    );
                }
            }
        }
    });
    Some(path)
}

/// Trace only exposed pixel edges. Internal pixel/run boundaries must not
/// become strokeText lines; counter-oriented contours preserve glyph holes.
fn append_glyph(path: &mut Path, glyph: &BitmapGlyph, x: f64, y: f64) {
    type Vertex = (i32, i32);
    let mut edges: HashMap<Vertex, Vec<Vertex>> = HashMap::new();
    let filled = |x: i32, y: i32| {
        x >= 0
            && y >= 0
            && x < glyph.width as i32
            && y < glyph.height as i32
            && glyph.pixels[y as usize * glyph.width as usize + x as usize] != 0
    };
    for y in 0..glyph.height as i32 {
        for x in 0..glyph.width as i32 {
            if !filled(x, y) {
                continue;
            }
            for (outside, from, to) in [
                ((x, y - 1), (x, y), (x + 1, y)),
                ((x + 1, y), (x + 1, y), (x + 1, y + 1)),
                ((x, y + 1), (x + 1, y + 1), (x, y + 1)),
                ((x - 1, y), (x, y + 1), (x, y)),
            ] {
                if !filled(outside.0, outside.1) {
                    edges.entry(from).or_default().push(to);
                }
            }
        }
    }
    let position = |v: Vertex| {
        (
            x + glyph.left as f64 + v.0 as f64,
            y + glyph.top as f64 + v.1 as f64,
        )
    };
    while let Some(start) = edges.keys().next().copied() {
        path.0.move_to(position(start));
        let mut current = start;
        let mut previous = (start.0 - 1, start.1);
        loop {
            let outgoing = edges.get_mut(&current).expect("balanced bitmap perimeter");
            // At a diagonal pixel contact, turn right to keep the contours separate.
            let incoming = (current.0 - previous.0, current.1 - previous.1);
            let index = outgoing
                .iter()
                .enumerate()
                .min_by_key(|(_, to)| {
                    let direction = (to.0 - current.0, to.1 - current.1);
                    let cross = incoming.0 * direction.1 - incoming.1 * direction.0;
                    let dot = incoming.0 * direction.0 + incoming.1 * direction.1;
                    if cross > 0 {
                        0
                    } else if dot > 0 {
                        1
                    } else if cross < 0 {
                        2
                    } else {
                        3
                    }
                })
                .map(|(index, _)| index)
                .expect("nonempty bitmap perimeter");
            let next = outgoing.swap_remove(index);
            if outgoing.is_empty() {
                edges.remove(&current);
            }
            if next == start {
                path.0.close_path();
                break;
            }
            path.0.line_to(position(next));
            previous = current;
            current = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use kurbo::{PathEl, Shape};

    use super::*;

    fn check(width: u32, height: u32, pixels: &[u8], area: f64, perimeter: f64, contours: usize) {
        let glyph = BitmapGlyph {
            width,
            height,
            left: -2,
            top: -4,
            pixels: pixels.into(),
        };
        let mut path = Path::new();
        append_glyph(&mut path, &glyph, 10., 20.);
        assert_eq!(path.0.area(), area);
        assert!((path.0.perimeter(1e-6) - perimeter).abs() < 1e-6);
        assert_eq!(
            path.0
                .elements()
                .iter()
                .filter(|el| matches!(el, PathEl::ClosePath))
                .count(),
            contours
        );
        if area != 0. {
            let bounds = path.0.bounding_box();
            assert_eq!((bounds.x0, bounds.y0), (8., 16.));
        }
    }

    #[test]
    fn connected_pixels_have_no_internal_stroke_edges() {
        check(2, 3, &[1, 1, 1, 1, 1, 1], 6., 10., 1);
    }

    #[test]
    fn glyph_holes_and_diagonal_contacts_keep_their_contours() {
        check(3, 3, &[1, 1, 1, 1, 0, 1, 1, 1, 1], 8., 16., 2);
        check(2, 2, &[1, 0, 0, 1], 2., 8., 2);
        check(0, 0, &[], 0., 0., 0);
    }
}
