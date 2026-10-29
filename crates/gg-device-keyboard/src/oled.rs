// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apex Pro TKL Gen 3's 128 × 40 one-bit screen: a bitmap, the keyboard's packing, and
//! text drawn with the 8 × 8 [`crate::font`].

use anyhow::{ensure, Result};

use crate::font;

pub const WIDTH: usize = 128;
pub const HEIGHT: usize = 40;
/// Bytes in a packed frame, and in a row-major picture: one bit per pixel.
pub const PACKED: usize = WIDTH * HEIGHT / 8;
/// Bytes per row in a row-major picture.
pub const ROW_BYTES: usize = WIDTH / 8;

/// A screen's worth of pixels, `true` lit.
#[derive(Clone, PartialEq, Eq)]
pub struct Bitmap {
    px: [bool; WIDTH * HEIGHT],
}

impl Default for Bitmap {
    fn default() -> Self {
        Bitmap {
            px: [false; WIDTH * HEIGHT],
        }
    }
}

impl std::fmt::Debug for Bitmap {
    /// One line per row, `#` lit: readable in a failed test.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for y in 0..HEIGHT {
            let row: String = (0..WIDTH)
                .map(|x| if self.get(x, y) { '#' } else { '.' })
                .collect();
            writeln!(f, "{row}")?;
        }
        Ok(())
    }
}

impl Bitmap {
    pub fn get(&self, x: usize, y: usize) -> bool {
        x < WIDTH && y < HEIGHT && self.px[y * WIDTH + x]
    }

    /// Light or clear one pixel; outside the screen does nothing.
    pub fn set(&mut self, x: usize, y: usize, on: bool) {
        if x < WIDTH && y < HEIGHT {
            self.px[y * WIDTH + x] = on;
        }
    }

    /// A picture given row by row, top first: 16 bytes a row, the leftmost pixel in each
    /// byte's high bit (the layout of a PBM file's raster, or a canvas thresholded to bits).
    pub fn from_rows(rows: &[u8]) -> Result<Self> {
        ensure!(
            rows.len() == PACKED,
            "a {WIDTH}×{HEIGHT} picture is {PACKED} bytes, not {}",
            rows.len()
        );
        let mut b = Bitmap::default();
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let byte = rows[y * ROW_BYTES + x / 8];
                b.set(x, y, byte & (0x80 >> (x % 8)) != 0);
            }
        }
        Ok(b)
    }

    /// The frame the keyboard takes: five pages of eight rows, each page 128 column bytes,
    /// bit 0 the page's top row (SSD1306 page order).
    pub fn packed(&self) -> [u8; PACKED] {
        let mut out = [0u8; PACKED];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                if self.get(x, y) {
                    out[(y / 8) * WIDTH + x] |= 1 << (y % 8);
                }
            }
        }
        out
    }

    /// Draw `text` with its top-left corner at `(x, y)`, each font pixel `scale` × `scale`.
    pub fn draw_text(&mut self, x: usize, y: usize, text: &str, scale: usize) {
        for (i, c) in text.chars().enumerate() {
            let glyph = font::glyph(c);
            let gx = x + i * 8 * scale;
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..8 {
                    if bits & (1 << col) != 0 {
                        for dy in 0..scale {
                            for dx in 0..scale {
                                self.set(gx + col * scale + dx, y + row * scale + dy, true);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The most lines of text the screen holds, and characters per line, at the smallest size.
pub const MAX_LINES: usize = HEIGHT / 8;
pub const MAX_COLUMNS: usize = WIDTH / 8;

/// Lines of text, each at its own scale, stacked and centred both ways. Lines that don't
/// fit are cut at the screen's edge.
pub fn lines(lines: &[(&str, usize)]) -> Bitmap {
    let mut b = Bitmap::default();
    const GAP: usize = 2;
    let height: usize =
        lines.iter().map(|(_, s)| 8 * s).sum::<usize>() + GAP * lines.len().saturating_sub(1);
    let mut y = HEIGHT.saturating_sub(height) / 2;
    for (text, scale) in lines {
        let width = text.chars().count() * 8 * scale;
        b.draw_text(WIDTH.saturating_sub(width) / 2, y, text, *scale);
        y += 8 * scale + GAP;
    }
    b
}

/// The biggest scale, 1 to 3, at which every line fits the screen.
pub fn fit_scale(text_lines: &[&str]) -> usize {
    let widest = text_lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0);
    (1..=3)
        .rev()
        .find(|&s| widest * 8 * s <= WIDTH && text_lines.len() * 8 * s <= HEIGHT)
        .unwrap_or(1)
}

/// Text as the user typed it: one to five lines, as big as they fit.
pub fn text(text: &str) -> Bitmap {
    let text_lines: Vec<&str> = text.lines().take(MAX_LINES).collect();
    let scale = fit_scale(&text_lines);
    let scaled: Vec<(&str, usize)> = text_lines.iter().map(|l| (*l, scale)).collect();
    lines(&scaled)
}

/// A clock: the time as big as it fits, with an optional smaller line under it.
pub fn clock(time: &str, under: Option<&str>) -> Bitmap {
    match under {
        None => lines(&[(time, fit_scale(&[time]))]),
        Some(under) => {
            // Leave room for the line under it: at most two thirds of the height.
            let scale = fit_scale(&[time]).min(2);
            lines(&[(time, scale), (under, 1)])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packing_puts_each_column_of_eight_rows_in_one_byte_top_row_in_bit_0() {
        let mut b = Bitmap::default();
        b.set(0, 0, true); // page 0, column 0, bit 0
        b.set(0, 7, true); // page 0, column 0, bit 7
        b.set(5, 9, true); // page 1, column 5, bit 1
        b.set(127, 39, true); // page 4, column 127, bit 7
        let p = b.packed();
        assert_eq!(p[0], 0x81);
        assert_eq!(p[128 + 5], 0x02);
        assert_eq!(p[4 * 128 + 127], 0x80);
        assert_eq!(p.iter().filter(|&&x| x != 0).count(), 3);
    }

    #[test]
    fn a_row_major_picture_round_trips_through_its_pixels() {
        let mut rows = [0u8; PACKED];
        rows[0] = 0x80; // (0, 0)
        rows[ROW_BYTES * 39 + 15] = 0x01; // (127, 39)
        let b = Bitmap::from_rows(&rows).unwrap();
        assert!(b.get(0, 0) && b.get(127, 39));
        assert!(!b.get(1, 0));
        assert!(Bitmap::from_rows(&rows[..639]).is_err());
    }

    #[test]
    fn drawing_outside_the_screen_is_clipped() {
        let mut b = Bitmap::default();
        b.draw_text(124, 36, "WW", 1);
        b.set(500, 500, true);
        assert!(b.get(124, 36) || b.get(125, 36) || b.get(124, 37));
    }

    #[test]
    fn text_is_as_big_as_it_fits() {
        assert_eq!(fit_scale(&["12:34"]), 3, "5 × 24 px = 120");
        assert_eq!(fit_scale(&["12:34:56"]), 2, "8 × 16 px = 128");
        assert_eq!(fit_scale(&["Hello there!"]), 1, "12 characters");
        assert_eq!(
            fit_scale(&["a", "b", "c"]),
            1,
            "three lines are 24 px at scale 1 only"
        );
        assert_eq!(fit_scale(&["a", "b"]), 2);
    }

    #[test]
    fn a_line_is_centred() {
        let b = text("I");
        // `I` at scale 3: 24 × 24, centred on a 128 × 40 screen.
        let lit: Vec<(usize, usize)> = (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .filter(|&(x, y)| b.get(x, y))
            .collect();
        let (min_x, max_x) = (
            lit.iter().map(|p| p.0).min().unwrap(),
            lit.iter().map(|p| p.0).max().unwrap(),
        );
        let (min_y, max_y) = (
            lit.iter().map(|p| p.1).min().unwrap(),
            lit.iter().map(|p| p.1).max().unwrap(),
        );
        assert!(min_x >= 52 && max_x < 76, "x {min_x}..={max_x}");
        assert!(min_y >= 8 && max_y < 32, "y {min_y}..={max_y}");
    }

    #[test]
    fn more_than_five_lines_keeps_the_first_five() {
        let five = text("1\n2\n3\n4\n5");
        assert_eq!(five, text("1\n2\n3\n4\n5\n6"));
    }

    #[test]
    fn a_clock_with_a_date_keeps_both_on_screen() {
        let b = clock("12:34:56", Some("Thu 29 Oct"));
        let rows_lit: Vec<usize> = (0..HEIGHT)
            .filter(|&y| (0..WIDTH).any(|x| b.get(x, y)))
            .collect();
        assert!(!rows_lit.is_empty());
        assert!(*rows_lit.last().unwrap() < HEIGHT);
        assert!(rows_lit.len() > 16, "two lines drawn");
    }
}
