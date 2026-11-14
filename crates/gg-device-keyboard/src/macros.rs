// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard macros in a slot's image, stored as GG 123 stores them.
//!
//! From four GG 123 saves of the same slot (our captures apex-20 to apex-22 and apex-30,
//! `docs/PROTOCOLS.md` 3f), diffed against the slot's factory image. The macro area is
//! Apex Control's opaque 9 600 bytes at image offset 2704 ([`at::MACROS`]): macros back to
//! back from its start, the rest zero. Each macro is an 8-byte header,
//! `77 <mode> <count u16 LE> 01 <times> <wait u16 LE>`, then `count` 4-byte steps:
//!
//! - a key: `02 <HID usage> 01 <01 down | 00 up>`;
//! - a wait: `04 00 <ms u16 LE>`;
//! - a mouse button: `00 <button> 00 <01 down | 00 up>` (only button 1 was seen).
//!
//! Modes ([`Mode`]): `01` plays once, `02` a number of times (`times`, with `wait` ms
//! between plays), `03` again and again while the key is held, `04` again and again from
//! one press to the next (`wait` ms apart). The header's fifth byte was `01` in every save;
//! what it means isn't known, so it's written as GG wrote it. `times` is 1 and `wait` GG's
//! 15 ms wherever GG's saves don't show the user's own. A key plays a macro through its
//! binding on the normal layer, at
//! `80 + 5 × its place` in the firmware's key order ([`FIRMWARE_KEYS`]): function `71`,
//! then `00 00` and where the macro starts in the area, in 4-byte words (`u16 LE`).
//!
//! Only that is written. A slot whose macros hold anything else (another mode or kind of
//! step, another fifth header byte, a macro on the Fn or deep-press layer, a binding that
//! points where no macro starts, bytes after the last macro) is never rewritten: the user's
//! own GG-made macros must not be lost ([`CANT_READ`]). GG's save also rewrites unrelated
//! fields (blank places, zone bytes); a write here changes only the macro area, the bound
//! keys' places and the CRC. A key whose macro is gone types itself again
//! (`51 <key> 00 00 00`, its factory binding).

use anyhow::{anyhow, bail, ensure, Result};

use crate::bindings::{KEYBOARD, MACRO};
use crate::onboard::{at, Image};

/// What a refused slot's error starts with.
pub const CANT_READ: &str = "this slot holds macros gglinux can't read yet";

/// The firmware's key order: the HID code of each of the 100 places a layer's bindings
/// have, 0 for an empty place (`deviceKeyIndexToHID` in Apex Control's code,
/// `evidence/apexkit-f7db8c5/Key.swift`). Our captures agree: GG bound O at place 23 and P
/// at place 24.
pub const FIRMWARE_KEYS: [u8; 100] = [
    0x35, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2D, 0x2E, 0x89, //
    0x2B, 0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13, 0x2F, 0x30, 0x31, //
    0x39, 0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F, 0x33, 0x34, 0x32, 0x28, //
    0xE1, 0x64, 0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10, 0x36, 0x37, 0x38, 0x87, 0xE5, //
    0xE0, 0xE3, 0xE2, 0x8B, 0x2C, 0x8A, 0x88, 0xE6, 0xE7, 0xF0, 0xE4, 0x2A, 0x00, 0x00, //
    0x29, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x49, //
    0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x52, 0x50, 0x51, 0x4F, 0x00, 0xF5, 0xF6, 0xF7, 0xF1, //
    0xFE, 0xFF,
];

/// Places with a key that can't play a macro: the six media and screen buttons (hard-wired,
/// as Apex Control's `ignoredHIDCodes`) and the Fn key, which keeps its job.
const NO_MACRO: [u8; 7] = [0xF5, 0xF6, 0xF7, 0xF1, 0xFE, 0xFF, 0xF0];

/// How many keys can have a macro. GG's saves show two; more aren't tried yet.
pub const MAX_MACROS: usize = 2;
/// The most steps one macro gets for now: GG's longest in our captures has 18, and no
/// longer one has been tried on the keyboard (review R2, K3).
pub const MAX_STEPS: usize = 64;

const TAG: u8 = 0x77;
/// The header's fifth byte, as every GG save has it.
const HEADER_FIFTH: u8 = 0x01;
/// GG's number of plays and wait between them, where the mode doesn't use them.
pub const DEFAULT_TIMES: u8 = 1;
pub const DEFAULT_DELAY_MS: u16 = 15;
const HEADER: usize = 8;
const STEP: usize = 4;
const KEY_STEP: u8 = 0x02;
const KEY_PAGE: u8 = 0x01;
const WAIT_STEP: u8 = 0x04;
const BUTTON_STEP: u8 = 0x00;
/// The only mouse button seen in GG's saves.
const LEFT_BUTTON: u8 = 1;
/// A binding: function and four codes.
const PLACE: usize = 5;
/// The keyboard usages a key step may press.
const USAGES: std::ops::RangeInclusive<u8> = 0x04..=0xE7;

/// One step of a macro, as a slot holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Key { usage: u8, down: bool },
    Button { button: u8, down: bool },
    Delay { ms: u16 },
}

/// How a macro plays: GG's play modes, with their codes (apex-20 to apex-22, apex-30).
/// GG greys out its fifth, Toggle Hold, so its code is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Once per press (GG's Play Once, `01`).
    Once,
    /// `times` times per press, `wait` apart (GG's Play N Times, `02`).
    Times,
    /// Again and again while the key is held (GG's Repeat While Pressed, `03`).
    WhileHeld,
    /// Again and again from one press to the next, `wait` apart (GG's Toggle
    /// Auto-Repeat, `04`).
    Toggle,
}

impl Mode {
    fn code(self) -> u8 {
        match self {
            Mode::Once => 0x01,
            Mode::Times => 0x02,
            Mode::WhileHeld => 0x03,
            Mode::Toggle => 0x04,
        }
    }

    fn of(code: u8) -> Option<Mode> {
        [Mode::Once, Mode::Times, Mode::WhileHeld, Mode::Toggle]
            .into_iter()
            .find(|m| m.code() == code)
    }
}

/// A key and the macro it plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotMacro {
    /// The key, by HID code.
    pub key: u8,
    pub mode: Mode,
    /// Plays per press: the user's with [`Mode::Times`] (1 to 255), else
    /// [`DEFAULT_TIMES`].
    pub times: u8,
    /// The wait between plays, in ms: the user's with [`Mode::Times`] and [`Mode::Toggle`]
    /// (1 to 65 535), else [`DEFAULT_DELAY_MS`].
    pub repeat_delay_ms: u16,
    pub events: Vec<Event>,
}

/// The place where key `code` keeps its bindings, if it can play a macro.
pub fn place(code: u8) -> Option<usize> {
    if code == 0 || NO_MACRO.contains(&code) {
        return None;
    }
    FIRMWARE_KEYS.iter().position(|&c| c == code)
}

fn cant_read(why: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("{CANT_READ} ({why}), so gglinux leaves it as it is")
}

/// A macro in the area: where it starts, in 4-byte words, how it plays and its steps.
struct Entry {
    start: u16,
    mode: Mode,
    times: u8,
    repeat_delay_ms: u16,
    events: Vec<Event>,
}

/// The macros in an area, back to back from its start; everything after them must be
/// zero.
fn decode_area(area: &[u8]) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut at = 0;
    while at < area.len() && area[at] == TAG {
        let header = area.get(at..at + HEADER).ok_or_else(|| {
            cant_read(format!("a macro's header runs past the area at byte {at}"))
        })?;
        let mode = Mode::of(header[1])
            .ok_or_else(|| cant_read(format!("a macro has play mode {:#04x}", header[1])))?;
        if header[4] != HEADER_FIFTH {
            return Err(cant_read(format!(
                "a macro's header has {:#04x} where GG writes {HEADER_FIFTH:#04x}",
                header[4]
            )));
        }
        let count = usize::from(u16::from_le_bytes([header[2], header[3]]));
        let steps = area
            .get(at + HEADER..at + HEADER + count * STEP)
            .ok_or_else(|| cant_read(format!("a macro of {count} steps runs past the area")))?;
        let events = steps
            .chunks_exact(STEP)
            .map(decode_step)
            .collect::<Result<Vec<Event>>>()?;
        entries.push(Entry {
            start: (at / STEP) as u16,
            mode,
            times: header[5],
            repeat_delay_ms: u16::from_le_bytes([header[6], header[7]]),
            events,
        });
        at += HEADER + count * STEP;
    }
    if let Some(i) = area[at.min(area.len())..].iter().position(|&b| b != 0) {
        return Err(cant_read(format!(
            "there are bytes after the last macro, at byte {}",
            at + i
        )));
    }
    Ok(entries)
}

fn decode_step(s: &[u8]) -> Result<Event> {
    let down = |b: u8| match b {
        0x01 => Ok(true),
        0x00 => Ok(false),
        other => Err(cant_read(format!(
            "a step says {other:#04x} for down or up"
        ))),
    };
    Ok(match (s[0], s[2]) {
        (KEY_STEP, KEY_PAGE) => Event::Key {
            usage: s[1],
            down: down(s[3])?,
        },
        (WAIT_STEP, _) if s[1] == 0 => Event::Delay {
            ms: u16::from_le_bytes([s[2], s[3]]),
        },
        (BUTTON_STEP, 0x00) => Event::Button {
            button: s[1],
            down: down(s[3])?,
        },
        _ => return Err(cant_read(format!("a step is {s:02x?}"))),
    })
}

/// The macros `image` holds, by key in the firmware's key order. Refused, with
/// [`CANT_READ`], if its macro area or bindings hold anything not known.
pub fn read(image: &Image) -> Result<Vec<SlotMacro>> {
    let entries = decode_area(image.macros())?;
    for (layer, bindings) in [
        ("Fn", image.fn_bindings()),
        ("deep-press", image.second_bindings()),
    ] {
        if bindings.chunks_exact(PLACE).any(|b| b[0] == MACRO) {
            return Err(cant_read(format!("a key has a macro on the {layer} layer")));
        }
    }
    let mut out = Vec::new();
    for (p, b) in image.normal_bindings().chunks_exact(PLACE).enumerate() {
        if b[0] != MACRO {
            continue;
        }
        let key = FIRMWARE_KEYS[p];
        if place(key) != Some(p) {
            return Err(cant_read(format!(
                "place {p} has a macro, and gglinux doesn't know a key there that can play one"
            )));
        }
        if b[1..3] != [0, 0] {
            return Err(cant_read(format!(
                "key {key:#04x}'s macro binding is {b:02x?}"
            )));
        }
        let start = u16::from_le_bytes([b[3], b[4]]);
        let entry = entries.iter().find(|e| e.start == start).ok_or_else(|| {
            cant_read(format!(
                "key {key:#04x} points at word {start} of the macro area, where no macro starts"
            ))
        })?;
        out.push(SlotMacro {
            key,
            mode: entry.mode,
            times: entry.times,
            repeat_delay_ms: entry.repeat_delay_ms,
            events: entry.events.clone(),
        });
    }
    Ok(out)
}

/// One step as the area holds it, if GG's saves show it.
fn encode_step(e: &Event) -> Result<[u8; STEP]> {
    Ok(match *e {
        Event::Key { usage, down } => {
            ensure!(
                USAGES.contains(&usage),
                "usage {usage:#04x} isn't a keyboard key"
            );
            [KEY_STEP, usage, KEY_PAGE, u8::from(down)]
        }
        Event::Delay { ms } => {
            ensure!(ms > 0, "a wait of 0 ms isn't in GG's saves");
            let [lo, hi] = ms.to_le_bytes();
            [WAIT_STEP, 0x00, lo, hi]
        }
        Event::Button { button, down } => {
            ensure!(
                button == LEFT_BUTTON,
                "only mouse button {LEFT_BUTTON} is in GG's saves, not button {button}"
            );
            [BUTTON_STEP, button, 0x00, u8::from(down)]
        }
    })
}

/// A macro's header as GG writes it, if its numbers are ones GG's saves show for its
/// mode: a count only with Play N Times, a wait of the user's only with that and Toggle
/// Auto-Repeat.
fn encode_header(m: &SlotMacro, count: u16) -> Result<[u8; HEADER]> {
    let key = m.key;
    let counts = m.mode == Mode::Times;
    let waits = matches!(m.mode, Mode::Times | Mode::Toggle);
    ensure!(
        m.times >= 1 && (counts || m.times == DEFAULT_TIMES),
        "key {key:#04x}'s macro plays {} times in mode {:?}: GG's saves show only 1, or 1 to 255 with Play N Times",
        m.times,
        m.mode
    );
    ensure!(
        m.repeat_delay_ms >= 1 && (waits || m.repeat_delay_ms == DEFAULT_DELAY_MS),
        "key {key:#04x}'s macro has a wait of {} ms in mode {:?}: GG's saves show only {DEFAULT_DELAY_MS}, or 1 to 65535 with Play N Times and Toggle Auto-Repeat",
        m.repeat_delay_ms,
        m.mode
    );
    let [lo, hi] = count.to_le_bytes();
    let [wait_lo, wait_hi] = m.repeat_delay_ms.to_le_bytes();
    Ok([
        TAG,
        m.mode.code(),
        lo,
        hi,
        HEADER_FIFTH,
        m.times,
        wait_lo,
        wait_hi,
    ])
}

/// The area for `macros`, already in their order, and where each starts, in words.
fn encode_area(macros: &[(usize, &SlotMacro)]) -> Result<(Vec<u8>, Vec<u16>)> {
    let mut area = Vec::with_capacity(at::MACRO_BYTES);
    let mut starts = Vec::new();
    for (_, m) in macros {
        ensure!(
            m.events.len() <= MAX_STEPS,
            "key {:#04x}'s macro has {} steps: at most {MAX_STEPS} steps go into a slot for now, until a longer macro has been tried on the keyboard",
            m.key,
            m.events.len()
        );
        let count = u16::try_from(m.events.len())
            .ok()
            .filter(|&c| c > 0)
            .ok_or_else(|| anyhow!("key {:#04x}'s macro has no steps, or too many", m.key))?;
        starts.push((area.len() / STEP) as u16);
        area.extend(encode_header(m, count)?);
        for e in &m.events {
            area.extend(encode_step(e)?);
        }
    }
    ensure!(
        area.len() <= at::MACRO_BYTES,
        "the macros take {} bytes, and a slot holds {}",
        area.len(),
        at::MACRO_BYTES
    );
    area.resize(at::MACRO_BYTES, 0);
    Ok((area, starts))
}

/// `image` with `macros` as its macros: the macro area written anew, the keys bound to
/// them, and keys whose macro is gone typing themselves again. Nothing else changes.
/// Refused, with nothing changed, if the slot's own macros can't be read ([`read`]) or
/// `macros` can't be written as GG writes them.
pub fn write(image: &Image, macros: &[SlotMacro]) -> Result<Image> {
    let old = read(image)?;
    ensure!(
        macros.len() <= MAX_MACROS,
        "at most {MAX_MACROS} keys can have a macro for now, not {}",
        macros.len()
    );
    let mut placed = Vec::new();
    for m in macros {
        let p = place(m.key).ok_or_else(|| anyhow!("key {:#04x} can't play a macro", m.key))?;
        if placed.iter().any(|&(q, _)| q == p) {
            bail!("key {:#04x} has a macro twice", m.key);
        }
        placed.push((p, m));
    }
    // By place, as GG's save has them: O's before P's.
    placed.sort_by_key(|&(p, _)| p);
    let (area, starts) = encode_area(&placed)?;
    let mut places: Vec<(usize, [u8; PLACE])> = old
        .iter()
        .filter(|o| !macros.iter().any(|m| m.key == o.key))
        .filter_map(|o| Some((place(o.key)?, [KEYBOARD, o.key, 0, 0, 0])))
        .collect();
    for (&(p, _), start) in placed.iter().zip(starts) {
        let [lo, hi] = start.to_le_bytes();
        places.push((p, [MACRO, 0x00, 0x00, lo, hi]));
    }
    let mut new = image.clone();
    new.set_macros(&area, &places)?;
    Ok(new)
}

/// A slot image like a factory one, for tests and fakes: every key typing itself on the
/// normal layer (empty places `51 00 00 00 00`, the media buttons as the factory has
/// them), nothing on the other layers, no macros. Its other parts are
/// [`crate::onboard::fake::synthesized`]'s.
#[cfg(any(test, feature = "testing"))]
pub fn factory_like(name: &str) -> Image {
    use crate::onboard::{crc32_stm32, fake::synthesized, BODY_BYTES};
    // The media buttons' factory bindings: consumer usages (function `61`).
    const MEDIA: [(u8, u8); 6] = [
        (0xF5, 0xCD),
        (0xF6, 0xB5),
        (0xF7, 0xB6),
        (0xF1, 0xE2),
        (0xFE, 0xE9),
        (0xFF, 0xEA),
    ];
    let mut b = synthesized(name).serialize();
    for (p, &code) in FIRMWARE_KEYS.iter().enumerate() {
        let normal = match MEDIA.iter().find(|m| m.0 == code) {
            Some(&(_, usage)) => [0x61, usage, 0, 0, 0],
            None => [KEYBOARD, code, 0, 0, 0],
        };
        let n = at::NORMAL_BINDINGS + p * PLACE;
        b[n..n + PLACE].copy_from_slice(&normal);
        let f = at::FN_BINDINGS + p * PLACE;
        b[f..f + PLACE].copy_from_slice(&[KEYBOARD, 0, 0, 0, 0]);
    }
    b[at::SECOND_BINDINGS..at::OLED].fill(0);
    b[at::MACROS..at::MACROS + at::MACRO_BYTES].fill(0);
    let crc = crc32_stm32(&b[..BODY_BYTES]);
    b[at::CRC..].copy_from_slice(&crc.to_le_bytes());
    Image::parse(&b).expect("a factory-like image checks out")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::IMAGE_BYTES;

    const O: u8 = 0x12;
    const P: u8 = 0x13;

    /// What GG wrote into the macro area, up to the zeros after it (apex-20 to apex-22).
    const GG_ONE: &str =
        "7701090001010f000204010104000f00020401000400390102050101020501000400730c0001000100010000";
    const GG_TWO: &str = "7701120001010f00021b0101021b01000400db00021c0101021c01000400db00021d0101021d01000400a60102e001010206010104000f00020601000400100002e001000400c80b00010001000100007701090001010f000204010104000f00020401000400390102050101020501000400730c0001000100010000";
    const GG_REPEAT: &str = "7701120001010f00021b0101021b01000400db00021c0101021c01000400db00021d0101021d01000400a60102e001010206010104000f00020601000400100002e001000400c80b00010001000100007703090001010f000204010104000f00020401000400390102050101020501000400730c0001000100010000";
    /// GG's save of capture apex-30: its first macro on O with Toggle Auto-Repeat, 200 ms
    /// apart (`77 04 09 00 01 01 c8 00`), and on P with Play N Times, 3 times 100 ms apart
    /// (`77 02 09 00 01 03 64 00`, at word 11).
    const GG_MODES: &str = "770409000101c8000204010104000f00020401000400390102050101020501000400730c000100010001000077020900010364000204010104000f00020401000400390102050101020501000400730c0001000100010000";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// A whole area: `used`, then zeros.
    fn area(used: &str) -> Vec<u8> {
        let mut a = hex(used);
        a.resize(at::MACRO_BYTES, 0);
        a
    }

    fn tap(usage: u8) -> [Event; 2] {
        [true, false].map(|down| Event::Key { usage, down })
    }

    fn wait(ms: u16) -> Event {
        Event::Delay { ms }
    }

    fn click() -> [Event; 2] {
        [true, false].map(|down| Event::Button { button: 1, down })
    }

    /// GG's first macro: A↓ 15 ms A↑ 313 ms B↓ B↑ 3187 ms Btn1↓ Btn1↑.
    fn first() -> Vec<Event> {
        let [a_down, a_up] = tap(0x04);
        let mut e = vec![a_down, wait(15), a_up, wait(313)];
        e.extend(tap(0x05));
        e.push(wait(3187));
        e.extend(click());
        e
    }

    /// GG's second: X Y (219 ms) Z (422 ms), Ctrl+C with 15 and 16 ms inside, 3016 ms, a
    /// click.
    fn second() -> Vec<Event> {
        let mut e = Vec::new();
        e.extend(tap(0x1B));
        e.push(wait(219));
        e.extend(tap(0x1C));
        e.push(wait(219));
        e.extend(tap(0x1D));
        e.push(wait(422));
        let ([ctrl_down, ctrl_up], [c_down, c_up]) = (tap(0xE0), tap(0x06));
        e.extend([
            ctrl_down,
            c_down,
            wait(15),
            c_up,
            wait(16),
            ctrl_up,
            wait(3016),
        ]);
        e.extend(click());
        e
    }

    /// A macro on `key` with GG's numbers (once, 15 ms).
    fn on(key: u8, mode: Mode, events: Vec<Event>) -> SlotMacro {
        played(key, mode, 1, 15, events)
    }

    fn played(
        key: u8,
        mode: Mode,
        times: u8,
        repeat_delay_ms: u16,
        events: Vec<Event>,
    ) -> SlotMacro {
        SlotMacro {
            key,
            mode,
            times,
            repeat_delay_ms,
            events,
        }
    }

    /// apex-30's two keys.
    fn ggs_modes() -> [SlotMacro; 2] {
        [
            played(O, Mode::Toggle, 1, 200, first()),
            played(P, Mode::Times, 3, 100, first()),
        ]
    }

    fn binding(image: &Image, code: u8) -> [u8; 5] {
        let p = place(code).unwrap();
        image.normal_bindings()[p * 5..p * 5 + 5]
            .try_into()
            .unwrap()
    }

    /// `image` with GG's area and bound places put in by hand, as GG's save has them.
    fn as_gg_saved(used: &str, places: &[(u8, [u8; 5])]) -> Image {
        let mut bytes = factory_like("Config 5").serialize();
        bytes[at::MACROS..at::MACROS + at::MACRO_BYTES].copy_from_slice(&area(used));
        for &(code, b) in places {
            let at = at::NORMAL_BINDINGS + place(code).unwrap() * 5;
            bytes[at..at + 5].copy_from_slice(&b);
        }
        let crc = crate::onboard::crc32_stm32(&bytes[..crate::onboard::BODY_BYTES]);
        bytes[at::CRC..].copy_from_slice(&crc.to_le_bytes());
        Image::parse(&bytes).unwrap()
    }

    /// Only the macro area, the given places and the CRC differ between `a` and `b`.
    fn only_macros_and(a: &Image, b: &Image, codes: &[u8]) {
        let places: Vec<usize> = codes.iter().map(|&c| place(c).unwrap()).collect();
        for (i, (x, y)) in a.bytes().iter().zip(b.bytes()).enumerate() {
            let in_area = (at::MACROS..at::MACROS + at::MACRO_BYTES).contains(&i);
            let in_place = places.iter().any(|&p| {
                (at::NORMAL_BINDINGS + p * 5..at::NORMAL_BINDINGS + p * 5 + 5).contains(&i)
            });
            if !in_area && !in_place && i < at::CRC {
                assert_eq!(x, y, "byte {i}");
            }
        }
    }

    #[test]
    fn the_firmware_order_puts_o_and_p_where_gg_bound_them() {
        assert_eq!(place(O), Some(23));
        assert_eq!(place(P), Some(24));
        assert_eq!(place(0x35), Some(0));
        assert_eq!(place(0x4F), Some(92));
        // The Fn key and the media buttons can't have a macro; nor can a key not there.
        for code in [0xF0, 0xF5, 0xFF, 0x00, 0x46, 0x68] {
            assert_eq!(place(code), None, "{code:#04x}");
        }
        // Every key appears once.
        for (i, &c) in FIRMWARE_KEYS.iter().enumerate().filter(|(_, &c)| c != 0) {
            assert!(!FIRMWARE_KEYS[..i].contains(&c), "{c:#04x}");
        }
    }

    #[test]
    fn one_macro_on_p_is_ggs_first_save_byte_for_byte() {
        let old = factory_like("Config 5");
        let new = write(&old, &[on(P, Mode::Once, first())]).unwrap();
        assert_eq!(new.macros(), &area(GG_ONE)[..]);
        assert_eq!(binding(&new, P), [0x71, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(binding(&new, O), [0x51, O, 0, 0, 0], "O untouched");
        only_macros_and(&old, &new, &[P]);
        assert_eq!(new, as_gg_saved(GG_ONE, &[(P, [0x71, 0, 0, 0, 0])]));
    }

    #[test]
    fn two_macros_on_o_and_p_are_ggs_second_save_byte_for_byte() {
        let old = factory_like("Config 5");
        // In any order: they go in by the keys' places, O's first.
        let new = write(
            &old,
            &[on(P, Mode::Once, first()), on(O, Mode::Once, second())],
        )
        .unwrap();
        assert_eq!(new.macros(), &area(GG_TWO)[..]);
        assert_eq!(binding(&new, O), [0x71, 0, 0, 0x00, 0x00]);
        // 8 + 18 × 4 = 80 bytes in: word 20.
        assert_eq!(binding(&new, P), [0x71, 0, 0, 0x14, 0x00]);
        only_macros_and(&old, &new, &[O, P]);
    }

    #[test]
    fn repeat_while_held_on_p_is_ggs_third_save_byte_for_byte() {
        let new = write(
            &factory_like("Config 5"),
            &[on(O, Mode::Once, second()), on(P, Mode::WhileHeld, first())],
        )
        .unwrap();
        assert_eq!(new.macros(), &area(GG_REPEAT)[..]);
    }

    /// apex-30: Play N Times and Toggle Auto-Repeat, with their numbers, byte for byte;
    /// I keeps typing itself, as GG's Text Macro on it isn't stored on the keyboard.
    #[test]
    fn play_n_times_and_toggle_on_p_and_o_are_ggs_apex30_save_byte_for_byte() {
        let old = factory_like("Config 5");
        let [o, p] = ggs_modes();
        let new = write(&old, &[p, o]).unwrap();
        assert_eq!(new.macros(), &area(GG_MODES)[..]);
        assert_eq!(binding(&new, O), [0x71, 0, 0, 0x00, 0x00]);
        // 8 + 9 × 4 = 44 bytes in: word 11.
        assert_eq!(binding(&new, P), [0x71, 0, 0, 0x0B, 0x00]);
        assert_eq!(binding(&new, 0x0C), [0x51, 0x0C, 0, 0, 0], "I");
        only_macros_and(&old, &new, &[O, P]);
        let bound = [(O, [0x71, 0, 0, 0, 0]), (P, [0x71, 0, 0, 0x0B, 0])];
        assert_eq!(new, as_gg_saved(GG_MODES, &bound));
        // And GG's bytes read back as those two.
        assert_eq!(read(&as_gg_saved(GG_MODES, &bound)).unwrap(), ggs_modes());
    }

    /// The numbers go only where a capture shows them: a count with Play N Times, a wait
    /// with it and with Toggle Auto-Repeat; elsewhere GG's 1 and 15 ms.
    #[test]
    fn a_play_modes_numbers_outside_what_gg_writes_are_refused() {
        let old = factory_like("Config 5");
        let cases = [
            (played(P, Mode::Times, 0, 100, first()), "times"),
            (played(P, Mode::Times, 3, 0, first()), "wait"),
            (played(P, Mode::Toggle, 0, 200, first()), "times"),
            (played(P, Mode::Toggle, 2, 200, first()), "times"),
            (played(P, Mode::Toggle, 1, 0, first()), "wait"),
            (played(P, Mode::Once, 3, 15, first()), "times"),
            (played(P, Mode::Once, 1, 100, first()), "wait"),
            (played(P, Mode::WhileHeld, 2, 15, first()), "times"),
            (played(P, Mode::WhileHeld, 1, 200, first()), "wait"),
        ];
        for (m, why) in cases {
            let what = format!("{:?} {} {}", m.mode, m.times, m.repeat_delay_ms);
            let e = format!("{:#}", write(&old, &[m]).unwrap_err());
            assert!(e.contains(why), "{what}: {e}");
        }
        // The ends of the ranges are taken.
        for m in [
            played(P, Mode::Times, 255, 65535, first()),
            played(P, Mode::Times, 1, 1, first()),
            played(P, Mode::Toggle, 1, 1, first()),
        ] {
            let header = write(&old, std::slice::from_ref(&m)).unwrap().macros()[..8].to_vec();
            let [lo, hi] = m.repeat_delay_ms.to_le_bytes();
            assert_eq!(header[4..], [0x01, m.times, lo, hi]);
        }
    }

    #[test]
    fn ggs_saves_read_back_as_the_macros_it_was_given() {
        let one = as_gg_saved(GG_ONE, &[(P, [0x71, 0, 0, 0, 0])]);
        assert_eq!(read(&one).unwrap(), [on(P, Mode::Once, first())]);
        let bound = [(O, [0x71, 0, 0, 0, 0]), (P, [0x71, 0, 0, 0x14, 0])];
        assert_eq!(
            read(&as_gg_saved(GG_TWO, &bound)).unwrap(),
            [on(O, Mode::Once, second()), on(P, Mode::Once, first())]
        );
        assert_eq!(
            read(&as_gg_saved(GG_REPEAT, &bound)).unwrap(),
            [on(O, Mode::Once, second()), on(P, Mode::WhileHeld, first())]
        );
        assert_eq!(read(&factory_like("Config 5")).unwrap(), []);
    }

    #[test]
    fn a_key_whose_macro_is_gone_types_itself_again() {
        let bound = [(O, [0x71, 0, 0, 0, 0]), (P, [0x71, 0, 0, 0x14, 0])];
        let two = as_gg_saved(GG_TWO, &bound);
        let new = write(&two, &[on(P, Mode::Once, first())]).unwrap();
        assert_eq!(binding(&new, O), [0x51, O, 0, 0, 0]);
        assert_eq!(binding(&new, P), [0x71, 0, 0, 0, 0]);
        assert_eq!(
            new.macros(),
            &area(GG_ONE)[..],
            "the rest of the area zeroed"
        );
        only_macros_and(&two, &new, &[O, P]);
        // And with none left, the slot is as it came from the factory.
        let none = write(&new, &[]).unwrap();
        assert_eq!(none, factory_like("Config 5"));
    }

    #[test]
    fn a_slot_with_macros_it_cant_read_is_never_rewritten() {
        let bound = [(P, [0x71, 0, 0, 0, 0])];
        let mut cases: Vec<(Image, &str)> = Vec::new();
        // Another play mode (Toggle Hold's code isn't known: GG greys it out).
        cases.push((
            as_gg_saved(&GG_ONE.replacen("7701", "7705", 1), &bound),
            "mode",
        ));
        // A header whose fifth byte isn't GG's 01.
        cases.push((
            as_gg_saved(&GG_ONE.replacen("01010f00", "02010f00", 1), &bound),
            "header",
        ));
        // A kind of step not seen.
        cases.push((
            as_gg_saved(&GG_ONE.replacen("0204010104", "0504010104", 1), &bound),
            "step",
        ));
        // A key step whose third byte isn't 01.
        cases.push((
            as_gg_saved(&GG_ONE.replacen("02040101", "02040201", 1), &bound),
            "step",
        ));
        // A binding that points where no macro starts.
        cases.push((as_gg_saved(GG_ONE, &[(P, [0x71, 0, 0, 0x01, 0])]), "points"));
        // A binding with other bytes before its start.
        cases.push((
            as_gg_saved(GG_ONE, &[(P, [0x71, 0, 0x01, 0, 0])]),
            "binding",
        ));
        // Bytes after the last macro.
        cases.push((as_gg_saved(&format!("{GG_ONE}ff"), &bound), "after"));
        // A count that runs past the area.
        cases.push((as_gg_saved("7701ff7f01010f00", &[]), "past"));
        // A macro on the Fn layer.
        let mut fn_layer = as_gg_saved(GG_ONE, &bound).serialize();
        fn_layer[at::FN_BINDINGS + 5 * 3] = MACRO;
        let crc = crate::onboard::crc32_stm32(&fn_layer[..crate::onboard::BODY_BYTES]);
        fn_layer[at::CRC..].copy_from_slice(&crc.to_le_bytes());
        cases.push((Image::parse(&fn_layer).unwrap(), "Fn"));
        for (image, why) in cases {
            let e = format!("{:#}", read(&image).unwrap_err());
            assert!(e.starts_with(CANT_READ) && e.contains(why), "{why}: {e}");
            let e = format!("{:#}", write(&image, &[]).unwrap_err());
            assert!(e.starts_with(CANT_READ), "{why}: {e}");
        }
    }

    #[test]
    fn macros_it_cant_write_as_gg_does_are_refused() {
        let old = factory_like("Config 5");
        let one = || first();
        let cases: Vec<(Vec<SlotMacro>, &str)> = vec![
            (
                vec![
                    on(O, Mode::Once, one()),
                    on(P, Mode::Once, one()),
                    on(0x14, Mode::Once, one()),
                ],
                "at most 2",
            ),
            (
                vec![on(P, Mode::Once, one()), on(P, Mode::Once, one())],
                "twice",
            ),
            (vec![on(0xF0, Mode::Once, one())], "can't play a macro"),
            (vec![on(0x46, Mode::Once, one())], "can't play a macro"),
            (vec![on(P, Mode::Once, vec![])], "no steps"),
            (vec![on(P, Mode::Once, vec![wait(0)])], "wait"),
            (
                vec![on(
                    P,
                    Mode::Once,
                    vec![Event::Button {
                        button: 2,
                        down: true,
                    }],
                )],
                "button",
            ),
            (vec![on(P, Mode::Once, tap(0xF0).to_vec())], "usage"),
            // Review R2, K3: no macro longer than 64 steps until one has been tried.
            (vec![on(P, Mode::Once, vec![wait(1); 65])], "64 steps"),
        ];
        for (macros, why) in cases {
            let e = format!("{:#}", write(&old, &macros).unwrap_err());
            assert!(e.contains(why), "{why}: {e}");
        }
        // 64 steps on each of two keys are taken.
        let both = [
            on(O, Mode::Once, vec![wait(1); 64]),
            on(P, Mode::Once, vec![wait(1); 64]),
        ];
        let new = write(&old, &both).unwrap();
        assert_eq!(read(&new).unwrap(), both);
    }

    /// Review R2, K9: the real apex-30 area and normal layer, copied byte for byte from
    /// `~/projects/gg-m11/captures/apex-30.slot5-gg-modes-text.bin` (area = bytes 2704 to
    /// 12304, [`GG_MODES`] then zeros; layer = bytes 80 to 580), read as the modes this
    /// codec writes. Its I key stays `51 0c`: GG's Text Macro on it isn't on the keyboard.
    #[test]
    fn the_real_apex30_image_reads_as_play_n_times_and_toggle() {
        // sha256 of the .bin: f88603a86f503008e4c8dd59cb82616379608f50eace7393b4ea41e172e71b4c
        const APEX30_NORMAL: &str = "5135000000511e000000511f00000051200000005121000000512200000051230000005124000000512500000051260000005127000000512d000000512e0000005189000000512b0000005114000000511a000000510800000051150000005117000000511c0000005118000000510c00000071000000007100000b00512f0000005130000000513100000051390000005104000000511600000051070000005109000000510a000000510b000000510d000000510e000000510f000000513300000051340000005132000000512800000051e10000005164000000511d000000511b00000051060000005119000000510500000051110000005110000000513600000051370000005138000000518700000051e500000051e000000051e300000051e2000000518b000000512c000000518a000000518800000051e600000051e700000051f000000051e4000000512a000000000000000000000000005129000000513a000000513b000000513c000000513d000000513e000000513f0000005140000000514100000051420000005143000000514400000051450000005149000000514a000000514b000000514c000000514d000000514e000000515200000051500000005151000000514f000000000000000061cd00000061b500000061b600000061e200000061e900000061ea000000";
        let mut bytes = factory_like("Config 5").serialize();
        let layer = hex(APEX30_NORMAL);
        assert_eq!(layer.len(), 500);
        bytes[at::NORMAL_BINDINGS..at::NORMAL_BINDINGS + 500].copy_from_slice(&layer);
        bytes[at::MACROS..at::MACROS + at::MACRO_BYTES].copy_from_slice(&area(GG_MODES));
        let crc = crate::onboard::crc32_stm32(&bytes[..crate::onboard::BODY_BYTES]);
        bytes[at::CRC..].copy_from_slice(&crc.to_le_bytes());
        let image = Image::parse(&bytes).unwrap();
        assert_eq!(read(&image).unwrap(), ggs_modes());
        assert_eq!(binding(&image, 0x0C), [0x51, 0x0C, 0, 0, 0], "I");
    }

    #[test]
    fn the_factory_like_image_checks_out_and_has_no_macros() {
        let image = factory_like("Config 5");
        assert_eq!(image.bytes().len(), IMAGE_BYTES);
        assert_eq!(image.name(), "Config 5");
        assert!(image.macros().iter().all(|&b| b == 0));
        assert_eq!(binding(&image, 0x04), [KEYBOARD, 0x04, 0, 0, 0]);
    }

    #[test]
    fn a_refusal_names_what_wasnt_known() {
        let image = as_gg_saved(&GG_ONE.replacen("7701", "7705", 1), &[]);
        let e = format!("{:#}", read(&image).unwrap_err());
        assert!(e.contains("mode 0x05"), "{e}");
    }

    /// Through the slot transaction: with the onboard family off, the allowlist lets
    /// nothing out, the first read included.
    #[test]
    fn with_the_onboard_family_off_a_macros_write_sends_nothing() {
        use crate::onboard::fake::FakeSlots;
        use crate::onboard::{Restore, Step, Timing};
        let slots = FakeSlots::new();
        slots.set_slot(4, factory_like("Config 5").bytes());
        let kb = slots.keyboard();
        let macros = [on(P, Mode::Once, first())];
        let e = kb
            .write_slot(
                4,
                &Timing::KEYBOARD,
                |old| Ok(old.serialize()),
                |old| write(old, &macros),
            )
            .unwrap_err();
        assert_eq!(e.step, Step::Read);
        assert!(matches!(e.restore, Restore::Untouched));
        assert!(format!("{e}").contains("keyboard_onboard"), "{e}");
        assert!(slots.wire().sent().is_empty());
        assert_eq!(slots.slot(4), factory_like("Config 5").serialize());
    }
}
