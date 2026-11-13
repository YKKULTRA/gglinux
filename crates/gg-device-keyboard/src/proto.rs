// SPDX-License-Identifier: GPL-3.0-or-later
//! Apex Pro TKL Gen 3 (`1038:1642`) protocol: lighting, the OLED screen and the switches.
//!
//! Lighting is from OpenRGB's `SteelSeriesApexController` (GPL-2.0-or-later), verified on
//! this keyboard 2026-09-27: an init, a direct frame (all red, then only G green) and the
//! onboard hand-back all behaved as sent.
//!
//! The screen and switch commands are from Apex Control's protocol reference
//! (github.com/Rikearon/apex-control, `docs/PROTOCOL.md`, CC BY 4.0, checked on that author's
//! keyboard, firmware 1.19.7) and agree with apex-pro-tkl-gen3-linux (MIT) on the screen. They
//! all write the keyboard's RAM only: a power cycle brings back what its onboard profile holds,
//! so the daemon sends them again whenever the keyboard connects. Nothing here writes flash.

use anyhow::{ensure, Result};

pub const OP_INIT: u8 = 0x4B;
pub const OP_DIRECT: u8 = 0x40;
pub const OP_ONBOARD: u8 = 0x41;
pub const OP_SET_BRIGHTNESS: u8 = 0x23;
pub const OP_GET_BRIGHTNESS: u8 = 0xA3;
pub const OP_REGION: u8 = 0xF5;
pub const OP_FIRMWARE: u8 = 0x90;

/// The screen's command; the next byte says what to do.
pub const OP_OLED: u8 = 0x1F;
/// A frame for the screen, held until the next one or a power cycle.
pub const OLED_LIVE: u8 = 0x81;
/// Give the screen back to the keyboard's own menus.
pub const OLED_RELEASE: u8 = 0x82;

/// The switches' command; the next byte says which setting.
pub const OP_SWITCHES: u8 = 0x38;
/// Actuation thresholds, one `(code, h, l)` block per analog key.
pub const SW_THRESHOLDS: u8 = 0x61;
/// Release mode per key, one of the `RELEASE_*` values.
pub const SW_RELEASE_MODE: u8 = 0x62;
pub const RELEASE_NORMAL: u8 = 0x00;
pub const RELEASE_RAPID_TRIGGER: u8 = 0x02;
/// Protection Mode, and Protection Mode with Rapid Trigger: what GG 123 sends for its shield
/// switch (our capture of 2026-11-13: key A `03`, with Rapid Trigger `04`, back to `02`/`00`).
pub const RELEASE_PROTECTION: u8 = 0x03;
pub const RELEASE_RAPID_TRIGGER_PROTECTION: u8 = 0x04;
/// Rapid Trigger's re-trigger distance per key, in tenths of a millimetre.
pub const SW_RT_SENSITIVITY: u8 = 0x65;
/// Rapid Tap on or off.
pub const SW_RAPID_TAP_ENABLE: u8 = 0x66;
/// Rapid Tap's key pairs.
pub const SW_RAPID_TAP_PAIRS: u8 = 0x67;

/// Every plain opcode gglinux may send. The firmware-update and flash commands (`0x02`
/// erase, `0x03` write, `0x04`) share this interface and can brick the keyboard; an
/// allowlist makes them — and anything else unvetted, typos included — impossible to frame.
/// Only [`crate::allow`] lets narrow shapes of `0x02` and `0x03` out (the five profile
/// slots), and only while the daemon has the onboard family on.
pub const ALLOWED: [u8; 7] = [
    OP_INIT,
    OP_DIRECT,
    OP_ONBOARD,
    OP_SET_BRIGHTNESS,
    OP_GET_BRIGHTNESS,
    OP_REGION,
    OP_FIRMWARE,
];

/// Opcodes whose next byte is a sub-command, and the sub-commands allowed. Left out on
/// purpose: `0x38 0x83`, which stores a screen image in flash.
pub const ALLOWED_SUB: [(u8, &[u8]); 2] = [
    (OP_OLED, &[OLED_LIVE, OLED_RELEASE]),
    (
        OP_SWITCHES,
        &[
            SW_THRESHOLDS,
            SW_RELEASE_MODE,
            SW_RT_SENSITIVITY,
            SW_RAPID_TAP_ENABLE,
            SW_RAPID_TAP_PAIRS,
        ],
    ),
];

/// Command reports: report id 0, opcode, arguments, zero padding.
pub const COMMAND_REPORT: usize = 65;
/// A direct lighting frame is one 643-byte feature report.
pub const DIRECT_REPORT: usize = 643;
/// The control interface's feature report: report id 0 and the 644 bytes its descriptor
/// declares. Screen frames and switch tables use it.
pub const FEATURE_REPORT: usize = 645;
/// An input report, the keyboard's answer to a query: 64 bytes, no report id.
pub const INPUT_REPORT: usize = 64;

/// HID usage code of every LED, in the order a direct frame lists them.
pub const KEYS: [u8; 112] = [
    0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13,
    0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23,
    0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x32, 0x33, 0x34,
    0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44,
    0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0x64, 0xE0,
    0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xF0, 0x31, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x53, 0x54,
    0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F, 0x60, 0x61, 0x62, 0x63, 0xFB,
];

/// The 68 keys with adjustable (OmniPoint) switches, by HID code: letters, digits, Enter,
/// Backspace, Tab, Space, the punctuation keys, Caps Lock, the ISO and JIS extras, the
/// modifiers and the SteelSeries key. Escape, the F-row, the navigation cluster and the
/// arrows have fixed switches. Every switch table lists all 68, in this order.
pub const ANALOG_KEYS: [u8; 68] = [
    0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13,
    0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23,
    0x24, 0x25, 0x26, 0x27, 0x28, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x31, 0x32, 0x33, 0x34,
    0x35, 0x36, 0x37, 0x38, 0x39, 0x64, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0xE0, 0xE1, 0xE2, 0xE3, 0xE4,
    0xE5, 0xE6, 0xE7, 0xF0,
];

/// Actuation points, in tenths of a millimetre: 0.1 to 4.0 mm.
pub const ACTUATION: std::ops::RangeInclusive<u8> = 1..=40;
/// Rapid Trigger's re-trigger distance, in tenths of a millimetre: 0.1 to 2.0 mm.
pub const RT_SENSITIVITY: std::ops::RangeInclusive<u8> = 1..=20;
/// The most Rapid Tap pairs the keyboard takes.
pub const MAX_TAP_PAIRS: usize = 10;

/// The raw Hall-sensor thresholds `(release l, actuate h)` for each actuation point,
/// 0.1 mm first. From the vendor's device description, as Apex Control publishes it; the
/// keyboard accepts and stores them, but the travel at each step hasn't been measured.
#[rustfmt::skip]
const THRESHOLDS: [(u8, u8); 40] = [
    (4, 4), (3, 4), (4, 5), (5, 7), (7, 8), (8, 10), (10, 11), (11, 13), (13, 15), (15, 17),
    (17, 19), (19, 22), (22, 24), (24, 27), (27, 30), (30, 34), (34, 37), (37, 40), (40, 44), (44, 49),
    (49, 54), (54, 59), (59, 65), (65, 72), (72, 79), (79, 86), (86, 94), (94, 103), (103, 113), (113, 124),
    (124, 136), (136, 148), (148, 164), (164, 180), (180, 189), (189, 199), (199, 209), (209, 219), (211, 219), (213, 219),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// How a Rapid Tap pair settles both keys held at once. The values are the firmware's; the
/// names follow what each is documented to do, which hasn't been checked on hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TapMode {
    /// The key pressed last counts.
    LastWins = 0,
    /// Neither counts while both are held.
    Neither = 1,
    /// The pair's first key always wins.
    FirstWins = 2,
    /// The pair's second key always wins.
    SecondWins = 3,
}

/// Two opposing keys for Rapid Tap (SOCD), such as A and D.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapPair {
    pub a: u8,
    pub b: u8,
    pub mode: TapMode,
    /// Send both keys rather than holding the losing one back.
    pub report_both: bool,
}

pub fn check(op: u8) -> Result<()> {
    ensure!(
        ALLOWED.contains(&op),
        "opcode {op:#04x} is not on the keyboard's allowlist"
    );
    Ok(())
}

/// Check a whole report before it goes out: report id 0, then an allowed opcode, and for
/// the screen and switch commands an allowed sub-command too. This is
/// [`crate::allow::check`] with every gated family off.
pub fn check_report(report: &[u8]) -> Result<()> {
    crate::allow::check(report, crate::allow::Families::default())
}

/// A command report for an allowed opcode.
pub fn command(op: u8, args: &[u8]) -> Result<Vec<u8>> {
    check(op)?;
    ensure!(
        args.len() <= COMMAND_REPORT - 2,
        "command arguments too long"
    );
    let mut report = vec![0u8; COMMAND_REPORT];
    report[1] = op;
    report[2..2 + args.len()].copy_from_slice(args);
    Ok(report)
}

/// A direct lighting frame: a colour for every LED, looked up by HID code.
pub fn direct_frame(color_of: impl Fn(u8) -> Rgb) -> Vec<u8> {
    let mut report = vec![0u8; DIRECT_REPORT];
    report[1] = OP_DIRECT;
    report[2] = KEYS.len() as u8;
    for (i, &code) in KEYS.iter().enumerate() {
        let c = color_of(code);
        report[3 + i * 4..7 + i * 4].copy_from_slice(&[code, c.r, c.g, c.b]);
    }
    report
}

/// The region byte from a `0xF5` reply (`f5 00 <region>`); 1 is US ANSI.
pub fn decode_region(reply: &[u8]) -> Option<u8> {
    match reply {
        [OP_REGION, 0x00, region, ..] => Some(*region),
        _ => None,
    }
}

/// A screen frame: 640 bytes, column-packed as [`crate::oled::Bitmap::packed`] makes them.
pub fn oled_frame(packed: &[u8; crate::oled::PACKED]) -> Vec<u8> {
    let mut report = vec![0u8; FEATURE_REPORT];
    report[1] = OP_OLED;
    report[2] = OLED_LIVE;
    report[3..3 + packed.len()].copy_from_slice(packed);
    report
}

/// Hand the screen back to the keyboard's own menus.
pub fn oled_release() -> Vec<u8> {
    let mut report = vec![0u8; COMMAND_REPORT];
    report[1] = OP_OLED;
    report[2] = OLED_RELEASE;
    report
}

/// The `(h, l)` threshold pair for an actuation point in tenths of a millimetre, clamped
/// to 0.1–4.0 mm. `h` actuates, `l` releases.
pub fn thresholds(actuation: u8) -> (u8, u8) {
    let i = actuation.clamp(*ACTUATION.start(), *ACTUATION.end()) - 1;
    let (l, h) = THRESHOLDS[usize::from(i)];
    (h, l)
}

/// The header every per-key switch table starts with.
fn switch_table(sub: u8) -> Vec<u8> {
    let mut report = vec![0u8; FEATURE_REPORT];
    report[1] = OP_SWITCHES;
    report[2] = sub;
    report[3] = ANALOG_KEYS.len() as u8;
    report
}

/// `(h, l)` GG 123 leaves in the actuation table for an analog key the board doesn't
/// have, whatever the setting: our capture apex-10 (19:20:44 and 19:21:09), where the
/// seven keys a US board lacks ([`US_ABSENT`]) stay at `2f 2a` at 1.5 mm and at 1.8 mm.
pub const ABSENT_POINT: (u8, u8) = (0x2F, 0x2A);
/// The release mode GG 123 leaves for such a key: `00`, with Rapid Trigger on for every
/// other key (apex-10, 19:20:52).
pub const ABSENT_RELEASE: u8 = RELEASE_NORMAL;
/// The analog keys a US board doesn't have: the ISO keys `32` and `64` and the JIS keys
/// `87`–`8B` (apex-10).
pub const US_ABSENT: [u8; 7] = [0x32, 0x64, 0x87, 0x88, 0x89, 0x8A, 0x8B];

/// The actuation table: every analog key's actuation point, in tenths of a millimetre.
/// For a board with every analog key; see [`actuation_table_for`].
pub fn actuation_table(actuation_of: impl Fn(u8) -> u8) -> Vec<u8> {
    actuation_table_for(|_| true, actuation_of)
}

/// The actuation table on a board with the keys `has_key` names: their actuation points,
/// and GG's [`ABSENT_POINT`] for the analog keys it doesn't have.
pub fn actuation_table_for(
    has_key: impl Fn(u8) -> bool,
    actuation_of: impl Fn(u8) -> u8,
) -> Vec<u8> {
    let mut report = switch_table(SW_THRESHOLDS);
    report[4] = 0; // the primary actuation point
    for (i, &code) in ANALOG_KEYS.iter().enumerate() {
        let (h, l) = if has_key(code) {
            thresholds(actuation_of(code))
        } else {
            ABSENT_POINT
        };
        report[5 + i * 3..8 + i * 3].copy_from_slice(&[code, h, l]);
    }
    report
}

/// The release-mode table: which analog keys have Rapid Trigger, Protection Mode, or both.
/// For a board with every analog key; see [`release_mode_table_for`].
pub fn release_mode_table(
    rapid_trigger: impl Fn(u8) -> bool,
    protection: impl Fn(u8) -> bool,
) -> Vec<u8> {
    release_mode_table_for(|_| true, rapid_trigger, protection)
}

/// The release-mode table on a board with the keys `has_key` names: their modes, and GG's
/// [`ABSENT_RELEASE`] for the analog keys it doesn't have.
pub fn release_mode_table_for(
    has_key: impl Fn(u8) -> bool,
    rapid_trigger: impl Fn(u8) -> bool,
    protection: impl Fn(u8) -> bool,
) -> Vec<u8> {
    let mut report = switch_table(SW_RELEASE_MODE);
    for (i, &code) in ANALOG_KEYS.iter().enumerate() {
        let mode = match (has_key(code), rapid_trigger(code), protection(code)) {
            (false, ..) => ABSENT_RELEASE,
            (true, false, false) => RELEASE_NORMAL,
            (true, true, false) => RELEASE_RAPID_TRIGGER,
            (true, false, true) => RELEASE_PROTECTION,
            (true, true, true) => RELEASE_RAPID_TRIGGER_PROTECTION,
        };
        report[4 + i * 2..6 + i * 2].copy_from_slice(&[code, mode]);
    }
    report
}

/// The Rapid Trigger distance table, in tenths of a millimetre, clamped to 0.1–2.0 mm.
pub fn sensitivity_table(sensitivity_of: impl Fn(u8) -> u8) -> Vec<u8> {
    let mut report = switch_table(SW_RT_SENSITIVITY);
    for (i, &code) in ANALOG_KEYS.iter().enumerate() {
        let s = sensitivity_of(code).clamp(*RT_SENSITIVITY.start(), *RT_SENSITIVITY.end());
        report[4 + i * 2..6 + i * 2].copy_from_slice(&[code, s]);
    }
    report
}

/// Rapid Tap on or off.
pub fn rapid_tap_enable(enabled: bool) -> Vec<u8> {
    let mut report = vec![0u8; COMMAND_REPORT];
    report[1] = OP_SWITCHES;
    report[2] = SW_RAPID_TAP_ENABLE;
    report[3] = u8::from(enabled);
    report
}

/// Rapid Tap's pairs: at most ten, of two different analog keys, each key in one pair.
pub fn rapid_tap_pairs(pairs: &[TapPair]) -> Result<Vec<u8>> {
    ensure!(
        pairs.len() <= MAX_TAP_PAIRS,
        "at most {MAX_TAP_PAIRS} Rapid Tap pairs"
    );
    let mut seen = Vec::new();
    for p in pairs {
        ensure!(p.a != p.b, "a Rapid Tap pair needs two different keys");
        for code in [p.a, p.b] {
            ensure!(
                ANALOG_KEYS.contains(&code),
                "key {code:#04x} has no adjustable switch, so it can't be in a Rapid Tap pair"
            );
            ensure!(
                !seen.contains(&code),
                "key {code:#04x} is in two Rapid Tap pairs"
            );
            seen.push(code);
        }
    }
    let mut report = vec![0u8; COMMAND_REPORT];
    report[1] = OP_SWITCHES;
    report[2] = SW_RAPID_TAP_PAIRS;
    report[3] = pairs.len() as u8;
    for (i, p) in pairs.iter().enumerate() {
        let flags = p.mode as u8 | if p.report_both { 0x80 } else { 0 };
        report[4 + i * 6..10 + i * 6].copy_from_slice(&[i as u8, p.a, p.b, flags, 0, 0]);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes written as hex pairs with spaces, as the capture decoder prints them.
    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    /// GG 123 setting every key to 1.5 mm on the owner's US board (apex-10, 19:20:44):
    /// the report after its id, up to the end of the 68 blocks; zeros after.
    const GG_ALL_1_5_MM: &str = "38 61 44 00 \
        04 1e 1b 05 1e 1b 06 1e 1b 07 1e 1b 08 1e 1b 09 1e 1b 0a 1e 1b 0b 1e 1b 0c 1e 1b 0d 1e 1b \
        0e 1e 1b 0f 1e 1b 10 1e 1b 11 1e 1b 12 1e 1b 13 1e 1b 14 1e 1b 15 1e 1b 16 1e 1b 17 1e 1b \
        18 1e 1b 19 1e 1b 1a 1e 1b 1b 1e 1b 1c 1e 1b 1d 1e 1b 1e 1e 1b 1f 1e 1b 20 1e 1b 21 1e 1b \
        22 1e 1b 23 1e 1b 24 1e 1b 25 1e 1b 26 1e 1b 27 1e 1b 28 1e 1b 2a 1e 1b 2b 1e 1b 2c 1e 1b \
        2d 1e 1b 2e 1e 1b 2f 1e 1b 30 1e 1b 31 1e 1b 32 2f 2a 33 1e 1b 34 1e 1b 35 1e 1b 36 1e 1b \
        37 1e 1b 38 1e 1b 39 1e 1b 64 2f 2a 87 2f 2a 88 2f 2a 89 2f 2a 8a 2f 2a 8b 2f 2a e0 1e 1b \
        e1 1e 1b e2 1e 1b e3 1e 1b e4 1e 1b e5 1e 1b e6 1e 1b e7 1e 1b f0 1e 1b";

    /// GG 123 turning Rapid Trigger on for every key on the same board (apex-10, 19:20:52).
    const GG_ALL_RAPID_TRIGGER: &str = "38 62 44 \
        04 02 05 02 06 02 07 02 08 02 09 02 0a 02 0b 02 0c 02 0d 02 0e 02 0f 02 10 02 11 02 12 02 \
        13 02 14 02 15 02 16 02 17 02 18 02 19 02 1a 02 1b 02 1c 02 1d 02 1e 02 1f 02 20 02 21 02 \
        22 02 23 02 24 02 25 02 26 02 27 02 28 02 2a 02 2b 02 2c 02 2d 02 2e 02 2f 02 30 02 31 02 \
        32 00 33 02 34 02 35 02 36 02 37 02 38 02 39 02 64 00 87 00 88 00 89 00 8a 00 8b 00 e0 02 \
        e1 02 e2 02 e3 02 e4 02 e5 02 e6 02 e7 02 f0 02";

    fn us_board(code: u8) -> bool {
        !US_ABSENT.contains(&code)
    }

    #[test]
    fn on_a_us_board_the_actuation_table_is_ggs_byte_for_byte() {
        let gg = hex(GG_ALL_1_5_MM);
        assert_eq!(gg.len(), 4 + 68 * 3);
        // Whatever the settings say for the seven keys the board doesn't have.
        let ours = actuation_table_for(us_board, |code| if us_board(code) { 15 } else { 4 });
        assert_eq!(ours[1..1 + gg.len()], gg[..]);
        assert!(ours[1 + gg.len()..].iter().all(|&b| b == 0), "padding");
        let blocks: Vec<&[u8]> = ours[5..5 + 68 * 3].chunks(3).collect();
        assert_eq!(blocks.iter().filter(|b| b[1..] == [0x1e, 0x1b]).count(), 61);
        assert_eq!(blocks.iter().filter(|b| b[1..] == [0x2f, 0x2a]).count(), 7);
        assert!(check_report(&ours).is_ok());
        // 1.8 mm (19:21:09): the 61 keys move, the seven stay.
        let at_1_8 = actuation_table_for(us_board, |_| 18);
        for (block, &code) in at_1_8[5..5 + 68 * 3].chunks(3).zip(&ANALOG_KEYS) {
            let want = if us_board(code) {
                [0x28, 0x25]
            } else {
                [0x2f, 0x2a]
            };
            assert_eq!(block, [code, want[0], want[1]], "{code:#04x}");
        }
        // A board with every key: the old table.
        assert_eq!(
            actuation_table_for(|_| true, |_| 15),
            actuation_table(|_| 15)
        );
    }

    #[test]
    fn on_a_us_board_the_rapid_trigger_table_is_ggs_byte_for_byte() {
        let gg = hex(GG_ALL_RAPID_TRIGGER);
        assert_eq!(gg.len(), 3 + 68 * 2);
        // Protection asked for too: the seven keys still get mode 00.
        let ours = release_mode_table_for(us_board, |_| true, |code| !us_board(code));
        assert_eq!(ours[1..1 + gg.len()], gg[..]);
        assert!(ours[1 + gg.len()..].iter().all(|&b| b == 0), "padding");
        let modes: Vec<u8> = ours[4..4 + 68 * 2].chunks(2).map(|p| p[1]).collect();
        assert_eq!(modes.iter().filter(|&&m| m == 0x02).count(), 61);
        assert_eq!(modes.iter().filter(|&&m| m == 0x00).count(), 7);
        assert!(check_report(&ours).is_ok());
        // Rapid Trigger off for all (19:21:00): every key 00.
        let off = release_mode_table_for(us_board, |_| false, |_| false);
        assert!(off[4..4 + 68 * 2].chunks(2).all(|p| p[1] == 0));
        assert_eq!(
            release_mode_table_for(|_| true, |_| true, |_| false),
            release_mode_table(|_| true, |_| false)
        );
    }

    #[test]
    fn only_the_seven_vetted_opcodes_pass() {
        let allowed: Vec<u8> = (0..=255u8).filter(|&op| check(op).is_ok()).collect();
        assert_eq!(allowed, [0x23, 0x40, 0x41, 0x4B, 0x90, 0xA3, 0xF5]);
        for fizz in [0x02, 0x03, 0x04] {
            assert!(
                command(fizz, &[]).is_err(),
                "firmware opcode {fizz:#04x} must never be framed"
            );
        }
    }

    #[test]
    fn only_the_vetted_screen_and_switch_sub_commands_pass() {
        let passes = |op: u8| -> Vec<u8> {
            (0..=255u8)
                .filter(|&sub| check_report(&[0, op, sub, 0, 0]).is_ok())
                .collect()
        };
        assert_eq!(passes(OP_OLED), [0x81, 0x82]);
        assert_eq!(passes(OP_SWITCHES), [0x61, 0x62, 0x65, 0x66, 0x67]);
        assert!(
            check_report(&[0, OP_SWITCHES, 0x83, 0]).is_err(),
            "0x38 0x83 stores a screen image in flash"
        );
        for flash in [0x02, 0x03, 0x04] {
            for sub in 0..=255u8 {
                assert!(
                    check_report(&[0, flash, sub, 0]).is_err(),
                    "{flash:#04x} {sub:#04x} writes flash"
                );
            }
        }
    }

    #[test]
    fn a_report_must_start_with_report_id_zero() {
        assert!(check_report(&[0x40, 0x40]).is_err());
        assert!(check_report(&[0]).is_err());
        assert!(check_report(&command(OP_INIT, &[]).unwrap()).is_ok());
    }

    #[test]
    fn only_the_primary_actuation_layer_may_be_written() {
        let mut table = actuation_table(|_| 15);
        assert!(check_report(&table).is_ok());
        table[4] = 2; // the second (dual) actuation point
        assert!(check_report(&table).is_err());
    }

    #[test]
    fn commands_are_65_byte_reports_with_report_id_zero() {
        let init = command(OP_INIT, &[]).unwrap();
        assert_eq!(init.len(), 65);
        assert_eq!(&init[..3], &[0x00, 0x4B, 0x00]);
        assert!(
            command(OP_REGION, &[0; 64]).is_err(),
            "arguments longer than the report"
        );
    }

    #[test]
    fn a_direct_frame_lists_every_led() {
        let frame = direct_frame(|code| {
            if code == 0x0A {
                Rgb { r: 0, g: 255, b: 0 }
            } else {
                Rgb::default()
            }
        });
        assert_eq!(frame.len(), 643);
        assert_eq!(&frame[..3], &[0x00, 0x40, 112]);
        let g = KEYS.iter().position(|&c| c == 0x0A).unwrap();
        assert_eq!(
            &frame[3 + g * 4..7 + g * 4],
            &[0x0A, 0, 255, 0],
            "G green, as seen on the keyboard"
        );
        assert_eq!(&frame[3..7], &[0x04, 0, 0, 0], "A dark");
        assert!(frame[3 + 112 * 4..].iter().all(|&b| b == 0), "padding");
    }

    #[test]
    fn region_decodes_from_the_reply() {
        assert_eq!(
            decode_region(&[0xF5, 0x00, 0x01, 0x00]),
            Some(1),
            "captured on this unit: US"
        );
        assert_eq!(decode_region(&[0xA3, 0x00, 0x01]), None);
    }

    #[test]
    fn the_analog_keys_are_the_68_documented_ones() {
        // `4–40, 42–57, 100, 135–139, 224–231, 240` in the protocol reference.
        let documented: Vec<u8> = (4..=40)
            .chain(42..=57)
            .chain([100])
            .chain(135..=139)
            .chain(224..=231)
            .chain([240])
            .collect();
        assert_eq!(ANALOG_KEYS.to_vec(), documented);
        assert!(!ANALOG_KEYS.contains(&0x29), "Escape has a fixed switch");
    }

    #[test]
    fn a_screen_frame_is_one_full_feature_report() {
        let mut packed = [0u8; crate::oled::PACKED];
        packed[0] = 0x01;
        packed[639] = 0x80;
        let frame = oled_frame(&packed);
        assert_eq!(frame.len(), 645, "report id + the descriptor's 644 bytes");
        assert_eq!(&frame[..4], &[0x00, 0x1F, 0x81, 0x01]);
        assert_eq!(frame[642], 0x80, "the last pixel byte");
        assert_eq!(&frame[643..], &[0, 0], "padding");
        assert!(check_report(&frame).is_ok());
        let release = oled_release();
        assert_eq!(release.len(), 65);
        assert_eq!(&release[..4], &[0x00, 0x1F, 0x82, 0x00]);
        assert!(check_report(&release).is_ok());
    }

    #[test]
    fn actuation_points_map_to_the_published_thresholds_h_first() {
        assert_eq!(thresholds(1), (4, 4), "0.1 mm");
        assert_eq!(thresholds(15), (30, 27), "1.5 mm: l 27, h 30");
        assert_eq!(thresholds(40), (219, 213), "4.0 mm");
        assert_eq!(thresholds(0), thresholds(1), "clamped up");
        assert_eq!(thresholds(99), thresholds(40), "clamped down");
        for level in ACTUATION {
            let (h, l) = thresholds(level);
            assert!(l <= h, "{level}: release must not sit below actuation");
        }
    }

    #[test]
    fn the_actuation_table_lists_every_analog_key_with_its_own_point() {
        let table = actuation_table(|code| if code == 0x1A { 4 } else { 20 });
        assert_eq!(table.len(), 645);
        assert_eq!(&table[..5], &[0x00, 0x38, 0x61, 68, 0x00]);
        let w = ANALOG_KEYS.iter().position(|&c| c == 0x1A).unwrap();
        assert_eq!(&table[5 + w * 3..8 + w * 3], &[0x1A, 7, 5], "W at 0.4 mm");
        assert_eq!(&table[5..8], &[0x04, 49, 44], "A at 2.0 mm");
        assert!(table[5 + 68 * 3..].iter().all(|&b| b == 0), "padding");
        assert!(check_report(&table).is_ok());
    }

    /// The four tables GG 123 sent as key A's switches were clicked (apex-3 capture):
    /// `38 62 44 04 02 05 00 …`, `… 04 04 …`, `… 04 03 …`, `… 04 00 …`.
    #[test]
    fn protection_mode_is_release_mode_3_and_4_with_rapid_trigger() {
        let a = |code| code == 0x04;
        let none = |_| false;
        for (rapid, protection, mode) in [
            (true, false, 2),
            (true, true, 4),
            (false, true, 3),
            (false, false, 0),
        ] {
            let table = release_mode_table(|c| rapid && a(c), |c| protection && a(c));
            assert_eq!(&table[1..8], &[0x38, 0x62, 0x44, 0x04, mode, 0x05, 0x00]);
        }
        // Every key protected: `38 62 44 04 03 05 03 06 03 …` (apex-2 capture).
        let all = release_mode_table(none, |_| true);
        assert!(all[4..4 + 68 * 2].chunks(2).all(|p| p[1] == 3));
    }

    #[test]
    fn rapid_trigger_tables_mark_mode_2_and_clamp_the_distance() {
        let modes = release_mode_table(|code| code == 0x04 || code == 0x07, |_| false);
        assert_eq!(&modes[..4], &[0x00, 0x38, 0x62, 68]);
        assert_eq!(
            &modes[4..10],
            &[0x04, 2, 0x05, 0, 0x06, 0],
            "A on, B and C off"
        );
        let d = ANALOG_KEYS.iter().position(|&c| c == 0x07).unwrap();
        assert_eq!(&modes[4 + d * 2..6 + d * 2], &[0x07, 2], "D on");
        assert!(check_report(&modes).is_ok());

        let sens = sensitivity_table(|code| if code == 0x04 { 0 } else { 50 });
        assert_eq!(&sens[..4], &[0x00, 0x38, 0x65, 68]);
        assert_eq!(&sens[4..8], &[0x04, 1, 0x05, 20], "clamped to 0.1–2.0 mm");
        assert!(check_report(&sens).is_ok());
    }

    #[test]
    fn rapid_tap_pairs_are_framed_with_index_mode_and_flag() {
        let ad = TapPair {
            a: 0x04,
            b: 0x07,
            mode: TapMode::LastWins,
            report_both: false,
        };
        let ws = TapPair {
            a: 0x1A,
            b: 0x16,
            mode: TapMode::SecondWins,
            report_both: true,
        };
        let report = rapid_tap_pairs(&[ad, ws]).unwrap();
        assert_eq!(report.len(), 65);
        assert_eq!(&report[..4], &[0x00, 0x38, 0x67, 2]);
        assert_eq!(&report[4..10], &[0, 0x04, 0x07, 0x00, 0, 0]);
        assert_eq!(&report[10..16], &[1, 0x1A, 0x16, 0x83, 0, 0]);
        assert!(check_report(&report).is_ok());
        assert_eq!(&rapid_tap_enable(true)[..4], &[0x00, 0x38, 0x66, 1]);
        assert!(check_report(&rapid_tap_enable(false)).is_ok());
    }

    #[test]
    fn bad_rapid_tap_pairs_are_refused() {
        let pair = |a, b| TapPair {
            a,
            b,
            mode: TapMode::LastWins,
            report_both: false,
        };
        assert!(rapid_tap_pairs(&[pair(0x04, 0x04)]).is_err(), "same key");
        assert!(
            rapid_tap_pairs(&[pair(0x04, 0x4F)]).is_err(),
            "the right arrow has a fixed switch"
        );
        assert!(
            rapid_tap_pairs(&[pair(0x04, 0x07), pair(0x07, 0x16)]).is_err(),
            "D in two pairs"
        );
        let eleven: Vec<TapPair> = (0..11).map(|i| pair(0x04 + 2 * i, 0x05 + 2 * i)).collect();
        assert!(rapid_tap_pairs(&eleven).is_err(), "eleven pairs");
        assert!(rapid_tap_pairs(&eleven[..10]).is_ok(), "ten fit");
    }
}
