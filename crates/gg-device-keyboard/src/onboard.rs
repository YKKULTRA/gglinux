// SPDX-License-Identifier: GPL-3.0-or-later
//! The five onboard profile slots in the keyboard's flash: what it loads when it powers up.
//!
//! From Apex Control's protocol reference (github.com/Rikearon/apex-control,
//! `docs/PROTOCOL.md` at `f7db8c5`, © 2026 Henrique Aron, CC BY 4.0), "Onboard profiles &
//! the flash filesystem": a slot is the file `0x80 + slot`, a 12 324-byte image (a 12 320-byte
//! body and its STM32 CRC-32); `83 01` reads it in chunks, `02 01` erases it, `03 01` writes
//! it in 500-byte chunks, `B1` has the keyboard check it and `B2` makes the keyboard run it.
//! The allowlist lets them out only with the onboard family on, and only for those five
//! files ([`crate::allow`]).
//!
//! An [`Image`] is kept whole, as read: the parts gglinux can't rebuild (macros, the zone
//! list, protection, the GUID, the binding layers) go back byte for byte. A slot is only
//! ever written by [`Keyboard::write_slot`], one checked transaction:
//!
//! 1. read the slot and check its size, schemas and CRC;
//! 2. have the caller save a backup and hand back what the file holds; it must be the slot
//!    exactly, or nothing is erased;
//! 3. make the new image from the old one and check it the same way;
//! 4. erase, write 25 chunks, have the keyboard check the slot, read it all back and
//!    compare;
//! 5. if anything in 4 fails, write the old image back the same way and report both.

use std::fmt;
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};

use crate::allow::{
    FILE_SUB, OP_ERASE, OP_LOAD_SLOT, OP_READ_CHUNK, OP_VALIDATE_SLOT, OP_WRITE_CHUNK,
};
use crate::oled::{Bitmap, PACKED};
use crate::proto::{self, ANALOG_KEYS, COMMAND_REPORT, FEATURE_REPORT};
use crate::{Keyboard, Switches, TapPair};

/// The keyboard has slots 0 to 4 (1 to 5 to the user).
pub const SLOTS: u8 = 5;
/// A slot's image: the body and its CRC.
pub const IMAGE_BYTES: usize = 12_324;
/// The part of the image the CRC covers.
pub const BODY_BYTES: usize = 12_320;
/// Bytes in one chunk read or written.
pub const CHUNK_BYTES: usize = 500;
/// Chunks in an image: 24 of 500 bytes and the last of 324.
pub const CHUNKS: usize = IMAGE_BYTES.div_ceil(CHUNK_BYTES);
/// The profile schema this layout is for.
pub const PROFILE_SCHEMA: u32 = 3;
/// The device schema this layout is for.
pub const DEVICE_SCHEMA: u32 = 8;
/// Per-key places in the switch tables (the first 70 of the firmware's key order).
pub const KEY_PLACES: usize = 70;

/// Where each field starts in an image ("Image layout", profile schema 3, device schema 8,
/// firmware 1.19.0 and later). All integers are little-endian.
pub mod at {
    /// `u32`, the profile schema, 3.
    pub const SCHEMA: usize = 0;
    /// 24 bytes of ASCII, zero-padded ("Config 1" … from the factory).
    pub const NAME: usize = 4;
    pub const NAME_BYTES: usize = 24;
    /// 48 bytes: the lock-key and Fn-key colours and brightness, with the Fn-lit keys'
    /// 32-byte mask at 40. Kept as it is.
    pub const LIGHTING: usize = 28;
    /// `u32`, the key that holds the Fn layer, by HID code (factory `0xF0`).
    pub const FN_KEY: usize = 76;
    /// 100 × 5 bytes each: the normal and the Fn layer's bindings, in the firmware's key
    /// order. Kept as they are.
    pub const NORMAL_BINDINGS: usize = 80;
    pub const FN_BINDINGS: usize = 580;
    /// `u32`, the device schema, 8.
    pub const DEVICE_SCHEMA: usize = 1080;
    /// 70 `h` bytes, then 70 `l` bytes: the primary actuation thresholds.
    pub const PRIMARY_H: usize = 1084;
    pub const PRIMARY_L: usize = 1154;
    /// Three zone levels, then a 70-byte per-key zone list. Kept as they are.
    pub const ZONES: usize = 1224;
    /// 70 bytes each: second actuation `h` and `l` (`FF FF` is off), release modes and
    /// Rapid Trigger distances.
    pub const SECOND_H: usize = 1306;
    pub const SECOND_L: usize = 1376;
    pub const RELEASE: usize = 1446;
    pub const SENSITIVITY: usize = 1516;
    /// `u32` milliseconds, then 70 per-key distances: Protection Mode. Kept as they are.
    pub const PROTECTION_MS: usize = 1586;
    pub const PROTECTION_DISTANCE: usize = 1590;
    /// 70 × 5 bytes: the second actuation point's bindings. Kept as they are.
    pub const SECOND_BINDINGS: usize = 1660;
    /// 640 bytes: the screen picture, packed as for `1F 81`.
    pub const OLED: usize = 2010;
    /// 10 × `key 1, key 2, mode | 0x80 to send both, 0, 0`, then the on/off byte.
    pub const RAPID_TAP: usize = 2650;
    pub const RAPID_TAP_ENABLE: usize = 2700;
    /// 9 600 bytes of macro events. Kept as they are.
    pub const MACROS: usize = 2704;
    pub const MACRO_BYTES: usize = 9_600;
    /// 16 bytes. Kept as it is.
    pub const GUID: usize = 12_304;
    /// `u32`, the CRC of everything before it.
    pub const CRC: usize = 12_320;
}

/// The bytes before a chunk's data in `03 01` and `83 01`: report id, opcode, sub-command,
/// file, size (`u16`), offset (`u32`).
const FILE_HEADER: usize = 10;
const TAP_ENTRY: usize = 5;
const TAP_ENTRIES: usize = 10;

/// One byte into a CRC-32 with polynomial `0x04C11DB7`, most significant bit first, as
/// the STM32's CRC unit works.
fn crc_byte(mut crc: u32, byte: u8) -> u32 {
    crc ^= u32::from(byte) << 24;
    for _ in 0..8 {
        crc = if crc & 0x8000_0000 != 0 {
            (crc << 1) ^ 0x04C1_1DB7
        } else {
            crc << 1
        };
    }
    crc
}

/// The STM32 hardware CRC-32 of `bytes` ("CRC"): init `0xFFFFFFFF`, no reflection, no
/// final XOR, fed as 32-bit little-endian words, so each word's last byte goes in first.
/// `bytes` must be whole words; an image body is.
pub fn crc32_stm32(bytes: &[u8]) -> u32 {
    debug_assert_eq!(bytes.len() % 4, 0, "the CRC unit takes whole words");
    bytes.chunks_exact(4).fold(0xFFFF_FFFF, |crc, word| {
        word.iter().rev().fold(crc, |crc, &b| crc_byte(crc, b))
    })
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// A slot's image that checked out: the right size, schemas 3 and 8, and its CRC. Every
/// change through it keeps the CRC right; everything it doesn't change stays as read.
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    bytes: Vec<u8>,
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Image")
            .field("name", &self.name())
            .field("crc", &format_args!("{:#010x}", self.crc()))
            .finish()
    }
}

/// A Rapid Tap pair as a slot holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapEntry {
    pub a: u8,
    pub b: u8,
    /// The mode, with `0x80` set to send both keys.
    pub flags: u8,
}

/// A threshold table: `h` (actuate) and `l` (release) for each of the 70 places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds<'a> {
    pub h: &'a [u8],
    pub l: &'a [u8],
}

impl Image {
    /// Check `bytes` and keep them: the size, the profile and device schemas, the CRC.
    pub fn parse(bytes: &[u8]) -> Result<Image> {
        ensure!(
            bytes.len() == IMAGE_BYTES,
            "a slot's image is {IMAGE_BYTES} bytes, not {}",
            bytes.len()
        );
        let schema = u32_at(bytes, at::SCHEMA);
        ensure!(
            schema == PROFILE_SCHEMA,
            "the image has profile schema {schema}, not {PROFILE_SCHEMA}: it's laid out for other firmware"
        );
        let device = u32_at(bytes, at::DEVICE_SCHEMA);
        ensure!(
            device == DEVICE_SCHEMA,
            "the image has device schema {device}, not {DEVICE_SCHEMA}: it's laid out for other firmware"
        );
        let stored = u32_at(bytes, at::CRC);
        let worked_out = crc32_stm32(&bytes[..BODY_BYTES]);
        ensure!(
            stored == worked_out,
            "the image's checksum doesn't match (it says {stored:#010x}, its bytes give {worked_out:#010x})"
        );
        Ok(Image {
            bytes: bytes.to_vec(),
        })
    }

    /// The image as the keyboard stores it, CRC included.
    pub fn serialize(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The CRC stored at the end.
    pub fn crc(&self) -> u32 {
        u32_at(&self.bytes, at::CRC)
    }

    /// The slot's name, up to its first zero byte.
    pub fn name(&self) -> String {
        let raw = &self.bytes[at::NAME..at::NAME + at::NAME_BYTES];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        String::from_utf8_lossy(&raw[..end]).into_owned()
    }

    /// Work the CRC out again after a change.
    fn seal(&mut self) {
        let crc = crc32_stm32(&self.bytes[..BODY_BYTES]);
        self.bytes[at::CRC..].copy_from_slice(&crc.to_le_bytes());
    }

    /// Name the slot: printable ASCII, at most 24 characters.
    pub fn set_name(&mut self, name: &str) -> Result<()> {
        ensure!(
            name.len() <= at::NAME_BYTES,
            "a slot's name is at most {} characters",
            at::NAME_BYTES
        );
        ensure!(
            name.bytes().all(|b| (0x20..0x7F).contains(&b)),
            "a slot's name can only have plain letters, digits, spaces and punctuation"
        );
        let field = &mut self.bytes[at::NAME..at::NAME + at::NAME_BYTES];
        field.fill(0);
        field[..name.len()].copy_from_slice(name.as_bytes());
        self.seal();
        Ok(())
    }

    /// The key that holds the Fn layer, by HID code (the field is 4 bytes).
    pub fn fn_key(&self) -> u32 {
        u32_at(&self.bytes, at::FN_KEY)
    }

    fn places(&self, start: usize) -> &[u8] {
        &self.bytes[start..start + KEY_PLACES]
    }

    /// The primary actuation thresholds, by place.
    pub fn primary(&self) -> Thresholds<'_> {
        Thresholds {
            h: self.places(at::PRIMARY_H),
            l: self.places(at::PRIMARY_L),
        }
    }

    /// The second actuation thresholds, by place; `FF FF` where it's off.
    pub fn second(&self) -> Thresholds<'_> {
        Thresholds {
            h: self.places(at::SECOND_H),
            l: self.places(at::SECOND_L),
        }
    }

    /// The release modes, by place: 0 normal, 2 Rapid Trigger.
    pub fn release_modes(&self) -> &[u8] {
        self.places(at::RELEASE)
    }

    /// Rapid Trigger's distance, by place, in tenths of a millimetre.
    pub fn sensitivity(&self) -> &[u8] {
        self.places(at::SENSITIVITY)
    }

    /// Protection Mode: how long, in milliseconds, and each place's distance. Read only.
    pub fn protection(&self) -> (u32, &[u8]) {
        (
            u32_at(&self.bytes, at::PROTECTION_MS),
            self.places(at::PROTECTION_DISTANCE),
        )
    }

    /// The screen picture, packed as `1F 81` takes it.
    pub fn oled(&self) -> &[u8] {
        &self.bytes[at::OLED..at::OLED + PACKED]
    }

    /// Rapid Tap: whether it's on, and the pairs (empty entries left out).
    pub fn rapid_tap(&self) -> (bool, Vec<TapEntry>) {
        let entries = (0..TAP_ENTRIES)
            .map(|i| {
                let e = &self.bytes[at::RAPID_TAP + i * TAP_ENTRY..][..TAP_ENTRY];
                TapEntry {
                    a: e[0],
                    b: e[1],
                    flags: e[2],
                }
            })
            .filter(|e| e.a != 0 || e.b != 0)
            .collect();
        (self.bytes[at::RAPID_TAP_ENABLE] != 0, entries)
    }

    /// The macro events, opaque.
    pub fn macros(&self) -> &[u8] {
        &self.bytes[at::MACROS..at::MACROS + at::MACRO_BYTES]
    }

    pub fn guid(&self) -> &[u8] {
        &self.bytes[at::GUID..at::CRC]
    }

    /// Put `picture` in as the slot's screen picture.
    pub fn set_oled(&mut self, picture: &Bitmap) {
        self.bytes[at::OLED..at::OLED + PACKED].copy_from_slice(&picture.packed());
        self.seal();
    }

    /// Put Rapid Tap in: on or off, and up to 10 pairs; the entries after them are
    /// cleared. Refused, with nothing changed, as `38 67` would be.
    pub fn set_rapid_tap(&mut self, enabled: bool, pairs: &[TapPair]) -> Result<()> {
        // The same checks as the RAM command's.
        proto::rapid_tap_pairs(pairs)?;
        let table = &mut self.bytes[at::RAPID_TAP..at::RAPID_TAP_ENABLE];
        table.fill(0);
        for (entry, p) in table.chunks_exact_mut(TAP_ENTRY).zip(pairs) {
            let flags = p.mode as u8 | if p.report_both { 0x80 } else { 0 };
            entry[..3].copy_from_slice(&[p.a, p.b, flags]);
        }
        self.bytes[at::RAPID_TAP_ENABLE] = u8::from(enabled);
        self.seal();
        Ok(())
    }

    /// Put the primary actuation points, release modes and Rapid Trigger distances in, for
    /// each place `table` names an adjustable key for. The others stay as they are.
    pub fn set_switches(&mut self, table: &KeyTable, switches: &Switches<'_>) {
        for place in 0..KEY_PLACES {
            let code = table.code(place);
            if code == 0 {
                continue;
            }
            let (h, l) = proto::thresholds((switches.actuation)(code));
            self.bytes[at::PRIMARY_H + place] = h;
            self.bytes[at::PRIMARY_L + place] = l;
            self.bytes[at::RELEASE + place] = if (switches.rapid_trigger)(code) { 2 } else { 0 };
            self.bytes[at::SENSITIVITY + place] = (switches.sensitivity)(code);
        }
        self.seal();
    }
}

/// Which key each of an image's 70 per-key places is for: the firmware's own key order
/// (`deviceKeyIndexToHID` in Apex Control's code). The protocol reference doesn't list it,
/// so gglinux doesn't have it yet, and a slot write leaves those tables as the slot has
/// them until a capture or the published table gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyTable {
    codes: [u8; KEY_PLACES],
}

impl KeyTable {
    /// A table from each place's HID code (0 for a place with no adjustable key). Each
    /// code must be an adjustable key, and none may come twice.
    pub fn new(codes: [u8; KEY_PLACES]) -> Result<KeyTable> {
        for (place, &code) in codes.iter().enumerate().filter(|(_, &c)| c != 0) {
            ensure!(
                ANALOG_KEYS.contains(&code),
                "key {code:#04x} has no adjustable switch"
            );
            ensure!(
                !codes[..place].contains(&code),
                "key {code:#04x} has two places"
            );
        }
        Ok(KeyTable { codes })
    }

    /// The HID code at `place`; 0 for none.
    pub fn code(&self, place: usize) -> u8 {
        self.codes.get(place).copied().unwrap_or(0)
    }
}

/// The file a slot lives in, `0x80 + slot`.
pub fn file(slot: u8) -> Result<u8> {
    ensure!(
        slot < SLOTS,
        "the keyboard has slots 1 to {SLOTS}, not {}",
        u16::from(slot) + 1
    );
    Ok(0x80 + slot)
}

/// Check a chunk's place in the image, as the allowlist does: 1 to 500 bytes, all inside.
fn check_chunk(offset: u32, size: usize) -> Result<()> {
    ensure!(
        (1..=CHUNK_BYTES).contains(&size),
        "a chunk is 1 to {CHUNK_BYTES} bytes, not {size}"
    );
    ensure!(
        u64::from(offset) + size as u64 <= IMAGE_BYTES as u64,
        "bytes {offset} to {} run past the {IMAGE_BYTES}-byte image",
        u64::from(offset) + size as u64
    );
    Ok(())
}

/// The header of `03 01` and `83 01`, in a feature report.
fn file_report(op: u8, slot: u8, offset: u32, size: usize) -> Result<Vec<u8>> {
    let file = file(slot)?;
    check_chunk(offset, size)?;
    let mut report = vec![0u8; FEATURE_REPORT];
    report[1..4].copy_from_slice(&[op, FILE_SUB, file]);
    report[4..6].copy_from_slice(&(size as u16).to_le_bytes());
    report[6..10].copy_from_slice(&offset.to_le_bytes());
    Ok(report)
}

/// An output report for a slot command: `op`, then `args`.
fn slot_command(op: u8, args: &[u8]) -> Vec<u8> {
    let mut report = vec![0u8; COMMAND_REPORT];
    report[1] = op;
    report[2..2 + args.len()].copy_from_slice(args);
    report
}

/// Where each chunk of an image starts and how long it is: 24 of 500 bytes and the last
/// of 324, so no chunk runs past the image.
pub fn chunks() -> impl Iterator<Item = (u32, usize)> {
    (0..IMAGE_BYTES)
        .step_by(CHUNK_BYTES)
        .map(|offset| (offset as u32, CHUNK_BYTES.min(IMAGE_BYTES - offset)))
}

/// Erase a slot: output report `02 01 <file>`. The keyboard answers when it's done.
pub fn erase_report(slot: u8) -> Result<Vec<u8>> {
    Ok(slot_command(OP_ERASE, &[FILE_SUB, file(slot)?]))
}

/// Write one chunk: feature report `03 01 <file> <size> <offset> <data>`. A chunk shorter
/// than 500 bytes is padded to 500 with `0xFF` after its data, as the reference pads its
/// last one; `size` still says how many of the bytes are the image's.
pub fn write_chunk_report(slot: u8, offset: u32, data: &[u8]) -> Result<Vec<u8>> {
    let mut report = file_report(OP_WRITE_CHUNK, slot, offset, data.len())?;
    report[FILE_HEADER..FILE_HEADER + data.len()].copy_from_slice(data);
    report[FILE_HEADER + data.len()..FILE_HEADER + CHUNK_BYTES].fill(0xFF);
    Ok(report)
}

/// Ask for one chunk back: feature report `83 01 <file> <size> <offset>`, then read the
/// feature report.
pub fn read_chunk_report(slot: u8, offset: u32, size: usize) -> Result<Vec<u8>> {
    file_report(OP_READ_CHUNK, slot, offset, size)
}

/// Have the keyboard check a slot: output report `B1 <slot>`.
pub fn validate_report(slot: u8) -> Result<Vec<u8>> {
    file(slot)?;
    Ok(slot_command(OP_VALIDATE_SLOT, &[slot]))
}

/// Make the keyboard run a slot: output report `B2 <slot>`. No answer comes.
pub fn load_report(slot: u8) -> Result<Vec<u8>> {
    file(slot)?;
    Ok(slot_command(OP_LOAD_SLOT, &[slot]))
}

/// A chunk's data from the reply to `83 01` (the bytes after the report id): `83 <err>
/// <data>`.
pub fn chunk_data(reply: &[u8], size: usize) -> Result<&[u8]> {
    match reply {
        [OP_READ_CHUNK, 0x00, data @ ..] if data.len() >= size => Ok(&data[..size]),
        [OP_READ_CHUNK, 0x00, ..] => bail!(
            "the keyboard sent {} of the chunk's {size} bytes",
            reply.len() - 2
        ),
        [OP_READ_CHUNK, err, ..] => bail!("the keyboard refused the read (error {err:#04x})"),
        [op, ..] => bail!("the keyboard answered with command {op:#04x}'s reply, not the chunk"),
        [] => bail!("the keyboard sent nothing back"),
    }
}

/// How long to wait in a write. [`Timing::KEYBOARD`] is the reference's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Between two chunk writes.
    pub chunk_gap: Duration,
    /// For the erase's answer.
    pub erase_ms: i32,
    /// For the check's answer.
    pub reply_ms: i32,
}

impl Timing {
    /// About 13 ms between chunks; up to 6 s for the erase.
    pub const KEYBOARD: Timing = Timing {
        chunk_gap: Duration::from_millis(13),
        erase_ms: 6_000,
        reply_ms: 1_000,
    };
}

/// A step of a slot write, for saying where one failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Read,
    Backup,
    Build,
    Erase,
    /// Writing chunk n, from 1.
    Chunk(usize),
    Validate,
    ReadBack,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::Read => write!(f, "reading the slot"),
            Step::Backup => write!(f, "making the backup"),
            Step::Build => write!(f, "making the new image"),
            Step::Erase => write!(f, "erasing the slot"),
            Step::Chunk(n) => write!(f, "writing part {n} of {CHUNKS}"),
            Step::Validate => write!(f, "the keyboard's own check of the slot"),
            Step::ReadBack => write!(f, "reading the slot back"),
        }
    }
}

/// What happened to the slot after a write failed.
#[derive(Debug)]
pub enum Restore {
    /// It failed before the erase: the slot was never touched.
    Untouched,
    /// The old image was written back and read back the same.
    Restored,
    /// Writing the old image back failed too.
    Failed(anyhow::Error),
}

/// A slot write that didn't go through: where, why, and what was done about it.
#[derive(Debug)]
pub struct WriteError {
    pub slot: u8,
    pub step: Step,
    pub error: anyhow::Error,
    pub restore: Restore,
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "slot {}: {} failed: {:#}",
            u16::from(self.slot) + 1,
            self.step,
            self.error
        )?;
        match &self.restore {
            Restore::Untouched => write!(f, "; the slot wasn't changed"),
            Restore::Restored => write!(
                f,
                "; the slot as it was before was written back, and it reads back the same"
            ),
            Restore::Failed(e) => write!(
                f,
                "; writing the slot as it was back failed too ({e:#}): restore it from its backup file"
            ),
        }
    }
}

impl std::error::Error for WriteError {}

/// What a slot write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    /// The new image was the slot's already: nothing was erased or written.
    Unchanged,
    /// Written, checked by the keyboard and read back the same.
    Written,
}

impl Keyboard {
    /// Read a slot's whole image, chunk by chunk, unchecked.
    pub fn read_slot(&self, slot: u8) -> Result<Vec<u8>> {
        let mut image = Vec::with_capacity(IMAGE_BYTES);
        for (i, (offset, size)) in chunks().enumerate() {
            let reply = self.exchange_feature(&read_chunk_report(slot, offset, size)?)?;
            let data =
                chunk_data(&reply, size).with_context(|| format!("part {} of {CHUNKS}", i + 1))?;
            image.extend_from_slice(data);
        }
        Ok(image)
    }

    /// Read a slot and check it.
    pub fn read_image(&self, slot: u8) -> Result<Image> {
        let bytes = self.read_slot(slot)?;
        Image::parse(&bytes)
            .with_context(|| format!("slot {} didn't read as a whole image", u16::from(slot) + 1))
    }

    /// Make the keyboard run `slot`.
    pub fn load_slot(&self, slot: u8) -> Result<()> {
        self.output(&load_report(slot)?)
    }

    /// Write a slot as one checked transaction (see the module docs). `backup` saves the
    /// slot as read and returns what the saved file holds; `change` makes the new image
    /// from the old one.
    pub fn write_slot(
        &self,
        slot: u8,
        timing: &Timing,
        backup: impl FnOnce(&Image) -> Result<Vec<u8>>,
        change: impl FnOnce(&Image) -> Result<Image>,
    ) -> Result<Written, WriteError> {
        let untouched = |step, error| WriteError {
            slot,
            step,
            error,
            restore: Restore::Untouched,
        };
        let old = self
            .read_image(slot)
            .map_err(|e| untouched(Step::Read, e))?;
        let saved = backup(&old).map_err(|e| untouched(Step::Backup, e))?;
        if saved != old.bytes() {
            return Err(untouched(
                Step::Backup,
                anyhow!("the backup file doesn't hold the slot as it was read"),
            ));
        }
        let new = change(&old)
            .and_then(|image| Image::parse(&image.serialize()))
            .map_err(|e| untouched(Step::Build, e))?;
        if new == old {
            return Ok(Written::Unchanged);
        }
        // From the erase on, the slot is the keyboard's only copy no more: any failure
        // writes the old image back the same way.
        if let Err((step, error)) = self.flash(slot, &new, timing) {
            let restore = match self.flash(slot, &old, timing) {
                Ok(()) => Restore::Restored,
                Err((again, e)) => Restore::Failed(e.context(again.to_string())),
            };
            return Err(WriteError {
                slot,
                step,
                error,
                restore,
            });
        }
        Ok(Written::Written)
    }

    /// Erase `slot`, write `image` into it, have the keyboard check it, and read it back.
    fn flash(&self, slot: u8, image: &Image, timing: &Timing) -> Result<(), (Step, anyhow::Error)> {
        let at = |step: Step| move |e: anyhow::Error| (step, e);
        let erase = erase_report(slot).map_err(at(Step::Erase))?;
        // The answer comes when the erase is done. What it holds isn't documented, so
        // only its arrival counts; the read-back is the real check.
        match self.exchange_output(&erase, timing.erase_ms) {
            Ok(Some(reply)) if reply.first() == Some(&OP_ERASE) => {}
            Ok(Some(reply)) => {
                return Err((
                    Step::Erase,
                    anyhow!(
                        "the keyboard answered the erase with command {:#04x}'s reply",
                        reply[0]
                    ),
                ))
            }
            Ok(None) => {
                return Err((
                    Step::Erase,
                    anyhow!("no answer within {} s", timing.erase_ms / 1000),
                ))
            }
            Err(e) => return Err((Step::Erase, e)),
        }
        let bytes = image.bytes();
        for (i, (offset, size)) in chunks().enumerate() {
            if i > 0 {
                std::thread::sleep(timing.chunk_gap);
            }
            let step = Step::Chunk(i + 1);
            let data = &bytes[offset as usize..offset as usize + size];
            let report = write_chunk_report(slot, offset, data).map_err(at(step))?;
            self.feature(&report).map_err(at(step))?;
        }
        let check = validate_report(slot).map_err(at(Step::Validate))?;
        match self.exchange_output(&check, timing.reply_ms) {
            Ok(Some(reply)) if reply.get(..2) == Some(&[OP_VALIDATE_SLOT, 0x00][..]) => {}
            Ok(Some(reply)) if reply[0] == OP_VALIDATE_SLOT => {
                return Err((
                    Step::Validate,
                    anyhow!(
                        "the keyboard found the slot bad (it answered {:#04x})",
                        reply.get(1).copied().unwrap_or(0)
                    ),
                ))
            }
            Ok(Some(reply)) => {
                return Err((
                    Step::Validate,
                    anyhow!(
                        "the keyboard answered with command {:#04x}'s reply",
                        reply[0]
                    ),
                ))
            }
            Ok(None) => return Err((Step::Validate, anyhow!("no answer"))),
            Err(e) => return Err((Step::Validate, e)),
        }
        // The first read after a change may still serve the slot as it was: read again
        // before calling a difference real.
        let mut back = self.read_slot(slot).map_err(at(Step::ReadBack))?;
        if back != bytes {
            back = self.read_slot(slot).map_err(at(Step::ReadBack))?;
        }
        if back != bytes {
            let first = back
                .iter()
                .zip(bytes)
                .position(|(a, b)| a != b)
                .unwrap_or(back.len().min(bytes.len()));
            return Err((
                Step::ReadBack,
                anyhow!(
                    "the slot reads back differently from what was written, from byte {first} on"
                ),
            ));
        }
        Ok(())
    }
}

/// A keyboard whose five slots are kept in memory, for tests: [`fake::FakeSlots`].
#[cfg(any(test, feature = "testing"))]
pub mod fake {
    use std::sync::{Arc, Mutex, MutexGuard};

    use anyhow::{ensure, Result};

    use super::{at, crc32_stm32, file, Image, BODY_BYTES, IMAGE_BYTES, KEY_PLACES, SLOTS};
    use crate::allow::{
        FILE_SUB, OP_ERASE, OP_LOAD_SLOT, OP_READ_CHUNK, OP_VALIDATE_SLOT, OP_WRITE_CHUNK,
    };
    use crate::proto::{self, FEATURE_REPORT, INPUT_REPORT};
    use crate::testing::FakeTransport;
    use crate::{Keyboard, Transport};

    fn put32(b: &mut [u8], at: usize, v: u32) {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// A made-up image that checks out, named `name`: factory-like values where the
    /// reference gives them (Fn on `0xF0`, 1.5 mm, distance 2, zone levels 4, protection
    /// 500 ms and 20) and a fixed pattern in the parts gglinux keeps as they are, so a test
    /// can tell they came through. Not a real factory image.
    pub fn synthesized(name: &str) -> Image {
        let mut b = vec![0u8; IMAGE_BYTES];
        put32(&mut b, at::SCHEMA, super::PROFILE_SCHEMA);
        let n = &name.as_bytes()[..name.len().min(at::NAME_BYTES)];
        b[at::NAME..at::NAME + n.len()].copy_from_slice(n);
        put32(&mut b, at::FN_KEY, 0xF0);
        put32(&mut b, at::DEVICE_SCHEMA, super::DEVICE_SCHEMA);
        let (h, l) = proto::thresholds(15);
        b[at::PRIMARY_H..at::PRIMARY_H + KEY_PLACES].fill(h);
        b[at::PRIMARY_L..at::PRIMARY_L + KEY_PLACES].fill(l);
        for i in 0..3 {
            put32(&mut b, at::ZONES + i * 4, 4);
        }
        b[at::SECOND_H..at::RELEASE].fill(0xFF);
        b[at::SENSITIVITY..at::SENSITIVITY + KEY_PLACES].fill(2);
        put32(&mut b, at::PROTECTION_MS, 500);
        b[at::PROTECTION_DISTANCE..at::PROTECTION_DISTANCE + KEY_PLACES].fill(20);
        // The kept parts: a pattern that differs from byte to byte and from slot to slot.
        let mut x = n
            .iter()
            .fold(0x9E37_79B9u32, |x, &c| x.rotate_left(5) ^ u32::from(c));
        let mut next = || {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        };
        for range in [
            at::LIGHTING..at::FN_KEY,
            at::NORMAL_BINDINGS..at::DEVICE_SCHEMA,
            at::ZONES + 12..at::SECOND_H,
            at::SECOND_BINDINGS..at::OLED,
            at::MACROS..at::CRC,
        ] {
            for byte in &mut b[range] {
                *byte = next();
            }
        }
        let crc = crc32_stm32(&b[..BODY_BYTES]);
        b[at::CRC..].copy_from_slice(&crc.to_le_bytes());
        Image { bytes: b }
    }

    #[derive(Default)]
    struct Flash {
        slots: Vec<Vec<u8>>,
        /// What a slot held before it last changed, until it's next read: the first read
        /// after a change gets that, as the keyboard serves the state from before a
        /// command on the first read after it.
        before: Vec<Option<Vec<u8>>>,
        /// The reply to the last `83 01`, held for feature reads.
        read: Option<Vec<u8>>,
        /// The input report owed for the last output report.
        answer: Option<Vec<u8>>,
        loaded: Option<u8>,
        /// The next chunk write over this byte lands wrong (once).
        corrupt_at: Option<usize>,
        /// Reads of this byte come back wrong this many times, from the next erase on.
        corrupt_reads: Option<(usize, u32)>,
        corrupt_armed: bool,
        /// The next `B1` says the slot is bad (once).
        refuse_check: bool,
        /// The next output report gets no answer (once).
        drop_answer: bool,
    }

    fn size_offset(report: &[u8]) -> (usize, usize) {
        let size = usize::from(u16::from_le_bytes([report[4], report[5]]));
        let offset = u32::from_le_bytes([report[6], report[7], report[8], report[9]]) as usize;
        (size, offset)
    }

    impl Flash {
        fn changing(&mut self, slot: usize) {
            if self.before[slot].is_none() {
                self.before[slot] = Some(self.slots[slot].clone());
            }
        }

        fn erase(&mut self, slot: usize) {
            self.changing(slot);
            self.corrupt_armed = self.corrupt_reads.is_some();
            self.slots[slot].fill(0xFF);
        }

        /// Flash only clears bits: a write over bytes that weren't erased mixes them.
        fn write_chunk(&mut self, slot: usize, report: &[u8]) {
            let (size, offset) = size_offset(report);
            self.changing(slot);
            for i in 0..size {
                let at = offset + i;
                let mut byte = report[10 + i];
                if self.corrupt_at == Some(at) {
                    self.corrupt_at = None;
                    byte ^= 0x5A;
                }
                self.slots[slot][at] &= byte;
            }
        }

        fn read_chunk(&mut self, slot: usize, report: &[u8]) -> Vec<u8> {
            let (size, offset) = size_offset(report);
            let image = self.before[slot]
                .take()
                .unwrap_or_else(|| self.slots[slot].clone());
            let mut reply = vec![OP_READ_CHUNK, 0x00];
            reply.extend_from_slice(&image[offset..offset + size]);
            if let Some((at, times)) = self.corrupt_reads.filter(|_| self.corrupt_armed) {
                if (offset..offset + size).contains(&at) {
                    reply[2 + at - offset] ^= 0x5A;
                    self.corrupt_reads = (times > 1).then_some((at, times - 1));
                }
            }
            reply
        }
    }

    /// Five slots in memory behind a [`FakeTransport`]: every report goes through the
    /// wrapped fake first (so [`FakeTransport::fail_send`] and [`FakeTransport::sent`]
    /// work as there), then the slot commands act on the slots. Like the keyboard, the
    /// first read of a slot after it changed still gets what it held before. It checks
    /// nothing: the allowlist is the [`Keyboard`]'s.
    #[derive(Clone)]
    pub struct FakeSlots {
        wire: FakeTransport,
        flash: Arc<Mutex<Flash>>,
    }

    impl Default for FakeSlots {
        fn default() -> Self {
            FakeSlots::new()
        }
    }

    impl FakeSlots {
        /// Five slots named "Config 1" to "Config 5", each a [`synthesized`] image.
        pub fn new() -> FakeSlots {
            let slots: Vec<Vec<u8>> = (1..=SLOTS)
                .map(|n| synthesized(&format!("Config {n}")).serialize())
                .collect();
            FakeSlots {
                wire: FakeTransport::new(),
                flash: Arc::new(Mutex::new(Flash {
                    before: vec![None; slots.len()],
                    slots,
                    ..Flash::default()
                })),
            }
        }

        /// A keyboard on these slots, every gated family off.
        pub fn keyboard(&self) -> Keyboard {
            Keyboard::with_transport(Box::new(self.clone()))
        }

        /// The fake every report passes through.
        pub fn wire(&self) -> &FakeTransport {
            &self.wire
        }

        /// What a slot holds now.
        pub fn slot(&self, slot: u8) -> Vec<u8> {
            self.lock().slots[usize::from(slot)].clone()
        }

        pub fn set_slot(&self, slot: u8, bytes: &[u8]) {
            assert_eq!(bytes.len(), IMAGE_BYTES);
            self.lock().slots[usize::from(slot)] = bytes.to_vec();
        }

        /// The slot `B2` last asked for.
        pub fn loaded(&self) -> Option<u8> {
            self.lock().loaded
        }

        /// The next chunk written over image byte `at` lands wrong, once.
        pub fn corrupt_next_write_at(&self, at: usize) {
            self.lock().corrupt_at = Some(at);
        }

        /// From the next erase on, the next `times` reads of image byte `at` come back
        /// wrong, though the slot holds it right.
        pub fn corrupt_reads_after_erase(&self, at: usize, times: u32) {
            self.lock().corrupt_reads = Some((at, times));
        }

        /// The keyboard's next check of a slot says it's bad, once.
        pub fn refuse_next_check(&self) {
            self.lock().refuse_check = true;
        }

        /// The next output report gets no answer, once (an erase that times out).
        pub fn drop_next_answer(&self) {
            self.lock().drop_answer = true;
        }

        fn lock(&self) -> MutexGuard<'_, Flash> {
            self.flash.lock().unwrap_or_else(|e| e.into_inner())
        }
    }

    /// The slot a file command names, if it's one of the five.
    fn slot_of(report: &[u8]) -> Option<usize> {
        if report.get(2) != Some(&FILE_SUB) {
            return None;
        }
        let f = *report.get(3)?;
        (0..SLOTS)
            .find(|&s| file(s).ok() == Some(f))
            .map(usize::from)
    }

    /// Copy `reply` into `buf` from `at` and zero the rest of a `size`-byte report.
    fn fill(buf: &mut [u8], at: usize, reply: &[u8], size: usize) -> usize {
        let len = size.min(buf.len());
        buf[at..len].fill(0);
        let n = reply.len().min(len - at);
        buf[at..at + n].copy_from_slice(&reply[..n]);
        len
    }

    impl Transport for FakeSlots {
        fn send_feature(&self, report: &[u8]) -> Result<()> {
            self.wire.send_feature(report)?;
            let mut f = self.lock();
            f.read = None;
            match (report.get(1).copied(), slot_of(report)) {
                (Some(OP_WRITE_CHUNK), Some(slot)) => f.write_chunk(slot, report),
                (Some(OP_READ_CHUNK), Some(slot)) => f.read = Some(f.read_chunk(slot, report)),
                _ => {}
            }
            Ok(())
        }

        fn get_feature(&self, buf: &mut [u8]) -> Result<usize> {
            let read = self.lock().read.clone();
            match read {
                Some(reply) => {
                    ensure!(
                        buf.first() == Some(&0),
                        "a feature read needs report id 0 in the buffer's first byte"
                    );
                    Ok(fill(buf, 1, &reply, FEATURE_REPORT))
                }
                None => self.wire.get_feature(buf),
            }
        }

        fn write(&self, report: &[u8]) -> Result<()> {
            self.wire.write(report)?;
            let mut f = self.lock();
            let answer = match (report.get(1).copied(), report.get(2).copied()) {
                (Some(OP_ERASE), _) => slot_of(report).map(|slot| {
                    f.erase(slot);
                    vec![OP_ERASE, 0x00]
                }),
                (Some(OP_VALIDATE_SLOT), Some(slot)) if slot < SLOTS => {
                    let good = Image::parse(&f.slots[usize::from(slot)]).is_ok()
                        && !std::mem::take(&mut f.refuse_check);
                    Some(vec![OP_VALIDATE_SLOT, u8::from(!good)])
                }
                (Some(OP_LOAD_SLOT), Some(slot)) => {
                    f.loaded = Some(slot);
                    None
                }
                _ => None,
            };
            f.answer = if std::mem::take(&mut f.drop_answer) {
                None
            } else {
                answer
            };
            Ok(())
        }

        fn read_timeout(&self, buf: &mut [u8], ms: i32) -> Result<usize> {
            let answer = self.lock().answer.take();
            match answer {
                Some(reply) => Ok(fill(buf, 0, &reply, INPUT_REPORT)),
                None => self.wire.read_timeout(buf, ms),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{synthesized, FakeSlots};
    use super::*;
    use crate::testing::{Kind, Sent};
    use crate::{Families, TapMode};

    const ONBOARD: Families = Families {
        bindings: false,
        onboard: true,
    };
    const QUICK: Timing = Timing {
        chunk_gap: Duration::ZERO,
        erase_ms: 10,
        reply_ms: 10,
    };

    /// CRC-32/MPEG-2: the same register fed byte by byte, in order.
    fn mpeg2(bytes: &[u8]) -> u32 {
        bytes.iter().fold(0xFFFF_FFFF, |crc, &b| crc_byte(crc, b))
    }

    #[test]
    fn the_crc_is_the_stm32_units() {
        // The catalogue check value of CRC-32/MPEG-2, the byte-wise form of the same CRC.
        assert_eq!(mpeg2(b"123456789"), 0x0376_E6E7);
        // Word-wise, little-endian: each word goes in last byte first.
        assert_eq!(crc32_stm32(b"4321"), mpeg2(b"1234"));
        assert_eq!(crc32_stm32(b"43218765"), mpeg2(b"12345678"));
        // The usual STM32 example: the word 0x12345678 gives 0xDF8A8A2B.
        assert_eq!(crc32_stm32(&0x1234_5678u32.to_le_bytes()), 0xDF8A_8A2B);
        assert_eq!(crc32_stm32(&[]), 0xFFFF_FFFF, "nothing fed: the init value");
    }

    #[test]
    fn an_image_round_trips_byte_for_byte() {
        let made = synthesized("Config 3");
        let bytes = made.serialize();
        assert_eq!(bytes.len(), IMAGE_BYTES);
        let parsed = Image::parse(&bytes).unwrap();
        assert_eq!(parsed.serialize(), bytes);
        assert_eq!(parsed, made);
        assert_eq!(parsed.crc(), crc32_stm32(&bytes[..BODY_BYTES]));
        assert_eq!(parsed.name(), "Config 3");
        assert_eq!(parsed.fn_key(), 0xF0);
        let (h, l) = proto::thresholds(15);
        assert_eq!(parsed.primary().h, &[h; 70][..]);
        assert_eq!(parsed.primary().l, &[l; 70][..]);
        assert_eq!(parsed.second().h, &[0xFF; 70][..]);
        assert_eq!(parsed.second().l, &[0xFF; 70][..]);
        assert_eq!(parsed.release_modes(), &[0u8; 70][..]);
        assert_eq!(parsed.sensitivity(), &[2u8; 70][..]);
        assert_eq!(parsed.protection(), (500, &[20u8; 70][..]));
        assert_eq!(parsed.oled(), &[0u8; 640][..]);
        assert_eq!(parsed.rapid_tap(), (false, vec![]));
        assert_eq!(parsed.macros(), &bytes[2704..12304]);
        assert_eq!(parsed.guid(), &bytes[12304..12320]);
        // The kept parts aren't blank, so a test that they came through means something.
        assert!(parsed.macros().iter().filter(|&&b| b != 0).count() > 9_000);
        assert_ne!(synthesized("Config 4").macros(), parsed.macros());
    }

    #[test]
    fn a_bad_image_is_refused_with_its_reason() {
        let good = synthesized("Config 1").serialize();
        let why = |bytes: &[u8]| format!("{:#}", Image::parse(bytes).unwrap_err());
        assert!(why(&good[..IMAGE_BYTES - 1]).contains("12324 bytes"));
        let mut long = good.clone();
        long.push(0xFF);
        assert!(why(&long).contains("12324 bytes"));
        let mut crc = good.clone();
        crc[100] ^= 1;
        assert!(why(&crc).contains("checksum"), "{}", why(&crc));
        // A schema the layout isn't for, even with a CRC that matches.
        for (at, word) in [
            (at::SCHEMA, "profile schema"),
            (at::DEVICE_SCHEMA, "device schema"),
        ] {
            let mut other = good.clone();
            other[at] = 4;
            let c = crc32_stm32(&other[..BODY_BYTES]);
            other[at::CRC..].copy_from_slice(&c.to_le_bytes());
            assert!(why(&other).contains(word), "{}", why(&other));
        }
        let mut blank = vec![0xFFu8; IMAGE_BYTES];
        assert!(Image::parse(&blank).is_err(), "an erased slot");
        blank.fill(0);
        assert!(Image::parse(&blank).is_err(), "zeros");
    }

    /// Every body byte outside the `(start, end)` spans is the same in both, and `after`
    /// checks out.
    fn only_changed(before: &Image, after: &Image, spans: &[(usize, usize)]) {
        for (i, (a, b)) in before.bytes()[..BODY_BYTES]
            .iter()
            .zip(&after.bytes()[..BODY_BYTES])
            .enumerate()
        {
            if !spans.iter().any(|&(start, end)| (start..end).contains(&i)) {
                assert_eq!(a, b, "byte {i} changed");
            }
        }
        assert_eq!(after.crc(), crc32_stm32(&after.bytes()[..BODY_BYTES]));
        Image::parse(after.bytes()).unwrap();
    }

    #[test]
    fn the_name_goes_in_and_nothing_else_moves() {
        let old = synthesized("Config 5");
        let mut new = old.clone();
        new.set_name("Games").unwrap();
        assert_eq!(new.name(), "Games");
        assert_eq!(
            &new.bytes()[4..28],
            b"Games\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0"
        );
        only_changed(&old, &new, &[(4, 28)]);
        assert_ne!(new.crc(), old.crc());
        let mut same = old.clone();
        assert!(same.set_name("twenty-five characters!!!").is_err());
        assert!(same.set_name("tab\there").is_err());
        assert!(same.set_name("café").is_err());
        assert_eq!(same, old);
        same.set_name("exactly twenty-four char").unwrap();
        assert_eq!(same.name(), "exactly twenty-four char");
    }

    #[test]
    fn the_screen_picture_goes_in_and_nothing_else_moves() {
        let old = synthesized("Config 2");
        let mut new = old.clone();
        let mut pic = Bitmap::default();
        pic.set(0, 0, true);
        pic.set(127, 39, true);
        new.set_oled(&pic);
        assert_eq!(new.oled(), &pic.packed()[..]);
        assert_eq!(new.oled()[0], 0x01, "column 0, page 0, top row: bit 0");
        assert_eq!(
            new.oled()[4 * 128 + 127],
            0x80,
            "column 127, last page, bottom row"
        );
        only_changed(&old, &new, &[(at::OLED, at::OLED + PACKED)]);
        assert_ne!(new.crc(), old.crc());
    }

    #[test]
    fn rapid_tap_goes_in_as_38_67_would_send_it() {
        let old = synthesized("Config 2");
        let mut new = old.clone();
        let pairs = [
            TapPair {
                a: 0x04,
                b: 0x07,
                mode: TapMode::LastWins,
                report_both: false,
            },
            TapPair {
                a: 0x1A,
                b: 0x16,
                mode: TapMode::SecondWins,
                report_both: true,
            },
        ];
        new.set_rapid_tap(true, &pairs).unwrap();
        let tap = &new.bytes()[at::RAPID_TAP..at::RAPID_TAP + 51];
        assert_eq!(tap[..10], [0x04, 0x07, 0x00, 0, 0, 0x1A, 0x16, 0x83, 0, 0]);
        assert!(
            tap[10..50].iter().all(|&b| b == 0),
            "the other eight cleared"
        );
        assert_eq!(tap[50], 1, "on");
        assert_eq!(
            new.rapid_tap(),
            (
                true,
                vec![
                    TapEntry {
                        a: 4,
                        b: 7,
                        flags: 0
                    },
                    TapEntry {
                        a: 0x1A,
                        b: 0x16,
                        flags: 0x83
                    }
                ]
            )
        );
        only_changed(&old, &new, &[(at::RAPID_TAP, at::RAPID_TAP + 51)]);
        new.set_rapid_tap(false, &[]).unwrap();
        assert_eq!(new.rapid_tap(), (false, vec![]));

        // Refused as the RAM command refuses them, and nothing changes.
        let mut same = old.clone();
        let fixed = TapPair {
            a: 0x29,
            b: 0x07,
            mode: TapMode::LastWins,
            report_both: false,
        };
        assert!(
            same.set_rapid_tap(true, &[fixed]).is_err(),
            "Escape has a fixed switch"
        );
        assert!(
            same.set_rapid_tap(true, &[pairs[0]; 11]).is_err(),
            "eleven pairs"
        );
        assert_eq!(same, old);
    }

    #[test]
    fn switches_go_in_only_where_the_key_table_names_a_key() {
        let mut codes = [0u8; KEY_PLACES];
        codes[0] = 0x04; // A
        codes[5] = 0x2C; // Space
        let table = KeyTable::new(codes).unwrap();
        assert_eq!((table.code(5), table.code(6), table.code(99)), (0x2C, 0, 0));
        let old = synthesized("Config 4");
        let mut new = old.clone();
        let switches = Switches {
            actuation: &|code| if code == 0x04 { 1 } else { 40 },
            rapid_trigger: &|code| code == 0x2C,
            sensitivity: &|_| 5,
            rapid_tap: false,
            tap_pairs: &[],
        };
        new.set_switches(&table, &switches);
        let (h1, l1) = proto::thresholds(1);
        let (h40, l40) = proto::thresholds(40);
        assert_eq!((new.primary().h[0], new.primary().l[0]), (h1, l1));
        assert_eq!((new.primary().h[5], new.primary().l[5]), (h40, l40));
        assert_eq!((new.release_modes()[0], new.release_modes()[5]), (0, 2));
        assert_eq!((new.sensitivity()[0], new.sensitivity()[5]), (5, 5));
        for p in (0..KEY_PLACES).filter(|p| ![0, 5].contains(p)) {
            assert_eq!(new.primary().h[p], old.primary().h[p], "place {p}");
            assert_eq!(new.primary().l[p], old.primary().l[p], "place {p}");
            assert_eq!(new.release_modes()[p], old.release_modes()[p]);
            assert_eq!(new.sensitivity()[p], old.sensitivity()[p]);
        }
        only_changed(
            &old,
            &new,
            &[(at::PRIMARY_H, at::ZONES), (at::RELEASE, at::PROTECTION_MS)],
        );
        // The second actuation point and the zones aren't the switches'.
        assert_eq!(new.second(), old.second());
        assert_eq!(
            new.bytes()[at::ZONES..at::SECOND_H],
            old.bytes()[at::ZONES..at::SECOND_H]
        );

        assert!(
            KeyTable::new([0x29; KEY_PLACES]).is_err(),
            "Escape isn't adjustable"
        );
        let mut twice = [0u8; KEY_PLACES];
        twice[1] = 0x04;
        twice[2] = 0x04;
        assert!(KeyTable::new(twice).is_err());
    }

    #[test]
    fn the_chunks_cover_the_image_exactly_and_the_last_is_324_bytes() {
        let all: Vec<(u32, usize)> = chunks().collect();
        assert_eq!(all.len(), CHUNKS);
        assert_eq!(CHUNKS, 25);
        for (i, &(offset, size)) in all[..24].iter().enumerate() {
            assert_eq!((offset, size), (i as u32 * 500, 500));
        }
        assert_eq!(all[24], (12_000, 324));
        assert_eq!(all.iter().map(|c| c.1).sum::<usize>(), IMAGE_BYTES);
    }

    #[test]
    fn the_slot_reports_are_byte_exact() {
        let mut erase = vec![0u8; 65];
        erase[1..4].copy_from_slice(&[0x02, 0x01, 0x84]);
        assert_eq!(erase_report(4).unwrap(), erase);
        let mut validate = vec![0u8; 65];
        validate[1..3].copy_from_slice(&[0xB1, 0x02]);
        assert_eq!(validate_report(2).unwrap(), validate);
        let mut load = vec![0u8; 65];
        load[1..3].copy_from_slice(&[0xB2, 0x00]);
        assert_eq!(load_report(0).unwrap(), load);
        assert_eq!(file(0).unwrap(), 0x80);
        assert_eq!(file(4).unwrap(), 0x84);

        let mut read = vec![0u8; 645];
        read[1..10].copy_from_slice(&[0x83, 0x01, 0x81, 0xF4, 0x01, 0xE8, 0x03, 0x00, 0x00]);
        assert_eq!(read_chunk_report(1, 1000, 500).unwrap(), read);
        let mut last = vec![0u8; 645];
        last[1..10].copy_from_slice(&[0x83, 0x01, 0x80, 0x44, 0x01, 0xE0, 0x2E, 0x00, 0x00]);
        assert_eq!(read_chunk_report(0, 12_000, 324).unwrap(), last);

        let data: Vec<u8> = (0..500).map(|i| i as u8).collect();
        let w = write_chunk_report(3, 500, &data).unwrap();
        assert_eq!(w.len(), 645);
        assert_eq!(
            w[..10],
            [0x00, 0x03, 0x01, 0x83, 0xF4, 0x01, 0xF4, 0x01, 0x00, 0x00]
        );
        assert_eq!(w[10..510], data[..]);
        assert!(w[510..].iter().all(|&b| b == 0));
        // The last chunk: size 324, then 0xFF up to 500 bytes of data, then zeros.
        let w = write_chunk_report(0, 12_000, &data[..324]).unwrap();
        assert_eq!(w.len(), 645);
        assert_eq!(
            w[..10],
            [0x00, 0x03, 0x01, 0x80, 0x44, 0x01, 0xE0, 0x2E, 0x00, 0x00]
        );
        assert_eq!(w[10..334], data[..324]);
        assert!(w[334..510].iter().all(|&b| b == 0xFF));
        assert!(w[510..].iter().all(|&b| b == 0));

        // Each passes the allowlist with the onboard family on, and only then.
        for r in [
            erase_report(4).unwrap(),
            validate_report(4).unwrap(),
            load_report(4).unwrap(),
            read_chunk_report(4, 12_000, 324).unwrap(),
            write_chunk_report(4, 12_000, &data[..324]).unwrap(),
            write_chunk_report(0, 0, &data).unwrap(),
        ] {
            crate::allow::check(&r, ONBOARD).unwrap();
            assert!(crate::allow::check(&r, Families::default()).is_err());
        }
    }

    #[test]
    fn the_builders_refuse_what_the_keyboard_must_never_get() {
        let data = [0u8; 501];
        assert!(file(5).is_err());
        for slot in [5, 0x80, 0xFF] {
            assert!(erase_report(slot).is_err());
            assert!(validate_report(slot).is_err());
            assert!(load_report(slot).is_err());
            assert!(read_chunk_report(slot, 0, 500).is_err());
            assert!(write_chunk_report(slot, 0, &data[..500]).is_err());
        }
        assert!(write_chunk_report(0, 0, &data).is_err(), "501 bytes");
        assert!(write_chunk_report(0, 0, &[]).is_err(), "nothing");
        assert!(
            write_chunk_report(0, 12_000, &data[..325]).is_err(),
            "past the image"
        );
        assert!(
            write_chunk_report(0, 12_000, &data[..500]).is_err(),
            "a 500-byte last chunk"
        );
        assert!(write_chunk_report(0, u32::MAX, &data[..1]).is_err());
        assert!(read_chunk_report(0, 0, 0).is_err());
        assert!(read_chunk_report(0, 0, 501).is_err());
        assert!(read_chunk_report(0, 12_324, 1).is_err());
        assert!(read_chunk_report(0, u32::MAX, 1).is_err());
    }

    #[test]
    fn a_reply_must_be_the_chunk_asked_for() {
        let mut reply = vec![0x83, 0x00];
        reply.extend(1..=10u8);
        assert_eq!(chunk_data(&reply, 4).unwrap(), &[1, 2, 3, 4]);
        let e = format!("{:#}", chunk_data(&[0x83, 0x01, 9], 1).unwrap_err());
        assert!(e.contains("0x01"), "{e}");
        assert!(
            chunk_data(&[0x40, 0x00, 1, 2], 2).is_err(),
            "another command's answer"
        );
        assert!(chunk_data(&[0x83, 0x00, 1], 2).is_err(), "too short");
    }

    fn keyboard(slots: &FakeSlots) -> Keyboard {
        let kb = slots.keyboard();
        kb.set_families(ONBOARD);
        kb
    }

    #[test]
    fn a_slot_is_read_in_25_chunks_as_feature_reports() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let image = kb.read_image(2).unwrap();
        assert_eq!(image.name(), "Config 3");
        assert_eq!(image.serialize(), slots.slot(2));
        let sent = slots.wire().sent();
        assert_eq!(sent.len(), 25);
        for (s, (offset, size)) in sent.iter().zip(chunks()) {
            assert_eq!(s.kind, Kind::Feature);
            assert_eq!(s.report, read_chunk_report(2, offset, size).unwrap());
        }
    }

    #[test]
    fn with_the_family_off_nothing_reaches_the_keyboard() {
        let slots = FakeSlots::new();
        let kb = slots.keyboard();
        let e = kb.read_slot(0).unwrap_err();
        assert!(format!("{e:#}").contains("keyboard_onboard"), "{e:#}");
        assert!(kb.load_slot(0).is_err());
        let e = kb
            .write_slot(0, &QUICK, |old| Ok(old.serialize()), |old| Ok(old.clone()))
            .unwrap_err();
        assert_eq!(e.step, Step::Read);
        assert!(matches!(e.restore, Restore::Untouched));
        assert!(slots.wire().sent().is_empty());
    }

    #[test]
    fn loading_a_slot_sends_b2() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        kb.load_slot(3).unwrap();
        assert_eq!(slots.loaded(), Some(3));
        assert_eq!(
            slots.wire().sent(),
            [Sent {
                kind: Kind::Output,
                report: load_report(3).unwrap()
            }]
        );
        assert!(kb.load_slot(5).is_err());
    }

    /// The change the write tests make: a new name (in the first chunk, so a lagging first
    /// read shows) and a screen picture.
    fn renamed(old: &Image) -> Result<Image> {
        let mut new = old.clone();
        new.set_name("Renamed")?;
        let mut pic = Bitmap::default();
        pic.set(3, 3, true);
        new.set_oled(&pic);
        Ok(new)
    }

    fn ops(sent: &[Sent]) -> Vec<u8> {
        sent.iter().map(|s| s.report[1]).collect()
    }

    #[test]
    fn a_write_goes_read_backup_erase_chunks_check_read_back() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let old = slots.slot(4);
        let mut sent_at_backup = None;
        let wire = slots.wire().clone();
        let written = kb
            .write_slot(
                4,
                &QUICK,
                |image| {
                    sent_at_backup = Some(wire.sent().len());
                    Ok(image.serialize())
                },
                renamed,
            )
            .unwrap();
        assert_eq!(written, Written::Written);
        let expect = renamed(&Image::parse(&old).unwrap()).unwrap();
        assert_eq!(slots.slot(4), expect.serialize());
        let sent = slots.wire().sent();
        assert_eq!(
            sent_at_backup,
            Some(25),
            "the backup comes after the 25 reads"
        );
        let o = ops(&sent);
        assert!(o[..25].iter().all(|&op| op == 0x83));
        assert_eq!(sent[25].kind, Kind::Output);
        assert_eq!(sent[25].report, erase_report(4).unwrap());
        for (i, (offset, size)) in chunks().enumerate() {
            let data = &expect.bytes()[offset as usize..offset as usize + size];
            assert_eq!(sent[26 + i].kind, Kind::Feature);
            assert_eq!(
                sent[26 + i].report,
                write_chunk_report(4, offset, data).unwrap()
            );
        }
        assert_eq!(sent[51].kind, Kind::Output);
        assert_eq!(sent[51].report, validate_report(4).unwrap());
        // The first read after the write lags, so the read-back runs twice.
        assert!(o[52..].iter().all(|&op| op == 0x83));
        assert_eq!(o.len(), 52 + 50);
    }

    #[test]
    fn nothing_is_erased_without_a_backup_that_holds_the_slot() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let old = slots.slot(1);
        let e = kb
            .write_slot(1, &QUICK, |_| bail!("the disk is full"), renamed)
            .unwrap_err();
        assert_eq!(e.step, Step::Backup);
        assert!(matches!(e.restore, Restore::Untouched));
        let text = e.to_string();
        assert!(
            text.contains("the disk is full") && text.contains("wasn't changed"),
            "{text}"
        );
        let e = kb
            .write_slot(
                1,
                &QUICK,
                |image| {
                    let mut b = image.serialize();
                    b[9] ^= 1;
                    Ok(b)
                },
                renamed,
            )
            .unwrap_err();
        assert_eq!(e.step, Step::Backup, "{e}");
        let e = kb
            .write_slot(
                1,
                &QUICK,
                |image| Ok(image.serialize()[..100].to_vec()),
                renamed,
            )
            .unwrap_err();
        assert_eq!(e.step, Step::Backup, "{e}");
        assert!(!ops(&slots.wire().sent()).contains(&0x02), "never erased");
        assert_eq!(slots.slot(1), old);
    }

    #[test]
    fn a_slot_that_doesnt_check_out_is_never_written() {
        let slots = FakeSlots::new();
        let mut bad = slots.slot(0);
        bad[200] ^= 0xFF;
        slots.set_slot(0, &bad);
        let kb = keyboard(&slots);
        let mut backed_up = false;
        let e = kb
            .write_slot(
                0,
                &QUICK,
                |i| {
                    backed_up = true;
                    Ok(i.serialize())
                },
                renamed,
            )
            .unwrap_err();
        assert_eq!(e.step, Step::Read);
        assert!(
            format!("{:#}", e.error).contains("checksum"),
            "{:#}",
            e.error
        );
        assert!(!backed_up);
        assert!(!ops(&slots.wire().sent()).contains(&0x02));
        assert_eq!(slots.slot(0), bad);
    }

    #[test]
    fn a_new_image_that_doesnt_check_out_is_never_written() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let e = kb
            .write_slot(2, &QUICK, |i| Ok(i.serialize()), |_| bail!("no picture"))
            .unwrap_err();
        assert_eq!(e.step, Step::Build);
        assert!(matches!(e.restore, Restore::Untouched));
        assert!(!ops(&slots.wire().sent()).contains(&0x02));
        // The same image: nothing to write, so nothing is erased.
        slots.wire().clear_sent();
        let w = kb
            .write_slot(2, &QUICK, |i| Ok(i.serialize()), |i| Ok(i.clone()))
            .unwrap();
        assert_eq!(w, Written::Unchanged);
        assert!(ops(&slots.wire().sent()).iter().all(|&op| op == 0x83));
    }

    /// The sends in a write that goes through: 25 reads, the erase, 25 chunks, the check
    /// and two read-backs (the first lags).
    const SENDS: usize = 25 + 1 + 25 + 1 + 50;

    #[test]
    fn a_failure_at_any_send_restores_the_slot_or_never_touches_it() {
        for n in 1..=SENDS + 2 {
            let slots = FakeSlots::new();
            let kb = keyboard(&slots);
            let old = slots.slot(4);
            slots.wire().fail_send(n);
            let wire = slots.wire().clone();
            let mut backed_up_after = None;
            let result = kb.write_slot(
                4,
                &QUICK,
                |image| {
                    backed_up_after = Some(wire.sent().len());
                    Ok(image.serialize())
                },
                renamed,
            );
            let sent = slots.wire().sent();
            let first_erase = ops(&sent).iter().position(|&op| op == 0x02);
            if let Some(erase) = first_erase {
                assert!(
                    backed_up_after.is_some_and(|b| b <= erase),
                    "send {n}: backup first"
                );
            }
            match result {
                Err(e) if n <= 25 => {
                    assert_eq!(e.step, Step::Read, "send {n}");
                    assert!(matches!(e.restore, Restore::Untouched), "send {n}");
                    assert_eq!(first_erase, None, "send {n}: nothing erased");
                    assert_eq!(backed_up_after, None, "send {n}");
                    assert_eq!(slots.slot(4), old, "send {n}");
                }
                Err(e) => {
                    assert!(n <= SENDS, "send {n}: {e}");
                    assert!(matches!(e.restore, Restore::Restored), "send {n}: {e}");
                    assert_eq!(slots.slot(4), old, "send {n}: the old image is back");
                    let step = match n - 25 {
                        1 => Step::Erase,
                        c @ 2..=26 => Step::Chunk(c - 1),
                        27 => Step::Validate,
                        _ => Step::ReadBack,
                    };
                    assert_eq!(e.step, step, "send {n}");
                    assert!(e.to_string().contains("written back"), "{e}");
                }
                Ok(w) => {
                    assert!(n > SENDS, "send {n} failed, yet the write says {w:?}");
                    let new = renamed(&Image::parse(&old).unwrap()).unwrap();
                    assert_eq!(slots.slot(4), new.serialize());
                }
            }
        }
    }

    #[test]
    fn a_chunk_that_lands_wrong_is_caught_and_the_slot_restored() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let old = slots.slot(3);
        slots.corrupt_next_write_at(7_777);
        let e = kb
            .write_slot(3, &QUICK, |i| Ok(i.serialize()), renamed)
            .unwrap_err();
        // The keyboard's own check sees the CRC no longer matches.
        assert_eq!(e.step, Step::Validate);
        assert!(matches!(e.restore, Restore::Restored), "{e}");
        assert_eq!(slots.slot(3), old);
    }

    #[test]
    fn a_read_back_that_differs_is_caught_and_the_slot_restored() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let old = slots.slot(3);
        // Both read-backs of the write see byte 7777 wrong; the restore's reads are fine.
        slots.corrupt_reads_after_erase(7_777, 2);
        let e = kb
            .write_slot(3, &QUICK, |i| Ok(i.serialize()), renamed)
            .unwrap_err();
        assert_eq!(e.step, Step::ReadBack);
        assert!(format!("{:#}", e.error).contains("7777"), "{:#}", e.error);
        assert!(matches!(e.restore, Restore::Restored), "{e}");
        assert_eq!(slots.slot(3), old);
        // Once is the lag: a second read that matches is a write that went through.
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        slots.corrupt_reads_after_erase(7_777, 1);
        let w = kb
            .write_slot(3, &QUICK, |i| Ok(i.serialize()), renamed)
            .unwrap();
        assert_eq!(w, Written::Written);
    }

    #[test]
    fn the_keyboards_own_check_saying_no_restores_the_slot() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let old = slots.slot(0);
        slots.refuse_next_check();
        let e = kb
            .write_slot(0, &QUICK, |i| Ok(i.serialize()), renamed)
            .unwrap_err();
        assert_eq!(e.step, Step::Validate);
        assert!(matches!(e.restore, Restore::Restored), "{e}");
        assert_eq!(slots.slot(0), old);
    }

    #[test]
    fn an_erase_that_never_answers_restores_the_slot() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let old = slots.slot(2);
        slots.drop_next_answer();
        let e = kb
            .write_slot(2, &QUICK, |i| Ok(i.serialize()), renamed)
            .unwrap_err();
        assert_eq!(e.step, Step::Erase);
        assert!(
            format!("{:#}", e.error).contains("no answer"),
            "{:#}",
            e.error
        );
        assert!(matches!(e.restore, Restore::Restored), "{e}");
        assert_eq!(slots.slot(2), old);
    }

    #[test]
    fn when_the_restore_fails_too_both_errors_are_reported() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        // Chunk 4 of the write fails (send 30), then chunk 2 of the restore (send 33:
        // 31 is its erase).
        slots.wire().fail_send(30).fail_send(33);
        let e = kb
            .write_slot(1, &QUICK, |i| Ok(i.serialize()), renamed)
            .unwrap_err();
        assert_eq!(e.step, Step::Chunk(4));
        let Restore::Failed(again) = &e.restore else {
            panic!("{e}")
        };
        let text = e.to_string();
        assert!(
            text.contains("part 4 of 25") && text.contains("failed too"),
            "{text}"
        );
        assert!(format!("{again:#}").contains("part 2 of 25"), "{again:#}");
        assert!(text.contains("backup file"), "{text}");
    }
}
