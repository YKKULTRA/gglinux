// SPDX-License-Identifier: GPL-3.0-or-later
//! Full-keyboard bitmaps (SDK `json-handlers-full-keyboard-lighting.md`): an event's frame
//! carries `bitmap`, 132 colours that the SDK lays on "a 22x6 grid that is automatically
//! mapped to the nearest appropriate keys on the user's keyboard. The first color specified
//! corresponds to the top left of the keyboard, the 23rd color corresponds to the left side
//! of the second row, etc. Any parts of the array that do not map to a key on the user's
//! keyboard are ignored."
//!
//! So colour `i` is grid row `i / 22`, column `i % 22`, read like text. The doc doesn't say
//! how wide a cell is. gglinux takes the grid as a full-size keyboard's: six rows (the
//! function row and the five below it) and 22 columns one key wide (15 for the main keys,
//! 3 for the navigation keys, 4 for the keypad), which is why parts of it can fall where a
//! smaller keyboard has no key. Each key of the TKL takes the cell under the middle of the
//! key, on its row; the keypad's four columns land on no key and are ignored. This is
//! gglinux's reading of the doc, not checked against GG.

use std::sync::OnceLock;

use gg_device_keyboard::layout::US;
use serde_json::Value;

use crate::proto::Rgb;

pub const COLUMNS: usize = 22;
pub const ROWS: usize = 6;
/// Colours in a bitmap.
pub const CELLS: usize = COLUMNS * ROWS;

/// The grid cell of every key on the TKL: `(cell, HID code)`, in the layout's order.
pub fn cells() -> &'static [(usize, u8)] {
    static CELLS_OF_KEYS: OnceLock<Vec<(usize, u8)>> = OnceLock::new();
    CELLS_OF_KEYS.get_or_init(|| {
        // The layout's rows, top first: the function row, then the five below it.
        let mut rows: Vec<f32> = US.iter().map(|k| k.3).collect();
        rows.sort_by(f32::total_cmp);
        rows.dedup();
        US.iter()
            .filter_map(|&(code, _, x, y, w)| {
                let row = rows.iter().position(|&r| r == y)?;
                let column = ((x + w / 2.0).floor() as usize).min(COLUMNS - 1);
                (row < ROWS).then_some((row * COLUMNS + column, code))
            })
            .collect()
    })
}

/// The colours of a frame's `bitmap`, a cell each: a cell that isn't `[r, g, b]` (each 0 to
/// 255), or that a short list lacks, is `None` and paints nothing. Colours past the 132nd
/// are ignored. `None` if it isn't a list at all.
pub fn colours(v: &Value) -> Option<Vec<Option<Rgb>>> {
    let list = v.as_array()?;
    Some(
        (0..CELLS)
            .map(|i| {
                let c = list.get(i)?.as_array()?;
                let channel = |n: usize| {
                    c.get(n)?
                        .as_u64()
                        .filter(|&v| v <= 255)
                        .map(|v| v as u8)
                };
                (c.len() == 3).then_some(())?;
                Some(Rgb {
                    r: channel(0)?,
                    g: channel(1)?,
                    b: channel(2)?,
                })
            })
            .collect(),
    )
}

/// `excluded-events`: a list of event names.
pub fn excluded(v: &Value) -> Result<Vec<String>, String> {
    v.as_array()
        .and_then(|l| {
            l.iter()
                .map(|e| e.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| "excluded-events must be a list of event names".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cell(code: u8) -> usize {
        cells()
            .iter()
            .find(|&&(_, c)| c == code)
            .map(|&(cell, _)| cell)
            .unwrap_or_else(|| panic!("{code:#04x} has no cell"))
    }

    #[test]
    fn colour_0_is_the_top_left_key_and_colour_22_starts_the_second_row() {
        assert_eq!(cell(0x29), 0, "Esc");
        assert_eq!(cell(0x35), 22, "` starts the number row");
        assert_eq!(cell(0x2B), 44, "Tab");
        assert_eq!(cell(0x39), 66, "Caps Lock");
        assert_eq!(cell(0xE1), 89, "left Shift, 2.25 keys wide: its middle is in column 1");
        assert_eq!(cell(0xE0), 110, "left Ctrl starts the bottom row");
    }

    #[test]
    fn keys_take_the_column_under_their_middle() {
        // A full-size board's columns: F1 sits over 2, F5 over 3 + a half-key gap.
        assert_eq!(cell(0x3A), 2, "F1");
        assert_eq!(cell(0x3E), 7, "F5, after the gap");
        assert_eq!(cell(0x45), 14, "F12");
        assert_eq!(cell(0x14), 46, "Q: half a key in, its middle on column 2");
        assert_eq!(cell(0x1A), 47, "W");
        assert_eq!(cell(0x04), 68, "A");
        assert_eq!(cell(0x2C), 116, "the space bar's middle, column 6");
        assert_eq!(cell(0x49), 37, "Insert, the first navigation column (15)");
        assert_eq!(cell(0x4F), 127, "right arrow, column 17");
        assert_eq!(cell(0xFB), 17, "the button beside the screen, where Pause would be");
    }

    #[test]
    fn every_key_has_its_own_cell_and_the_keypad_columns_have_none() {
        let mut seen: Vec<usize> = cells().iter().map(|&(c, _)| c).collect();
        assert_eq!(seen.len(), US.len(), "every key of the TKL");
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), US.len(), "no two keys share a cell");
        assert!(seen.iter().all(|&c| c < CELLS && c % COLUMNS < 18));
    }

    #[test]
    fn a_bitmap_is_132_colours_and_what_isnt_a_colour_paints_nothing() {
        let mut list = vec![json!([1, 2, 3]); CELLS + 5];
        list[1] = json!([300, 0, 0]);
        list[2] = json!("red");
        list[3] = json!([1, 2]);
        let c = colours(&json!(list)).unwrap();
        assert_eq!(c.len(), CELLS, "past the 132nd: ignored");
        assert_eq!(c[0], Some(Rgb { r: 1, g: 2, b: 3 }));
        assert_eq!(&c[1..4], &[None, None, None]);
        let short = colours(&json!([[9, 9, 9]])).unwrap();
        assert_eq!((short[0], short[1]), (Some(Rgb { r: 9, g: 9, b: 9 }), None));
        assert_eq!(colours(&json!("nope")), None);
    }

    #[test]
    fn excluded_events_are_a_list_of_names() {
        assert_eq!(
            excluded(&json!(["AMMO", "HEALTH"])),
            Ok(vec!["AMMO".to_string(), "HEALTH".to_string()])
        );
        assert!(excluded(&json!("AMMO")).is_err());
        assert!(excluded(&json!([1])).is_err());
    }
}
