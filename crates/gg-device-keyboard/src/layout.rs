// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apex Pro TKL Gen 3 as the UI draws it: HID usage code, label, and position and
//! width in key units (a standard key is 1 × 1). [`for_region`] picks the board for the
//! region byte the keyboard reports: US (ANSI), five ISO boards, or Japanese (JIS).
//!
//! The board has no Print Screen, Scroll Lock or Pause: its screen sits there, with a
//! roller and the `0xFB` button beside it. Of those, only the button has an LED, so only
//! it is a key here. The positions follow the US board's usual TKL grid; the screen
//! corner and the ISO/JIS shapes are inferred, not measured on a keyboard.

#[path = "layouts/ansi.rs"]
mod ansi;
#[path = "layouts/iso.rs"]
mod iso;
#[path = "layouts/jis.rs"]
mod jis;

pub use ansi::US;
pub use iso::{DE, FR, NORDIC, TR, UK};
pub use jis::JIS;

/// The board is this many key units wide, on every region.
pub const WIDTH: f32 = 18.25;

/// (HID code, label, x, y, width)
pub type Key = (u8, &'static str, f32, f32, f32);

/// Where the screen sits, (x, y, width, height): above the navigation keys, left of the
/// button that has an LED. It isn't a key.
pub const SCREEN: (f32, f32, f32, f32) = (15.25, 0.0, 1.75, 1.0);

/// The regions the keyboard reports (the `F5` reply), by name, for a setting that
/// overrides it.
pub const REGIONS: [(&str, u8); 7] = [
    ("US", 1),
    ("UK", 3),
    ("German", 4),
    ("French", 6),
    ("Nordic", 10),
    ("Japanese", 13),
    ("Turkish", 20),
];

/// The board for a region byte; US when it hasn't been read or is one we don't know.
pub fn for_region(region: Option<u8>) -> &'static [Key] {
    match region {
        Some(3) => &UK,
        Some(4) => &DE,
        Some(6) => &FR,
        Some(10) => &NORDIC,
        Some(13) => &JIS,
        Some(20) => &TR,
        _ => &US,
    }
}

/// The old generic US TKL drawing, with Print Screen, Scroll Lock and Pause, which this
/// board doesn't have. Nothing draws it any more: only the key-binding messages still take
/// their key names from it (the daemon's and the fake's); once they use [`US`], it goes.
pub const ANSI_TKL: [(u8, &str, f32, f32, f32); 87] = [
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
    (0x46, "PrtSc", 15.25, 0.0, 1.0),
    (0x47, "ScrLk", 16.25, 0.0, 1.0),
    (0x48, "Pause", 17.25, 0.0, 1.0),
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
    (0x31, "\\", 13.5, 2.5, 1.5),
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
    (0x28, "Enter", 12.75, 3.5, 2.25),
    (0xE1, "Shift", 0.0, 4.5, 2.25),
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
    (0xE6, "Alt", 10.0, 5.5, 1.25),
    (0xF0, "Fn", 11.25, 5.5, 1.25),
    (0xE7, "Menu", 12.5, 5.5, 1.25),
    (0xE4, "Ctrl", 13.75, 5.5, 1.25),
    (0x50, "←", 15.25, 5.5, 1.0),
    (0x51, "↓", 16.25, 5.5, 1.0),
    (0x4F, "→", 17.25, 5.5, 1.0),
];

#[cfg(test)]
#[path = "layouts/tests.rs"]
mod tests;
