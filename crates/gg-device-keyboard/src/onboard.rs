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
//!    compare. The check's answer decides nothing (what a good one looks like isn't
//!    documented): the read-back does, as in Apex Control's code;
//! 5. if anything in 4 fails, write the old image back, and keep at it: each send is tried
//!    again, an erase that doesn't answer is waited out, the read-back always runs, and
//!    the whole pass is done up to three times. Both are reported.
//!
//! Before the erase and the check, which wait for an input report, the input reports
//! nobody read are thrown away; while waiting, reports that answer other commands are
//! skipped. Each chunk read waits 13 ms for its answer and is tried up to four times.
//!
//! A slot that doesn't check out any more (a write cut short) is never written that way.
//! [`Keyboard::recover_slot`] puts a backup into it, saving its bytes first.

use std::fmt;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};
use tracing::{debug, info, warn};

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
/// Bytes in one chunk read or written. The last is this size on the wire too: its 176
/// bytes past the image are `0xFF` when written and dropped when read, as Apex Control's
/// code does it.
pub const CHUNK_BYTES: usize = 500;
/// Chunks in an image: 25 of 500 bytes, the last holding the image's last 324.
pub const CHUNKS: usize = IMAGE_BYTES.div_ceil(CHUNK_BYTES);
/// Where the last chunk starts.
const LAST_CHUNK: u32 = ((CHUNKS - 1) * CHUNK_BYTES) as u32;
/// The profile schema this layout is for.
pub const PROFILE_SCHEMA: u32 = 3;
/// The device schema this layout is for.
pub const DEVICE_SCHEMA: u32 = 8;
/// Per-key places in the switch tables (the first 70 of the firmware's key order).
pub const KEY_PLACES: usize = 70;
/// Tries for each chunk read, as Apex Control's code reads.
pub const READ_TRIES: usize = 4;
/// Tries for each send while putting an image back.
pub const SEND_TRIES: usize = 3;
/// Whole passes at putting an image back: the first and two more.
pub const RESTORES: usize = 3;

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
            self.bytes[at::RELEASE + place] =
                match ((switches.rapid_trigger)(code), (switches.protection)(code)) {
                    (false, false) => proto::RELEASE_NORMAL,
                    (true, false) => proto::RELEASE_RAPID_TRIGGER,
                    (false, true) => proto::RELEASE_PROTECTION,
                    (true, true) => proto::RELEASE_RAPID_TRIGGER_PROTECTION,
                };
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

/// Where a chunk may start, as the allowlist has it: a multiple of 500 from 0 to 12 000.
/// Returns how many of its bytes are the image's: 500, or 324 for the last.
fn check_chunk(offset: u32) -> Result<usize> {
    ensure!(
        offset.is_multiple_of(CHUNK_BYTES as u32) && offset <= LAST_CHUNK,
        "a chunk starts at a multiple of {CHUNK_BYTES} from 0 to {LAST_CHUNK}, not {offset}"
    );
    Ok(CHUNK_BYTES.min(IMAGE_BYTES - offset as usize))
}

/// The header of `03 01` and `83 01`, in a feature report: always 500 bytes.
fn file_report(op: u8, slot: u8, offset: u32) -> Result<Vec<u8>> {
    let file = file(slot)?;
    check_chunk(offset)?;
    let mut report = vec![0u8; FEATURE_REPORT];
    report[1..4].copy_from_slice(&[op, FILE_SUB, file]);
    report[4..6].copy_from_slice(&(CHUNK_BYTES as u16).to_le_bytes());
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

/// Where each chunk of an image starts and how many of its 500 bytes are the image's:
/// 24 whole ones, and the last's first 324.
pub fn chunks() -> impl Iterator<Item = (u32, usize)> {
    (0..IMAGE_BYTES)
        .step_by(CHUNK_BYTES)
        .map(|offset| (offset as u32, CHUNK_BYTES.min(IMAGE_BYTES - offset)))
}

/// Erase a slot: output report `02 01 <file>`. The keyboard answers when it's done.
pub fn erase_report(slot: u8) -> Result<Vec<u8>> {
    Ok(slot_command(OP_ERASE, &[FILE_SUB, file(slot)?]))
}

/// Write one chunk: feature report `03 01 <file> F4 01 <offset> <data>`, `data` being the
/// image's bytes at `offset`, all of them. The last chunk is sent as Apex Control's code
/// sends it: size 500 at 12 000, the image's last 324 bytes, then 176 × `0xFF`.
pub fn write_chunk_report(slot: u8, offset: u32, data: &[u8]) -> Result<Vec<u8>> {
    let mut report = file_report(OP_WRITE_CHUNK, slot, offset)?;
    let held = check_chunk(offset)?;
    ensure!(
        data.len() == held,
        "the chunk at {offset} is {held} bytes of the image, not {}",
        data.len()
    );
    report[FILE_HEADER..FILE_HEADER + held].copy_from_slice(data);
    report[FILE_HEADER + held..FILE_HEADER + CHUNK_BYTES].fill(0xFF);
    Ok(report)
}

/// Ask for one chunk back: feature report `83 01 <file> F4 01 <offset>`, then read the
/// feature report. The last too is asked for as 500 bytes, as Apex Control's code reads
/// it; the 176 past the image are dropped.
pub fn read_chunk_report(slot: u8, offset: u32) -> Result<Vec<u8>> {
    file_report(OP_READ_CHUNK, slot, offset)
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

/// Whether `bytes` are a whole image with its CRC right, whatever its schemas.
fn whole(bytes: &[u8]) -> bool {
    bytes.len() == IMAGE_BYTES && u32_at(bytes, at::CRC) == crc32_stm32(&bytes[..BODY_BYTES])
}

/// How long to wait in a write. [`Timing::KEYBOARD`] is Apex Control's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// After each chunk written, the last too (so before the check).
    pub chunk_gap: Duration,
    /// Between a chunk read's request and reading its answer.
    pub settle: Duration,
    /// Before a failed read or send is tried again.
    pub retry_gap: Duration,
    /// For the erase's answer.
    pub erase: Duration,
    /// For the check's answer.
    pub check: Duration,
}

impl Timing {
    /// 13 ms after each chunk written and before each chunk read's answer, 50 ms before
    /// trying again, up to 6 s for the erase and 2 s for the check (`ApexDevice.swift`,
    /// `OnboardProfile.swift`).
    pub const KEYBOARD: Timing = Timing {
        chunk_gap: Duration::from_millis(13),
        settle: Duration::from_millis(13),
        retry_gap: Duration::from_millis(50),
        erase: Duration::from_secs(6),
        check: Duration::from_secs(2),
    };

    /// About the longest a write takes when every wait runs out: the read, the write
    /// and [`RESTORES`] passes at putting the slot back, each waiting the whole erase and
    /// check time, plus the reports themselves. Reads that need all their tries aside.
    pub fn longest_write(&self) -> Duration {
        let chunks = CHUNKS as u32;
        let read = self.settle * chunks;
        let pass = self.erase + self.chunk_gap * chunks + self.check + read * 2;
        let reports = Duration::from_millis(2) * (chunks * 4 * (1 + RESTORES as u32));
        read + pass * (1 + RESTORES as u32) + reports
    }
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
    /// Seeing whether a slot is one a backup may be put into as damaged.
    Damaged,
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
            Step::Damaged => write!(f, "putting a backup into a damaged slot"),
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
    /// Writing the old image back failed too, every try: the slot may be erased or half
    /// written.
    Failed(anyhow::Error),
    /// The slot didn't check out before ([`Keyboard::recover_slot`]), and still doesn't.
    NotRecovered,
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
        let n = u16::from(self.slot) + 1;
        write!(f, "slot {n}: {} failed: {:#}", self.step, self.error)?;
        match &self.restore {
            Restore::Untouched => write!(f, "; the slot wasn't changed"),
            Restore::Restored => write!(
                f,
                "; the slot as it was before was written back, and it reads back the same"
            ),
            Restore::Failed(e) => write!(
                f,
                "; writing the slot as it was back failed too, {RESTORES} times ({e:#}), so it \
                 may be erased or half written: don't load it. Put its backup back with \
                 `ggctl onboard restore {n} <backup file> --damaged`"
            ),
            Restore::NotRecovered => write!(
                f,
                "; the slot still doesn't check out after {RESTORES} tries, and the backup file \
                 is as it was: try `ggctl onboard restore {n} <backup file> --damaged` again \
                 (unplug the keyboard and plug it back in first if it keeps failing)"
            ),
        }
    }
}

impl std::error::Error for WriteError {}

/// What the keyboard answered to its own check of a slot (`B1`). What a good answer looks
/// like isn't documented, so it decides nothing: the full read-back does, as in Apex
/// Control's code. Kept to show, until a session on the keyboard shows what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// `B1 00`.
    Passed,
    /// `B1` and this byte, not `00`.
    Other(u8),
    /// No answer in time, or it couldn't be read.
    NoAnswer,
}

impl Check {
    fn of(reply: &[u8]) -> Check {
        match reply.get(1).copied().unwrap_or(0) {
            0 => Check::Passed,
            b => Check::Other(b),
        }
    }

    /// What to tell the user about it, if anything.
    pub fn warning(self) -> Option<String> {
        let what = match self {
            Check::Passed => return None,
            Check::Other(b) => format!("answered {b:#04x}, not 0x00"),
            Check::NoAnswer => "didn't answer".to_owned(),
        };
        Some(format!(
            "the keyboard's own check of the slot {what}; what that means isn't known yet, and \
             the slot reads back as written, so the write stands"
        ))
    }
}

/// What a slot write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    /// The new image was the slot's already: nothing was erased or written.
    Unchanged,
    /// Written and read back the same; and what the keyboard's own check said.
    Written(Check),
}

/// How a pass of [`Keyboard::flash`] meets a problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// A new image: stop at the first problem, so the old image goes back.
    New,
    /// An image going back into a slot that's erased or half written already: never stop
    /// once the erase is out. Each send is tried again, an erase that doesn't answer is
    /// waited out, and the read-back always runs.
    Back,
}

fn sleep_until(deadline: Instant) {
    if let Some(left) = deadline.checked_duration_since(Instant::now()) {
        std::thread::sleep(left);
    }
}

/// An error's whole chain, on one line, for the log.
fn chain(e: &anyhow::Error) -> String {
    format!("{e:#}")
}

impl Keyboard {
    /// Read one chunk: the image's bytes in it. Each try waits `timing.settle` for the
    /// answer; a failed one is tried again, up to [`READ_TRIES`] times.
    fn read_chunk(&self, slot: u8, offset: u32, timing: &Timing) -> Result<Vec<u8>> {
        let request = read_chunk_report(slot, offset)?;
        let held = check_chunk(offset)?;
        let mut tries = 1;
        loop {
            let answer = self
                .exchange_feature_settled(&request, timing.settle)
                .and_then(|reply| Ok(chunk_data(&reply, CHUNK_BYTES)?[..held].to_vec()));
            match answer {
                Ok(data) => return Ok(data),
                Err(e) if tries < READ_TRIES => {
                    debug!(offset, attempt = tries, error = %chain(&e), "a chunk read failed; trying again");
                    tries += 1;
                    std::thread::sleep(timing.retry_gap);
                }
                Err(e) => return Err(e.context(format!("{READ_TRIES} tries"))),
            }
        }
    }

    /// Read a slot's whole image, chunk by chunk, unchecked.
    pub fn read_slot(&self, slot: u8, timing: &Timing) -> Result<Vec<u8>> {
        let mut image = Vec::with_capacity(IMAGE_BYTES);
        for (i, (offset, _)) in chunks().enumerate() {
            let data = self
                .read_chunk(slot, offset, timing)
                .with_context(|| format!("part {} of {CHUNKS}", i + 1))?;
            image.extend_from_slice(&data);
        }
        Ok(image)
    }

    /// Read a slot and check it.
    pub fn read_image(&self, slot: u8, timing: &Timing) -> Result<Image> {
        let bytes = self.read_slot(slot, timing)?;
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
        let n = u16::from(slot) + 1;
        let bytes = self
            .read_slot(slot, timing)
            .map_err(|e| untouched(Step::Read, e))?;
        let old = Image::parse(&bytes).map_err(|e| {
            untouched(
                Step::Read,
                anyhow!(
                    "slot {n} doesn't check out ({e:#}). gglinux only writes over a slot it read \
                     whole; if a write to it was cut short, put its backup back with `ggctl \
                     onboard restore {n} <backup file> --damaged`"
                ),
            )
        })?;
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
        // writes the old image back, and keeps at it.
        match self.flash(slot, &new, timing, Pass::New) {
            Ok(check) => Ok(Written::Written(check)),
            Err((step, error)) => {
                warn!(slot = n, %step, error = %chain(&error), "keyboard slot write failed; writing the slot back as it was");
                let restore = match self.put_back(slot, &old, timing) {
                    Ok(_) => Restore::Restored,
                    Err((again, e)) => Restore::Failed(e.context(again.to_string())),
                };
                Err(WriteError {
                    slot,
                    step,
                    error,
                    restore,
                })
            }
        }
    }

    /// Put `image` (a backup that checks out) into a slot that doesn't check out any more:
    /// a write cut short, a restore that didn't get through. Refused, with nothing sent but
    /// the read, if the slot checks out (write it the usual way, which keeps a backup of it
    /// first) or holds a whole image laid out for other firmware. `damaged` saves the
    /// slot's bytes as read and returns what the saved file holds; it must be them exactly,
    /// or nothing is erased. Then the image goes in as a restore puts one back: erase,
    /// 25 chunks, the check, the read-back, each send tried again, up to [`RESTORES`] times.
    pub fn recover_slot(
        &self,
        slot: u8,
        timing: &Timing,
        image: &Image,
        damaged: impl FnOnce(&[u8]) -> Result<Vec<u8>>,
    ) -> Result<Check, WriteError> {
        let untouched = |step, error| WriteError {
            slot,
            step,
            error,
            restore: Restore::Untouched,
        };
        let raw = self
            .read_slot(slot, timing)
            .map_err(|e| untouched(Step::Read, e))?;
        match Image::parse(&raw) {
            Ok(_) => {
                return Err(untouched(
                    Step::Damaged,
                    anyhow!(
                        "it checks out, so it isn't damaged: write the backup without --damaged, \
                         which keeps a backup of the slot first"
                    ),
                ))
            }
            Err(e) if whole(&raw) => {
                return Err(untouched(
                    Step::Damaged,
                    anyhow!("it holds a whole image gglinux doesn't write over: {e:#}"),
                ))
            }
            Err(_) => {}
        }
        let saved = damaged(&raw).map_err(|e| untouched(Step::Backup, e))?;
        if saved != raw {
            return Err(untouched(
                Step::Backup,
                anyhow!("the file doesn't hold the slot as it was read"),
            ));
        }
        self.put_back(slot, image, timing)
            .map_err(|(step, error)| WriteError {
                slot,
                step,
                error,
                restore: Restore::NotRecovered,
            })
    }

    /// Write `image` back into `slot`, up to [`RESTORES`] times, until it reads back the
    /// same.
    fn put_back(
        &self,
        slot: u8,
        image: &Image,
        timing: &Timing,
    ) -> Result<Check, (Step, anyhow::Error)> {
        let mut attempt = 1;
        loop {
            match self.flash(slot, image, timing, Pass::Back) {
                Ok(check) => {
                    info!(slot = slot + 1, attempt, "keyboard slot written back");
                    return Ok(check);
                }
                Err(failure) if attempt == RESTORES => return Err(failure),
                Err((step, e)) => {
                    warn!(slot = slot + 1, attempt, %step, error = %chain(&e), "writing the keyboard slot back didn't go through; again");
                    attempt += 1;
                    std::thread::sleep(timing.retry_gap);
                }
            }
        }
    }

    /// Erase `slot`, write `image` into it, have the keyboard check it, and read it back.
    fn flash(
        &self,
        slot: u8,
        image: &Image,
        timing: &Timing,
        pass: Pass,
    ) -> Result<Check, (Step, anyhow::Error)> {
        let at = |step: Step| move |e: anyhow::Error| (step, e);
        let erase = erase_report(slot).map_err(at(Step::Erase))?;
        // Answers nobody read would pass for the erase's.
        self.drain_input();
        self.send_tries(pass, timing, || self.output(&erase))
            .map_err(at(Step::Erase))?;
        self.await_erase(timing, pass)?;
        let bytes = image.bytes();
        let mut failed = None;
        for (i, (offset, size)) in chunks().enumerate() {
            let step = Step::Chunk(i + 1);
            let data = &bytes[offset as usize..offset as usize + size];
            let report = write_chunk_report(slot, offset, data).map_err(at(step))?;
            let sent = self.send_tries(pass, timing, || self.feature(&report));
            // After every chunk, the last too: the check comes after the same gap.
            std::thread::sleep(timing.chunk_gap);
            match (sent, pass) {
                (Ok(()), _) => {}
                (Err(e), Pass::New) => return Err((step, e)),
                // The read-back shows whether it landed anyway.
                (Err(e), Pass::Back) => {
                    failed.get_or_insert((step, e));
                }
            }
        }
        let check = self.check(slot, timing, pass)?;
        match (self.read_back(slot, bytes, timing), failed) {
            (Ok(()), _) => Ok(check),
            (Err(_), Some(failure)) => Err(failure),
            (Err(e), None) => Err((Step::ReadBack, e)),
        }
    }

    /// Send with `send`: once while writing a new image, up to [`SEND_TRIES`] times while
    /// putting one back.
    fn send_tries(&self, pass: Pass, timing: &Timing, send: impl Fn() -> Result<()>) -> Result<()> {
        let tries = match pass {
            Pass::New => 1,
            Pass::Back => SEND_TRIES,
        };
        let mut attempt = 1;
        loop {
            match send() {
                Ok(()) => return Ok(()),
                Err(e) if attempt < tries => {
                    warn!(attempt, error = %chain(&e), "a send to the keyboard slot failed; trying again");
                    attempt += 1;
                    std::thread::sleep(timing.retry_gap);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Wait for the erase's answer, which comes when the erase is done. What it holds
    /// isn't documented, so only its arrival counts (an input report that starts with
    /// `02`); the read-back is the real check. Without one, nothing goes to the slot
    /// before the whole erase time is up, as the erase may still be running.
    fn await_erase(&self, timing: &Timing, pass: Pass) -> Result<(), (Step, anyhow::Error)> {
        let deadline = Instant::now() + timing.erase;
        let missing = match self.await_input(OP_ERASE, deadline) {
            Ok(Some(reply)) => {
                debug!(answer = ?&reply[..reply.len().min(4)], "the slot is erased");
                return Ok(());
            }
            Ok(None) => anyhow!("no answer within {:?}", timing.erase),
            Err(e) => e,
        };
        sleep_until(deadline);
        match pass {
            Pass::New => Err((Step::Erase, missing)),
            Pass::Back => {
                warn!(error = %chain(&missing), "the erase didn't answer; its whole time was waited out");
                Ok(())
            }
        }
    }

    /// Have the keyboard check `slot` (`B1`), and say what it answered. Only a check that
    /// couldn't be sent stops a new image: not knowing what the check does, a write isn't
    /// called done without it.
    fn check(&self, slot: u8, timing: &Timing, pass: Pass) -> Result<Check, (Step, anyhow::Error)> {
        let report = validate_report(slot).map_err(|e| (Step::Validate, e))?;
        // A late answer to the erase would pass for this one's.
        self.drain_input();
        if let Err(e) = self.send_tries(pass, timing, || self.output(&report)) {
            if pass == Pass::New {
                return Err((Step::Validate, e));
            }
            warn!(error = %chain(&e), "the keyboard's own check of the slot couldn't be sent");
            return Ok(Check::NoAnswer);
        }
        let deadline = Instant::now() + timing.check;
        let check = match self.await_input(OP_VALIDATE_SLOT, deadline) {
            Ok(Some(reply)) => {
                info!(slot = slot + 1, answer = ?&reply[..reply.len().min(4)], "the keyboard checked the slot");
                Check::of(&reply)
            }
            Ok(None) => Check::NoAnswer,
            Err(e) => {
                warn!(error = %chain(&e), "reading the keyboard's own check of the slot failed");
                Check::NoAnswer
            }
        };
        if let Some(warning) = check.warning() {
            warn!(slot = slot + 1, "{warning}");
        }
        // The check may still be running: the read-back waits its whole time out.
        if check == Check::NoAnswer {
            sleep_until(deadline);
        }
        Ok(check)
    }

    /// Read the slot back and compare it with `expect`. The first read after a change may
    /// still serve the slot as it was: it reads again before calling a difference real.
    fn read_back(&self, slot: u8, expect: &[u8], timing: &Timing) -> Result<()> {
        let mut back = self.read_slot(slot, timing)?;
        if back != expect {
            back = self.read_slot(slot, timing)?;
        }
        if back != expect {
            let first = back
                .iter()
                .zip(expect)
                .position(|(a, b)| a != b)
                .unwrap_or(back.len().min(expect.len()));
            bail!("the slot reads back differently from what was written, from byte {first} on");
        }
        Ok(())
    }
}

/// A keyboard whose five slots are kept in memory, for tests: [`fake::FakeSlots`].
#[cfg(any(test, feature = "testing"))]
pub mod fake {
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::time::{Duration, Instant};

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
        /// The reply to the last `83 01`, held for feature reads, and when it's ready.
        read: Option<(Vec<u8>, Instant)>,
        /// The reply to the `83 01` before it: what a read before the new one is ready
        /// gets.
        earlier: Option<Vec<u8>>,
        /// How long a chunk read's answer takes to be ready.
        answer_delay: Duration,
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
        /// This many erases from now get no answer.
        mute_erases: u32,
        /// This many checks (`B1`) from now get no answer.
        mute_checks: u32,
        /// A byte other than `0xFF` was written past the image's end.
        past_image: bool,
        /// Every report that reached the slots: its opcode, and when.
        times: Vec<(u8, Instant)>,
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
        /// Past the image's end there's nothing to keep, only to check.
        fn write_chunk(&mut self, slot: usize, report: &[u8]) {
            let (size, offset) = size_offset(report);
            self.changing(slot);
            for i in 0..size {
                let at = offset + i;
                let mut byte = report[10 + i];
                if at >= IMAGE_BYTES {
                    self.past_image |= byte != 0xFF;
                    continue;
                }
                if self.corrupt_at == Some(at) {
                    self.corrupt_at = None;
                    byte ^= 0x5A;
                }
                self.slots[slot][at] &= byte;
            }
        }

        /// The chunk asked for; past the image's end, `0xFF`.
        fn read_chunk(&mut self, slot: usize, report: &[u8]) -> Vec<u8> {
            let (size, offset) = size_offset(report);
            let image = self.before[slot]
                .take()
                .unwrap_or_else(|| self.slots[slot].clone());
            let mut reply = vec![OP_READ_CHUNK, 0x00];
            reply.extend((offset..offset + size).map(|at| image.get(at).copied().unwrap_or(0xFF)));
            if let Some((at, times)) = self.corrupt_reads.filter(|_| self.corrupt_armed) {
                if (offset..offset + size).contains(&at) {
                    reply[2 + at - offset] ^= 0x5A;
                    self.corrupt_reads = (times > 1).then_some((at, times - 1));
                }
            }
            reply
        }

        fn answer_output(&mut self, report: &[u8]) -> Option<Vec<u8>> {
            match (report.get(1).copied(), report.get(2).copied()) {
                (Some(OP_ERASE), _) => {
                    let slot = slot_of(report)?;
                    self.erase(slot);
                    if self.mute_erases > 0 {
                        self.mute_erases -= 1;
                        return None;
                    }
                    Some(vec![OP_ERASE, 0x00])
                }
                (Some(OP_VALIDATE_SLOT), Some(slot)) if slot < SLOTS => {
                    let good = Image::parse(&self.slots[usize::from(slot)]).is_ok()
                        && !std::mem::take(&mut self.refuse_check);
                    if self.mute_checks > 0 {
                        self.mute_checks -= 1;
                        return None;
                    }
                    Some(vec![OP_VALIDATE_SLOT, u8::from(!good)])
                }
                (Some(OP_LOAD_SLOT), Some(slot)) => {
                    self.loaded = Some(slot);
                    None
                }
                _ => None,
            }
        }
    }

    /// Five slots in memory behind a [`FakeTransport`]: every report goes through the
    /// wrapped fake first (so [`FakeTransport::fail_send`], [`FakeTransport::fail_reads`],
    /// its input queue and [`FakeTransport::sent`] work as there), then the slot commands
    /// act on the slots. Like the keyboard, the first read of a slot after it changed still
    /// gets what it held before. It checks nothing: the allowlist is the [`Keyboard`]'s.
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
        /// A keyboard on these slots, every gated family off, with no pace between reports:
        /// the slot tests send thousands, and this fake drops none.
        pub fn keyboard(&self) -> Keyboard {
            let kb = Keyboard::with_transport(Box::new(self.clone()));
            kb.set_report_gap(Duration::ZERO);
            kb
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

        /// The next `n` erases get no answer (they still erase).
        pub fn mute_erases(&self, n: u32) {
            self.lock().mute_erases = n;
        }

        /// The next `n` checks (`B1`) get no answer.
        pub fn mute_checks(&self, n: u32) {
            self.lock().mute_checks = n;
        }

        /// A chunk read's answer is ready only `delay` after its request: a feature read
        /// sooner gets the answer to the request before, as the keyboard's feature report
        /// still holds the last command's answer.
        pub fn answer_reads_after(&self, delay: Duration) {
            self.lock().answer_delay = delay;
        }

        /// Whether any byte other than `0xFF` was written past the image's end.
        pub fn wrote_past_image(&self) -> bool {
            self.lock().past_image
        }

        /// The opcode of every report that reached the slots, and when.
        pub fn times(&self) -> Vec<(u8, Instant)> {
            self.lock().times.clone()
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
            let op = report.get(1).copied().unwrap_or(0);
            f.times.push((op, Instant::now()));
            let earlier = f.read.take().map(|(reply, _)| reply);
            match (op, slot_of(report)) {
                (OP_WRITE_CHUNK, Some(slot)) => f.write_chunk(slot, report),
                (OP_READ_CHUNK, Some(slot)) => {
                    let reply = f.read_chunk(slot, report);
                    f.earlier = earlier;
                    f.read = Some((reply, Instant::now() + f.answer_delay));
                }
                _ => {}
            }
            Ok(())
        }

        fn get_feature(&self, buf: &mut [u8]) -> Result<usize> {
            self.wire.count_read()?;
            let f = self.lock();
            let reply = match &f.read {
                Some((_, ready)) if Instant::now() < *ready => f.earlier.clone(),
                Some((reply, _)) => Some(reply.clone()),
                None => None,
            };
            drop(f);
            match reply {
                Some(reply) => {
                    ensure!(
                        buf.first() == Some(&0),
                        "a feature read needs report id 0 in the buffer's first byte"
                    );
                    Ok(fill(buf, 1, &reply, FEATURE_REPORT))
                }
                None => self.wire.feature_reply(buf),
            }
        }

        fn write(&self, report: &[u8]) -> Result<()> {
            self.wire.write(report)?;
            let mut f = self.lock();
            f.times
                .push((report.get(1).copied().unwrap_or(0), Instant::now()));
            let answer = f.answer_output(report);
            f.answer = if std::mem::take(&mut f.drop_answer) {
                None
            } else {
                answer
            };
            Ok(())
        }

        fn read_timeout(&self, buf: &mut [u8], _ms: i32) -> Result<usize> {
            self.wire.count_read()?;
            // The input queue first: reports nobody read come before any answer.
            if let Some(report) = self.wire.take_queued_input() {
                return Ok(fill(buf, 0, &report, INPUT_REPORT));
            }
            let answer = self.lock().answer.take();
            match answer {
                Some(reply) => Ok(fill(buf, 0, &reply, INPUT_REPORT)),
                None => Ok(self.wire.input_reply(buf)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::fake::{synthesized, FakeSlots};
    use super::*;
    use crate::testing::{Key, Kind, Sent};
    use crate::{Families, TapMode};

    const ONBOARD: Families = Families {
        bindings: false,
        onboard: true,
        protection: false,
    };
    const QUICK: Timing = Timing {
        chunk_gap: Duration::ZERO,
        settle: Duration::ZERO,
        retry_gap: Duration::ZERO,
        erase: Duration::from_millis(10),
        check: Duration::from_millis(10),
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
        // Apex Control's own vectors, from the code that reproduced a real image's CRC
        // (`OnboardProfileTests.swift`, testSTM32CRCVectors).
        assert_eq!(crc32_stm32(&[0, 0, 0, 0]), 0xC704_DD7B);
        assert_eq!(crc32_stm32(&[1, 2, 3, 4]), 0x1DAB_E74F);
        assert_eq!(crc32_stm32(b"ABCDEFGH"), 0x8DCB_FB33);
        assert_eq!(crc32_stm32(&[0xFF; BODY_BYTES]), 0x908E_DE87);
    }

    #[test]
    fn the_layout_offsets_are_the_references_as_numbers() {
        // "Image layout (profile schema 3, device schema 8, fw >= 1.19.0)", row by row.
        let table = [
            (at::SCHEMA, 0),
            (at::NAME, 4),
            (at::NAME_BYTES, 24),
            (at::LIGHTING, 28),
            (at::FN_KEY, 76),
            (at::NORMAL_BINDINGS, 80),
            (at::FN_BINDINGS, 580),
            (at::DEVICE_SCHEMA, 1080),
            (at::PRIMARY_H, 1084),
            (at::PRIMARY_L, 1154),
            (at::ZONES, 1224),
            (at::SECOND_H, 1306),
            (at::SECOND_L, 1376),
            (at::RELEASE, 1446),
            (at::SENSITIVITY, 1516),
            (at::PROTECTION_MS, 1586),
            (at::PROTECTION_DISTANCE, 1590),
            (at::SECOND_BINDINGS, 1660),
            (at::OLED, 2010),
            (at::RAPID_TAP, 2650),
            (at::RAPID_TAP_ENABLE, 2700),
            (at::MACROS, 2704),
            (at::MACRO_BYTES, 9600),
            (at::GUID, 12_304),
            (at::CRC, 12_320),
            (BODY_BYTES, 12_320),
            (IMAGE_BYTES, 12_324),
            (CHUNK_BYTES, 500),
            (CHUNKS, 25),
        ];
        for (i, (ours, theirs)) in table.into_iter().enumerate() {
            assert_eq!(ours, theirs, "row {i}");
        }
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
            protection: &|_| false,
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
        assert_eq!(read_chunk_report(1, 1000).unwrap(), read);
        // Apex Control's read of the same chunk (testReadChunkRequestFraming, slot 0 at 500).
        assert_eq!(
            read_chunk_report(0, 500).unwrap()[1..10],
            [0x83, 0x01, 0x80, 0xF4, 0x01, 0xF4, 0x01, 0x00, 0x00]
        );
        // The last is read as 500 bytes too, as Apex Control reads it; the 176 past the
        // image are dropped.
        let mut last = vec![0u8; 645];
        last[1..10].copy_from_slice(&[0x83, 0x01, 0x80, 0xF4, 0x01, 0xE0, 0x2E, 0x00, 0x00]);
        assert_eq!(read_chunk_report(0, 12_000).unwrap(), last);

        let data: Vec<u8> = (0..500).map(|i| i as u8).collect();
        let w = write_chunk_report(3, 500, &data).unwrap();
        assert_eq!(w.len(), 645);
        assert_eq!(
            w[..10],
            [0x00, 0x03, 0x01, 0x83, 0xF4, 0x01, 0xF4, 0x01, 0x00, 0x00]
        );
        assert_eq!(w[10..510], data[..]);
        assert!(w[510..].iter().all(|&b| b == 0));
        // The last chunk exactly as Apex Control sends it (writeChunkReports): size 500 at
        // offset 12000, the image's last 324 bytes, then 176 x 0xFF, then zeros.
        let w = write_chunk_report(0, 12_000, &data[..324]).unwrap();
        assert_eq!(w.len(), 645);
        assert_eq!(
            w[..10],
            [0x00, 0x03, 0x01, 0x80, 0xF4, 0x01, 0xE0, 0x2E, 0x00, 0x00]
        );
        assert_eq!(w[10..334], data[..324]);
        assert!(w[334..510].iter().all(|&b| b == 0xFF));
        assert!(w[510..].iter().all(|&b| b == 0));

        // Each passes the allowlist with the onboard family on, and only then.
        for r in [
            erase_report(4).unwrap(),
            validate_report(4).unwrap(),
            load_report(4).unwrap(),
            read_chunk_report(4, 12_000).unwrap(),
            write_chunk_report(4, 12_000, &data[..324]).unwrap(),
            write_chunk_report(0, 0, &data).unwrap(),
        ] {
            crate::allow::check(&r, ONBOARD).unwrap();
            assert!(crate::allow::check(&r, Families::default()).is_err());
        }
    }

    #[test]
    fn every_chunk_of_a_write_has_the_one_shape_the_allowlist_takes() {
        let image = synthesized("Config 5");
        for (offset, size) in chunks() {
            let data = &image.bytes()[offset as usize..offset as usize + size];
            let w = write_chunk_report(4, offset, data).unwrap();
            assert!(offset.is_multiple_of(500));
            assert_eq!(w[4..6], [0xF4, 0x01], "size 500 at {offset}");
            assert_eq!(u32::from_le_bytes([w[6], w[7], w[8], w[9]]), offset);
            assert_eq!(&w[10..10 + size], data);
            assert!(w[10 + size..510].iter().all(|&b| b == 0xFF), "{offset}");
            assert!(w[510..].iter().all(|&b| b == 0));
            crate::allow::check(&w, ONBOARD).unwrap();
            crate::allow::check(&read_chunk_report(4, offset).unwrap(), ONBOARD).unwrap();
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
            assert!(read_chunk_report(slot, 0).is_err());
            assert!(write_chunk_report(slot, 0, &data[..500]).is_err());
        }
        assert!(write_chunk_report(0, 0, &data).is_err(), "501 bytes");
        assert!(
            write_chunk_report(0, 0, &data[..499]).is_err(),
            "not all 500"
        );
        assert!(write_chunk_report(0, 0, &[]).is_err(), "nothing");
        assert!(
            write_chunk_report(0, 12_000, &data[..325]).is_err(),
            "past the image"
        );
        assert!(
            write_chunk_report(0, 12_000, &data[..500]).is_err(),
            "500 image bytes at 12000"
        );
        assert!(
            write_chunk_report(0, 12_000, &data[..1]).is_err(),
            "not all 324"
        );
        assert!(
            write_chunk_report(0, 250, &data[..500]).is_err(),
            "between chunks"
        );
        assert!(write_chunk_report(0, 12_500, &data[..1]).is_err());
        assert!(write_chunk_report(0, u32::MAX, &data[..1]).is_err());
        assert!(read_chunk_report(0, 250).is_err(), "between chunks");
        assert!(read_chunk_report(0, 12_323).is_err());
        assert!(read_chunk_report(0, 12_500).is_err());
        assert!(read_chunk_report(0, u32::MAX).is_err());
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

    #[test]
    fn a_chunk_reply_is_read_from_the_literal_bytes() {
        // A slot's first chunk as the feature read brings it: report id, `83 00`, then the
        // image from byte 0 (schema 3, then "Config 5").
        let fake = crate::testing::FakeTransport::new();
        let mut reply = vec![0x83, 0x00, 0x03, 0x00, 0x00, 0x00];
        reply.extend_from_slice(b"Config 5");
        fake.answer_feature(crate::testing::Key::sub(0x83, 0x01), &reply);
        let kb = fake.keyboard();
        kb.set_families(ONBOARD);
        let got = kb.read_chunk(4, 0, &QUICK).unwrap();
        assert_eq!(got.len(), 500);
        assert_eq!(got[..12], *b"\x03\x00\x00\x00Config 5");
        assert!(got[12..].iter().all(|&b| b == 0));
        assert_eq!(
            fake.sent()[0].report[..10],
            [0x00, 0x83, 0x01, 0x84, 0xF4, 0x01, 0x00, 0x00, 0x00, 0x00]
        );
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
        let image = kb.read_image(2, &QUICK).unwrap();
        assert_eq!(image.name(), "Config 3");
        assert_eq!(image.serialize(), slots.slot(2));
        let sent = slots.wire().sent();
        assert_eq!(sent.len(), 25);
        for (s, (offset, _)) in sent.iter().zip(chunks()) {
            assert_eq!(s.kind, Kind::Feature);
            assert_eq!(s.report, read_chunk_report(2, offset).unwrap());
        }
    }

    #[test]
    fn with_the_family_off_nothing_reaches_the_keyboard() {
        let slots = FakeSlots::new();
        let kb = slots.keyboard();
        let e = kb.read_slot(0, &QUICK).unwrap_err();
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
        assert_eq!(written, Written::Written(Check::Passed));
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
        assert!(!slots.wrote_past_image(), "only 0xFF past the image");
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
        // It says how to put a backup back into it.
        assert!(
            e.to_string()
                .contains("ggctl onboard restore 1 <backup file> --damaged"),
            "{e}"
        );
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
    /// The sends of a restore that goes through: the erase, 25 chunks, the check and two
    /// read-backs.
    const RESTORE_SENDS: usize = 1 + 25 + 1 + 50;

    /// What a slot may end as after any failure: the write went through and the slot
    /// holds the new image, or it didn't and holds the old one; and the result says which.
    #[track_caller]
    fn old_or_new(slots: &FakeSlots, slot: u8, old: &[u8], result: &Result<Written, WriteError>) {
        let new = renamed(&Image::parse(old).unwrap()).unwrap().serialize();
        match result {
            Ok(Written::Written(_)) => assert_eq!(slots.slot(slot), new),
            Ok(Written::Unchanged) => panic!("unchanged"),
            Err(e) => {
                assert!(
                    matches!(e.restore, Restore::Untouched | Restore::Restored),
                    "{e}"
                );
                assert_eq!(slots.slot(slot), old, "{e}");
            }
        }
    }

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
            old_or_new(&slots, 4, &old, &result);
            match (n, &result) {
                // A chunk read is tried again, so the reads and the read-backs go through.
                (1..=25, Ok(_)) => assert_eq!(sent.len(), SENDS, "send {n}: one more try"),
                (26, Err(e)) => assert_eq!(e.step, Step::Erase),
                (27..=51, Err(e)) => assert_eq!(e.step, Step::Chunk(n - 26)),
                (52, Err(e)) => assert_eq!(e.step, Step::Validate),
                (53.., Ok(_)) => {}
                (_, other) => panic!("send {n}: {other:?}"),
            }
            if let Err(e) = &result {
                assert!(matches!(e.restore, Restore::Restored), "send {n}: {e}");
                assert!(e.to_string().contains("written back"), "{e}");
            }
        }
    }

    #[test]
    fn a_failed_read_anywhere_restores_the_slot_or_never_touches_it() {
        // From each read of a write on, READ_TRIES reads fail in a row.
        let mut seen = [0usize; 3];
        for n in 1..=100 {
            let slots = FakeSlots::new();
            let kb = keyboard(&slots);
            let old = slots.slot(4);
            slots.wire().fail_reads(n..n + READ_TRIES);
            let result = kb.write_slot(4, &QUICK, |i| Ok(i.serialize()), renamed);
            old_or_new(&slots, 4, &old, &result);
            seen[match &result {
                Ok(_) => 0,
                Err(e) if matches!(e.restore, Restore::Untouched) => 1,
                Err(_) => 2,
            }] += 1;
        }
        assert!(
            seen.iter().all(|&n| n > 0),
            "written, untouched, restored: {seen:?}"
        );
    }

    #[test]
    fn old_input_reports_are_never_taken_for_an_answer() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let old = slots.slot(4);
        // Answers nobody read, from before the write...
        slots
            .wire()
            .queue_input(&[0x90, 0x00, b'1'])
            .queue_input(&[0xB1, 0x01]);
        // ... and late ones that come in while it waits for the erase and the check.
        slots
            .wire()
            .queue_input_after(Key::op(0x02), &[0xB1, 0x00])
            .queue_input_after(Key::op(0xB1), &[0x02, 0x00]);
        let w = kb
            .write_slot(4, &QUICK, |i| Ok(i.serialize()), renamed)
            .unwrap();
        assert_eq!(w, Written::Written(Check::Passed));
        let new = renamed(&Image::parse(&old).unwrap()).unwrap();
        assert_eq!(slots.slot(4), new.serialize());
        assert_eq!(slots.wire().queued_input(), 0);
    }

    #[test]
    fn the_restore_goes_through_with_old_input_reports_too() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let old = slots.slot(4);
        slots.wire().fail_send(30);
        slots
            .wire()
            .queue_input(&[0x40, 0x00])
            .queue_input(&[0x41, 0x00])
            .queue_input_after(Key::op(0x02), &[0x90, 0x00])
            .queue_input_after(Key::op(0x02), &[0xB2, 0x00]);
        let e = kb
            .write_slot(4, &QUICK, |i| Ok(i.serialize()), renamed)
            .unwrap_err();
        assert_eq!(e.step, Step::Chunk(4), "{e}");
        assert!(matches!(e.restore, Restore::Restored), "{e}");
        assert_eq!(slots.slot(4), old);
    }

    #[test]
    fn a_restore_goes_on_past_any_one_failed_send() {
        // Chunk 4 of the write fails (send 30); then each send of the restore fails once.
        for n in 31..=31 + RESTORE_SENDS + 1 {
            let slots = FakeSlots::new();
            let kb = keyboard(&slots);
            let old = slots.slot(1);
            slots.wire().fail_send(30).fail_send(n);
            let e = kb
                .write_slot(1, &QUICK, |i| Ok(i.serialize()), renamed)
                .unwrap_err();
            assert_eq!(e.step, Step::Chunk(4), "send {n}");
            assert!(matches!(e.restore, Restore::Restored), "send {n}: {e}");
            assert_eq!(slots.slot(1), old, "send {n}");
        }
    }

    #[test]
    fn a_restore_goes_on_past_failed_reads() {
        for n in 1..=160 {
            let slots = FakeSlots::new();
            let kb = keyboard(&slots);
            let old = slots.slot(1);
            slots.wire().fail_send(30).fail_reads(n..n + READ_TRIES);
            let result = kb.write_slot(1, &QUICK, |i| Ok(i.serialize()), renamed);
            assert!(result.is_err(), "read {n}");
            old_or_new(&slots, 1, &old, &result);
        }
    }

    #[test]
    fn a_restore_waits_out_an_erase_that_doesnt_answer_and_goes_on() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let old = slots.slot(2);
        // Neither the write's erase nor the restore's answers.
        slots.mute_erases(2);
        let start = Instant::now();
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
        assert!(start.elapsed() >= QUICK.erase * 2, "each erase waited out");
    }

    #[test]
    fn a_restore_reads_back_whatever_the_check_says() {
        for refuse in [true, false] {
            let slots = FakeSlots::new();
            let kb = keyboard(&slots);
            let old = slots.slot(0);
            slots.wire().fail_send(30);
            if refuse {
                slots.refuse_next_check();
            } else {
                slots.mute_checks(1);
            }
            let e = kb
                .write_slot(0, &QUICK, |i| Ok(i.serialize()), renamed)
                .unwrap_err();
            assert!(matches!(e.restore, Restore::Restored), "{refuse}: {e}");
            assert_eq!(slots.slot(0), old);
        }
    }

    #[test]
    fn a_restore_that_reads_back_wrong_is_done_again() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let old = slots.slot(3);
        slots.wire().fail_send(30);
        // The restore's first two read-backs see byte 7777 wrong.
        slots.corrupt_reads_after_erase(7_777, 2);
        let e = kb
            .write_slot(3, &QUICK, |i| Ok(i.serialize()), renamed)
            .unwrap_err();
        assert!(matches!(e.restore, Restore::Restored), "{e}");
        assert_eq!(slots.slot(3), old);
        let erases = ops(&slots.wire().sent())
            .iter()
            .filter(|&&o| o == 0x02)
            .count();
        assert_eq!(erases, 3, "the write's, then two restores");
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
        // The read-back sees it, whatever the keyboard's own check said.
        assert_eq!(e.step, Step::ReadBack);
        assert!(format!("{:#}", e.error).contains("7777"), "{:#}", e.error);
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
        assert_eq!(w, Written::Written(Check::Passed));
    }

    #[test]
    fn the_keyboards_own_check_decides_nothing_the_read_back_does() {
        for check in [Check::Other(1), Check::NoAnswer] {
            let slots = FakeSlots::new();
            let kb = keyboard(&slots);
            let old = slots.slot(0);
            if check == Check::NoAnswer {
                slots.mute_checks(1);
            } else {
                slots.refuse_next_check();
            }
            let start = Instant::now();
            let w = kb
                .write_slot(0, &QUICK, |i| Ok(i.serialize()), renamed)
                .unwrap();
            assert_eq!(w, Written::Written(check));
            let new = renamed(&Image::parse(&old).unwrap()).unwrap();
            assert_eq!(slots.slot(0), new.serialize());
            if check == Check::NoAnswer {
                assert!(start.elapsed() >= QUICK.check, "waited for it");
            }
            let warning = check.warning().unwrap();
            assert!(warning.contains("reads back as written"), "{warning}");
        }
        assert_eq!(Check::Passed.warning(), None);
        assert_eq!(Timing::KEYBOARD.check, Duration::from_secs(2));
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
    fn every_chunk_is_followed_by_a_gap_the_last_one_too() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let gap = Timing {
            chunk_gap: Duration::from_millis(5),
            ..QUICK
        };
        kb.write_slot(3, &gap, |i| Ok(i.serialize()), renamed)
            .unwrap();
        let times = slots.times();
        let writes: Vec<Instant> = times
            .iter()
            .filter(|(op, _)| *op == 0x03)
            .map(|t| t.1)
            .collect();
        assert_eq!(writes.len(), 25);
        for pair in writes.windows(2) {
            assert!(pair[1] - pair[0] >= gap.chunk_gap);
        }
        let check = times.iter().find(|(op, _)| *op == 0xB1).unwrap().1;
        assert!(check - writes[24] >= gap.chunk_gap, "a gap before B1 too");
    }

    #[test]
    fn a_chunk_read_waits_for_its_answer() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        slots.answer_reads_after(Duration::from_millis(4));
        // Read straight away, each chunk gets the answer before: the CRC catches it.
        assert!(kb.read_image(1, &QUICK).is_err());
        // Waiting 13 ms, as Apex Control does, each gets its own.
        let settled = Timing {
            settle: Duration::from_millis(13),
            ..QUICK
        };
        assert_eq!(
            kb.read_image(1, &settled).unwrap().serialize(),
            slots.slot(1)
        );
        assert_eq!(Timing::KEYBOARD.settle, Duration::from_millis(13));
    }

    #[test]
    fn the_longest_write_is_under_the_minute_ggctl_and_the_app_wait() {
        let longest = Timing::KEYBOARD.longest_write();
        assert!(
            longest > Duration::from_secs(30) && longest < Duration::from_secs(50),
            "{longest:?}"
        );
    }

    #[test]
    fn a_chunk_read_is_tried_up_to_four_times() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        // Reads 3 to 5 fail: chunk 3's first three tries.
        slots.wire().fail_reads(3..6);
        assert_eq!(kb.read_slot(0, &QUICK).unwrap(), slots.slot(0));
        assert_eq!(slots.wire().sent().len(), 25 + 3, "asked again each time");
        // Four in a row (reads 30 to 33, chunk 2 of the next read): it gives up.
        slots.wire().fail_reads(30..34);
        let e = kb.read_slot(0, &QUICK).unwrap_err();
        assert!(format!("{e:#}").contains("part 2 of 25"), "{e:#}");
    }

    #[test]
    fn a_slot_left_half_written_is_put_back_from_its_backup() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let old = Image::parse(&slots.slot(4)).unwrap();
        let new = renamed(&old).unwrap();
        // Chunk 4 fails, and then the keyboard takes nothing for the whole restore: three
        // tries of each of its three erases.
        slots.wire().fail_sends(30..40);
        let e = kb
            .write_slot(4, &QUICK, |i| Ok(i.serialize()), renamed)
            .unwrap_err();
        assert!(matches!(e.restore, Restore::Failed(_)), "{e}");
        let text = e.to_string();
        assert!(
            text.contains("part 4 of 25") && text.contains("failed too"),
            "{text}"
        );
        assert!(
            text.contains("ggctl onboard restore 5 <backup file> --damaged"),
            "{text}"
        );
        // What the slot holds: erased, with the write's first three chunks in.
        let half = slots.slot(4);
        assert_eq!(half[..1500], new.bytes()[..1500]);
        assert!(half[1500..].iter().all(|&b| b == 0xFF));
        // A plain write or restore won't touch it, and says what will.
        let e = kb
            .write_slot(4, &QUICK, |i| Ok(i.serialize()), |_| Ok(old.clone()))
            .unwrap_err();
        assert!(matches!(e.restore, Restore::Untouched));
        assert!(e.to_string().contains("--damaged"), "{e}");
        // The recovery does, from the backup, saving what the slot held first.
        let mut saved = None;
        let check = kb
            .recover_slot(4, &QUICK, &old, |raw| {
                saved = Some(raw.to_vec());
                Ok(raw.to_vec())
            })
            .unwrap();
        assert_eq!(check, Check::Passed);
        assert_eq!(saved.as_deref(), Some(&half[..]));
        assert_eq!(slots.slot(4), old.serialize());
    }

    #[test]
    fn the_recovery_is_only_for_a_slot_that_doesnt_check_out() {
        let slots = FakeSlots::new();
        let kb = keyboard(&slots);
        let backup = Image::parse(&slots.slot(0)).unwrap();
        let keep = |raw: &[u8]| -> Result<Vec<u8>> { Ok(raw.to_vec()) };
        // A slot that checks out is written the usual way, with a backup of it first.
        let e = kb.recover_slot(0, &QUICK, &backup, keep).unwrap_err();
        assert!(matches!(e.restore, Restore::Untouched));
        assert!(e.to_string().contains("without --damaged"), "{e}");
        // One laid out for other firmware, whole and with its CRC right, isn't gglinux's
        // to write over.
        let mut other = slots.slot(1);
        other[at::SCHEMA] = 4;
        let crc = crc32_stm32(&other[..BODY_BYTES]);
        other[at::CRC..].copy_from_slice(&crc.to_le_bytes());
        slots.set_slot(1, &other);
        let e = kb.recover_slot(1, &QUICK, &backup, keep).unwrap_err();
        assert!(e.to_string().contains("other firmware"), "{e}");
        // The slot's bytes are saved first, as read, or nothing is erased.
        let mut bad = slots.slot(2);
        bad[9000] ^= 0xFF;
        slots.set_slot(2, &bad);
        let e = kb
            .recover_slot(2, &QUICK, &backup, |_| bail!("the disk is full"))
            .unwrap_err();
        assert_eq!(e.step, Step::Backup);
        assert!(e.to_string().contains("the disk is full"), "{e}");
        let e = kb
            .recover_slot(2, &QUICK, &backup, |raw| Ok(raw[1..].to_vec()))
            .unwrap_err();
        assert_eq!(e.step, Step::Backup);
        assert!(!ops(&slots.wire().sent()).contains(&0x02), "nothing erased");
        assert_eq!(slots.slot(2), bad);
        // With the family off, nothing goes out.
        slots.wire().clear_sent();
        assert!(slots
            .keyboard()
            .recover_slot(2, &QUICK, &backup, keep)
            .is_err());
        assert!(slots.wire().sent().is_empty());
    }

    #[test]
    fn the_recovery_goes_on_past_any_one_failed_send() {
        let backup = synthesized("Config 3");
        let mut half = backup.serialize();
        half[6000..].fill(0xFF);
        for n in 1..=SENDS + 2 {
            let slots = FakeSlots::new();
            slots.set_slot(2, &half);
            let kb = keyboard(&slots);
            slots.wire().fail_send(n);
            let check = kb.recover_slot(2, &QUICK, &backup, |raw| Ok(raw.to_vec()));
            assert_eq!(check.unwrap(), Check::Passed, "send {n}");
            assert_eq!(slots.slot(2), backup.serialize(), "send {n}");
        }
    }

    #[test]
    fn a_recovery_that_cant_get_through_says_so() {
        let backup = synthesized("Config 3");
        let mut half = backup.serialize();
        half[6000..].fill(0xFF);
        let slots = FakeSlots::new();
        slots.set_slot(2, &half);
        let kb = keyboard(&slots);
        slots.wire().fail_sends(27..);
        let e = kb
            .recover_slot(2, &QUICK, &backup, |raw| Ok(raw.to_vec()))
            .unwrap_err();
        assert!(matches!(e.restore, Restore::NotRecovered), "{e}");
        let text = e.to_string();
        assert!(text.contains("still doesn't check out"), "{text}");
        assert!(text.contains("--damaged"), "{text}");
    }
}
