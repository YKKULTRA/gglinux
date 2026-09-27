//! Apex Pro TKL Gen 3 (`1038:1642`) lighting protocol.
//!
//! From OpenRGB's `SteelSeriesApexController` (GPL-2.0-or-later), verified on
//! this keyboard 2026-09-27: an init, a direct frame (all red, then only G
//! green) and the onboard hand-back all behaved as sent.

use anyhow::{ensure, Result};

pub const OP_INIT: u8 = 0x4B;
pub const OP_DIRECT: u8 = 0x40;
pub const OP_ONBOARD: u8 = 0x41;
pub const OP_SET_BRIGHTNESS: u8 = 0x23;
pub const OP_GET_BRIGHTNESS: u8 = 0xA3;
pub const OP_REGION: u8 = 0xF5;
pub const OP_FIRMWARE: u8 = 0x90;

/// Every opcode gglinux may send. The firmware-update (FIZZ) commands share
/// this interface and can brick the keyboard; an allowlist makes them — and
/// anything else unvetted, typos included — impossible to frame.
pub const ALLOWED: [u8; 7] =
    [OP_INIT, OP_DIRECT, OP_ONBOARD, OP_SET_BRIGHTNESS, OP_GET_BRIGHTNESS, OP_REGION, OP_FIRMWARE];

/// Command reports: report id 0, opcode, arguments, zero padding.
pub const COMMAND_REPORT: usize = 65;
/// A direct lighting frame is one 643-byte feature report.
pub const DIRECT_REPORT: usize = 643;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

pub fn check(op: u8) -> Result<()> {
    ensure!(ALLOWED.contains(&op), "opcode {op:#04x} is not on the keyboard's allowlist");
    Ok(())
}

/// A command report for an allowed opcode.
pub fn command(op: u8, args: &[u8]) -> Result<Vec<u8>> {
    check(op)?;
    ensure!(args.len() <= COMMAND_REPORT - 2, "command arguments too long");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_seven_vetted_opcodes_pass() {
        let allowed: Vec<u8> = (0..=255u8).filter(|&op| check(op).is_ok()).collect();
        assert_eq!(allowed, [0x23, 0x40, 0x41, 0x4B, 0x90, 0xA3, 0xF5]);
        for fizz in [0x02, 0x03, 0x04] {
            assert!(command(fizz, &[]).is_err(), "firmware opcode {fizz:#04x} must never be framed");
        }
    }

    #[test]
    fn commands_are_65_byte_reports_with_report_id_zero() {
        let init = command(OP_INIT, &[]).unwrap();
        assert_eq!(init.len(), 65);
        assert_eq!(&init[..3], &[0x00, 0x4B, 0x00]);
        assert!(command(OP_REGION, &[0; 64]).is_err(), "arguments longer than the report");
    }

    #[test]
    fn a_direct_frame_lists_every_led() {
        let frame = direct_frame(|code| if code == 0x0A { Rgb { r: 0, g: 255, b: 0 } } else { Rgb::default() });
        assert_eq!(frame.len(), 643);
        assert_eq!(&frame[..3], &[0x00, 0x40, 112]);
        let g = KEYS.iter().position(|&c| c == 0x0A).unwrap();
        assert_eq!(&frame[3 + g * 4..7 + g * 4], &[0x0A, 0, 255, 0], "G green, as seen on the keyboard");
        assert_eq!(&frame[3..7], &[0x04, 0, 0, 0], "A dark");
        assert!(frame[3 + 112 * 4..].iter().all(|&b| b == 0), "padding");
    }

    #[test]
    fn region_decodes_from_the_reply() {
        assert_eq!(decode_region(&[0xF5, 0x00, 0x01, 0x00]), Some(1), "captured on this unit: US");
        assert_eq!(decode_region(&[0xA3, 0x00, 0x01]), None);
    }
}
