// SPDX-License-Identifier: GPL-3.0-or-later
//! Key bindings: what each key does on its own, while Fn is held and past its second
//! actuation point, the key that holds Fn and the keys lit while it's held.
//!
//! From Apex Control's protocol reference (github.com/Rikearon/apex-control,
//! `docs/PROTOCOL.md` at `f7db8c5`, CC BY 4.0), "Key bindings": `36` writes a layer, `B6`
//! reads it back, `35` picks the Fn key, `3C` sets the Fn-lit keys, and `38 61 44 02` is the
//! second actuation point's table. All of them write the keyboard's RAM only. The
//! allowlist lets them out only with the bindings family on ([`crate::allow`]).
//!
//! A layer is written whole: one 6-byte [`Block`] (`key, function, code × 4`) per key, as
//! the keyboard reported it on a read, with the user's changes on top ([`merge`]). The
//! keyboard's own shortcuts on the Fn layer (function `62`) can't be made again once
//! erased, so a merge keeps every one of them byte for byte.
//!
//! Not checked on hardware yet: which 91 keys a layer has ([`KEYS`]) and the `FF FF` of a
//! key without a second actuation point ([`second_actuation_table`]).

use anyhow::{bail, ensure, Context, Result};

use crate::allow::{
    BLOCK, BLOCKS, FN_MASK_BYTES, OP_BINDINGS, OP_FN_KEY, OP_FN_MASK, OP_READ_BINDINGS,
    SECOND_LAYER,
};
use crate::proto::{self, ANALOG_KEYS, COMMAND_REPORT, FEATURE_REPORT, OP_SWITCHES, SW_THRESHOLDS};
use crate::Keyboard;

/// The 91 keys with a binding on the normal and Fn layers, by HID code, in the order they
/// are asked for. The source gives the count, not the list. This is every key with a
/// light (`04`–`45`, `49`–`52`, `E0`–`E7`, `F0`) and every key with an adjustable switch
/// (adding the ISO and JIS keys `64`, `87`–`8B`): 92 keys. The one left out is `FB`, the
/// play/pause button beside the screen, taken to be hard-wired like the other media
/// buttons. A wrong guess doesn't always make a read fail: the keyboard answers only for
/// the keys asked, so a key missing here goes unnoticed, and one it lacks might still get
/// a block. The list is checked read-only against a slot image before the first write.
pub const KEYS: [u8; 91] = [
    0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13,
    0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23,
    0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x31, 0x32, 0x33,
    0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43,
    0x44, 0x45, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0x64, 0x87, 0x88, 0x89,
    0x8A, 0x8B, 0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xF0,
];

/// A block's function byte: the key does nothing.
pub const UNBOUND: u8 = 0x00;
/// Mouse buttons 1 to 8 are functions `01` to `08`.
pub const MOUSE_BUTTONS: std::ops::RangeInclusive<u8> = 0x01..=0x08;
pub const CPI: u8 = 0x30;
pub const WHEEL_UP: u8 = 0x31;
pub const WHEEL_DOWN: u8 = 0x32;
pub const PAN_LEFT: u8 = 0x33;
pub const PAN_RIGHT: u8 = 0x34;
/// Up to four keyboard usages sent together; modifiers are usages `E0`–`E7`.
pub const KEYBOARD: u8 = 0x51;
/// A consumer-page usage, little-endian in the first two code bytes.
pub const CONSUMER: u8 = 0x61;
/// One of the keyboard's own functions: opaque, kept as read.
pub const FIRMWARE: u8 = 0x62;
pub const MACRO: u8 = 0x71;
/// Host-assisted ("EXTERNAL"): opaque, kept as read.
pub const EXTERNAL: u8 = 0x72;

/// The keyboard usages a key may send.
pub const USAGES: std::ops::RangeInclusive<u8> = 0x04..=0xE7;
/// The most usages one key sends at once.
pub const MAX_USAGES: usize = 4;
/// The most blocks one report holds.
const MAX_BLOCKS: usize = *BLOCKS.end() as usize;
/// `(h, l)` for a key without a second actuation point, as the flash image marks one.
/// Unverified for the RAM table until a capture shows it; the gate stays off till then.
pub const NO_SECOND_POINT: (u8, u8) = (0xFF, 0xFF);
/// The key that holds the Fn layer from the factory: the SteelSeries key (the flash
/// image's "Fn trigger key … factory `0xF0`").
pub const STOCK_FN_KEY: u8 = 0xF0;
/// How long a read waits before it asks again, when the keyboard's answer was still the
/// one to the command before ("the feature read slot holds the response to the last
/// command"). The source gives no time.
const READ_RETRY: std::time::Duration = std::time::Duration::from_millis(20);
/// How long the keyboard gets to put a read's answer in the read slot before it's read.
/// Read with no wait, the owner's keyboard still held the layer read just before ("answered
/// for layer 0 when asked for the Fn layer", 2026-11-13). Apex Control's code waits 13 ms
/// after each query.
const READ_SETTLE: std::time::Duration = std::time::Duration::from_millis(13);
/// How often a read is asked in all while the read slot still holds an earlier answer.
const READ_TRIES: usize = 4;

/// What a stock keyboard's Fn layer holds, as Apex Control read it: F9–F12, Left GUI, O,
/// I, T and Q run the keyboard's own shortcuts (brightness, media, the screen menu). For
/// fakes and tests only: gglinux never writes these from here, it keeps what the keyboard
/// reports.
pub const STOCK_FN_LAYER: [(u8, [u8; 4]); 9] = [
    (0x42, [0x01, 0x00, 0x00, 0x00]),
    (0x43, [0x02, 0x01, 0x00, 0x00]),
    (0x44, [0x03, 0x01, 0x00, 0x00]),
    (0x45, [0x04, 0x01, 0x00, 0x00]),
    (0xE3, [0x05, 0x00, 0x00, 0x00]),
    (0x12, [0x06, 0x00, 0x00, 0x00]),
    (0x0C, [0x07, 0x00, 0x00, 0x00]),
    (0x17, [0x08, 0x00, 0x00, 0x00]),
    (0x14, [0x0A, 0x00, 0x00, 0x00]),
];

/// One of the keyboard's three binding layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    /// What a key does on its own.
    Normal,
    /// What it does while the Fn key is held.
    Fn,
    /// What an adjustable key does past its second actuation point.
    Second,
}

impl Layer {
    pub const ALL: [Layer; 3] = [Layer::Normal, Layer::Fn, Layer::Second];

    /// The layer's byte in `36` and `B6`.
    pub fn byte(self) -> u8 {
        match self {
            Layer::Normal => 0x00,
            Layer::Fn => 0x01,
            Layer::Second => SECOND_LAYER,
        }
    }

    /// The keys the layer has, in the order they are asked for: 91, or the 68 adjustable
    /// keys past their second point.
    pub fn keys(self) -> &'static [u8] {
        match self {
            Layer::Normal | Layer::Fn => &KEYS,
            Layer::Second => &ANALOG_KEYS,
        }
    }

    /// The layer's name in messages.
    pub fn name(self) -> &'static str {
        match self {
            Layer::Normal => "normal",
            Layer::Fn => "Fn",
            Layer::Second => "deep-press",
        }
    }
}

/// What a key does, decoded from its block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Unbound,
    /// Mouse button 1 to 8.
    Mouse(u8),
    Cpi,
    WheelUp,
    WheelDown,
    PanLeft,
    PanRight,
    /// One to four keyboard usages sent together.
    Keys(Vec<u8>),
    /// A consumer-page usage (media keys).
    Consumer(u16),
    /// One of the keyboard's own functions; the four bytes as read.
    Firmware([u8; 4]),
    /// A macro, by slot (the first code byte); its format isn't known.
    Macro(u8),
    /// Host-assisted; the four bytes as read.
    External([u8; 4]),
    /// A function byte the source doesn't name, as read.
    Other {
        function: u8,
        codes: [u8; 4],
    },
}

impl Action {
    /// Whether this can only be kept as the keyboard reported it, never made up.
    pub fn kept_only(&self) -> bool {
        matches!(
            self,
            Action::Firmware(_) | Action::Macro(_) | Action::External(_) | Action::Other { .. }
        )
    }
}

/// One key's binding on one layer, as the keyboard holds it: `key, function, code × 4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub code: u8,
    pub function: u8,
    pub codes: [u8; 4],
}

impl Block {
    /// The factory binding: the key itself on the normal layer (`51 <key> 00 00 00`),
    /// nothing (`00`) on the others.
    pub fn default_for(layer: Layer, code: u8) -> Block {
        match layer {
            Layer::Normal => Block {
                code,
                function: KEYBOARD,
                codes: [code, 0, 0, 0],
            },
            Layer::Fn | Layer::Second => Block {
                code,
                function: UNBOUND,
                codes: [0; 4],
            },
        }
    }

    /// Key `code` doing `action`. The keyboard's own functions, macros and anything else
    /// opaque can't be made here: they're only ever kept from a read.
    pub fn new(code: u8, action: &Action) -> Result<Block> {
        let (function, codes) = match action {
            Action::Unbound => (UNBOUND, [0; 4]),
            Action::Mouse(button) => {
                ensure!(
                    MOUSE_BUTTONS.contains(button),
                    "mouse button {button} out of range (1 to 8)"
                );
                (*button, [0; 4])
            }
            Action::Cpi => (CPI, [0; 4]),
            Action::WheelUp => (WHEEL_UP, [0; 4]),
            Action::WheelDown => (WHEEL_DOWN, [0; 4]),
            Action::PanLeft => (PAN_LEFT, [0; 4]),
            Action::PanRight => (PAN_RIGHT, [0; 4]),
            Action::Keys(usages) => {
                ensure!(
                    (1..=MAX_USAGES).contains(&usages.len()),
                    "a key sends 1 to {MAX_USAGES} keys at once, not {}",
                    usages.len()
                );
                if let Some(u) = usages.iter().find(|u| !USAGES.contains(u)) {
                    bail!("{u:#04x} is not a keyboard key");
                }
                let mut codes = [0; 4];
                codes[..usages.len()].copy_from_slice(usages);
                (KEYBOARD, codes)
            }
            Action::Consumer(usage) => {
                ensure!(*usage != 0, "a media key needs a usage");
                let [lo, hi] = usage.to_le_bytes();
                (CONSUMER, [lo, hi, 0, 0])
            }
            opaque => bail!(
                "key {code:#04x}: {opaque:?} can only be kept as the keyboard reported it, not made up"
            ),
        };
        Ok(Block {
            code,
            function,
            codes,
        })
    }

    /// What the block says the key does.
    pub fn action(&self) -> Action {
        let c = self.codes;
        match self.function {
            UNBOUND => Action::Unbound,
            f if MOUSE_BUTTONS.contains(&f) => Action::Mouse(f),
            CPI => Action::Cpi,
            WHEEL_UP => Action::WheelUp,
            WHEEL_DOWN => Action::WheelDown,
            PAN_LEFT => Action::PanLeft,
            PAN_RIGHT => Action::PanRight,
            KEYBOARD => Action::Keys(c.iter().copied().filter(|&u| u != 0).collect()),
            CONSUMER => Action::Consumer(u16::from_le_bytes([c[0], c[1]])),
            FIRMWARE => Action::Firmware(c),
            MACRO => Action::Macro(c[0]),
            EXTERNAL => Action::External(c),
            function => Action::Other { function, codes: c },
        }
    }

    pub fn bytes(&self) -> [u8; BLOCK] {
        let c = self.codes;
        [self.code, self.function, c[0], c[1], c[2], c[3]]
    }

    fn from_bytes(b: &[u8]) -> Block {
        Block {
            code: b[0],
            function: b[1],
            codes: [b[2], b[3], b[4], b[5]],
        }
    }
}

/// A change the user asks for on one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The factory binding ([`Block::default_for`]); a key holding one of the keyboard's
    /// own functions keeps it.
    Default,
    To(Action),
}

/// Whether `blocks` is `layer` whole: each of its keys exactly once, and nothing else.
fn check_whole(layer: Layer, blocks: &[Block]) -> Result<()> {
    if let Some(b) = blocks.iter().find(|b| !layer.keys().contains(&b.code)) {
        bail!(
            "key {:#04x} has no binding on the {} layer",
            b.code,
            layer.name()
        );
    }
    for &code in layer.keys() {
        let n = blocks.iter().filter(|b| b.code == code).count();
        ensure!(
            n == 1,
            "the {} layer lists key {code:#04x} {n} times, not once",
            layer.name()
        );
    }
    Ok(())
}

/// What to write on `layer`: the layer as the keyboard reported it (`read`, whole) with
/// `changes` on top, in [`Layer::keys`] order. Keys not changed keep their block byte for
/// byte, and so does a key asked for the action it already has, and every key holding one
/// of the keyboard's own functions (`62`): those can't be made again once erased, so a
/// change to one is refused. Opaque actions ([`Action::kept_only`]) are only accepted
/// where the key already has that very action.
pub fn merge(layer: Layer, read: &[Block], changes: &[(u8, Change)]) -> Result<Vec<Block>> {
    check_whole(layer, read)?;
    let mut out: Vec<Block> = layer
        .keys()
        .iter()
        .filter_map(|&code| read.iter().find(|b| b.code == code).copied())
        .collect();
    for (code, change) in changes {
        let Some(slot) = out.iter_mut().find(|b| b.code == *code) else {
            bail!(
                "key {code:#04x} has no binding on the {} layer",
                layer.name()
            );
        };
        let was = *slot;
        let same = matches!(change, Change::To(a) if *a == was.action());
        *slot = match change {
            _ if was.function == FIRMWARE => {
                ensure!(
                    same || *change == Change::Default,
                    "key {code:#04x} runs one of the keyboard's own shortcuts on the {} layer \
                     (brightness, media or the screen menu); gglinux keeps it, since it can't \
                     be made again",
                    layer.name()
                );
                was
            }
            // The same action may be packed another way (a usage in a later code byte):
            // the keyboard's own bytes stay.
            Change::To(_) if same => was,
            Change::Default => Block::default_for(layer, *code),
            Change::To(a) if a.kept_only() => {
                bail!("key {code:#04x}: {a:?} can only be kept as the keyboard reported it, not made up")
            }
            Change::To(a) => Block::new(*code, a)?,
        };
    }
    Ok(out)
}

/// The Fn-lit keys' mask for an Fn layer: every key bound to something, bit `key & 7` of
/// byte `key / 8`.
pub fn fn_mask(fn_layer: &[Block]) -> [u8; FN_MASK_BYTES] {
    let mut mask = [0u8; FN_MASK_BYTES];
    for b in fn_layer.iter().filter(|b| b.function != UNBOUND) {
        mask[usize::from(b.code / 8)] |= 1 << (b.code & 7);
    }
    mask
}

/// A bindings report: `<op> <layer> <count> <blocks>`, a whole feature report.
fn blocks_report(op: u8, layer: Layer, blocks: &[[u8; BLOCK]]) -> Vec<u8> {
    let mut report = vec![0u8; FEATURE_REPORT];
    report[1] = op;
    report[2] = layer.byte();
    report[3] = blocks.len() as u8;
    for (i, b) in blocks.iter().enumerate() {
        report[4 + i * BLOCK..4 + (i + 1) * BLOCK].copy_from_slice(b);
    }
    report
}

/// One report holds 1 to 106 keys, each on `layer`.
fn check_count_and_keys(layer: Layer, codes: &[u8]) -> Result<()> {
    ensure!(
        (1..=MAX_BLOCKS).contains(&codes.len()),
        "one bindings report holds 1 to {MAX_BLOCKS} keys, not {}",
        codes.len()
    );
    if let Some(code) = codes.iter().find(|c| !layer.keys().contains(c)) {
        bail!(
            "key {code:#04x} has no binding on the {} layer",
            layer.name()
        );
    }
    Ok(())
}

/// Ask for `codes`' bindings on `layer`: `B6 <layer> <count> [key, 0 × 5] × count`. The
/// keyboard answers only for the keys named.
pub fn read_request(layer: Layer, codes: &[u8]) -> Result<Vec<u8>> {
    check_count_and_keys(layer, codes)?;
    let blocks: Vec<[u8; BLOCK]> = codes.iter().map(|&c| [c, 0, 0, 0, 0, 0]).collect();
    Ok(blocks_report(OP_READ_BINDINGS, layer, &blocks))
}

/// Write `blocks` on `layer`: `36 <layer> <count> <blocks>`.
pub fn write_report(layer: Layer, blocks: &[Block]) -> Result<Vec<u8>> {
    let codes: Vec<u8> = blocks.iter().map(|b| b.code).collect();
    check_count_and_keys(layer, &codes)?;
    let bytes: Vec<[u8; BLOCK]> = blocks.iter().map(Block::bytes).collect();
    Ok(blocks_report(OP_BINDINGS, layer, &bytes))
}

/// The reports that write `layer` whole, at most 106 keys a report: one, for each layer
/// this keyboard has.
pub fn write_reports(layer: Layer, blocks: &[Block]) -> Result<Vec<Vec<u8>>> {
    check_whole(layer, blocks)?;
    blocks
        .chunks(MAX_BLOCKS)
        .map(|chunk| write_report(layer, chunk))
        .collect()
}

/// The blocks of a reply to [`read_request`] for `asked` on `layer`, in `asked` order.
/// `reply` is what follows the report id: `B6 <err> <layer> <count> <blocks>`.
pub fn decode_reply(layer: Layer, asked: &[u8], reply: &[u8]) -> Result<Vec<Block>> {
    ensure!(reply.len() >= 4, "the keyboard's answer is too short");
    ensure!(
        reply[0] == OP_READ_BINDINGS,
        "the keyboard answered {:#04x} {:#04x}, not with the bindings",
        reply[0],
        reply[1]
    );
    ensure!(
        reply[1] == 0,
        "the keyboard refused to read the bindings back (error {})",
        reply[1]
    );
    ensure!(
        reply[2] == layer.byte(),
        "the keyboard answered for layer {} when asked for the {} layer",
        reply[2],
        layer.name()
    );
    let count = usize::from(reply[3]);
    ensure!(
        count == asked.len(),
        "the keyboard reported {count} keys of the {} asked for on the {} layer",
        asked.len(),
        layer.name()
    );
    ensure!(
        reply.len() >= 4 + count * BLOCK,
        "the keyboard's answer holds fewer than its {count} keys"
    );
    let got: Vec<Block> = reply[4..4 + count * BLOCK]
        .chunks(BLOCK)
        .map(Block::from_bytes)
        .collect();
    asked
        .iter()
        .map(|&code| {
            let mut found = got.iter().filter(|b| b.code == code);
            match (found.next(), found.next()) {
                (Some(b), None) => Ok(*b),
                (None, _) => bail!(
                    "the keyboard didn't report key {code:#04x} on the {} layer",
                    layer.name()
                ),
                (Some(_), Some(_)) => bail!(
                    "the keyboard reported key {code:#04x} twice on the {} layer",
                    layer.name()
                ),
            }
        })
        .collect()
}

/// Make `code` the key that holds the Fn layer: `35 <key>`, an output report.
pub fn fn_key_report(code: u8) -> Result<Vec<u8>> {
    ensure!(
        KEYS.contains(&code),
        "key {code:#04x} can't hold the Fn layer: it has no binding"
    );
    let mut report = vec![0u8; COMMAND_REPORT];
    report[1] = OP_FN_KEY;
    report[2] = code;
    Ok(report)
}

/// Light these keys while Fn is held: `3C <32-byte mask>`, an output report.
pub fn fn_mask_report(mask: &[u8; FN_MASK_BYTES]) -> Vec<u8> {
    let mut report = vec![0u8; COMMAND_REPORT];
    report[1] = OP_FN_MASK;
    report[2..2 + FN_MASK_BYTES].copy_from_slice(mask);
    report
}

/// The second actuation point's table, `38 61 44 02 [key, h, l] × 68`: the same
/// thresholds as the primary one ([`proto::thresholds`], tenths of a millimetre clamped to
/// 0.1–4.0 mm), and [`NO_SECOND_POINT`] for a key without one (unverified).
pub fn second_actuation_table(second_of: impl Fn(u8) -> Option<u8>) -> Vec<u8> {
    let mut report = vec![0u8; FEATURE_REPORT];
    report[1] = OP_SWITCHES;
    report[2] = SW_THRESHOLDS;
    report[3] = ANALOG_KEYS.len() as u8;
    report[4] = SECOND_LAYER;
    for (i, &code) in ANALOG_KEYS.iter().enumerate() {
        let (h, l) = second_of(code).map_or(NO_SECOND_POINT, proto::thresholds);
        report[5 + i * 3..8 + i * 3].copy_from_slice(&[code, h, l]);
    }
    report
}

/// Whether `reply` is an answer left in the read slot from before this read of `layer`:
/// another command's, or a bindings read of another layer. A refusal (`B6` with an error
/// byte) is this read's own answer.
fn stale(layer: Layer, reply: &[u8]) -> bool {
    match reply {
        [OP_READ_BINDINGS, 0, other, ..] => *other != layer.byte(),
        [OP_READ_BINDINGS, ..] => false,
        _ => true,
    }
}

impl Keyboard {
    /// Read `layer` back, every key it has, in [`Layer::keys`] order. The keyboard gets
    /// [`READ_SETTLE`] to answer; an answer that is still an earlier one (the switch tables'
    /// `38 00`, a colour frame's `40 00`, or the layer read just before) is asked for again
    /// a moment later, [`READ_TRIES`] times in all.
    pub fn read_bindings(&self, layer: Layer) -> Result<Vec<Block>> {
        let mut blocks = Vec::new();
        for asked in layer.keys().chunks(MAX_BLOCKS) {
            let request = read_request(layer, asked)?;
            let mut reply = self.exchange_feature_settled(&request, READ_SETTLE)?;
            for _ in 1..READ_TRIES {
                if !stale(layer, &reply) {
                    break;
                }
                std::thread::sleep(READ_RETRY);
                reply = self.exchange_feature_settled(&request, READ_SETTLE)?;
            }
            blocks.extend(
                decode_reply(layer, asked, &reply)
                    .with_context(|| format!("reading the {} layer back", layer.name()))?,
            );
        }
        Ok(blocks)
    }

    /// Write `layer` whole. Lasts until the keyboard loses power.
    pub fn write_bindings(&self, layer: Layer, blocks: &[Block]) -> Result<()> {
        for report in write_reports(layer, blocks)? {
            self.feature(&report)?;
        }
        Ok(())
    }

    /// Make `code` the key that holds the Fn layer.
    pub fn set_fn_key(&self, code: u8) -> Result<()> {
        self.output(&fn_key_report(code)?)
    }

    /// Light the keys in `mask` while Fn is held.
    pub fn set_fn_mask(&self, mask: &[u8; FN_MASK_BYTES]) -> Result<()> {
        self.output(&fn_mask_report(mask))
    }

    /// Set every adjustable key's second actuation point, in tenths of a millimetre;
    /// `None` for none.
    pub fn set_second_actuation(&self, second_of: impl Fn(u8) -> Option<u8>) -> Result<()> {
        self.feature(&second_actuation_table(second_of))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allow::{self, Families};
    use crate::testing::{FakeTransport, Key, Kind};

    const ON: Families = Families {
        bindings: true,
        onboard: false,
    };

    /// A stock keyboard's `layer`: the self-map on the normal layer, the nine shortcuts on
    /// the Fn layer, nothing past the second point.
    fn stock(layer: Layer) -> Vec<Block> {
        layer
            .keys()
            .iter()
            .map(|&code| match STOCK_FN_LAYER.iter().find(|s| s.0 == code) {
                Some(&(_, codes)) if layer == Layer::Fn => Block {
                    code,
                    function: FIRMWARE,
                    codes,
                },
                _ => Block::default_for(layer, code),
            })
            .collect()
    }

    /// The keyboard's answer to a read of `blocks`: `B6 00 <layer> <count> <blocks>`.
    fn reply(layer: Layer, blocks: &[Block]) -> Vec<u8> {
        let mut r = vec![OP_READ_BINDINGS, 0, layer.byte(), blocks.len() as u8];
        r.extend(blocks.iter().flat_map(|b| b.bytes()));
        r
    }

    #[test]
    fn the_layers_have_91_91_and_68_distinct_keys_and_every_adjustable_key_is_one() {
        for layer in Layer::ALL {
            let mut keys = layer.keys().to_vec();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(keys.len(), layer.keys().len(), "{layer:?}");
        }
        assert_eq!(KEYS.len(), 91);
        assert_eq!(Layer::Second.keys().len(), 68);
        assert!(ANALOG_KEYS.iter().all(|k| KEYS.contains(k)));
        // Every lit key but the button beside the screen.
        let lit: Vec<u8> = (0x04..=0x45)
            .chain(0x49..=0x52)
            .chain(0xE0..=0xE7)
            .chain([0xF0])
            .collect();
        assert_eq!(lit.len(), 85);
        assert!(lit.iter().all(|k| KEYS.contains(k)));
        for absent in [0x46, 0x47, 0x48, 0xF1, 0xF5, 0xFB, 0xFF, 0x53] {
            assert!(!KEYS.contains(&absent), "{absent:#04x}");
        }
        assert_eq!(Layer::ALL.map(Layer::byte), [0, 1, 2]);
    }

    #[test]
    fn the_sources_examples_encode_byte_for_byte() {
        // "04 51 04 00 00 00   A → KEYBOARD, usage A": the normal layer's resting state.
        assert_eq!(
            Block::default_for(Layer::Normal, 0x04).bytes(),
            [0x04, 0x51, 0x04, 0x00, 0x00, 0x00]
        );
        // Caps Lock = `51 E0 04 00 00` types Ctrl+A.
        let caps = Block::new(0x39, &Action::Keys(vec![0xE0, 0x04])).unwrap();
        assert_eq!(caps.bytes(), [0x39, 0x51, 0xE0, 0x04, 0x00, 0x00]);
        assert_eq!(caps.action(), Action::Keys(vec![0xE0, 0x04]));
        // The blank on the Fn and second layers is function 0.
        for layer in [Layer::Fn, Layer::Second] {
            assert_eq!(
                Block::default_for(layer, 0x1A).bytes(),
                [0x1A, 0, 0, 0, 0, 0]
            );
        }
    }

    #[test]
    fn every_action_the_source_names_encodes_and_decodes() {
        let cases: [(Action, [u8; 5]); 10] = [
            (Action::Unbound, [0x00, 0, 0, 0, 0]),
            (Action::Mouse(1), [0x01, 0, 0, 0, 0]),
            (Action::Mouse(8), [0x08, 0, 0, 0, 0]),
            (Action::Cpi, [0x30, 0, 0, 0, 0]),
            (Action::WheelUp, [0x31, 0, 0, 0, 0]),
            (Action::WheelDown, [0x32, 0, 0, 0, 0]),
            (Action::PanLeft, [0x33, 0, 0, 0, 0]),
            (Action::PanRight, [0x34, 0, 0, 0, 0]),
            // Play/pause, 205 = 0x00CD, little-endian.
            (Action::Consumer(205), [0x61, 0xCD, 0x00, 0, 0]),
            (
                Action::Keys(vec![0xE0, 0xE1, 0xE2, 0x29]),
                [0x51, 0xE0, 0xE1, 0xE2, 0x29],
            ),
        ];
        for (action, bytes) in cases {
            let b = Block::new(0x3A, &action).unwrap();
            assert_eq!(b.bytes()[0], 0x3A);
            assert_eq!(b.bytes()[1..], bytes, "{action:?}");
            assert_eq!(b.action(), action);
        }
        assert_eq!(
            Block::new(0x3A, &Action::Consumer(0x1234)).unwrap().codes,
            [0x34, 0x12, 0, 0]
        );
        let opaque = |function, codes| Block {
            code: 0x42,
            function,
            codes,
        };
        assert_eq!(
            opaque(0x62, [1, 0, 0, 0]).action(),
            Action::Firmware([1, 0, 0, 0])
        );
        assert_eq!(opaque(0x71, [3, 9, 9, 9]).action(), Action::Macro(3));
        assert_eq!(
            opaque(0x72, [1, 2, 3, 4]).action(),
            Action::External([1, 2, 3, 4])
        );
        assert_eq!(
            opaque(0x99, [1, 2, 3, 4]).action(),
            Action::Other {
                function: 0x99,
                codes: [1, 2, 3, 4]
            }
        );
    }

    #[test]
    fn out_of_range_actions_and_made_up_opaque_ones_are_refused() {
        for bad in [
            Action::Mouse(0),
            Action::Mouse(9),
            Action::Keys(vec![]),
            Action::Keys(vec![4, 5, 6, 7, 8]),
            Action::Keys(vec![0x03]),
            Action::Keys(vec![0xE8]),
            Action::Keys(vec![0xF0]),
            Action::Consumer(0),
            Action::Firmware([1, 0, 0, 0]),
            Action::Macro(1),
            Action::External([0; 4]),
            Action::Other {
                function: 0x99,
                codes: [0; 4],
            },
        ] {
            assert!(Block::new(0x04, &bad).is_err(), "{bad:?}");
        }
        Block::new(0x04, &Action::Keys(vec![0x04])).unwrap();
        Block::new(0x04, &Action::Keys(vec![0xE7])).unwrap();
    }

    #[test]
    fn a_read_request_lists_the_keys_wanted_byte_for_byte() {
        let r = read_request(Layer::Fn, &[0x04, 0x42]).unwrap();
        assert_eq!(r.len(), FEATURE_REPORT);
        assert_eq!(
            r[..16],
            [0x00, 0xB6, 0x01, 0x02, 0x04, 0, 0, 0, 0, 0, 0x42, 0, 0, 0, 0, 0]
        );
        assert!(r[16..].iter().all(|&b| b == 0));
        // A whole layer fits in one report, and the allowlist takes it with the family on.
        for layer in Layer::ALL {
            let r = read_request(layer, layer.keys()).unwrap();
            assert_eq!(usize::from(r[3]), layer.keys().len());
            assert_eq!(r[2], layer.byte());
            allow::check(&r, ON).unwrap();
            assert!(allow::check(&r, Families::default()).is_err());
        }
        // Range checks: no keys, more than one report holds, a key the layer lacks.
        assert!(read_request(Layer::Normal, &[]).is_err());
        let many: Vec<u8> = KEYS.iter().cycle().take(107).copied().collect();
        let e = read_request(Layer::Normal, &many).unwrap_err();
        assert!(e.to_string().contains("1 to 106 keys"), "{e}");
        assert!(
            read_request(Layer::Normal, &[0x46]).is_err(),
            "Print Screen"
        );
        assert!(
            read_request(Layer::Second, &[0x29]).is_err(),
            "Esc: fixed switch"
        );
    }

    #[test]
    fn a_write_report_carries_each_block_byte_for_byte() {
        let blocks = [
            Block::default_for(Layer::Normal, 0x04),
            Block::new(0x39, &Action::Keys(vec![0xE0, 0x04])).unwrap(),
        ];
        let r = write_report(Layer::Normal, &blocks).unwrap();
        assert_eq!(r.len(), FEATURE_REPORT);
        assert_eq!(
            r[..16],
            [
                0x00, 0x36, 0x00, 0x02, 0x04, 0x51, 0x04, 0x00, 0x00, 0x00, 0x39, 0x51, 0xE0, 0x04,
                0x00, 0x00
            ]
        );
        assert!(r[16..].iter().all(|&b| b == 0));
        assert!(write_report(Layer::Normal, &[]).is_err());
        assert!(
            write_report(Layer::Second, &[Block::default_for(Layer::Second, 0x3A)]).is_err(),
            "F1 has no deep press"
        );
        // A whole layer goes in one report, in the order given.
        for layer in Layer::ALL {
            let reports = write_reports(layer, &stock(layer)).unwrap();
            assert_eq!(reports.len(), 1, "{layer:?}");
            let r = &reports[0];
            assert_eq!(r[..4], [0x00, 0x36, layer.byte(), layer.keys().len() as u8]);
            for (i, b) in stock(layer).iter().enumerate() {
                assert_eq!(r[4 + i * 6..10 + i * 6], b.bytes());
            }
            assert!(r[4 + layer.keys().len() * 6..].iter().all(|&b| b == 0));
            allow::check(r, ON).unwrap();
            assert!(allow::check(r, Families::default()).is_err());
        }
        // Not whole: a key missing, or twice.
        let mut short = stock(Layer::Normal);
        short.pop();
        assert!(write_reports(Layer::Normal, &short).is_err());
        let mut twice = stock(Layer::Normal);
        twice[1] = twice[0];
        assert!(write_reports(Layer::Normal, &twice).is_err());
    }

    #[test]
    fn a_reply_is_decoded_from_offset_4_in_the_order_asked() {
        let asked = [0x04, 0x05];
        let blocks = [
            Block::new(0x05, &Action::Cpi).unwrap(),
            Block::default_for(Layer::Normal, 0x04),
        ];
        let mut r = reply(Layer::Normal, &blocks);
        r.resize(644, 0);
        let got = decode_reply(Layer::Normal, &asked, &r).unwrap();
        assert_eq!(got, [blocks[1], blocks[0]]);
    }

    #[test]
    fn literal_replies_decode_to_the_sources_blocks() {
        // `B6 00 01 01 | 42 62 01 00 00 00`: the Fn layer, one key, F9 running shortcut 01.
        let f9 = [0xB6, 0x00, 0x01, 0x01, 0x42, 0x62, 0x01, 0x00, 0x00, 0x00];
        let got = decode_reply(Layer::Fn, &[0x42], &f9).unwrap();
        assert_eq!(
            got,
            [Block {
                code: 0x42,
                function: FIRMWARE,
                codes: [0x01, 0, 0, 0]
            }]
        );
        assert_eq!(got[0].action(), Action::Firmware([0x01, 0, 0, 0]));
        // `B6 00 00 02 | 39 51 E0 04 00 00 | 04 51 04 00 00 00`: Caps Lock types Ctrl+A,
        // A types itself, on the normal layer, padded with zeros as a whole report.
        let mut normal = vec![0xB6, 0x00, 0x00, 0x02];
        normal.extend([0x39, 0x51, 0xE0, 0x04, 0x00, 0x00]);
        normal.extend([0x04, 0x51, 0x04, 0x00, 0x00, 0x00]);
        normal.resize(FEATURE_REPORT - 1, 0);
        let got = decode_reply(Layer::Normal, &[0x04, 0x39], &normal).unwrap();
        assert_eq!(got[0], Block::default_for(Layer::Normal, 0x04));
        assert_eq!(got[1].action(), Action::Keys(vec![0xE0, 0x04]));
    }

    #[test]
    fn a_read_that_finds_another_commands_answer_is_asked_once_more() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        kb.set_families(ON);
        // The read slot still holds the switch tables' `38 00` the first time.
        let mut answers = 0;
        fake.answer_feature_with(Key::sub(0xB6, 0x01), move |_| {
            answers += 1;
            if answers == 1 {
                vec![0x38, 0x00]
            } else {
                reply(Layer::Fn, &stock(Layer::Fn))
            }
        });
        assert_eq!(kb.read_bindings(Layer::Fn).unwrap(), stock(Layer::Fn));
        let request = read_request(Layer::Fn, &KEYS).unwrap();
        let sent = fake.sent();
        assert_eq!(sent.len(), 2, "one retry");
        assert!(sent.iter().all(|s| s.report == request));
        // Still another command's answer: refused, saying what it found, after its tries.
        fake.clear_sent();
        fake.answer_feature(Key::sub(0xB6, 0x00), &[0x40, 0x00]);
        let e = format!("{:#}", kb.read_bindings(Layer::Normal).unwrap_err());
        assert!(e.contains("answered 0x40 0x00"), "{e}");
        assert_eq!(fake.sent().len(), READ_TRIES);
        // A real answer that's wrong isn't asked again.
        fake.clear_sent();
        fake.answer_feature(Key::sub(0xB6, 0x00), &[0xB6, 0x01]);
        assert!(kb.read_bindings(Layer::Normal).is_err());
        assert_eq!(fake.sent().len(), 1);
    }

    /// As the owner's keyboard did on 2026-11-13: the Fn layer's read still found the
    /// normal layer's answer in the read slot.
    #[test]
    fn a_read_that_finds_the_layer_read_before_is_asked_again() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        kb.set_families(ON);
        let mut answers = 0;
        fake.answer_feature_with(Key::sub(0xB6, 0x01), move |_| {
            answers += 1;
            if answers <= 2 {
                reply(Layer::Normal, &stock(Layer::Normal))
            } else {
                reply(Layer::Fn, &stock(Layer::Fn))
            }
        });
        assert_eq!(kb.read_bindings(Layer::Fn).unwrap(), stock(Layer::Fn));
        assert_eq!(
            fake.sent().len(),
            3,
            "asked until the answer is the Fn layer's"
        );
        // A slot that never moves on is refused, naming the layer it held.
        fake.clear_sent();
        fake.answer_feature(
            Key::sub(0xB6, 0x01),
            &reply(Layer::Normal, &stock(Layer::Normal)),
        );
        let e = format!("{:#}", kb.read_bindings(Layer::Fn).unwrap_err());
        assert!(e.contains("answered for layer 0"), "{e}");
        assert_eq!(fake.sent().len(), READ_TRIES);
    }

    #[test]
    fn a_reply_that_isnt_one_is_an_error_saying_why() {
        let asked = [0x04];
        let a = [Block::default_for(Layer::Normal, 0x04)];
        let good = reply(Layer::Normal, &a);
        let bad = |edit: &dyn Fn(&mut Vec<u8>)| {
            let mut r = good.clone();
            edit(&mut r);
            format!("{:#}", decode_reply(Layer::Normal, &asked, &r).unwrap_err())
        };
        // The read slot holds the last command's answer: `40 00` after a colour frame.
        assert!(bad(&|r| *r = vec![0x40, 0x00, 0, 0]).contains("not with the bindings"));
        // No block list in the request: err 1.
        assert!(bad(&|r| r[1] = 1).contains("error 1"));
        assert!(bad(&|r| r[2] = 1).contains("layer"));
        assert!(bad(&|r| r[3] = 2).contains("2 keys of the 1"));
        assert!(bad(&|r| r[4] = 0x05).contains("didn't report key 0x04"));
        assert!(bad(&|r| r.truncate(3)).contains("too short"));
        assert!(bad(&|r| r.truncate(8)).contains("fewer"));
        let two = reply(Layer::Normal, &[a[0], a[0]]);
        assert!(decode_reply(Layer::Normal, &[0x04, 0x05], &two)
            .unwrap_err()
            .to_string()
            .contains("twice"));
    }

    #[test]
    fn a_merge_changes_only_the_keys_asked_and_keeps_the_rest_byte_for_byte() {
        let read = stock(Layer::Normal);
        let merged = merge(
            Layer::Normal,
            &read,
            &[(0x39, Change::To(Action::Keys(vec![0xE0, 0x04])))],
        )
        .unwrap();
        assert_eq!(merged.len(), 91);
        for (m, r) in merged.iter().zip(&read) {
            if m.code == 0x39 {
                assert_eq!(m.bytes(), [0x39, 0x51, 0xE0, 0x04, 0, 0]);
            } else {
                assert_eq!(m, r);
            }
        }
        // Back to the default: the self-map on the normal layer, function 0 elsewhere.
        let mut changed = read.clone();
        changed[0] = Block::new(0x04, &Action::Unbound).unwrap();
        let back = merge(Layer::Normal, &changed, &[(0x04, Change::Default)]).unwrap();
        assert_eq!(back[0].bytes(), [0x04, 0x51, 0x04, 0, 0, 0]);
        let mut deep = stock(Layer::Second);
        deep[0] = Block::new(0x04, &Action::Mouse(4)).unwrap();
        let back = merge(Layer::Second, &deep, &[(0x04, Change::Default)]).unwrap();
        assert_eq!(back[0].bytes(), [0x04, 0, 0, 0, 0, 0]);
        // A read in another order comes out in the layer's.
        let mut shuffled = read.clone();
        shuffled.reverse();
        assert_eq!(merge(Layer::Normal, &shuffled, &[]).unwrap(), read);
    }

    #[test]
    fn a_key_named_with_the_action_it_already_has_keeps_its_block_as_read() {
        let mut read = stock(Layer::Normal);
        // The same actions gglinux would make, packed differently: B types B with its
        // usage in the second code byte, C mutes with bytes after its usage.
        read[1] = Block {
            code: 0x05,
            function: KEYBOARD,
            codes: [0x00, 0x05, 0x00, 0x00],
        };
        read[2] = Block {
            code: 0x06,
            function: CONSUMER,
            codes: [0xE2, 0x00, 0x07, 0x00],
        };
        let merged = merge(
            Layer::Normal,
            &read,
            &[
                (0x05, Change::To(Action::Keys(vec![0x05]))),
                (0x06, Change::To(Action::Consumer(226))),
            ],
        )
        .unwrap();
        assert_eq!(merged, read, "byte for byte");
        // A different action is made new.
        let merged = merge(
            Layer::Normal,
            &read,
            &[(0x05, Change::To(Action::Keys(vec![0x06])))],
        )
        .unwrap();
        assert_eq!(merged[1].bytes(), [0x05, 0x51, 0x06, 0, 0, 0]);
    }

    #[test]
    fn a_merge_keeps_every_firmware_shortcut_and_refuses_to_change_one() {
        let read = stock(Layer::Fn);
        let merged = merge(
            Layer::Fn,
            &read,
            &[
                (0x3A, Change::To(Action::Consumer(226))),
                (0x42, Change::Default),
                (0x43, Change::To(Action::Firmware([0x02, 0x01, 0, 0]))),
            ],
        )
        .unwrap();
        let shortcuts = |blocks: &[Block]| -> Vec<[u8; 6]> {
            blocks
                .iter()
                .filter(|b| b.function == FIRMWARE)
                .map(Block::bytes)
                .collect()
        };
        assert_eq!(shortcuts(&merged).len(), 9);
        assert_eq!(shortcuts(&merged), shortcuts(&read));
        assert_eq!(
            merged.iter().find(|b| b.code == 0x3A).unwrap().bytes(),
            [0x3A, 0x61, 0xE2, 0x00, 0, 0],
            "Fn+F1 mutes"
        );
        for change in [
            Change::To(Action::Unbound),
            Change::To(Action::Consumer(205)),
            Change::To(Action::Firmware([0x09, 0, 0, 0])),
        ] {
            let e = merge(Layer::Fn, &read, &[(0x42, change.clone())]).unwrap_err();
            assert!(e.to_string().contains("own shortcuts"), "{change:?}: {e}");
        }
        // A shortcut can't be made up on another key either.
        let e = merge(
            Layer::Fn,
            &read,
            &[(0x3A, Change::To(Action::Firmware([0x01, 0, 0, 0])))],
        )
        .unwrap_err();
        assert!(e.to_string().contains("not made up"), "{e}");
    }

    #[test]
    fn a_merge_needs_the_whole_layer_and_keys_it_has() {
        let mut read = stock(Layer::Normal);
        read.pop();
        assert!(merge(Layer::Normal, &read, &[]).is_err());
        let read = stock(Layer::Second);
        let e = merge(Layer::Second, &read, &[(0x29, Change::To(Action::Cpi))]).unwrap_err();
        assert!(e.to_string().contains("deep-press"), "{e}");
        // A macro the keyboard has may be kept, not moved.
        let mut read = stock(Layer::Normal);
        read[1] = Block {
            code: 0x05,
            function: MACRO,
            codes: [2, 0, 0, 0],
        };
        let kept = merge(
            Layer::Normal,
            &read,
            &[(0x05, Change::To(Action::Macro(2)))],
        )
        .unwrap();
        assert_eq!(kept[1], read[1]);
        assert!(merge(
            Layer::Normal,
            &read,
            &[(0x06, Change::To(Action::Macro(2)))]
        )
        .is_err());
    }

    #[test]
    fn the_fn_mask_has_a_bit_for_every_bound_key() {
        let mask = fn_mask(&stock(Layer::Fn));
        let mut want = [0u8; 32];
        for (code, _) in STOCK_FN_LAYER {
            want[usize::from(code / 8)] |= 1 << (code & 7);
        }
        assert_eq!(mask, want);
        // F9 (0x42) is bit 2 of byte 8; Left GUI (0xE3) bit 3 of byte 28.
        assert_eq!(mask[8], 0b0011_1100, "F9 to F12");
        assert_eq!(mask[28], 0b1000);
        assert_eq!(fn_mask(&stock(Layer::Second)), [0; 32]);
    }

    #[test]
    fn the_fn_key_and_mask_reports_are_byte_for_byte_and_gated() {
        let r = fn_key_report(0xF0).unwrap();
        assert_eq!(r.len(), COMMAND_REPORT);
        assert_eq!(r[..3], [0x00, 0x35, 0xF0]);
        assert!(r[3..].iter().all(|&b| b == 0));
        assert!(fn_key_report(0x46).is_err(), "no such key");
        assert!(fn_key_report(0x00).is_err());
        let mut mask = [0u8; 32];
        mask[8] = 0x04;
        mask[31] = 0x80;
        let m = fn_mask_report(&mask);
        assert_eq!(m.len(), COMMAND_REPORT);
        assert_eq!(m[..2], [0x00, 0x3C]);
        assert_eq!(m[2..34], mask);
        assert!(m[34..].iter().all(|&b| b == 0));
        for report in [r, m] {
            allow::check(&report, ON).unwrap();
            assert!(allow::check(&report, Families::default()).is_err());
        }
    }

    #[test]
    fn the_second_point_table_is_the_primary_table_on_layer_2_with_ff_for_none() {
        let t = second_actuation_table(|code| (code == 0x1A).then_some(30));
        assert_eq!(t.len(), FEATURE_REPORT);
        assert_eq!(t[..5], [0x00, 0x38, 0x61, 0x44, 0x02]);
        let w = ANALOG_KEYS.iter().position(|&c| c == 0x1A).unwrap();
        // 3.0 mm is l 113, h 124: h first.
        assert_eq!(t[5 + w * 3..8 + w * 3], [0x1A, 124, 113]);
        assert_eq!(t[5..8], [0x04, 0xFF, 0xFF], "A has none");
        for (i, &code) in ANALOG_KEYS.iter().enumerate() {
            assert_eq!(t[5 + i * 3], code);
        }
        assert!(t[5 + 68 * 3..].iter().all(|&b| b == 0));
        // The same thresholds as the primary table, clamped the same way.
        let primary = proto::actuation_table(|_| 40);
        let second = second_actuation_table(|_| Some(40));
        assert_eq!(primary[5..], second[5..]);
        assert_eq!(primary[..4], second[..4]);
        assert_eq!(
            second_actuation_table(|_| Some(0))[5..8],
            [0x04, 4, 4],
            "below 0.1 mm: 0.1"
        );
        assert_eq!(
            second_actuation_table(|_| Some(99))[5..8],
            [0x04, 219, 213],
            "past 4.0 mm: 4.0"
        );
        allow::check(&t, ON).unwrap();
        assert!(allow::check(&t, Families::default()).is_err());
    }

    #[test]
    fn the_keyboard_reads_a_whole_layer_with_one_request_and_decodes_it() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        kb.set_families(ON);
        fake.answer_feature_with(Key::sub(0xB6, 0x01), |req| {
            let codes: Vec<u8> = (0..usize::from(req[3])).map(|i| req[4 + i * 6]).collect();
            let blocks: Vec<Block> = stock(Layer::Fn)
                .into_iter()
                .filter(|b| codes.contains(&b.code))
                .collect();
            reply(Layer::Fn, &blocks)
        });
        assert_eq!(kb.read_bindings(Layer::Fn).unwrap(), stock(Layer::Fn));
        let sent = fake.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].kind, Kind::Feature);
        assert_eq!(sent[0].report, read_request(Layer::Fn, &KEYS).unwrap());
        // Unscripted, the keyboard answers `B6 00`: no bindings in it.
        let e = kb.read_bindings(Layer::Normal).unwrap_err();
        assert!(format!("{e:#}").contains("normal layer"), "{e:#}");
    }

    #[test]
    fn the_keyboard_writes_and_sets_the_fn_key_mask_and_second_point_with_the_right_kinds() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        kb.set_families(ON);
        kb.write_bindings(Layer::Normal, &stock(Layer::Normal))
            .unwrap();
        kb.set_fn_key(0xE7).unwrap();
        kb.set_fn_mask(&fn_mask(&stock(Layer::Fn))).unwrap();
        kb.set_second_actuation(|_| None).unwrap();
        let sent: Vec<(Kind, u8)> = fake.sent().iter().map(|s| (s.kind, s.report[1])).collect();
        assert_eq!(
            sent,
            [
                (Kind::Feature, 0x36),
                (Kind::Output, 0x35),
                (Kind::Output, 0x3C),
                (Kind::Feature, 0x38),
            ]
        );
        assert_eq!(
            fake.sent()[0].report,
            write_report(Layer::Normal, &stock(Layer::Normal)).unwrap()
        );
        assert!(
            kb.write_bindings(Layer::Normal, &stock(Layer::Normal)[1..])
                .is_err(),
            "not whole: nothing sent"
        );
        assert_eq!(fake.sent().len(), 4);
    }

    #[test]
    fn with_the_family_off_the_allowlist_refuses_every_bindings_write_and_nothing_goes_out() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        let refusals = [
            kb.read_bindings(Layer::Normal).unwrap_err(),
            kb.write_bindings(Layer::Fn, &stock(Layer::Fn)).unwrap_err(),
            kb.set_fn_key(0xF0).unwrap_err(),
            kb.set_fn_mask(&[0; 32]).unwrap_err(),
            kb.set_second_actuation(|_| Some(30)).unwrap_err(),
        ];
        for e in refusals {
            assert!(format!("{e:#}").contains("keyboard_bindings"), "{e:#}");
        }
        assert!(fake.sent().is_empty());
    }
}
