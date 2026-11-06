// SPDX-License-Identifier: GPL-3.0-or-later
//! The ISO Apex Pro TKL Gen 3 boards: UK, German, French, Nordic and Turkish. Beside the
//! US board's keys they have `0x64` right of a shorter left Shift and `0x32` (`#` on UK)
//! left of a tall Enter, and no `0x31`. The drawing has no key heights, so the tall Enter
//! is drawn on the row where it starts, above the home row.

use super::Key;

/// The ISO shape, with the UK's labels.
const ISO: [Key; 86] = [
    (0x29, "Esc", 0.0, 0.0, 1.0),
    (0x3A, "F1", 2.0, 0.0, 1.0),
    (0x3B, "F2", 3.0, 0.0, 1.0),
    (0x3C, "F3", 4.0, 0.0, 1.0),
    (0x3D, "F4", 5.0, 0.0, 1.0),
    (0x3E, "F5", 6.5, 0.0, 1.0),
    (0x3F, "F6", 7.5, 0.0, 1.0),
    (0x40, "F7", 8.5, 0.0, 1.0),
    (0x41, "F8", 9.5, 0.0, 1.0),
    (0x42, "F9", 11.0, 0.0, 1.0),
    (0x43, "F10", 12.0, 0.0, 1.0),
    (0x44, "F11", 13.0, 0.0, 1.0),
    (0x45, "F12", 14.0, 0.0, 1.0),
    (0xFB, "Media", 17.25, 0.0, 1.0),
    (0x35, "`", 0.0, 1.5, 1.0),
    (0x1E, "1", 1.0, 1.5, 1.0),
    (0x1F, "2", 2.0, 1.5, 1.0),
    (0x20, "3", 3.0, 1.5, 1.0),
    (0x21, "4", 4.0, 1.5, 1.0),
    (0x22, "5", 5.0, 1.5, 1.0),
    (0x23, "6", 6.0, 1.5, 1.0),
    (0x24, "7", 7.0, 1.5, 1.0),
    (0x25, "8", 8.0, 1.5, 1.0),
    (0x26, "9", 9.0, 1.5, 1.0),
    (0x27, "0", 10.0, 1.5, 1.0),
    (0x2D, "-", 11.0, 1.5, 1.0),
    (0x2E, "=", 12.0, 1.5, 1.0),
    (0x2A, "Backspace", 13.0, 1.5, 2.0),
    (0x49, "Ins", 15.25, 1.5, 1.0),
    (0x4A, "Home", 16.25, 1.5, 1.0),
    (0x4B, "PgUp", 17.25, 1.5, 1.0),
    (0x2B, "Tab", 0.0, 2.5, 1.5),
    (0x14, "Q", 1.5, 2.5, 1.0),
    (0x1A, "W", 2.5, 2.5, 1.0),
    (0x08, "E", 3.5, 2.5, 1.0),
    (0x15, "R", 4.5, 2.5, 1.0),
    (0x17, "T", 5.5, 2.5, 1.0),
    (0x1C, "Y", 6.5, 2.5, 1.0),
    (0x18, "U", 7.5, 2.5, 1.0),
    (0x0C, "I", 8.5, 2.5, 1.0),
    (0x12, "O", 9.5, 2.5, 1.0),
    (0x13, "P", 10.5, 2.5, 1.0),
    (0x2F, "[", 11.5, 2.5, 1.0),
    (0x30, "]", 12.5, 2.5, 1.0),
    (0x28, "Enter", 13.5, 2.5, 1.5),
    (0x4C, "Del", 15.25, 2.5, 1.0),
    (0x4D, "End", 16.25, 2.5, 1.0),
    (0x4E, "PgDn", 17.25, 2.5, 1.0),
    (0x39, "Caps", 0.0, 3.5, 1.75),
    (0x04, "A", 1.75, 3.5, 1.0),
    (0x16, "S", 2.75, 3.5, 1.0),
    (0x07, "D", 3.75, 3.5, 1.0),
    (0x09, "F", 4.75, 3.5, 1.0),
    (0x0A, "G", 5.75, 3.5, 1.0),
    (0x0B, "H", 6.75, 3.5, 1.0),
    (0x0D, "J", 7.75, 3.5, 1.0),
    (0x0E, "K", 8.75, 3.5, 1.0),
    (0x0F, "L", 9.75, 3.5, 1.0),
    (0x33, ";", 10.75, 3.5, 1.0),
    (0x34, "'", 11.75, 3.5, 1.0),
    (0x32, "#", 12.75, 3.5, 1.0),
    (0xE1, "Shift", 0.0, 4.5, 1.25),
    (0x64, "\\", 1.25, 4.5, 1.0),
    (0x1D, "Z", 2.25, 4.5, 1.0),
    (0x1B, "X", 3.25, 4.5, 1.0),
    (0x06, "C", 4.25, 4.5, 1.0),
    (0x19, "V", 5.25, 4.5, 1.0),
    (0x05, "B", 6.25, 4.5, 1.0),
    (0x11, "N", 7.25, 4.5, 1.0),
    (0x10, "M", 8.25, 4.5, 1.0),
    (0x36, ",", 9.25, 4.5, 1.0),
    (0x37, ".", 10.25, 4.5, 1.0),
    (0x38, "/", 11.25, 4.5, 1.0),
    (0xE5, "Shift", 12.25, 4.5, 2.75),
    (0x52, "↑", 16.25, 4.5, 1.0),
    (0xE0, "Ctrl", 0.0, 5.5, 1.25),
    (0xE3, "Win", 1.25, 5.5, 1.25),
    (0xE2, "Alt", 2.5, 5.5, 1.25),
    (0x2C, "", 3.75, 5.5, 6.25),
    (0xE6, "AltGr", 10.0, 5.5, 1.25),
    (0xF0, "Fn", 11.25, 5.5, 1.25),
    (0xE7, "Menu", 12.5, 5.5, 1.25),
    (0xE4, "Ctrl", 13.75, 5.5, 1.25),
    (0x50, "←", 15.25, 5.5, 1.0),
    (0x51, "↓", 16.25, 5.5, 1.0),
    (0x4F, "→", 17.25, 5.5, 1.0),
];

/// `board` with some keys' labels changed. Naming a key the board doesn't have stops the
/// build.
const fn relabel<const N: usize>(board: [Key; N], labels: &[(u8, &'static str)]) -> [Key; N] {
    let mut board = board;
    let mut i = 0;
    while i < labels.len() {
        let (code, label) = labels[i];
        let mut k = 0;
        while k < N && board[k].0 != code {
            k += 1;
        }
        assert!(k < N, "a relabelled key isn't on the board");
        board[k].1 = label;
        i += 1;
    }
    board
}

/// The UK board, region 3.
pub static UK: [Key; 86] = ISO;

/// The German (QWERTZ) board, region 4.
pub static DE: [Key; 86] = relabel(
    ISO,
    &[
        (0x35, "^"),
        (0x2D, "ß"),
        (0x2E, "´"),
        (0x1C, "Z"),
        (0x2F, "Ü"),
        (0x30, "+"),
        (0x33, "Ö"),
        (0x34, "Ä"),
        (0x64, "<"),
        (0x1D, "Y"),
        (0x38, "-"),
    ],
);

/// The French (AZERTY) board, region 6. The number row shows its digits, which French
/// keys carry beside their symbols.
pub static FR: [Key; 86] = relabel(
    ISO,
    &[
        (0x35, "²"),
        (0x2D, ")"),
        (0x2E, "="),
        (0x14, "A"),
        (0x1A, "Z"),
        (0x2F, "^"),
        (0x30, "$"),
        (0x04, "Q"),
        (0x33, "M"),
        (0x34, "ù"),
        (0x32, "*"),
        (0x64, "<"),
        (0x1D, "W"),
        (0x10, ","),
        (0x36, ";"),
        (0x37, ":"),
        (0x38, "!"),
    ],
);

/// The Nordic board, region 10.
pub static NORDIC: [Key; 86] = relabel(
    ISO,
    &[
        (0x35, "§"),
        (0x2D, "+"),
        (0x2E, "´"),
        (0x2F, "Å"),
        (0x30, "¨"),
        (0x33, "Ö"),
        (0x34, "Ä"),
        (0x32, "'"),
        (0x64, "<"),
        (0x38, "-"),
    ],
);

/// The Turkish Q board, region 20.
pub static TR: [Key; 86] = relabel(
    ISO,
    &[
        (0x35, "\""),
        (0x2D, "*"),
        (0x2E, "-"),
        (0x2F, "Ğ"),
        (0x30, "Ü"),
        (0x33, "Ş"),
        (0x34, "İ"),
        (0x32, ","),
        (0x64, "<"),
        (0x36, "Ö"),
        (0x37, "Ç"),
        (0x38, "."),
    ],
);
