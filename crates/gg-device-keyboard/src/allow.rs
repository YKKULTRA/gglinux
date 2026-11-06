// SPDX-License-Identifier: GPL-3.0-or-later
//! What may be sent to the keyboard. [`crate::Keyboard`] checks every report here right
//! before its transport gets it, with the gated families the daemon has on.
//!
//! Always allowed: the plain opcodes in [`crate::proto::ALLOWED`] and the screen and switch
//! sub-commands in [`crate::proto::ALLOWED_SUB`], the actuation table on its primary layer only.
//! They write the keyboard's RAM.
//!
//! Two gated families come on top, each only while its experimental gate is on
//! ([`Families`]; both off by default):
//!
//! - **bindings** (`keyboard_bindings`): `36 <layer 0..=2> <count 1..=106> <blocks>` (a
//!   feature report: write a layer's bindings), `B6 <layer> <count> <blocks>` (a feature
//!   report: ask for them back), `35 <key>` (an output report: the Fn key, then zeros),
//!   `3C <32-byte mask>` (an output report: the Fn-lit keys, then zeros) and the second
//!   actuation point's table `38 61 44 02 [key, h, l] × 68`. RAM only.
//! - **onboard** (`keyboard_onboard`), the profile slots in flash: `02 01 <file>` (erase),
//!   `B1 <slot>` (validate) and `B2 <slot>` (load), output reports followed by zeros;
//!   `03 01 <file> <size u16> <offset u32> <data>` (write a chunk, a feature report that
//!   holds all `size` bytes) and `83 01 <file> <size u16> <offset u32>` (ask for a chunk
//!   back, then zeros), with `size` 1 to 500 and `offset + size` within the 12 324-byte
//!   image. `file` is `0x80 + slot`, slot 0 to 4. Integers are little-endian.
//!
//! Never, whatever is on: `04` (the firmware's file checksum), `02`, `03` or `83` with a
//! sub-command other than `01` or a file other than the five slots' (other files hold
//! what the keyboard needs to run), `75` (writes the region to flash), `38 83` (a screen
//! picture to flash), and anything not listed here. A gated report also may not be longer
//! than a feature report. Each refusal says which rule refused it.
//!
//! The shapes are from Apex Control's protocol reference (github.com/Rikearon/apex-control,
//! `docs/PROTOCOL.md` at `f7db8c5`, CC BY 4.0): "Key bindings", "Actuation & rapid
//! trigger" and "Onboard profiles & the flash filesystem". Which kind of report each one
//! is goes by that document; the check here is on the bytes.

use std::ops::RangeInclusive;

use anyhow::{bail, ensure, Result};

use crate::proto::{
    ALLOWED, ALLOWED_SUB, ANALOG_KEYS, FEATURE_REPORT, OP_OLED, OP_SWITCHES, SW_THRESHOLDS,
};

/// The gated command families [`check`] lets through, each only while the daemon has its
/// experimental gate on. Both off by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Families {
    /// `keyboard_bindings`: key bindings, the Fn key, the second actuation point.
    pub bindings: bool,
    /// `keyboard_onboard`: the onboard profile slots in flash.
    pub onboard: bool,
}

/// Write one layer's bindings: `36 <layer> <count> [key, function, code × 4] × count`.
pub const OP_BINDINGS: u8 = 0x36;
/// Ask for one layer's bindings back, write then read, same shape as [`OP_BINDINGS`].
pub const OP_READ_BINDINGS: u8 = 0xB6;
/// Pick the key that holds the Fn layer: `35 <key>`.
pub const OP_FN_KEY: u8 = 0x35;
/// Which keys light while Fn is held: `3C <32-byte mask>`, bit `key & 7` of byte `key / 8`.
pub const OP_FN_MASK: u8 = 0x3C;
/// Erase a slot's file: `02 01 <file>`; the answer comes when the erase is done.
pub const OP_ERASE: u8 = 0x02;
/// Write a chunk of a slot's file: `03 01 <file> <size> <offset> <data>`.
pub const OP_WRITE_CHUNK: u8 = 0x03;
/// Read a chunk of a slot's file back: `83 01 <file> <size> <offset>`.
pub const OP_READ_CHUNK: u8 = 0x83;
/// Have the keyboard check a slot it was written: `B1 <slot>`.
pub const OP_VALIDATE_SLOT: u8 = 0xB1;
/// Make the keyboard run a slot: `B2 <slot>`.
pub const OP_LOAD_SLOT: u8 = 0xB2;

/// The file commands' one known sub-command.
pub const FILE_SUB: u8 = 0x01;
/// The five profile slots' files, `0x80 + slot`.
pub const SLOT_FILES: RangeInclusive<u8> = 0x80..=0x84;
pub const SLOTS: RangeInclusive<u8> = 0..=4;
/// Binding layers: 0 normal, 1 Fn, 2 past the second actuation point.
pub const LAYERS: RangeInclusive<u8> = 0..=2;
/// Keys in one bindings report: `(645 − 5) / 6` fit.
pub const BLOCKS: RangeInclusive<u8> = 1..=106;
/// One key's bindings block: key, function, four codes.
pub const BLOCK: usize = 6;
/// The Fn-lit keys' mask.
pub const FN_MASK_BYTES: usize = 32;
/// Bytes in one flash chunk.
pub const CHUNK: RangeInclusive<u16> = 1..=500;
/// A slot's image: a 12 320-byte body and its CRC.
pub const IMAGE_BYTES: u32 = 12_324;
/// The second actuation point's layer byte in a `38 61` table.
pub const SECOND_LAYER: u8 = 0x02;

/// The firmware's file checksum: never sent.
const OP_FILE_CRC: u8 = 0x04;
/// Writes the region to flash on every call: never sent.
const OP_WRITE_REGION: u8 = 0x75;
/// The file header: report id, opcode, sub-command, file, size, offset.
const FILE_HEADER: usize = 10;

/// A gated family, as refusals name it.
struct Family {
    what: &'static str,
    gate: &'static str,
}

const BINDINGS: Family = Family {
    what: "key bindings",
    gate: "keyboard_bindings",
};
const ONBOARD: Family = Family {
    what: "onboard profile slot commands",
    gate: "keyboard_onboard",
};

/// Check a whole report, report id first, before it goes out with `families` on.
pub fn check(report: &[u8], families: Families) -> Result<()> {
    ensure!(
        report.len() >= 2 && report[0] == 0,
        "keyboard reports start with report id 0"
    );
    let op = report[1];
    match op {
        OP_SWITCHES => switches(report, families),
        OP_OLED => sub_command(report),
        _ if ALLOWED.contains(&op) => Ok(()),
        OP_BINDINGS | OP_READ_BINDINGS => {
            gated(report, binding_blocks, BINDINGS, families.bindings)
        }
        OP_FN_KEY => gated(report, fn_key, BINDINGS, families.bindings),
        OP_FN_MASK => gated(report, fn_mask, BINDINGS, families.bindings),
        OP_ERASE => gated(report, erase, ONBOARD, families.onboard),
        OP_WRITE_CHUNK | OP_READ_CHUNK => gated(report, chunk, ONBOARD, families.onboard),
        OP_VALIDATE_SLOT | OP_LOAD_SLOT => gated(report, slot, ONBOARD, families.onboard),
        OP_FILE_CRC => bail!("keyboard command 0x04 (the firmware's file checksum) is never sent"),
        OP_WRITE_REGION => {
            bail!("keyboard command 0x75 writes the region to flash: it is never sent")
        }
        _ => bail!("opcode {op:#04x} is not on the keyboard's allowlist"),
    }
}

/// The screen's and the switches' sub-command must be one of theirs.
fn sub_command(report: &[u8]) -> Result<()> {
    let op = report[1];
    let subs = ALLOWED_SUB
        .iter()
        .find(|(o, _)| *o == op)
        .map_or(&[][..], |(_, subs)| *subs);
    let sub = report.get(2).copied().unwrap_or(0);
    ensure!(
        subs.contains(&sub),
        "keyboard command {op:#04x} {sub:#04x} is not on the allowlist"
    );
    Ok(())
}

/// The switch tables. Byte 4 of an actuation table picks its layer: the primary one
/// always, the second actuation point's with the bindings family.
fn switches(report: &[u8], families: Families) -> Result<()> {
    sub_command(report)?;
    if report[2] != SW_THRESHOLDS || report.get(4) == Some(&0) {
        return Ok(());
    }
    ensure!(
        report.get(4) == Some(&SECOND_LAYER),
        "only the primary actuation layer may be written"
    );
    let table = 5 + 3 * ANALOG_KEYS.len();
    ensure!(
        report.get(3) == Some(&(ANALOG_KEYS.len() as u8)),
        "the second actuation table (38 61 44 02) lists all 68 analog keys"
    );
    ensure!(
        (table..=FEATURE_REPORT).contains(&report.len()),
        "the second actuation table is a whole table of {table} to {FEATURE_REPORT} bytes, not {}",
        report.len()
    );
    open(report, BINDINGS, families.bindings)
}

/// A gated command: its shape first, so a shape that's never sent says so whatever is on,
/// then its family.
fn gated(report: &[u8], shape: fn(&[u8]) -> Result<()>, family: Family, on: bool) -> Result<()> {
    ensure!(
        report.len() <= FEATURE_REPORT,
        "keyboard command {:#04x} is longer than a feature report ({FEATURE_REPORT} bytes)",
        report[1]
    );
    shape(report)?;
    open(report, family, on)
}

fn open(report: &[u8], family: Family, on: bool) -> Result<()> {
    ensure!(
        on,
        "keyboard command {:#04x} is one of the {}, which stay unsent until {} is turned on",
        report[1],
        family.what,
        family.gate
    );
    Ok(())
}

/// `36`/`B6 <layer> <count>` and room for `count` blocks.
fn binding_blocks(r: &[u8]) -> Result<()> {
    let op = r[1];
    let layer = r.get(2).copied();
    ensure!(
        layer.is_some_and(|l| LAYERS.contains(&l)),
        "keyboard command {op:#04x}: layer {} is not 0 (normal), 1 (Fn) or 2 (second actuation)",
        hex(layer)
    );
    let Some(count) = r.get(3).copied() else {
        bail!("keyboard command {op:#04x} has no key count");
    };
    ensure!(
        BLOCKS.contains(&count),
        "keyboard command {op:#04x}: {count} keys in one report, not 1 to {}",
        BLOCKS.end()
    );
    let need = 4 + BLOCK * usize::from(count);
    ensure!(
        r.len() >= need,
        "keyboard command {op:#04x}: {count} keys need {need} bytes, the report has {}",
        r.len()
    );
    Ok(())
}

/// `35 <key>`, then zeros.
fn fn_key(r: &[u8]) -> Result<()> {
    ensure!(
        r.len() >= 3,
        "keyboard command 0x35 names the Fn key, and this one names none"
    );
    zeros_after(r, 3)
}

/// `3C <32-byte mask>`, then zeros.
fn fn_mask(r: &[u8]) -> Result<()> {
    ensure!(
        r.len() >= 2 + FN_MASK_BYTES,
        "keyboard command 0x3c carries a 32-byte mask; this one has {} bytes",
        r.len() - 2
    );
    zeros_after(r, 2 + FN_MASK_BYTES)
}

/// `02`, `03` and `83`: sub-command `01` on one of the five slots' files, or never.
fn slot_file(r: &[u8]) -> Result<()> {
    let op = r[1];
    let sub = r.get(2).copied();
    ensure!(
        sub == Some(FILE_SUB),
        "keyboard command {op:#04x} with sub-command {} is never sent: only 0x01, on a profile slot",
        hex(sub)
    );
    let file = r.get(3).copied();
    ensure!(
        file.is_some_and(|f| SLOT_FILES.contains(&f)),
        "keyboard command {op:#04x} 0x01 on file {} is never sent: only the profile slots' files 0x80 to 0x84",
        hex(file)
    );
    Ok(())
}

/// `02 01 <file>`, then zeros.
fn erase(r: &[u8]) -> Result<()> {
    slot_file(r)?;
    zeros_after(r, 4)
}

/// `03 01 <file> <size> <offset> <data>` and `83 01 <file> <size> <offset>`, inside the image.
fn chunk(r: &[u8]) -> Result<()> {
    let op = r[1];
    slot_file(r)?;
    ensure!(
        r.len() >= FILE_HEADER,
        "keyboard command {op:#04x} 0x01 needs its {FILE_HEADER}-byte header: size and offset"
    );
    let size = u16::from_le_bytes([r[4], r[5]]);
    let offset = u32::from_le_bytes([r[6], r[7], r[8], r[9]]);
    ensure!(
        CHUNK.contains(&size),
        "keyboard command {op:#04x} 0x01: a chunk is 1 to {} bytes, not {size}",
        CHUNK.end()
    );
    let end = u64::from(offset) + u64::from(size);
    ensure!(
        end <= u64::from(IMAGE_BYTES),
        "keyboard command {op:#04x} 0x01: bytes {offset} to {end} run past the image ({IMAGE_BYTES} bytes)"
    );
    if op == OP_WRITE_CHUNK {
        let held = r.len() - FILE_HEADER;
        ensure!(
            held >= usize::from(size),
            "keyboard command 0x03 0x01: the report holds {held} of its chunk's {size} bytes"
        );
        Ok(())
    } else {
        zeros_after(r, FILE_HEADER)
    }
}

/// `B1`/`B2 <slot>`, then zeros.
fn slot(r: &[u8]) -> Result<()> {
    let op = r[1];
    let Some(slot) = r.get(2).copied() else {
        bail!("keyboard command {op:#04x} names no slot");
    };
    ensure!(
        SLOTS.contains(&slot),
        "keyboard command {op:#04x}: slot {slot} is not 0 to 4"
    );
    zeros_after(r, 3)
}

/// Nothing may follow a command's own `len` bytes but zero padding.
fn zeros_after(r: &[u8], len: usize) -> Result<()> {
    ensure!(
        r.iter().skip(len).all(|&b| b == 0),
        "keyboard command {:#04x}: only zeros may follow its first {len} bytes",
        r[1]
    );
    Ok(())
}

fn hex(b: Option<u8>) -> String {
    b.map_or_else(|| "none".to_owned(), |b| format!("{b:#04x}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{self, COMMAND_REPORT};

    const OFF: Families = Families {
        bindings: false,
        onboard: false,
    };
    const BINDINGS_ON: Families = Families {
        bindings: true,
        onboard: false,
    };
    const ONBOARD_ON: Families = Families {
        bindings: false,
        onboard: true,
    };
    const BOTH: Families = Families {
        bindings: true,
        onboard: true,
    };
    const ALL: [Families; 4] = [OFF, BINDINGS_ON, ONBOARD_ON, BOTH];

    /// A report of `len` bytes: report id 0, then `head`, then zeros.
    fn report(len: usize, head: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; len];
        r[1..1 + head.len()].copy_from_slice(head);
        r
    }

    fn output(head: &[u8]) -> Vec<u8> {
        report(COMMAND_REPORT, head)
    }

    fn feature(head: &[u8]) -> Vec<u8> {
        report(FEATURE_REPORT, head)
    }

    fn head(r: &[u8]) -> &[u8] {
        &r[..r.len().min(12)]
    }

    /// Passes with exactly the families in `with`, and says why it doesn't with the others.
    #[track_caller]
    fn passes_only_with(r: &[u8], with: &[Families], why_not: &str) {
        for f in ALL {
            match check(r, f) {
                Ok(()) => assert!(with.contains(&f), "{f:?} let {:02x?} through", head(r)),
                Err(e) => {
                    assert!(!with.contains(&f), "{f:?} refused {:02x?}: {e:#}", head(r));
                    let e = format!("{e:#}");
                    assert!(e.contains(why_not), "{f:?}: {e:?} should say {why_not:?}");
                }
            }
        }
    }

    /// Refused whatever is on, saying `why`.
    #[track_caller]
    fn never(r: &[u8], why: &str) {
        passes_only_with(r, &[], why);
    }

    const BINDINGS_GATE: &str = "keyboard_bindings";
    const ONBOARD_GATE: &str = "keyboard_onboard";

    /// A bindings report: `op layer count`, then `count` blocks, in `len` bytes.
    fn bindings_report(op: u8, layer: u8, count: u8, len: usize) -> Vec<u8> {
        let mut r = report(len, &[op, layer, count]);
        for i in 0..usize::from(count) {
            let at = 4 + i * BLOCK;
            if at + BLOCK <= len {
                // `04 51 04 00 00 00`: A sends A, the doc's resting state.
                r[at..at + BLOCK].copy_from_slice(&[0x04 + i as u8, 0x51, 0x04 + i as u8, 0, 0, 0]);
            }
        }
        r
    }

    #[test]
    fn binding_writes_and_reads_need_the_bindings_family_and_their_shape() {
        for op in [OP_BINDINGS, OP_READ_BINDINGS] {
            for layer in LAYERS {
                passes_only_with(
                    &bindings_report(op, layer, 1, FEATURE_REPORT),
                    &[BINDINGS_ON, BOTH],
                    BINDINGS_GATE,
                );
            }
            let full = bindings_report(op, 0, 106, FEATURE_REPORT);
            assert_eq!(4 + 106 * BLOCK, 640, "106 blocks fit a feature report");
            passes_only_with(&full, &[BINDINGS_ON, BOTH], BINDINGS_GATE);
            // Exactly long enough for its blocks.
            passes_only_with(
                &bindings_report(op, 1, 2, 4 + 2 * BLOCK),
                &[BINDINGS_ON, BOTH],
                BINDINGS_GATE,
            );
            never(&bindings_report(op, 3, 1, FEATURE_REPORT), "layer 0x03");
            never(&bindings_report(op, 0xFF, 1, FEATURE_REPORT), "layer 0xff");
            never(&bindings_report(op, 0, 0, FEATURE_REPORT), "0 keys");
            never(&bindings_report(op, 0, 107, FEATURE_REPORT), "107 keys");
            never(
                &bindings_report(op, 0, 2, 4 + 2 * BLOCK - 1),
                "need 16 bytes",
            );
            never(&report(3, &[op, 0]), "count");
            never(&report(FEATURE_REPORT + 1, &[op, 0, 1]), "645 bytes");
        }
    }

    #[test]
    fn the_fn_key_and_its_mask_need_the_bindings_family_and_nothing_after_them() {
        passes_only_with(
            &output(&[OP_FN_KEY, 0xF0]),
            &[BINDINGS_ON, BOTH],
            BINDINGS_GATE,
        );
        passes_only_with(
            &report(3, &[OP_FN_KEY, 0x39]),
            &[BINDINGS_ON, BOTH],
            BINDINGS_GATE,
        );
        never(&report(2, &[OP_FN_KEY]), "names the Fn key");
        never(&output(&[OP_FN_KEY, 0xF0, 0x01]), "zeros");

        let mut mask = output(&[OP_FN_MASK]);
        mask[2] = 0x10; // A: bit 4 of byte 0
        mask[2 + 31] = 0x01; // 0xF8
        passes_only_with(&mask, &[BINDINGS_ON, BOTH], BINDINGS_GATE);
        passes_only_with(
            &report(34, &[OP_FN_MASK]),
            &[BINDINGS_ON, BOTH],
            BINDINGS_GATE,
        );
        never(&report(33, &[OP_FN_MASK]), "32-byte mask");
        let mut past = output(&[OP_FN_MASK]);
        past[34] = 0x01;
        never(&past, "zeros");
    }

    #[test]
    fn the_second_actuation_table_is_refused_with_families_off_and_accepted_with_bindings() {
        let mut table = proto::actuation_table(|_| 20);
        assert!(check(&table, OFF).is_ok(), "the primary layer always");
        table[4] = SECOND_LAYER;
        assert_eq!(&table[..5], &[0x00, 0x38, 0x61, 0x44, 0x02]);
        passes_only_with(&table, &[BINDINGS_ON, BOTH], BINDINGS_GATE);
        assert!(check(&table, ONBOARD_ON).is_err(), "not with onboard alone");

        table[4] = 0x01;
        never(&table, "primary actuation layer");
        table[4] = SECOND_LAYER;
        table[3] = 0x43;
        never(&table, "all 68 analog keys");
        table[3] = 0x44;
        never(&table[..5 + 3 * 68 - 1], "whole table");
        passes_only_with(&table[..5 + 3 * 68], &[BINDINGS_ON, BOTH], BINDINGS_GATE);
    }

    fn file_report(op: u8, file: u8, size: u16, offset: u32, len: usize) -> Vec<u8> {
        let mut r = report(len, &[op, FILE_SUB, file]);
        r[4..6].copy_from_slice(&size.to_le_bytes());
        r[6..10].copy_from_slice(&offset.to_le_bytes());
        if op == OP_WRITE_CHUNK {
            for b in r.iter_mut().skip(FILE_HEADER).take(usize::from(size)) {
                *b = 0xFF;
            }
        }
        r
    }

    #[test]
    fn erasing_validating_and_loading_a_slot_need_the_onboard_family() {
        for file in SLOT_FILES {
            passes_only_with(
                &output(&[OP_ERASE, 0x01, file]),
                &[ONBOARD_ON, BOTH],
                ONBOARD_GATE,
            );
        }
        for slot in SLOTS {
            for op in [OP_VALIDATE_SLOT, OP_LOAD_SLOT] {
                passes_only_with(&output(&[op, slot]), &[ONBOARD_ON, BOTH], ONBOARD_GATE);
            }
        }
        for op in [OP_VALIDATE_SLOT, OP_LOAD_SLOT] {
            never(&output(&[op, 5]), "slot 5");
            never(&output(&[op, 0xFF]), "slot 255");
            never(&output(&[op, 1, 1]), "zeros");
            never(&report(2, &[op]), "slot");
        }
        never(&output(&[OP_ERASE, 0x01, 0x85]), "file 0x85");
        never(&output(&[OP_ERASE, 0x01, 0x7F]), "file 0x7f");
        never(&output(&[OP_ERASE, 0x01, 0x00]), "file 0x00");
        never(&output(&[OP_ERASE, 0x00, 0x80]), "sub-command 0x00");
        never(&output(&[OP_ERASE, 0x02, 0x80]), "sub-command 0x02");
        never(&report(3, &[OP_ERASE, 0x01]), "file none");
        never(&output(&[OP_ERASE, 0x01, 0x80, 0x01]), "zeros");
    }

    #[test]
    fn a_chunk_write_must_hold_its_bytes_and_stay_inside_the_image() {
        let w = |file, size, offset, len| file_report(OP_WRITE_CHUNK, file, size, offset, len);
        for file in SLOT_FILES {
            passes_only_with(
                &w(file, 500, 0, FEATURE_REPORT),
                &[ONBOARD_ON, BOTH],
                ONBOARD_GATE,
            );
        }
        let last = 12_324 - 500;
        passes_only_with(
            &w(0x84, 500, last, FEATURE_REPORT),
            &[ONBOARD_ON, BOTH],
            ONBOARD_GATE,
        );
        passes_only_with(
            &w(0x84, 324, 12_000, FEATURE_REPORT),
            &[ONBOARD_ON, BOTH],
            ONBOARD_GATE,
        );
        passes_only_with(&w(0x84, 1, 12_323, 11), &[ONBOARD_ON, BOTH], ONBOARD_GATE);
        never(&w(0x84, 500, last + 1, FEATURE_REPORT), "past the image");
        never(&w(0x84, 500, 12_000, FEATURE_REPORT), "past the image");
        never(&w(0x84, 500, u32::MAX, FEATURE_REPORT), "past the image");
        never(&w(0x84, 0, 0, FEATURE_REPORT), "1 to 500 bytes, not 0");
        never(&w(0x84, 501, 0, FEATURE_REPORT), "1 to 500 bytes, not 501");
        never(&w(0x84, 500, 0, FILE_HEADER + 499), "holds 499");
        never(&w(0x85, 500, 0, FEATURE_REPORT), "file 0x85");
        never(&report(9, &[OP_WRITE_CHUNK, 0x01, 0x84]), "header");
        let mut sub = w(0x84, 500, 0, FEATURE_REPORT);
        sub[2] = 0x02;
        never(&sub, "sub-command 0x02");
        never(
            &report(FEATURE_REPORT + 1, &[OP_WRITE_CHUNK, 0x01, 0x84, 1]),
            "645 bytes",
        );
    }

    #[test]
    fn a_chunk_read_request_has_the_same_bounds_and_nothing_after_its_header() {
        let r = |file, size, offset| file_report(OP_READ_CHUNK, file, size, offset, FEATURE_REPORT);
        passes_only_with(&r(0x80, 500, 0), &[ONBOARD_ON, BOTH], ONBOARD_GATE);
        passes_only_with(&r(0x82, 36, 12_288), &[ONBOARD_ON, BOTH], ONBOARD_GATE);
        passes_only_with(
            &file_report(OP_READ_CHUNK, 0x80, 500, 0, FILE_HEADER),
            &[ONBOARD_ON, BOTH],
            ONBOARD_GATE,
        );
        never(&r(0x82, 36, 12_289), "past the image");
        never(&r(0x80, 501, 0), "not 501");
        never(&r(0x80, 0, 0), "not 0");
        never(&r(0x7F, 500, 0), "file 0x7f");
        let mut tail = r(0x80, 500, 0);
        tail[FILE_HEADER] = 0x01;
        never(&tail, "zeros");
        let mut sub = r(0x80, 500, 0);
        sub[2] = 0x00;
        never(&sub, "sub-command 0x00");
    }

    #[test]
    fn flash_and_firmware_commands_outside_the_slots_are_never_sent() {
        for sub in 0..=255u8 {
            for op in [OP_ERASE, OP_WRITE_CHUNK, OP_READ_CHUNK] {
                for file in [0x00, 0x01, 0x7F, 0x80, 0x84, 0x85, 0xFF] {
                    let r = file_report(op, file, 1, 0, FEATURE_REPORT);
                    let mut r = r;
                    r[2] = sub;
                    if sub != FILE_SUB || !SLOT_FILES.contains(&file) {
                        never(&r, "never sent");
                    }
                }
            }
            never(&output(&[0x04, sub]), "never sent");
            never(&feature(&[0x04, sub]), "never sent");
            never(&output(&[0x75, sub]), "never sent");
        }
        never(&feature(&[OP_SWITCHES, 0x83, 0x00]), "not on the allowlist");
        never(&output(&[0x34, 0x01]), "allowlist");
        never(&output(&[0xF2]), "allowlist");
    }

    #[test]
    fn with_every_family_off_only_todays_opcodes_pass() {
        // Every opcode with a plausible body: only today's let out.
        let mut passed = Vec::new();
        for op in 0..=255u8 {
            let bodies = [
                output(&[op]),
                output(&[op, 0x01, 0x80]),
                bindings_report(op, 0, 1, FEATURE_REPORT),
                file_report(op, 0x80, 1, 0, FEATURE_REPORT),
                feature(&[op, 0x61, 0x44, 0x00]),
                feature(&[op, 0x81]),
            ];
            if bodies.iter().any(|r| check(r, OFF).is_ok()) {
                passed.push(op);
            }
        }
        assert_eq!(
            passed,
            [0x1F, 0x23, 0x38, 0x40, 0x41, 0x4B, 0x90, 0xA3, 0xF5]
        );
        // And `proto::check_report` is this check with the families off.
        for op in 0..=255u8 {
            let r = bindings_report(op, 0, 1, FEATURE_REPORT);
            assert_eq!(proto::check_report(&r).is_ok(), check(&r, OFF).is_ok());
        }
    }

    #[test]
    fn every_report_todays_code_frames_still_passes_with_the_families_off() {
        let pair = |a, b| proto::TapPair {
            a,
            b,
            mode: proto::TapMode::SecondWins,
            report_both: true,
        };
        let framed = [
            proto::command(proto::OP_INIT, &[]).unwrap(),
            proto::command(proto::OP_ONBOARD, &[]).unwrap(),
            proto::command(proto::OP_REGION, &[]).unwrap(),
            proto::command(proto::OP_FIRMWARE, &[0x00]).unwrap(),
            proto::command(proto::OP_SET_BRIGHTNESS, &[0x64]).unwrap(),
            proto::command(proto::OP_GET_BRIGHTNESS, &[]).unwrap(),
            proto::direct_frame(|c| proto::Rgb {
                r: c,
                g: 0xFF,
                b: 1,
            }),
            proto::oled_frame(&[0xA5; crate::oled::PACKED]),
            proto::oled_release(),
            proto::actuation_table(|c| c % 40 + 1),
            proto::release_mode_table(|c| c % 2 == 0),
            proto::sensitivity_table(|c| c % 20 + 1),
            proto::rapid_tap_enable(true),
            proto::rapid_tap_enable(false),
            proto::rapid_tap_pairs(&[]).unwrap(),
            proto::rapid_tap_pairs(&[pair(0x04, 0x07), pair(0x1A, 0x16)]).unwrap(),
        ];
        for r in &framed {
            for f in ALL {
                assert!(check(r, f).is_ok(), "{f:?} refused {:02x?}", &r[..4]);
            }
        }
        assert!(framed.iter().all(|r| r.len() <= FEATURE_REPORT));
    }

    // --- the property test ------------------------------------------------------------

    /// A small seeded generator (Knuth's MMIX constants), so a failure repeats.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn byte(&mut self) -> u8 {
            self.next() as u8
        }

        fn pick<T: Copy>(&mut self, from: &[T]) -> T {
            from[self.below(from.len())]
        }
    }

    const OPS: [u8; 22] = [
        0x02, 0x03, 0x04, 0x1F, 0x23, 0x34, 0x35, 0x36, 0x38, 0x3C, 0x40, 0x41, 0x4B, 0x75, 0x83,
        0x90, 0xA3, 0xB1, 0xB2, 0xB6, 0xF2, 0xF5,
    ];

    /// A report of one listed shape, with random values where the shape allows any.
    fn shaped(rng: &mut Lcg) -> Vec<u8> {
        let op = rng.pick(&OPS);
        match op {
            OP_BINDINGS | OP_READ_BINDINGS => {
                let count = 1 + rng.below(106) as u8;
                let need = 4 + BLOCK * usize::from(count);
                let mut r = bindings_report(
                    op,
                    rng.below(3) as u8,
                    count,
                    need + rng.below(FEATURE_REPORT - need + 1),
                );
                for b in r.iter_mut().skip(4).take(BLOCK * usize::from(count)) {
                    *b = rng.byte();
                }
                r
            }
            OP_FN_KEY => output(&[op, rng.byte()]),
            OP_FN_MASK => {
                let mut r = output(&[op]);
                for b in &mut r[2..34] {
                    *b = rng.byte();
                }
                r
            }
            OP_SWITCHES => {
                let mut r = proto::actuation_table(|c| c % 40 + 1);
                r[4] = rng.pick(&[0, SECOND_LAYER]);
                r
            }
            OP_ERASE => output(&[op, FILE_SUB, rng.pick(&[0x80, 0x81, 0x82, 0x83, 0x84])]),
            OP_WRITE_CHUNK | OP_READ_CHUNK => {
                let size = 1 + rng.below(500) as u16;
                let offset = rng.below(12_324 - usize::from(size) + 1) as u32;
                let len = if op == OP_WRITE_CHUNK {
                    FILE_HEADER
                        + usize::from(size)
                        + rng.below(FEATURE_REPORT - FILE_HEADER - usize::from(size) + 1)
                } else {
                    FILE_HEADER + rng.below(FEATURE_REPORT - FILE_HEADER + 1)
                };
                let mut r = file_report(op, 0x80 + rng.below(5) as u8, size, offset, len);
                if op == OP_WRITE_CHUNK {
                    for b in r.iter_mut().skip(FILE_HEADER).take(usize::from(size)) {
                        *b = rng.byte();
                    }
                }
                r
            }
            OP_VALIDATE_SLOT | OP_LOAD_SLOT => output(&[op, rng.below(5) as u8]),
            _ => {
                let mut r = output(&[op]);
                for b in &mut r[2..] {
                    *b = rng.byte();
                }
                r
            }
        }
    }

    /// Anything at all, 2 to 645 bytes, report id mostly 0 and the opcode mostly one that
    /// means something, so the edges of every shape get reached.
    fn random(rng: &mut Lcg) -> Vec<u8> {
        let len = 2 + rng.below(FEATURE_REPORT - 1);
        let mut r: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        if rng.below(10) != 0 {
            r[0] = 0;
        }
        if rng.below(4) != 0 {
            r[1] = rng.pick(&OPS);
        }
        if len > 2 && rng.below(2) == 0 {
            r[2] = rng.pick(&[
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x61, 0x62, 0x65, 0x66, 0x67, 0x81, 0x82, 0x83,
            ]);
        }
        if len > 3 && rng.below(2) == 0 {
            r[3] = rng.pick(&[0x00, 0x01, 0x44, 0x6A, 0x6B, 0x7F, 0x80, 0x82, 0x84, 0x85]);
        }
        if len > 4 && rng.below(2) == 0 {
            r[4] = rng.pick(&[0x00, 0x01, 0x02, 0x03]);
        }
        if rng.below(2) == 0 {
            let from = 3 + rng.below(60);
            for b in r.iter_mut().skip(from) {
                *b = 0;
            }
        }
        r
    }

    /// Change one or two bytes, or the length, of a report.
    fn mutate(rng: &mut Lcg, mut r: Vec<u8>) -> Vec<u8> {
        for _ in 0..1 + rng.below(2) {
            match rng.below(4) {
                0 => {
                    let new = 2 + rng.below(FEATURE_REPORT - 1);
                    r.resize(new, if rng.below(2) == 0 { 0 } else { rng.byte() });
                }
                1 => {
                    let at = rng.below(r.len());
                    r[at] = rng.byte();
                }
                _ => {
                    // The header bytes, where the rules are.
                    let at = rng.below(r.len().min(FILE_HEADER));
                    r[at] = rng.byte();
                }
            }
        }
        r
    }

    /// The listed shapes written out again, apart from [`check`] and plainly, as the oracle.
    fn listed(r: &[u8], f: Families) -> bool {
        let at = |i: usize| r.get(i).copied();
        let zeros_from = |i: usize| r.iter().skip(i).all(|&b| b == 0);
        let le16 = |i: usize| u32::from(r[i]) | u32::from(r[i + 1]) << 8;
        let le32 = |i: usize| le16(i) as u64 | (le16(i + 2) as u64) << 16;
        if r.len() < 2 || r[0] != 0 {
            return false;
        }
        let fits = r.len() <= 645;
        match r[1] {
            0x23 | 0x40 | 0x41 | 0x4B | 0x90 | 0xA3 | 0xF5 => true,
            0x1F => matches!(at(2), Some(0x81 | 0x82)),
            0x38 => match at(2) {
                Some(0x62 | 0x65 | 0x66 | 0x67) => true,
                Some(0x61) => {
                    at(4) == Some(0)
                        || (f.bindings
                            && fits
                            && at(3) == Some(68)
                            && at(4) == Some(2)
                            && r.len() >= 5 + 68 * 3)
                }
                _ => false,
            },
            0x36 | 0xB6 => {
                f.bindings
                    && fits
                    && matches!(at(2), Some(0..=2))
                    && matches!(at(3), Some(n) if (1..=106).contains(&n) && r.len() >= 4 + 6 * n as usize)
            }
            0x35 => f.bindings && fits && r.len() >= 3 && zeros_from(3),
            0x3C => f.bindings && fits && r.len() >= 34 && zeros_from(34),
            0x02 => {
                f.onboard
                    && fits
                    && at(2) == Some(1)
                    && matches!(at(3), Some(0x80..=0x84))
                    && zeros_from(4)
            }
            0x03 | 0x83 => {
                f.onboard
                    && fits
                    && r.len() >= 10
                    && at(2) == Some(1)
                    && matches!(at(3), Some(0x80..=0x84))
                    && (1..=500).contains(&le16(4))
                    && le32(6) + u64::from(le16(4)) <= 12_324
                    && if r[1] == 0x03 {
                        r.len() >= 10 + le16(4) as usize
                    } else {
                        zeros_from(10)
                    }
            }
            0xB1 | 0xB2 => f.onboard && fits && matches!(at(2), Some(0..=4)) && zeros_from(3),
            _ => false,
        }
    }

    #[test]
    fn no_random_report_passes_unless_it_has_a_listed_shape() {
        const REPORTS: usize = 120_000;
        let mut rng = Lcg(0x5EED_0611_0D0D);
        // Per opcode: (passed with its family on, refused with it on).
        let mut seen = std::collections::BTreeMap::<u8, (usize, usize)>::new();
        for _ in 0..REPORTS {
            let r = match rng.below(3) {
                0 => random(&mut rng),
                1 => shaped(&mut rng),
                _ => {
                    let r = shaped(&mut rng);
                    mutate(&mut rng, r)
                }
            };
            for f in ALL {
                let passed = check(&r, f).is_ok();
                assert_eq!(
                    passed,
                    listed(&r, f),
                    "{f:?}, {} bytes: {:02x?}",
                    r.len(),
                    head(&r)
                );
                if passed {
                    let op = r[1];
                    assert!(r[0] == 0 && op != 0x04 && op != 0x75, "{r:02x?}");
                    if matches!(op, 0x02 | 0x03 | 0x83) {
                        assert!(r[2] == 0x01 && (0x80..=0x84).contains(&r[3]), "{r:02x?}");
                    }
                    if f == OFF {
                        assert!(ALLOWED.contains(&op) || ALLOWED_SUB.iter().any(|(o, _)| *o == op));
                    }
                }
                if f == BOTH && r[0] == 0 {
                    let e = seen.entry(r[1]).or_default();
                    if passed {
                        e.0 += 1;
                    } else {
                        e.1 += 1;
                    }
                }
            }
        }
        // The generator reached both sides of every gated shape.
        for op in [0x02, 0x03, 0x35, 0x36, 0x38, 0x3C, 0x83, 0xB1, 0xB2, 0xB6] {
            let (passed, refused) = seen.get(&op).copied().unwrap_or_default();
            assert!(
                passed > 100 && refused > 100,
                "{op:#04x}: {passed} passed, {refused} refused"
            );
        }
    }
}
