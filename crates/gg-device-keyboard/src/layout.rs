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

#[cfg(test)]
#[path = "layouts/tests.rs"]
mod tests;
