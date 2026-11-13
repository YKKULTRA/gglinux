// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard macros: named key sequences, the keys that play them, and the onboard slot
//! they were written into.
//!
//! The Apex Pro TKL Gen 3 plays a macro itself, from the profile slot it runs. GG 123 keeps
//! macros in the slot's image and sends nothing live (our captures apex-20 to apex-22,
//! `docs/PROTOCOLS.md` 3f), so a macro works only once it's written into a slot
//! ([`crate::Request::WriteMacros`]).
//!
//! Only what GG's saves show is offered: key, wait and left-button steps, GG's play modes
//! but the one it greys out (Toggle Hold), keys on the normal layer, and at most two keys
//! with a macro. The
//! steps can be written as words ([`Macro::parse_steps`], [`Macro::steps_text`]): `a`,
//! `ctrl+c`, `down:shift`, `up:shift`, `wait:100`, `click:1`, `text:"hello"`. Key names
//! are the ones `ggctl bind` takes ([`MacroEvent::usage_by_name`]).
//!
//! Everything here hangs off the exported types, so other crates reach it through them.

use serde::{Deserialize, Serialize};

/// One step of a macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MacroEvent {
    /// A keyboard key goes down or up, by HID usage.
    Key { code: u8, down: bool },
    /// A mouse button goes down or up.
    Button { button: u8, down: bool },
    /// Wait, in milliseconds.
    Delay { ms: u16 },
}

/// How a macro plays when its key is pressed: GG 123's play modes, in its order (their
/// codes in the slot are `01` to `04`; capture apex-30, `docs/PROTOCOLS.md` 3f). GG's fifth,
/// Toggle Hold, is greyed out in GG and isn't offered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MacroMode {
    /// Once per press (GG's Play Once).
    #[default]
    Once,
    /// [`MacroBinding::times`] times per press, [`MacroBinding::repeat_delay_ms`] apart
    /// (GG's Play N Times).
    Times,
    /// Again and again while the key is held (GG's Repeat While Pressed).
    WhileHeld,
    /// Again and again from one press until the next, [`MacroBinding::repeat_delay_ms`]
    /// apart (GG's Toggle Auto-Repeat).
    Toggle,
}

impl MacroMode {
    /// Every mode, in GG's order.
    pub const ALL: [MacroMode; 4] = [
        MacroMode::Once,
        MacroMode::Times,
        MacroMode::WhileHeld,
        MacroMode::Toggle,
    ];

    /// It plays a set number of times: [`MacroBinding::times`] counts.
    pub fn counts(self) -> bool {
        self == MacroMode::Times
    }

    /// Its wait between plays can be set ([`MacroBinding::repeat_delay_ms`]): the modes a
    /// capture shows with a wait of the user's (Play N Times at 100 ms, Toggle Auto-Repeat
    /// at 200 ms). Repeat While Pressed was only seen with GG's 15 ms, so it keeps that.
    pub fn waits(self) -> bool {
        matches!(self, MacroMode::Times | MacroMode::Toggle)
    }

    /// How it plays, in words.
    pub fn describe(self) -> &'static str {
        match self {
            MacroMode::Once => "once per press",
            MacroMode::Times => "a set number of times per press",
            MacroMode::WhileHeld => "again and again while held",
            MacroMode::Toggle => "again and again until pressed again",
        }
    }
}

/// A named sequence of steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Macro {
    pub name: String,
    pub events: Vec<MacroEvent>,
}

/// A key that plays a macro (on the normal layer), by HID code, and how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroBinding {
    pub code: u8,
    /// The [`Macro::name`] it plays.
    pub name: String,
    #[serde(default)]
    pub mode: MacroMode,
    /// How many times a press plays it with [`MacroMode::Times`], 1 to 255; 1 with the
    /// other modes, as GG writes them.
    #[serde(default = "default_times")]
    pub times: u8,
    /// The wait between two plays, in ms, with the modes that let it be set
    /// ([`MacroMode::waits`]); GG's 15 with the others.
    #[serde(default = "default_delay")]
    pub repeat_delay_ms: u16,
}

fn default_times() -> u8 {
    MacroBinding::DEFAULT_TIMES
}

fn default_delay() -> u16 {
    MacroBinding::DEFAULT_DELAY_MS
}

/// The macros and the keys that play them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroLibrary {
    #[serde(default)]
    pub macros: Vec<Macro>,
    #[serde(default)]
    pub bindings: Vec<MacroBinding>,
}

/// The macros as the daemon keeps them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacrosState {
    pub library: MacroLibrary,
    /// The slot (0..=4) the library was last written into, if it was.
    pub written_slot: Option<u8>,
    /// A write is in progress.
    pub busy: bool,
    /// Why the last write didn't go through, and what was done about it.
    pub error: Option<String>,
    /// The backup of the slot saved before the last write, if one was.
    #[serde(default)]
    pub backup: Option<String>,
    /// Something the last write noticed though it went through (what the keyboard's own
    /// check answered, or that the slot needs loading to take effect).
    #[serde(default)]
    pub note: Option<String>,
}

/// The key that holds the Fn layer from the factory: it keeps that job.
const FN_KEY: u8 = 0xF0;
/// The keyboard usages a step may press.
const USAGES: std::ops::RangeInclusive<u8> = 0x04..=0xE7;

impl MacroLibrary {
    /// The longest name a macro may have, in characters.
    pub const MAX_NAME: usize = 32;
    /// How many keys may have a macro. GG's saves show two; more aren't tried yet.
    pub const MAX_BOUND: usize = 2;
    /// The most steps a macro may have for now: GG's longest in our captures has 18, and
    /// nothing shows what the keyboard does with a long one (review R2, K3). A `text:`
    /// step counts as the taps it makes.
    pub const MAX_STEPS: usize = 64;
    /// Bytes of a slot's macro area (Apex Control's "macro events", 9 600 bytes).
    pub const AREA_BYTES: usize = 9_600;
    /// Bytes of a macro's header in the area (`77 <mode> <count u16> 01 <times> <wait u16>`).
    pub const HEADER_BYTES: usize = 8;
    /// Bytes of one step in the area.
    pub const EVENT_BYTES: usize = 4;
    /// The only mouse button GG's saves show in a keyboard macro: button 1 (`00 01 00 …`).
    /// Whether that byte counts buttons or is a bit mask isn't known, so no other is sent.
    pub const LEFT_BUTTON: u8 = 1;

    /// The macro called `name`.
    pub fn find(&self, name: &str) -> Option<&Macro> {
        self.macros.iter().find(|m| m.name == name)
    }

    /// The bytes the bound macros take in a slot: each key gets its own copy, since the
    /// play mode is part of it.
    pub fn slot_bytes(&self) -> usize {
        self.bindings
            .iter()
            .filter_map(|b| self.find(&b.name))
            .map(|m| Self::HEADER_BYTES + Self::EVENT_BYTES * m.events.len())
            .sum()
    }

    /// Check what can be checked without the keyboard, with a message meant for the user.
    /// Which keys have a place for a macro is the daemon's to check.
    pub fn validate(&self) -> Result<(), String> {
        for (i, m) in self.macros.iter().enumerate() {
            check_name(&m.name)?;
            if self.macros[..i].iter().any(|o| o.name == m.name) {
                return Err(format!("two macros are called '{}'", m.name));
            }
            Macro::check_steps(&m.name, &m.events)?;
        }
        let mut keys = Vec::new();
        for b in &self.bindings {
            let key = MacroBinding::key_name(b.code);
            if b.code == FN_KEY {
                return Err("the Fn key keeps its job: it can't play a macro".into());
            }
            if b.code < 0x04 {
                return Err(format!("{:#04x} is not a key", b.code));
            }
            if keys.contains(&b.code) {
                return Err(format!("key {key} has two macros; a key plays one"));
            }
            keys.push(b.code);
            if self.find(&b.name).is_none() {
                return Err(format!("key {key} plays no macro called '{}'", b.name));
            }
            b.check_play()?;
        }
        if keys.len() > Self::MAX_BOUND {
            return Err(format!(
                "at most {} keys can have a macro for now (GG's saves show two; more aren't tried yet), not {}",
                Self::MAX_BOUND,
                keys.len()
            ));
        }
        let bytes = self.slot_bytes();
        if bytes > Self::AREA_BYTES {
            return Err(format!(
                "the keys' macros take {bytes} bytes, and a slot holds {} (each key keeps its own copy, 8 bytes and 4 per step)",
                Self::AREA_BYTES
            ));
        }
        Ok(())
    }
}

/// A macro's name: something to call it by, short and plain.
fn check_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("a macro needs a name".into());
    }
    if name.chars().count() > MacroLibrary::MAX_NAME {
        return Err(format!(
            "a macro's name is at most {} characters",
            MacroLibrary::MAX_NAME
        ));
    }
    if name.chars().any(char::is_control) {
        return Err("a macro's name can only have plain characters".into());
    }
    Ok(())
}

impl Macro {
    /// Check one macro's steps: something to play, waits of 1 ms or more, only the left
    /// mouse button, keyboard keys only, and every key and button let go by the end.
    pub fn check_steps(name: &str, events: &[MacroEvent]) -> Result<(), String> {
        if events.is_empty() {
            return Err(format!("'{name}' needs at least one step"));
        }
        if events.len() > MacroLibrary::MAX_STEPS {
            return Err(format!(
                "'{name}' has {} steps, and a macro has at most {} for now, until a longer one has been tried on the keyboard (a text step counts each key going down and up)",
                events.len(),
                MacroLibrary::MAX_STEPS
            ));
        }
        // What's held: keys by usage, buttons as 0x100 + button.
        let mut held: Vec<u16> = Vec::new();
        for e in events {
            let (id, what, down) = match *e {
                MacroEvent::Delay { ms: 0 } => return Err(wait_range()),
                MacroEvent::Delay { .. } => continue,
                MacroEvent::Button { button, .. } if button != MacroLibrary::LEFT_BUTTON => {
                    return Err(left_only())
                }
                MacroEvent::Button { button, down } => {
                    (0x100 | u16::from(button), format!("mouse{button}"), down)
                }
                MacroEvent::Key { code, .. } if !USAGES.contains(&code) => {
                    return Err(format!("in '{name}', {code:#04x} is not a keyboard key"))
                }
                MacroEvent::Key { code, down } => (u16::from(code), usage_name(code), down),
            };
            match (down, held.iter().position(|&h| h == id)) {
                (true, None) => held.push(id),
                (true, Some(_)) => {
                    return Err(format!(
                        "in '{name}', {what} goes down again before it comes up"
                    ))
                }
                (false, Some(at)) => {
                    held.remove(at);
                }
                (false, None) => {
                    return Err(format!(
                        "in '{name}', {what} comes up without going down first"
                    ))
                }
            }
        }
        if let Some(&id) = held.first() {
            let what = if id >= 0x100 {
                format!("mouse{}", id & 0xFF)
            } else {
                usage_name(id as u8)
            };
            return Err(format!(
                "'{name}' ends with {what} still held down: add up:{what}"
            ));
        }
        Ok(())
    }

    /// A macro's steps written as words (see the module docs), as [`Macro::steps_text`]
    /// writes them. `text:"…"` types with the US layout and refuses what it can't type.
    pub fn parse_steps(text: &str) -> Result<Vec<MacroEvent>, String> {
        let mut events = Vec::new();
        for word in words(text)? {
            step(&word, &mut events)?;
        }
        if events.is_empty() {
            return Err("a macro needs at least one step".into());
        }
        Ok(events)
    }

    /// `events` as words that [`Macro::parse_steps`] reads back to the same steps.
    pub fn steps_text(events: &[MacroEvent]) -> String {
        let mut out = Vec::new();
        let mut i = 0;
        while i < events.len() {
            if let Some((word, used)) = combination(&events[i..]) {
                out.push(word);
                i += used;
                continue;
            }
            out.push(match events[i] {
                MacroEvent::Delay { ms } => format!("wait:{ms}"),
                MacroEvent::Button { button, down: true }
                    if events.get(i + 1)
                        == Some(&MacroEvent::Button {
                            button,
                            down: false,
                        }) =>
                {
                    i += 1;
                    format!("click:{button}")
                }
                MacroEvent::Button { button, down } => {
                    format!("{}:mouse{button}", if down { "down" } else { "up" })
                }
                MacroEvent::Key { code, down } => {
                    format!("{}:{}", if down { "down" } else { "up" }, usage_name(code))
                }
            });
            i += 1;
        }
        out.join(" ")
    }
}

impl MacroEvent {
    /// A keyboard usage by the name `ggctl bind` takes for it (`a`, `f1`, `ctrl`, `0x39`).
    pub fn usage_by_name(name: &str) -> Option<u8> {
        usage_by_name(name)
    }

    /// A usage's name, as [`MacroEvent::usage_by_name`] reads it; `0x..` for one without
    /// a name.
    pub fn usage_name(code: u8) -> String {
        usage_name(code)
    }
}

impl MacroBinding {
    /// What GG writes for the number of plays where it doesn't count.
    pub const DEFAULT_TIMES: u8 = 1;
    /// GG's wait between plays, in ms, where it isn't set (every save of apex-20 to
    /// apex-22, and the Play Once and Repeat While Pressed keys of apex-30).
    pub const DEFAULT_DELAY_MS: u16 = 15;

    /// Key `code` playing macro `name` in `mode`, with GG's numbers.
    pub fn new(code: u8, name: impl Into<String>, mode: MacroMode) -> MacroBinding {
        MacroBinding {
            code,
            name: name.into(),
            mode,
            times: Self::DEFAULT_TIMES,
            repeat_delay_ms: Self::DEFAULT_DELAY_MS,
        }
    }

    /// Play in `mode`, the numbers it doesn't use put back to GG's.
    pub fn set_mode(&mut self, mode: MacroMode) {
        self.mode = mode;
        if !mode.counts() {
            self.times = Self::DEFAULT_TIMES;
        }
        if !mode.waits() {
            self.repeat_delay_ms = Self::DEFAULT_DELAY_MS;
        }
    }

    /// How it plays, in words: "once per press", "3 times per press, 100 ms apart".
    pub fn play_text(&self) -> String {
        let how = match self.mode {
            MacroMode::Times if self.times == 1 => "1 time per press".to_owned(),
            MacroMode::Times => format!("{} times per press", self.times),
            mode => mode.describe().to_owned(),
        };
        if self.mode.waits() {
            format!("{how}, {} ms apart", self.repeat_delay_ms)
        } else {
            how
        }
    }

    /// The play mode's numbers: only those the mode uses may differ from GG's, as no
    /// capture shows another.
    fn check_play(&self) -> Result<(), String> {
        let key = Self::key_name(self.code);
        let how = self.mode.describe();
        if self.times == 0 {
            return Err(format!("key {key}: a macro plays 1 to 255 times"));
        }
        if !self.mode.counts() && self.times != Self::DEFAULT_TIMES {
            return Err(format!(
                "key {key} plays its macro {how}, so it takes no number of times"
            ));
        }
        if self.repeat_delay_ms == 0 {
            return Err(format!(
                "key {key}: the wait between plays is 1 to 65535 ms"
            ));
        }
        if !self.mode.waits() && self.repeat_delay_ms != Self::DEFAULT_DELAY_MS {
            return Err(format!(
                "key {key} plays its macro {how}, so it keeps GG's {} ms between plays",
                Self::DEFAULT_DELAY_MS
            ));
        }
        Ok(())
    }

    /// A key by the name `ggctl bind` takes for it: as [`MacroEvent::usage_by_name`], and
    /// `menu` and `fn` are the keys with those labels.
    pub fn key_by_name(name: &str) -> Option<u8> {
        match name.to_ascii_lowercase().as_str() {
            "menu" => Some(0xE7),
            "fn" | "steelseries" => Some(FN_KEY),
            _ => usage_by_name(name),
        }
    }

    /// A key's name, as [`MacroBinding::key_by_name`] reads it.
    pub fn key_name(code: u8) -> String {
        match code {
            0xE7 => "menu".into(),
            FN_KEY => "fn".into(),
            c => usage_name(c),
        }
    }
}

fn wait_range() -> String {
    "a wait is 1 to 65535 ms".into()
}

fn left_only() -> String {
    "only the left mouse button (click:1) is known to work in a keyboard macro so far".into()
}

/// The words of a steps text: split at spaces, but a `text:"…"` is one word, its quotes
/// and escapes taken off (`\"`, `\\`, `\n` for Enter, `\t` for Tab). A text word comes
/// back as `text:` and what it types.
fn words(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            return Ok(out);
        }
        let mut word = String::new();
        while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
            word.push(c);
            if word == "text:" && chars.peek() == Some(&'"') {
                chars.next();
                word.push_str(&quoted(&mut chars)?);
                break;
            }
        }
        out.push(word);
    }
}

/// What a quoted text holds, up to its closing quote, escapes taken off.
fn quoted(chars: &mut impl Iterator<Item = char>) -> Result<String, String> {
    let mut text = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Ok(text),
            '\\' => text.push(match chars.next() {
                Some('n') => '\n',
                Some('t') => '\t',
                Some(other) => other,
                None => break,
            }),
            c => text.push(c),
        }
    }
    Err("a text:\"…\" needs its closing quote".into())
}

/// Add the steps one word makes.
fn step(word: &str, events: &mut Vec<MacroEvent>) -> Result<(), String> {
    let lower = word.to_ascii_lowercase();
    if let Some(text) = word.strip_prefix("text:") {
        return type_text(text, events);
    }
    if let Some(ms) = lower.strip_prefix("wait:") {
        let ms: u16 = ms.parse().map_err(|_| wait_range())?;
        if ms == 0 {
            return Err(wait_range());
        }
        events.push(MacroEvent::Delay { ms });
        return Ok(());
    }
    if let Some(button) = lower.strip_prefix("click:") {
        let button = button_of(button)?;
        events.extend([true, false].map(|down| MacroEvent::Button { button, down }));
        return Ok(());
    }
    for (prefix, down) in [("down:", true), ("up:", false)] {
        if let Some(name) = lower.strip_prefix(prefix) {
            events.push(match name.strip_prefix("mouse") {
                Some(button) if !button.is_empty() => MacroEvent::Button {
                    button: button_of(button)?,
                    down,
                },
                _ => MacroEvent::Key {
                    code: usage_of(name)?,
                    down,
                },
            });
            return Ok(());
        }
    }
    let codes = lower
        .split('+')
        .map(usage_of)
        .collect::<Result<Vec<u8>, String>>()?;
    for (i, c) in codes.iter().enumerate() {
        if codes[..i].contains(c) {
            return Err(format!("'{word}' has {} twice", usage_name(*c)));
        }
    }
    events.extend(
        codes
            .iter()
            .map(|&code| MacroEvent::Key { code, down: true }),
    );
    events.extend(
        codes
            .iter()
            .rev()
            .map(|&code| MacroEvent::Key { code, down: false }),
    );
    Ok(())
}

fn usage_of(name: &str) -> Result<u8, String> {
    usage_by_name(name).ok_or_else(|| {
        format!("'{name}' is not a key name (try: a, f1, ctrl+c, wait:100, click:1, text:\"hi\")")
    })
}

fn button_of(word: &str) -> Result<u8, String> {
    match word {
        "1" | "left" => Ok(MacroLibrary::LEFT_BUTTON),
        _ => Err(left_only()),
    }
}

/// Tap each character of `text` as the US layout types it, Shift held for the ones on a
/// key's upper half.
fn type_text(text: &str, events: &mut Vec<MacroEvent>) -> Result<(), String> {
    for c in text.chars() {
        let (code, shift) = us_key(c).ok_or_else(|| {
            format!("'{c}' can't be typed with the US layout, so a text step can't have it")
        })?;
        let mut codes = vec![code];
        if shift {
            codes.insert(0, SHIFT);
        }
        events.extend(
            codes
                .iter()
                .map(|&code| MacroEvent::Key { code, down: true }),
        );
        events.extend(
            codes
                .iter()
                .rev()
                .map(|&code| MacroEvent::Key { code, down: false }),
        );
    }
    Ok(())
}

const SHIFT: u8 = 0xE1;

/// The key that types `c` on a US keyboard, and whether Shift goes with it.
fn us_key(c: char) -> Option<(u8, bool)> {
    const DIGITS: &str = "1234567890";
    const SHIFTED_DIGITS: &str = "!@#$%^&*()";
    // Unshifted and shifted characters of the keys from `-` (0x2D) to `/` (0x38).
    const PUNCT: [(char, char, u8); 11] = [
        ('-', '_', 0x2D),
        ('=', '+', 0x2E),
        ('[', '{', 0x2F),
        (']', '}', 0x30),
        ('\\', '|', 0x31),
        (';', ':', 0x33),
        ('\'', '"', 0x34),
        ('`', '~', 0x35),
        (',', '<', 0x36),
        ('.', '>', 0x37),
        ('/', '?', 0x38),
    ];
    let code = |i: usize, base: u8| base + i as u8;
    Some(match c {
        'a'..='z' => (code(c as usize - 'a' as usize, 0x04), false),
        'A'..='Z' => (code(c as usize - 'A' as usize, 0x04), true),
        '\n' => (0x28, false),
        '\t' => (0x2B, false),
        ' ' => (0x2C, false),
        _ => {
            if let Some(i) = DIGITS.find(c) {
                (code(i, 0x1E), false)
            } else if let Some(i) = SHIFTED_DIGITS.find(c) {
                (code(i, 0x1E), true)
            } else {
                let &(plain, _, key) = PUNCT.iter().find(|p| p.0 == c || p.1 == c)?;
                (key, c != plain)
            }
        }
    })
}

/// Keys pressed together at the start of `events`, written `a+b+c`: downs in order, then
/// their ups in reverse, the most keys that fit. With how many steps it took.
fn combination(events: &[MacroEvent]) -> Option<(String, usize)> {
    let downs: Vec<u8> = events
        .iter()
        .map_while(|e| match *e {
            MacroEvent::Key { code, down: true } => Some(code),
            _ => None,
        })
        .collect();
    (1..=downs.len()).rev().find_map(|n| {
        let keys = &downs[..n];
        if (0..n).any(|i| keys[..i].contains(&keys[i])) {
            return None;
        }
        let ups = events.get(n..2 * n)?;
        let reversed = keys.iter().rev().zip(ups).all(|(&k, e)| {
            *e == MacroEvent::Key {
                code: k,
                down: false,
            }
        });
        reversed.then(|| {
            let names: Vec<String> = keys.iter().map(|&k| usage_name(k)).collect();
            (names.join("+"), 2 * n)
        })
    })
}

/// HID keyboard usages by name, as `ggctl bind` names them.
const NAMES: [(&str, u8); 101] = [
    ("a", 0x04),
    ("b", 0x05),
    ("c", 0x06),
    ("d", 0x07),
    ("e", 0x08),
    ("f", 0x09),
    ("g", 0x0A),
    ("h", 0x0B),
    ("i", 0x0C),
    ("j", 0x0D),
    ("k", 0x0E),
    ("l", 0x0F),
    ("m", 0x10),
    ("n", 0x11),
    ("o", 0x12),
    ("p", 0x13),
    ("q", 0x14),
    ("r", 0x15),
    ("s", 0x16),
    ("t", 0x17),
    ("u", 0x18),
    ("v", 0x19),
    ("w", 0x1A),
    ("x", 0x1B),
    ("y", 0x1C),
    ("z", 0x1D),
    ("1", 0x1E),
    ("2", 0x1F),
    ("3", 0x20),
    ("4", 0x21),
    ("5", 0x22),
    ("6", 0x23),
    ("7", 0x24),
    ("8", 0x25),
    ("9", 0x26),
    ("0", 0x27),
    ("enter", 0x28),
    ("esc", 0x29),
    ("backspace", 0x2A),
    ("tab", 0x2B),
    ("space", 0x2C),
    ("minus", 0x2D),
    ("equal", 0x2E),
    ("lbracket", 0x2F),
    ("rbracket", 0x30),
    ("backslash", 0x31),
    ("hash", 0x32),
    ("semicolon", 0x33),
    ("quote", 0x34),
    ("grave", 0x35),
    ("comma", 0x36),
    ("period", 0x37),
    ("slash", 0x38),
    ("capslock", 0x39),
    ("f1", 0x3A),
    ("f2", 0x3B),
    ("f3", 0x3C),
    ("f4", 0x3D),
    ("f5", 0x3E),
    ("f6", 0x3F),
    ("f7", 0x40),
    ("f8", 0x41),
    ("f9", 0x42),
    ("f10", 0x43),
    ("f11", 0x44),
    ("f12", 0x45),
    ("printscreen", 0x46),
    ("scrolllock", 0x47),
    ("pause", 0x48),
    ("insert", 0x49),
    ("home", 0x4A),
    ("pageup", 0x4B),
    ("delete", 0x4C),
    ("end", 0x4D),
    ("pagedown", 0x4E),
    ("right", 0x4F),
    ("left", 0x50),
    ("down", 0x51),
    ("up", 0x52),
    ("iso", 0x64),
    ("application", 0x65),
    ("f13", 0x68),
    ("f14", 0x69),
    ("f15", 0x6A),
    ("f16", 0x6B),
    ("f17", 0x6C),
    ("f18", 0x6D),
    ("f19", 0x6E),
    ("f20", 0x6F),
    ("f21", 0x70),
    ("f22", 0x71),
    ("f23", 0x72),
    ("f24", 0x73),
    ("ctrl", 0xE0),
    ("shift", 0xE1),
    ("alt", 0xE2),
    ("super", 0xE3),
    ("rctrl", 0xE4),
    ("rshift", 0xE5),
    ("ralt", 0xE6),
    ("rsuper", 0xE7),
];

/// Other names people type, as `ggctl bind` takes them.
const ALIASES: [(&str, &str); 16] = [
    ("caps", "capslock"),
    ("escape", "esc"),
    ("return", "enter"),
    ("del", "delete"),
    ("ins", "insert"),
    ("pgup", "pageup"),
    ("pgdn", "pagedown"),
    ("prtsc", "printscreen"),
    ("control", "ctrl"),
    ("lctrl", "ctrl"),
    ("lshift", "shift"),
    ("lalt", "alt"),
    ("win", "super"),
    ("lsuper", "super"),
    ("altgr", "ralt"),
    ("backtick", "grave"),
];

fn usage_by_name(name: &str) -> Option<u8> {
    let name = name.to_ascii_lowercase();
    let name = ALIASES
        .iter()
        .find(|a| a.0 == name)
        .map_or(name.as_str(), |a| a.1);
    if let Some(hex) = name.strip_prefix("0x") {
        return u8::from_str_radix(hex, 16).ok();
    }
    NAMES.iter().find(|u| u.0 == name).map(|u| u.1)
}

fn usage_name(code: u8) -> String {
    NAMES
        .iter()
        .find(|u| u.1 == code)
        .map_or_else(|| format!("{code:#04x}"), |u| u.0.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use MacroEvent::{Button, Delay, Key};

    fn down(code: u8) -> MacroEvent {
        Key { code, down: true }
    }

    fn up(code: u8) -> MacroEvent {
        Key { code, down: false }
    }

    fn tap(code: u8) -> [MacroEvent; 2] {
        [down(code), up(code)]
    }

    fn wait(ms: u16) -> MacroEvent {
        Delay { ms }
    }

    fn click() -> [MacroEvent; 2] {
        [
            Button {
                button: 1,
                down: true,
            },
            Button {
                button: 1,
                down: false,
            },
        ]
    }

    fn parse(text: &str) -> Result<Vec<MacroEvent>, String> {
        Macro::parse_steps(text)
    }

    fn text(events: &[MacroEvent]) -> String {
        Macro::steps_text(events)
    }

    /// GG's first save (apex-20): A↓ 15 ms A↑ 313 ms B↓ B↑ 3187 ms Btn1↓ Btn1↑.
    fn gg_first() -> Vec<MacroEvent> {
        let mut e = vec![down(0x04), wait(15), up(0x04), wait(313)];
        e.extend(tap(0x05));
        e.push(wait(3187));
        e.extend(click());
        e
    }

    fn library(macros: &[(&str, Vec<MacroEvent>)], bound: &[(u8, &str)]) -> MacroLibrary {
        MacroLibrary {
            macros: macros
                .iter()
                .map(|(name, events)| Macro {
                    name: (*name).into(),
                    events: events.clone(),
                })
                .collect(),
            bindings: bound
                .iter()
                .map(|&(code, name)| MacroBinding::new(code, name, MacroMode::Once))
                .collect(),
        }
    }

    #[test]
    fn the_steps_read_as_words_and_back() {
        assert_eq!(
            text(&gg_first()),
            "down:a wait:15 up:a wait:313 b wait:3187 click:1"
        );
        assert_eq!(
            parse("down:a wait:15 up:a wait:313 b wait:3187 click:1").unwrap(),
            gg_first()
        );
        // A combination goes down in order and up in reverse.
        let copy = [down(0xE0), down(0x06), up(0x06), up(0xE0)];
        assert_eq!(parse("ctrl+c").unwrap(), copy);
        assert_eq!(text(&copy), "ctrl+c");
        // Names as `ggctl bind` takes them, aliases and hex too, in any case.
        assert_eq!(
            parse("Escape 0x39 Control+Shift+F12").unwrap(),
            [
                down(0x29),
                up(0x29),
                down(0x39),
                up(0x39),
                down(0xE0),
                down(0xE1),
                down(0x45),
                up(0x45),
                up(0xE1),
                up(0xE0)
            ]
        );
        // A button held down on its own.
        let drag = parse("down:mouse1 wait:200 up:mouse1").unwrap();
        assert_eq!(text(&drag), "down:mouse1 wait:200 up:mouse1");
        // Whatever the steps, the words give them back.
        let odd = vec![
            down(0xE0),
            down(0x04),
            up(0x04),
            down(0x05),
            up(0x05),
            up(0xE0),
            up(0x07),
        ];
        assert_eq!(text(&odd), "down:ctrl a b up:ctrl up:d");
        assert_eq!(parse(&text(&odd)).unwrap(), odd);
    }

    #[test]
    fn text_types_with_the_us_layout() {
        let hi = parse(r#"text:"Hi, 2!""#).unwrap();
        let mut want = vec![down(0xE1), down(0x0B), up(0x0B), up(0xE1)];
        want.extend(tap(0x0C));
        want.extend(tap(0x36));
        want.extend(tap(0x2C));
        want.extend(tap(0x1F));
        want.extend([down(0xE1), down(0x1E), up(0x1E), up(0xE1)]);
        assert_eq!(hi, want);
        // Without quotes, up to the next space; escapes for a quote, Enter and Tab.
        assert_eq!(parse("text:ok").unwrap(), parse("o k").unwrap());
        assert_eq!(
            parse(r#"text:"\"\n\t\\" wait:5"#).unwrap(),
            parse("shift+quote enter tab backslash wait:5").unwrap()
        );
        let e = parse("text:\"héllo\"").unwrap_err();
        assert!(e.contains("'é'") && e.contains("US"), "{e}");
        assert!(parse("text:\"open").unwrap_err().contains("quote"));
    }

    #[test]
    fn steps_gglinux_cant_send_are_refused_with_a_reason() {
        for (words, why) in [
            ("", "at least one step"),
            ("click:2", "left mouse button"),
            ("down:mouse3", "left mouse button"),
            ("wait:0", "1 to 65535"),
            ("wait:70000", "1 to 65535"),
            ("wait:soon", "1 to 65535"),
            ("frobnicate", "'frobnicate' is not a key name"),
            ("ctrl+ctrl", "twice"),
            ("down:", "key name"),
        ] {
            let e = parse(words).unwrap_err();
            assert!(e.contains(why), "{words:?}: {e}");
        }
    }

    #[test]
    fn a_library_checks_out_or_says_why_not() {
        library(&[("copy", parse("ctrl+c").unwrap())], &[(0x13, "copy")])
            .validate()
            .unwrap();
        let ok = parse("a").unwrap();
        let long_name = "n".repeat(33);
        let cases: Vec<(MacroLibrary, &str)> = vec![
            (library(&[("", ok.clone())], &[]), "needs a name"),
            (
                library(&[("x", ok.clone()), ("x", ok.clone())], &[]),
                "two macros are called 'x'",
            ),
            (library(&[(&long_name, ok.clone())], &[]), "at most 32"),
            (library(&[("x\ty", ok.clone())], &[]), "plain characters"),
            (library(&[("x", vec![])], &[]), "at least one step"),
            (library(&[("x", vec![wait(0)])], &[]), "1 to 65535"),
            (
                library(
                    &[(
                        "x",
                        vec![Button {
                            button: 2,
                            down: true,
                        }],
                    )],
                    &[],
                ),
                "left mouse button",
            ),
            (
                library(&[("x", vec![down(0xF0)])], &[]),
                "not a keyboard key",
            ),
            (
                library(&[("x", vec![down(0xE1), down(0x04), up(0x04)])], &[]),
                "shift still held down",
            ),
            (library(&[("x", vec![up(0x04)])], &[]), "without going down"),
            (
                library(&[("x", vec![down(0x04), down(0x04)])], &[]),
                "goes down again",
            ),
            (
                library(&[("x", ok.clone())], &[(0x13, "y")]),
                "no macro called 'y'",
            ),
            (
                library(&[("x", ok.clone())], &[(0x13, "x"), (0x13, "x")]),
                "has two macros",
            ),
            (library(&[("x", ok.clone())], &[(0xF0, "x")]), "Fn key"),
            (library(&[("x", ok.clone())], &[(0x02, "x")]), "not a key"),
            (
                library(
                    &[("x", ok.clone())],
                    &[(0x12, "x"), (0x13, "x"), (0x14, "x")],
                ),
                "at most 2 keys",
            ),
        ];
        for (lib, why) in cases {
            let e = lib.validate().unwrap_err();
            assert!(e.contains(why), "{lib:?}: {e}");
        }
    }

    /// Review R2, K3: at most 64 steps a macro until a long one has been tried on the
    /// keyboard (GG's longest in our captures has 18); a text counts as the taps it makes.
    #[test]
    fn a_macro_has_at_most_64_steps_for_now() {
        let mut long = Vec::new();
        for _ in 0..32 {
            long.extend(tap(0x04));
        }
        let lib = library(&[("long", long.clone())], &[(0x13, "long")]);
        lib.validate().unwrap();
        assert_eq!(lib.slot_bytes(), 8 + 64 * 4);
        long.push(wait(5));
        let e = library(&[("long", long)], &[]).validate().unwrap_err();
        assert!(e.contains("65 steps") && e.contains("at most 64"), "{e}");
        // 22 capitals are 4 steps each: 88.
        let text = parse(r#"text:"ABCDEFGHIJKLMNOPQRSTUV""#).unwrap();
        assert_eq!(text.len(), 88);
        let e = library(&[("shout", text)], &[]).validate().unwrap_err();
        assert!(e.contains("88 steps"), "{e}");
    }

    #[test]
    fn names_are_the_ones_ggctl_bind_takes() {
        assert_eq!(MacroEvent::usage_by_name("A"), Some(0x04));
        assert_eq!(MacroEvent::usage_by_name("pgdn"), Some(0x4E));
        assert_eq!(MacroEvent::usage_by_name("0x39"), Some(0x39));
        assert_eq!(
            MacroEvent::usage_by_name("menu"),
            None,
            "a key, not a usage"
        );
        assert_eq!(MacroBinding::key_by_name("menu"), Some(0xE7));
        assert_eq!(MacroBinding::key_by_name("fn"), Some(0xF0));
        assert_eq!(MacroEvent::usage_name(0x2D), "minus");
        assert_eq!(MacroEvent::usage_name(0x99), "0x99");
        assert_eq!(MacroBinding::key_name(0xE7), "menu");
        assert_eq!(MacroBinding::key_name(0x13), "p");
    }

    #[test]
    fn a_state_from_the_contract_reads_without_the_new_fields() {
        let state: MacrosState = serde_json::from_str(
            r#"{"library":{"macros":[],"bindings":[]},"written_slot":null,"busy":false,"error":null}"#,
        )
        .unwrap();
        assert_eq!(state.backup, None);
        assert_eq!(state.note, None);
        let json = serde_json::to_string(&MacroEvent::Delay { ms: 15 }).unwrap();
        assert_eq!(json, r#"{"kind":"delay","ms":15}"#);
    }

    /// A binding saved before the play modes had numbers reads with GG's defaults, and the
    /// new modes have plain names.
    #[test]
    fn a_binding_without_its_numbers_reads_with_ggs_defaults() {
        let b: MacroBinding =
            serde_json::from_str(r#"{"code":19,"name":"x","mode":"while_held"}"#).unwrap();
        assert_eq!(
            b,
            MacroBinding {
                code: 0x13,
                name: "x".into(),
                mode: MacroMode::WhileHeld,
                times: 1,
                repeat_delay_ms: 15,
            }
        );
        assert_eq!(b, MacroBinding::new(0x13, "x", MacroMode::WhileHeld));
        let json = serde_json::to_value(MacroMode::ALL).unwrap();
        assert_eq!(
            json,
            serde_json::json!(["once", "times", "while_held", "toggle"])
        );
    }

    /// GG's apex-30 save: P plays Macro 1 three times, 100 ms apart; O toggles it with
    /// 200 ms between plays. Both check out; the numbers a mode doesn't use stay GG's.
    #[test]
    fn the_play_modes_numbers_check_out_where_the_mode_uses_them() {
        let mut p = MacroBinding::new(0x13, "x", MacroMode::Times);
        p.times = 3;
        p.repeat_delay_ms = 100;
        let mut o = MacroBinding::new(0x12, "x", MacroMode::Toggle);
        o.repeat_delay_ms = 200;
        let mut lib = library(&[("x", parse("a").unwrap())], &[]);
        lib.bindings = vec![o.clone(), p.clone()];
        lib.validate().unwrap();
        assert_eq!(p.play_text(), "3 times per press, 100 ms apart");
        assert_eq!(
            o.play_text(),
            "again and again until pressed again, 200 ms apart"
        );
        let once = MacroBinding::new(0x13, "x", MacroMode::Once);
        assert_eq!(once.play_text(), "once per press");
        let mut one = p.clone();
        one.times = 1;
        assert_eq!(one.play_text(), "1 time per press, 100 ms apart");
        let bad = |b: MacroBinding| {
            let mut lib = lib.clone();
            lib.bindings = vec![b];
            lib.validate().unwrap_err()
        };
        let mut zero = p.clone();
        zero.times = 0;
        assert!(bad(zero).contains("1 to 255 times"));
        let mut no_wait = o.clone();
        no_wait.repeat_delay_ms = 0;
        assert!(bad(no_wait).contains("1 to 65535 ms"));
        for mode in [MacroMode::Once, MacroMode::WhileHeld, MacroMode::Toggle] {
            let mut b = MacroBinding::new(0x13, "x", mode);
            b.times = 3;
            let e = bad(b);
            assert!(e.contains("takes no number of times"), "{mode:?}: {e}");
        }
        for mode in [MacroMode::Once, MacroMode::WhileHeld] {
            let mut b = MacroBinding::new(0x13, "x", mode);
            b.repeat_delay_ms = 100;
            let e = bad(b);
            assert!(e.contains("keeps GG's 15 ms"), "{mode:?}: {e}");
        }
        // A new mode puts back the numbers it doesn't use.
        let mut back = p.clone();
        back.set_mode(MacroMode::Toggle);
        assert_eq!((back.times, back.repeat_delay_ms), (1, 100));
        back.set_mode(MacroMode::Once);
        assert_eq!(back, MacroBinding::new(0x13, "x", MacroMode::Once));
    }
}
