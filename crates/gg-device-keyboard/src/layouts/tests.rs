// SPDX-License-Identifier: GPL-3.0-or-later
//! Every board drawing, checked against the keyboard's own key lists.

use super::*;
use crate::proto::{ANALOG_KEYS, KEYS};

/// The LEDs Apex Control lists for this board, "Implementation notes verified on hardware".
fn has_led(code: u8) -> bool {
    matches!(code, 0x04..=0x45 | 0x49..=0x52 | 0xE0..=0xE7 | 0xF0 | 0xFB)
}

/// Keys with a fixed switch that every board has: Esc, the F-row, the six keys above the
/// arrows, the arrows, and the button beside the screen.
fn fixed_keys() -> Vec<u8> {
    let mut keys = vec![0x29];
    keys.extend(0x3A..=0x45);
    keys.extend(0x49..=0x52);
    keys.push(0xFB);
    keys
}

/// Analog keys only some boards have: US `\`, ISO `#` and the key beside left Shift, and
/// the five JIS keys.
const REGIONAL: [u8; 8] = [0x31, 0x32, 0x64, 0x87, 0x88, 0x89, 0x8A, 0x8B];

#[derive(Debug, PartialEq)]
enum Family {
    Ansi,
    Iso,
    Jis,
}

fn family(region: u8) -> Family {
    match region {
        1 => Family::Ansi,
        13 => Family::Jis,
        _ => Family::Iso,
    }
}

fn boards() -> Vec<(&'static str, u8, &'static [Key])> {
    REGIONS
        .iter()
        .map(|&(name, region)| (name, region, for_region(Some(region))))
        .collect()
}

fn codes(board: &[Key]) -> Vec<u8> {
    board.iter().map(|k| k.0).collect()
}

fn label(board: &[Key], code: u8) -> &'static str {
    board
        .iter()
        .find(|k| k.0 == code)
        .unwrap_or_else(|| panic!("no key {code:#04x}"))
        .1
}

fn key(board: &[Key], code: u8) -> Key {
    *board.iter().find(|k| k.0 == code).unwrap()
}

#[test]
fn the_regions_are_the_seven_the_keyboard_reports() {
    let ids: Vec<u8> = REGIONS.iter().map(|r| r.1).collect();
    assert_eq!(ids, vec![1, 3, 4, 6, 10, 13, 20]);
    let mut names: Vec<&str> = REGIONS.iter().map(|r| r.0).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), REGIONS.len(), "names are distinct");
}

#[test]
fn the_region_byte_picks_the_board_and_anything_else_is_us() {
    let us = for_region(Some(1));
    assert!(std::ptr::eq(for_region(None), us), "not read yet: US");
    for unknown in [0, 2, 5, 99, 255] {
        assert!(std::ptr::eq(for_region(Some(unknown)), us), "{unknown}");
    }
    let boards = boards();
    for (i, a) in boards.iter().enumerate() {
        for b in &boards[i + 1..] {
            assert!(!std::ptr::eq(a.2, b.2), "{} and {} share a board", a.0, b.0);
        }
    }
}

#[test]
fn every_board_lights_only_keys_the_frame_carries() {
    for (name, _, board) in boards() {
        for code in codes(board) {
            assert!(
                KEYS.contains(&code) || ANALOG_KEYS.contains(&code),
                "{name}: {code:#04x} is in no key list"
            );
            assert!(
                has_led(code) || ANALOG_KEYS.contains(&code),
                "{name}: {code:#04x} has no LED and no switch on this board"
            );
        }
    }
}

#[test]
fn no_board_draws_a_key_twice() {
    for (name, _, board) in boards() {
        let mut c = codes(board);
        c.sort_unstable();
        c.dedup();
        assert_eq!(c.len(), board.len(), "{name}");
    }
}

#[test]
fn print_screen_scroll_lock_and_pause_are_gone_and_the_screen_button_is_drawn() {
    for (name, _, board) in boards() {
        let c = codes(board);
        for gone in [0x46, 0x47, 0x48] {
            assert!(!c.contains(&gone), "{name}: {gone:#04x}");
        }
        for unlit in [0xF1, 0xF5, 0xF6, 0xF7, 0xFE, 0xFF] {
            assert!(!c.contains(&unlit), "{name}: {unlit:#04x} has no LED");
        }
        let (_, _, x, y, _) = key(board, 0xFB);
        assert_eq!(y, 0.0, "{name}: the button is on the top row");
        assert!(x >= SCREEN.0 + SCREEN.2, "{name}: right of the screen");
    }
}

#[test]
fn no_two_keys_overlap_and_none_covers_the_screen() {
    let overlap = |(ax, ay, aw): (f32, f32, f32), (bx, by, bw): (f32, f32, f32), bh: f32| {
        ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + 1.0
    };
    let (sx, sy, sw, sh) = SCREEN;
    for (name, _, board) in boards() {
        for (i, a) in board.iter().enumerate() {
            assert!(
                !overlap((a.2, a.3, a.4), (sx, sy, sw), sh),
                "{name}: {} covers the screen",
                a.1
            );
            for b in &board[i + 1..] {
                assert!(
                    !overlap((a.2, a.3, a.4), (b.2, b.3, b.4), 1.0),
                    "{name}: {} and {} overlap",
                    a.1,
                    b.1
                );
            }
        }
    }
}

#[test]
fn every_board_is_as_wide_as_the_width() {
    for (name, _, board) in boards() {
        assert!(
            board
                .iter()
                .all(|&(_, _, x, y, w)| x >= 0.0 && y >= 0.0 && w > 0.0 && x + w <= WIDTH),
            "{name}"
        );
        let right = board.iter().map(|k| k.2 + k.4).fold(0.0, f32::max);
        assert_eq!(right, WIDTH, "{name}: the app sizes the drawing by its keys");
    }
}

#[test]
fn every_board_has_its_own_analog_keys_and_every_fixed_one() {
    for (name, region, board) in boards() {
        let c = codes(board);
        let extras: &[u8] = match family(region) {
            Family::Ansi => &[0x31],
            Family::Iso => &[0x32, 0x64],
            Family::Jis => &[0x32, 0x87, 0x88, 0x89, 0x8A, 0x8B],
        };
        for &code in &ANALOG_KEYS {
            let wanted = !REGIONAL.contains(&code) || extras.contains(&code);
            assert_eq!(c.contains(&code), wanted, "{name}: analog {code:#04x}");
        }
        for code in fixed_keys() {
            assert!(c.contains(&code), "{name}: {code:#04x}");
        }
        assert_eq!(
            board.len(),
            fixed_keys().len() + ANALOG_KEYS.len() - REGIONAL.len() + extras.len(),
            "{name}: nothing else is drawn"
        );
    }
}

#[test]
fn the_us_board_is_84_keys_and_the_screen_button() {
    assert_eq!(for_region(Some(1)).len(), 85);
}

#[test]
fn the_rows_of_the_main_block_have_no_holes() {
    // Below the F-row, keys left of the navigation keys tile 15 units, except where the
    // tall ISO/JIS Enter reaches down into the home row: it's drawn on the row above.
    for (name, region, board) in boards() {
        for y in [1.5, 2.5, 3.5, 4.5, 5.5] {
            let mut row: Vec<Key> = board
                .iter()
                .copied()
                .filter(|k| k.3 == y && k.2 < 15.0)
                .collect();
            row.sort_by(|a, b| a.2.total_cmp(&b.2));
            let mut x = 0.0;
            for k in &row {
                assert_eq!(k.2, x, "{name}: a hole before {} on row {y}", k.1);
                x += k.4;
            }
            let end = if y == 3.5 && family(region) != Family::Ansi {
                13.75
            } else {
                15.0
            };
            assert_eq!(x, end, "{name}: row {y} ends at {x}");
        }
    }
}

#[test]
fn iso_and_jis_draw_the_tall_enter_on_the_row_above_the_home_row() {
    for (name, region, board) in boards() {
        let enter = key(board, 0x28);
        if family(region) == Family::Ansi {
            assert_eq!((enter.2, enter.3, enter.4), (12.75, 3.5, 2.25), "{name}");
        } else {
            assert_eq!((enter.2, enter.3, enter.4), (13.5, 2.5, 1.5), "{name}");
            assert_eq!(key(board, 0x32).3, 3.5, "{name}: # sits beside it");
        }
    }
}

#[test]
fn the_labels_follow_the_region() {
    let us = for_region(Some(1));
    assert_eq!((label(us, 0x1C), label(us, 0x1D)), ("Y", "Z"));
    assert_eq!(label(us, 0x31), "\\");
    assert_eq!(label(us, 0xE6), "Alt");

    let uk = for_region(Some(3));
    assert_eq!((label(uk, 0x32), label(uk, 0x64)), ("#", "\\"));
    assert_eq!(label(uk, 0xE6), "AltGr");

    let de = for_region(Some(4));
    assert_eq!((label(de, 0x1C), label(de, 0x1D)), ("Z", "Y"), "QWERTZ");
    assert_eq!(
        [0x2F, 0x33, 0x34, 0x2D, 0x64].map(|c| label(de, c)),
        ["Ü", "Ö", "Ä", "ß", "<"]
    );

    let fr = for_region(Some(6));
    assert_eq!(
        [0x14, 0x1A, 0x04, 0x33, 0x1D, 0x10].map(|c| label(fr, c)),
        ["A", "Z", "Q", "M", "W", ","],
        "AZERTY"
    );

    let nordic = for_region(Some(10));
    assert_eq!(
        [0x2F, 0x33, 0x34].map(|c| label(nordic, c)),
        ["Å", "Ö", "Ä"]
    );

    let tr = for_region(Some(20));
    assert_eq!(
        [0x37, 0x33, 0x2F, 0x30, 0x34, 0x36, 0x0C].map(|c| label(tr, c)),
        ["Ç", "Ş", "Ğ", "Ü", "İ", "Ö", "I"],
        "Turkish Q"
    );

    let jis = for_region(Some(13));
    assert_eq!(
        [0x89, 0x87, 0x8B, 0x8A, 0x88].map(|c| label(jis, c)),
        ["¥", "\\", "無変換", "変換", "かな"]
    );
    assert_eq!(label(jis, 0xE6), "Alt");
}

#[test]
fn the_keys_every_board_shares_sit_where_the_us_board_has_them() {
    // Only the main block's shape changes between regions: the F-row, the screen button,
    // the navigation keys and the arrows stay put.
    let us = for_region(Some(1));
    for (name, _, board) in boards() {
        for code in fixed_keys() {
            let (a, b) = (key(us, code), key(board, code));
            assert_eq!((a.2, a.3, a.4), (b.2, b.3, b.4), "{name}: {code:#04x}");
        }
    }
}

#[test]
fn the_iso_boards_differ_only_in_labels() {
    let shape = |board: &[Key]| -> Vec<(u8, f32, f32, f32)> {
        board.iter().map(|k| (k.0, k.2, k.3, k.4)).collect()
    };
    let uk = shape(for_region(Some(3)));
    for region in [4, 6, 10, 20] {
        assert_eq!(shape(for_region(Some(region))), uk, "region {region}");
    }
}

#[test]
fn every_key_but_space_has_a_label() {
    for (name, _, board) in boards() {
        for k in board {
            assert_eq!(k.1.is_empty(), k.0 == 0x2C, "{name}: {:#04x}", k.0);
        }
    }
}

#[test]
fn the_old_generic_drawing_still_has_87_keys_the_frame_can_light() {
    let mut codes: Vec<u8> = ANSI_TKL.iter().map(|k| k.0).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), 87);
    assert!(
        codes.iter().all(|c| KEYS.contains(c)),
        "every drawn key is in the frame"
    );
    assert!(ANSI_TKL
        .iter()
        .all(|&(_, _, x, _, w)| x >= 0.0 && x + w <= WIDTH));
}
