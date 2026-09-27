# Keyboard lighting (M4a) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** gglinux drives the Apex Pro TKL Gen 3's per-key RGB — static per-key colours, breathing / wave / spectrum effects, brightness — or hands it back to the keyboard.

**Architecture:** New `gg-device-keyboard` crate (allowlisted protocol, US ANSI TKL layout, device handle). A `gglinuxd` keyboard worker renders frames from a pure `effects` module, persists `lighting.json`, and hands back on exit. Two IPC requests; a Keyboard panel in the UI.

**Tech Stack:** Rust 2021 (hidapi), Svelte 5 + SVG.

**Spec:** `docs/superpowers/specs/2026-09-27-keyboard-lighting-design.md`

## Global Constraints

- Allowlist exactly `0x23 0x40 0x41 0x4B 0x90 0xA3 0xF5`; checked when framing **and** again at the write.
- Reports: commands 65 bytes `[0x00, op, args…]`; direct frame 643 bytes `[0x00, 0x40, 112, {code,r,g,b}×112]` sent as a feature report; `0x41` sent as an output report.
- Device: `1038:1642`, interface 1, usage page `0xFFC0`.
- Frames: 30 fps (33 ms) for animated effects; static on change plus a 2 s refresh. Reopen every 2 s.
- Lighting defaults to **off**; brightness 0–100 %; effect periods 0.5–60 s.
- Settings: `$XDG_CONFIG_HOME/gglinux/lighting.json` via `gg_ipc::config_dir()`.

## Review Focus

1. **An opcode outside the allowlist** reaching the wire (typo, future code) — `only_the_seven_vetted_opcodes_pass` (Task 2) plus the write-time check in `Keyboard::{feature, output}`.
2. **Daemon exits or crashes while lighting is on** → the keyboard must get its own lighting back on clean exit — Shutdown command awaited in `main` (Task 4); a crash leaves the last frame until replug (documented).
3. **Lighting off by default**: a fresh install must not touch the keyboard at all — `lighting_is_off_by_default_and_validated` (Task 1); the worker sends nothing while disabled and never taken.
4. **Out-of-range client values** (brightness 150, period 0.1 s) → error, nothing stored — same test (Task 1); worker validates first (Task 4).
5. **Keys missing from the picture** (numpad codes in the 112-table) stay dark in static mode — `static_colours_come_from_the_key_list_and_unlisted_keys_are_dark` (Task 3).

---

### Task 1: IPC — lighting types, `config_dir`

**Files:** `crates/gg-ipc/src/lib.rs`, `crates/gglinuxd/examples/fakemouse.rs`, `crates/gglinuxd/src/main.rs` (temporary arm)

- [ ] **Step 1: Failing tests** (gg-ipc):

```rust
    #[test]
    fn lighting_requests_parse_from_their_documented_json() {
        let line = r#"{"id":1,"req":{"method":"set_lighting","params":{"lighting":{"enabled":true,
            "effect":{"kind":"breathing","color":{"r":255,"g":0,"b":0},"period_s":3.0},
            "keys":[{"code":10,"color":{"r":0,"g":255,"b":0}}],"brightness":80}}}}"#;
        match serde_json::from_str::<RequestFrame>(line).unwrap().req {
            Request::SetLighting { lighting } => {
                assert!(lighting.enabled);
                assert_eq!(lighting.effect, Effect::Breathing { color: Rgb { r: 255, g: 0, b: 0 }, period_s: 3.0 });
                assert_eq!(lighting.keys, vec![KeyColor { code: 10, color: Rgb { r: 0, g: 255, b: 0 } }]);
                assert_eq!(lighting.brightness, 80);
            }
            other => panic!("parsed as {other:?}"),
        }
        serde_json::from_str::<RequestFrame>(r#"{"id":2,"req":{"method":"get_keyboard_state"}}"#)
            .expect("get_keyboard_state should parse");
        assert_eq!(serde_json::to_string(&Effect::Static).unwrap(), r#"{"kind":"static"}"#);
    }

    #[test]
    fn lighting_is_off_by_default_and_validated() {
        let d = Lighting::default();
        assert!(!d.enabled, "gglinux never takes the lighting until asked");
        assert!(d.validate().is_ok());
        assert!(Lighting { brightness: 101, ..d.clone() }.validate().is_err());
        assert!(Lighting { effect: Effect::Wave { period_s: 0.1 }, ..d.clone() }.validate().is_err());
        assert!(Lighting { effect: Effect::Spectrum { period_s: f32::NAN }, ..d.clone() }.validate().is_err());
        assert!(Lighting { effect: Effect::Static, ..d }.validate().is_ok());
    }
```

- [ ] **Step 2: Run** `cargo test -p gg-ipc` — compile errors.
- [ ] **Step 3: Implement** (after `SonarState`):

```rust
/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// What the keyboard lighting does.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    /// Each key its own colour, from [`Lighting::keys`].
    Static,
    /// Every key pulses one colour, one breath per `period_s`.
    Breathing { color: Rgb, period_s: f32 },
    /// A rainbow moving across the keyboard, one pass per `period_s`.
    Wave { period_s: f32 },
    /// Every key cycles the rainbow together, once per `period_s`.
    Spectrum { period_s: f32 },
}

/// One key's static colour, by HID usage code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyColor {
    pub code: u8,
    pub color: Rgb,
}

/// Keyboard lighting as gglinux runs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lighting {
    /// `false` leaves the lighting to the keyboard's own onboard profile.
    pub enabled: bool,
    pub effect: Effect,
    /// Static colours; keys not listed are dark.
    pub keys: Vec<KeyColor>,
    /// 0..=100 %, applied to every frame.
    pub brightness: u8,
}

impl Lighting {
    pub const PERIOD_S: std::ops::RangeInclusive<f32> = 0.5..=60.0;

    /// Check ranges, with a message meant for the user.
    pub fn validate(&self) -> Result<(), String> {
        if self.brightness > 100 {
            return Err(format!("brightness {}% out of range (0..=100)", self.brightness));
        }
        let period = match self.effect {
            Effect::Static => None,
            Effect::Breathing { period_s, .. } | Effect::Wave { period_s } | Effect::Spectrum { period_s } => {
                Some(period_s)
            }
        };
        if let Some(p) = period {
            if !(p.is_finite() && Self::PERIOD_S.contains(&p)) {
                return Err(format!("effect period {p} s out of range (0.5..=60)"));
            }
        }
        Ok(())
    }
}

impl Default for Lighting {
    fn default() -> Self {
        Lighting { enabled: false, effect: Effect::Wave { period_s: 4.0 }, keys: Vec::new(), brightness: 100 }
    }
}

/// A key on the keyboard picture, in key units (a standard key is 1 × 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyInfo {
    pub code: u8,
    pub label: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
}

/// The keyboard as the daemon sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyboardState {
    pub connected: bool,
    /// Every key and where it is, for drawing.
    pub layout: Vec<KeyInfo>,
    pub lighting: Lighting,
}
```

`Request`: `/// Keyboard connection, layout and lighting.` `GetKeyboardState,` `/// Replace the keyboard lighting.` `SetLighting { lighting: Lighting },`. `ResponseData`: `Keyboard { state: KeyboardState },`. `Event`: `/// Keyboard connection or lighting changed.` `KeyboardChanged { state: KeyboardState },`. After `socket_path`:

```rust
/// gglinux's config directory: `$XDG_CONFIG_HOME/gglinux`, else `~/.config/gglinux`.
pub fn config_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default().join(".config")
        });
    base.join("gglinux")
}
```

- [ ] **Step 4: Keep compiling.** `main.rs` `dispatch`: `Request::GetKeyboardState | Request::SetLighting { .. } => ResponseData::Error { message: "keyboard lighting is not wired up yet".into() },`. `fakemouse.rs`: field `keyboard: KeyboardState` initialised to `KeyboardState { connected: true, layout: Vec::new(), lighting: Lighting::default() }`; arms `GetKeyboardState => ResponseData::Keyboard { state: s.keyboard.clone() }`, `SetLighting { lighting } => { s.keyboard.lighting = lighting; ResponseData::Success }`; method names `get_keyboard_state`, `set_lighting`.
- [ ] **Step 5: Run** `cargo test -p gg-ipc && cargo build --workspace --all-targets` — 19 pass; builds.
- [ ] **Step 6: Commit** `IPC: keyboard lighting types and requests; config_dir`

---

### Task 2: `gg-device-keyboard` — protocol, layout, device

**Files:** Create `crates/gg-device-keyboard/{Cargo.toml, src/lib.rs, src/proto.rs, src/layout.rs}`; workspace members.

- [ ] **Step 1: Scaffold + failing tests.** `Cargo.toml` (deps `anyhow`, `hidapi`, `tracing`, workspace package fields, description "SteelSeries Apex Pro TKL Gen 3 lighting"). `lib.rs` initially `pub mod layout; pub mod proto;`. `proto.rs` tests:

```rust
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
```

`layout.rs` test:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::KEYS;

    #[test]
    fn the_ansi_tkl_has_87_distinct_keys_the_frame_can_light() {
        let mut codes: Vec<u8> = ANSI_TKL.iter().map(|k| k.0).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 87);
        assert!(codes.iter().all(|c| KEYS.contains(c)), "every drawn key is in the frame");
        assert!(ANSI_TKL.iter().all(|&(_, _, x, _, w)| x >= 0.0 && x + w <= WIDTH));
    }
}
```

- [ ] **Step 2: Run** `cargo test -p gg-device-keyboard` — compile errors.
- [ ] **Step 3: Implement.** Prepend to `proto.rs`:

```rust
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
```

Prepend to `layout.rs`:

```rust
//! The US ANSI TKL keyboard as the UI draws it: HID usage code, label, and
//! position and width in key units (a standard key is 1 × 1). The owner's
//! keyboard reports region 1 (US); ISO boards would add two keys and reshape
//! Enter.

/// The board is this many key units wide.
pub const WIDTH: f32 = 18.25;

/// (HID code, label, x, y, width)
pub const ANSI_TKL: [(u8, &str, f32, f32, f32); 87] = [
    (0x29, "Esc", 0.0, 0.0, 1.0),
    (0x3A, "F1", 2.0, 0.0, 1.0), (0x3B, "F2", 3.0, 0.0, 1.0), (0x3C, "F3", 4.0, 0.0, 1.0), (0x3D, "F4", 5.0, 0.0, 1.0),
    (0x3E, "F5", 6.5, 0.0, 1.0), (0x3F, "F6", 7.5, 0.0, 1.0), (0x40, "F7", 8.5, 0.0, 1.0), (0x41, "F8", 9.5, 0.0, 1.0),
    (0x42, "F9", 11.0, 0.0, 1.0), (0x43, "F10", 12.0, 0.0, 1.0), (0x44, "F11", 13.0, 0.0, 1.0), (0x45, "F12", 14.0, 0.0, 1.0),
    (0x46, "PrtSc", 15.25, 0.0, 1.0), (0x47, "ScrLk", 16.25, 0.0, 1.0), (0x48, "Pause", 17.25, 0.0, 1.0),
    (0x35, "`", 0.0, 1.5, 1.0),
    (0x1E, "1", 1.0, 1.5, 1.0), (0x1F, "2", 2.0, 1.5, 1.0), (0x20, "3", 3.0, 1.5, 1.0), (0x21, "4", 4.0, 1.5, 1.0),
    (0x22, "5", 5.0, 1.5, 1.0), (0x23, "6", 6.0, 1.5, 1.0), (0x24, "7", 7.0, 1.5, 1.0), (0x25, "8", 8.0, 1.5, 1.0),
    (0x26, "9", 9.0, 1.5, 1.0), (0x27, "0", 10.0, 1.5, 1.0), (0x2D, "-", 11.0, 1.5, 1.0), (0x2E, "=", 12.0, 1.5, 1.0),
    (0x2A, "Backspace", 13.0, 1.5, 2.0),
    (0x49, "Ins", 15.25, 1.5, 1.0), (0x4A, "Home", 16.25, 1.5, 1.0), (0x4B, "PgUp", 17.25, 1.5, 1.0),
    (0x2B, "Tab", 0.0, 2.5, 1.5),
    (0x14, "Q", 1.5, 2.5, 1.0), (0x1A, "W", 2.5, 2.5, 1.0), (0x08, "E", 3.5, 2.5, 1.0), (0x15, "R", 4.5, 2.5, 1.0),
    (0x17, "T", 5.5, 2.5, 1.0), (0x1C, "Y", 6.5, 2.5, 1.0), (0x18, "U", 7.5, 2.5, 1.0), (0x0C, "I", 8.5, 2.5, 1.0),
    (0x12, "O", 9.5, 2.5, 1.0), (0x13, "P", 10.5, 2.5, 1.0), (0x2F, "[", 11.5, 2.5, 1.0), (0x30, "]", 12.5, 2.5, 1.0),
    (0x31, "\\", 13.5, 2.5, 1.5),
    (0x4C, "Del", 15.25, 2.5, 1.0), (0x4D, "End", 16.25, 2.5, 1.0), (0x4E, "PgDn", 17.25, 2.5, 1.0),
    (0x39, "Caps", 0.0, 3.5, 1.75),
    (0x04, "A", 1.75, 3.5, 1.0), (0x16, "S", 2.75, 3.5, 1.0), (0x07, "D", 3.75, 3.5, 1.0), (0x09, "F", 4.75, 3.5, 1.0),
    (0x0A, "G", 5.75, 3.5, 1.0), (0x0B, "H", 6.75, 3.5, 1.0), (0x0D, "J", 7.75, 3.5, 1.0), (0x0E, "K", 8.75, 3.5, 1.0),
    (0x0F, "L", 9.75, 3.5, 1.0), (0x33, ";", 10.75, 3.5, 1.0), (0x34, "'", 11.75, 3.5, 1.0),
    (0x28, "Enter", 12.75, 3.5, 2.25),
    (0xE1, "Shift", 0.0, 4.5, 2.25),
    (0x1D, "Z", 2.25, 4.5, 1.0), (0x1B, "X", 3.25, 4.5, 1.0), (0x06, "C", 4.25, 4.5, 1.0), (0x19, "V", 5.25, 4.5, 1.0),
    (0x05, "B", 6.25, 4.5, 1.0), (0x11, "N", 7.25, 4.5, 1.0), (0x10, "M", 8.25, 4.5, 1.0), (0x36, ",", 9.25, 4.5, 1.0),
    (0x37, ".", 10.25, 4.5, 1.0), (0x38, "/", 11.25, 4.5, 1.0),
    (0xE5, "Shift", 12.25, 4.5, 2.75),
    (0x52, "↑", 16.25, 4.5, 1.0),
    (0xE0, "Ctrl", 0.0, 5.5, 1.25), (0xE3, "Win", 1.25, 5.5, 1.25), (0xE2, "Alt", 2.5, 5.5, 1.25),
    (0x2C, "", 3.75, 5.5, 6.25),
    (0xE6, "Alt", 10.0, 5.5, 1.25), (0xF0, "Fn", 11.25, 5.5, 1.25), (0xE7, "Menu", 12.5, 5.5, 1.25),
    (0xE4, "Ctrl", 13.75, 5.5, 1.25),
    (0x50, "←", 15.25, 5.5, 1.0), (0x51, "↓", 16.25, 5.5, 1.0), (0x4F, "→", 17.25, 5.5, 1.0),
];
```

`lib.rs` (replace):

```rust
//! Control for the SteelSeries **Apex Pro TKL Gen 3** (`1038:1642`) lighting.
//!
//! Everything sent is framed by [`proto`], which only frames opcodes on its
//! allowlist, and checked against it once more at the write.

use anyhow::{anyhow, Context, Result};
use hidapi::{HidApi, HidDevice};
use tracing::debug;

pub mod layout;
pub mod proto;

pub use proto::Rgb;

pub const VENDOR_ID: u16 = 0x1038;
pub const PRODUCT_ID: u16 = 0x1642;

/// The vendor interface OpenRGB drives, and the usage page that identifies it.
const CONTROL_INTERFACE: i32 = 1;
const VENDOR_USAGE_PAGE: u16 = 0xffc0;

const READ_TIMEOUT_MS: i32 = 200;

pub struct Keyboard {
    device: HidDevice,
}

impl Keyboard {
    /// Open the lighting interface of the first connected Apex Pro TKL Gen 3.
    pub fn open(api: &HidApi) -> Result<Self> {
        let path = api
            .device_list()
            .find(|d| {
                d.vendor_id() == VENDOR_ID
                    && d.product_id() == PRODUCT_ID
                    && d.interface_number() == CONTROL_INTERFACE
                    && d.usage_page() == VENDOR_USAGE_PAGE
            })
            .map(|d| d.path().to_owned())
            .ok_or_else(|| anyhow!("Apex Pro TKL Gen 3 lighting interface not found — is it plugged in?"))?;
        let device = api
            .open_path(&path)
            .with_context(|| format!("opening {}", path.to_string_lossy()))?;
        debug!(path = %path.to_string_lossy(), "opened keyboard lighting interface");
        Ok(Self { device })
    }

    /// Take over the lighting; send before the first frame.
    pub fn init(&self) -> Result<()> {
        self.feature(&proto::command(proto::OP_INIT, &[])?)
    }

    /// Show one frame: a colour for every LED, by HID code.
    pub fn show(&self, color_of: impl Fn(u8) -> Rgb) -> Result<()> {
        self.feature(&proto::direct_frame(color_of))
    }

    /// Give the lighting back to the keyboard's onboard profile.
    pub fn hand_back(&self) -> Result<()> {
        self.output(&proto::command(proto::OP_ONBOARD, &[])?)
    }

    /// The region (layout) byte; 1 is US ANSI.
    pub fn region(&self) -> Result<Option<u8>> {
        self.output(&proto::command(proto::OP_REGION, &[])?)?;
        let mut buf = [0u8; 64];
        let n = self
            .device
            .read_timeout(&mut buf, READ_TIMEOUT_MS)
            .context("reading the region reply")?;
        Ok(proto::decode_region(&buf[..n]))
    }

    /// Send a feature report. The allowlist is checked again here, at the one
    /// place bytes reach the wire.
    fn feature(&self, report: &[u8]) -> Result<()> {
        proto::check(report[1])?;
        self.device
            .send_feature_report(report)
            .with_context(|| format!("sending keyboard command {:#04x}", report[1]))
    }

    fn output(&self, report: &[u8]) -> Result<()> {
        proto::check(report[1])?;
        self.device
            .write(report)
            .map(|_| ())
            .with_context(|| format!("sending keyboard command {:#04x}", report[1]))
    }
}
```

- [ ] **Step 4: Run** `cargo test -p gg-device-keyboard && cargo clippy -p gg-device-keyboard --all-targets` — 5 pass; no warnings.
- [ ] **Step 5: Commit** `gg-device-keyboard: allowlisted lighting protocol, ANSI TKL layout`

---

### Task 3: Lighting effects (pure)

**Files:** Create `crates/gglinuxd/src/keyboard.rs` (shell: doc + `mod effects;`), `crates/gglinuxd/src/keyboard/effects.rs`; `main.rs` (`mod keyboard;`); `gglinuxd/Cargo.toml` (`gg-device-keyboard`).

- [ ] **Step 1: Failing tests** — `effects.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::KeyColor;

    const RED: gg_ipc::Rgb = gg_ipc::Rgb { r: 255, g: 0, b: 0 };

    fn lit(effect: Effect) -> Lighting {
        Lighting { enabled: true, effect, keys: Vec::new(), brightness: 100 }
    }

    #[test]
    fn hsv_hits_the_primaries() {
        assert_eq!(hsv(0.0), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(hsv(1.0 / 3.0), Rgb { r: 0, g: 255, b: 0 });
        assert_eq!(hsv(2.0 / 3.0), Rgb { r: 0, g: 0, b: 255 });
        assert_eq!(hsv(1.0), hsv(0.0), "the hue wraps");
    }

    #[test]
    fn static_colours_come_from_the_key_list_and_unlisted_keys_are_dark() {
        let l = Lighting { keys: vec![KeyColor { code: 0x0A, color: RED }], ..lit(Effect::Static) };
        let p = positions();
        assert_eq!(color(&l, &p, 0x0A, 5.0), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(color(&l, &p, 0x04, 5.0), Rgb::default());
        assert_eq!(color(&l, &p, 0x59, 5.0), Rgb::default(), "numpad codes the TKL lacks stay dark");
    }

    #[test]
    fn breathing_goes_from_dark_to_full_each_period() {
        let l = lit(Effect::Breathing { color: RED, period_s: 2.0 });
        let p = positions();
        assert_eq!(color(&l, &p, 0x04, 0.0), Rgb::default());
        assert_eq!(color(&l, &p, 0x04, 1.0), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(color(&l, &p, 0x04, 2.0), Rgb::default());
    }

    #[test]
    fn the_wave_moves_across_the_keyboard() {
        let l = lit(Effect::Wave { period_s: 4.0 });
        let p = positions();
        let esc = color(&l, &p, 0x29, 0.0);
        assert_ne!(esc, color(&l, &p, 0x48, 0.0), "Esc and Pause differ");
        assert_ne!(color(&l, &p, 0x29, 1.0), esc, "and it moves");
    }

    #[test]
    fn spectrum_is_one_colour_for_every_key() {
        let l = lit(Effect::Spectrum { period_s: 6.0 });
        let p = positions();
        assert_eq!(color(&l, &p, 0x29, 1.5), color(&l, &p, 0x48, 1.5));
    }

    #[test]
    fn brightness_scales_every_effect() {
        let p = positions();
        let off = Lighting { brightness: 0, ..lit(Effect::Spectrum { period_s: 6.0 }) };
        assert_eq!(color(&off, &p, 0x29, 1.0), Rgb::default());
        let half = Lighting { brightness: 50, keys: vec![KeyColor { code: 0x0A, color: RED }], ..lit(Effect::Static) };
        assert_eq!(color(&half, &p, 0x0A, 0.0).r, 128);
    }
}
```

- [ ] **Step 2: Run** `cargo test -p gglinuxd effects` — compile errors.
- [ ] **Step 3: Implement** — prepend:

```rust
//! Key colours for each lighting effect at a moment in time.
//!
//! Pure, so every effect is tested without the keyboard; the worker asks for
//! one frame at a time and sends it.

use std::collections::HashMap;
use std::f32::consts::TAU;

use gg_device_keyboard::layout::{ANSI_TKL, WIDTH};
use gg_device_keyboard::Rgb;
use gg_ipc::{Effect, Lighting};

/// Horizontal position of each key's centre, 0 (left edge) to 1 (right edge).
pub fn positions() -> HashMap<u8, f32> {
    ANSI_TKL.iter().map(|&(code, _, x, _, w)| (code, (x + w / 2.0) / WIDTH)).collect()
}

/// The colour of the LED with HID code `code`, `t` seconds into the effect.
pub fn color(lighting: &Lighting, positions: &HashMap<u8, f32>, code: u8, t: f32) -> Rgb {
    let raw = match lighting.effect {
        Effect::Static => lighting
            .keys
            .iter()
            .find(|k| k.code == code)
            .map(|k| rgb(k.color))
            .unwrap_or_default(),
        Effect::Breathing { color, period_s } => scale(rgb(color), (1.0 - (TAU * t / period_s).cos()) / 2.0),
        Effect::Wave { period_s } => {
            let x = positions.get(&code).copied().unwrap_or(0.0);
            hsv(x - t / period_s)
        }
        Effect::Spectrum { period_s } => hsv(t / period_s),
    };
    scale(raw, f32::from(lighting.brightness) / 100.0)
}

/// Whether the effect needs a stream of frames, or one holds.
pub fn animated(effect: &Effect) -> bool {
    !matches!(effect, Effect::Static)
}

/// A fully saturated, full-value colour; `hue` wraps at 1.
pub fn hsv(hue: f32) -> Rgb {
    let h = hue.rem_euclid(1.0) * 6.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    let byte = |v: f32| (v * 255.0).round() as u8;
    Rgb { r: byte(r), g: byte(g), b: byte(b) }
}

fn rgb(c: gg_ipc::Rgb) -> Rgb {
    Rgb { r: c.r, g: c.g, b: c.b }
}

fn scale(c: Rgb, k: f32) -> Rgb {
    let k = k.clamp(0.0, 1.0);
    let f = |v: u8| (f32::from(v) * k).round() as u8;
    Rgb { r: f(c.r), g: f(c.g), b: f(c.b) }
}
```

- [ ] **Step 4: Run** `cargo test -p gglinuxd effects` — 6 pass.
- [ ] **Step 5: Commit** `gglinuxd: keyboard lighting effects`

---

### Task 4: Keyboard worker and wiring

**Files:** `crates/gglinuxd/src/keyboard.rs`, `crates/gglinuxd/src/main.rs`

- [ ] **Step 1: Worker** — `keyboard.rs`:

```rust
//! Keyboard worker: the Apex Pro TKL Gen 3's lighting.
//!
//! One OS thread owns the device, like the other workers. With gglinux lighting
//! on it sends frames — 30 a second for animated effects, on change (and a slow
//! refresh) for static ones; with it off, or when the daemon exits, it hands the
//! lighting back to the keyboard's onboard profile. Off by default: a fresh
//! install never touches the keyboard.

mod effects;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use gg_device_keyboard::layout::ANSI_TKL;
use gg_device_keyboard::Keyboard;
use gg_ipc::{Event, KeyInfo, KeyboardState, Lighting};
use hidapi::HidApi;
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, error, info, warn};

use crate::clock::is_older_than;

const FRAME: Duration = Duration::from_millis(33);
const STATIC_REFRESH: Duration = Duration::from_secs(2);
const REOPEN_INTERVAL: Duration = Duration::from_secs(2);
const IDLE_WAIT: Duration = Duration::from_millis(500);

type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum Command {
    GetState(Reply<KeyboardState>),
    SetLighting { lighting: Lighting, reply: Reply<()> },
    /// Hand the lighting back and stop. Replies once the keyboard has been told.
    Shutdown(Reply<()>),
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| "keyboard worker is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "keyboard worker dropped the request".to_string())?
    }
}

pub fn spawn(event_tx: broadcast::Sender<Event>) -> Handle {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("gg-keyboard".into())
        .spawn(move || worker(rx, event_tx))
        .expect("spawn gg-keyboard thread");
    Handle { tx }
}

struct State {
    api: HidApi,
    device: Option<Keyboard>,
    /// Whether this handle has taken the lighting over (init sent).
    taken: bool,
    last_open_attempt: Option<Instant>,
    last_frame: Option<Instant>,
    lighting: Lighting,
    settings_path: PathBuf,
    positions: HashMap<u8, f32>,
    started: Instant,
    published: Option<KeyboardState>,
}

fn worker(rx: Receiver<Command>, event_tx: broadcast::Sender<Event>) {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            error!(error = %e, "keyboard worker could not initialise hidapi");
            return;
        }
    };
    let settings_path = gg_ipc::config_dir().join("lighting.json");
    let mut state = State {
        api,
        device: None,
        taken: false,
        last_open_attempt: None,
        last_frame: None,
        lighting: load(&settings_path),
        settings_path,
        positions: effects::positions(),
        started: Instant::now(),
        published: None,
    };

    loop {
        match rx.recv_timeout(next_wait(&state)) {
            Ok(command) => {
                if !handle(command, &mut state, &event_tx) {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                hand_back(&mut state);
                return;
            }
        }
        if state.device.is_none() {
            try_open(&mut state);
        }
        render(&mut state);
        publish(&mut state, &event_tx);
    }
}

/// How often frames are due for the current effect.
fn interval(lighting: &Lighting) -> Duration {
    if effects::animated(&lighting.effect) {
        FRAME
    } else {
        STATIC_REFRESH
    }
}

/// How long to wait for a command before the next frame is due.
fn next_wait(state: &State) -> Duration {
    if state.device.is_none() || !state.lighting.enabled {
        return IDLE_WAIT;
    }
    state
        .last_frame
        .map_or(Duration::ZERO, |t| interval(&state.lighting).saturating_sub(t.elapsed()))
}

fn handle(command: Command, state: &mut State, event_tx: &broadcast::Sender<Event>) -> bool {
    match command {
        Command::GetState(reply) => {
            let _ = reply.send(Ok(snapshot(state)));
        }
        Command::SetLighting { lighting, reply } => {
            if let Err(message) = lighting.validate() {
                let _ = reply.send(Err(message));
                return true;
            }
            let turned_off = state.lighting.enabled && !lighting.enabled;
            state.lighting = lighting;
            save(&state.settings_path, &state.lighting);
            if turned_off {
                hand_back(state);
            }
            state.last_frame = None; // draw the change now
            let _ = reply.send(Ok(()));
            publish(state, event_tx);
        }
        Command::Shutdown(reply) => {
            hand_back(state);
            let _ = reply.send(Ok(()));
            return false;
        }
    }
    true
}

fn try_open(state: &mut State) {
    if !is_older_than(state.last_open_attempt, REOPEN_INTERVAL) {
        return;
    }
    state.last_open_attempt = Some(Instant::now());
    if state.api.refresh_devices().is_err() {
        return;
    }
    match Keyboard::open(&state.api) {
        Ok(keyboard) => {
            info!("keyboard connected");
            state.device = Some(keyboard);
            state.taken = false;
            state.last_frame = None;
        }
        Err(e) => debug!(error = %format!("{e:#}"), "keyboard not available"),
    }
}

/// Send a frame if one is due.
fn render(state: &mut State) {
    if !state.lighting.enabled || !is_older_than(state.last_frame, interval(&state.lighting)) {
        return;
    }
    let Some(device) = state.device.as_ref() else { return };
    let t = state.started.elapsed().as_secs_f32();
    let result = if state.taken { Ok(()) } else { device.init() }
        .and_then(|()| device.show(|code| effects::color(&state.lighting, &state.positions, code, t)));
    match result {
        Ok(()) => {
            state.taken = true;
            state.last_frame = Some(Instant::now());
        }
        Err(e) => {
            info!(error = %format!("{e:#}"), "keyboard disconnected");
            state.device = None;
            state.taken = false;
        }
    }
}

/// Give the lighting back to the keyboard's onboard profile, if gglinux had it.
fn hand_back(state: &mut State) {
    if !state.taken {
        return;
    }
    if let Some(device) = state.device.as_ref() {
        match device.hand_back() {
            Ok(()) => debug!("keyboard lighting handed back"),
            Err(e) => warn!(error = %format!("{e:#}"), "could not hand the keyboard lighting back"),
        }
    }
    state.taken = false;
}

fn snapshot(state: &State) -> KeyboardState {
    KeyboardState {
        connected: state.device.is_some(),
        layout: ANSI_TKL
            .iter()
            .map(|&(code, label, x, y, w)| KeyInfo { code, label: label.to_owned(), x, y, w })
            .collect(),
        lighting: state.lighting.clone(),
    }
}

fn publish(state: &mut State, event_tx: &broadcast::Sender<Event>) {
    let now = snapshot(state);
    if state.published.as_ref() != Some(&now) {
        state.published = Some(now.clone());
        let _ = event_tx.send(Event::KeyboardChanged { state: now });
    }
}

/// Saved lighting; a missing or unreadable file means the defaults (lighting off).
fn load(path: &Path) -> Lighting {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            warn!(path = %path.display(), error = %e, "ignoring unreadable lighting settings");
            Lighting::default()
        }),
        Err(_) => Lighting::default(),
    }
}

/// Save atomically; a failure is logged, never fatal.
fn save(path: &Path, lighting: &Lighting) {
    let write = || -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(lighting)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    if let Err(e) = write() {
        warn!(path = %path.display(), error = %format!("{e:#}"), "could not save lighting settings");
    }
}
```

(keep `mod effects;` from Task 3; the file's doc comment replaces Task 3's shell doc.)

- [ ] **Step 2: Wire `main.rs`** — `mod keyboard;`, `use keyboard::{Command as KeyboardCommand, Handle as KeyboardHandle};`, `Workers.keyboard: KeyboardHandle` spawned with `keyboard::spawn(event_tx.clone())`. Replace the temporary arm with:

```rust
        Request::GetKeyboardState => match workers.keyboard.send(KeyboardCommand::GetState).await {
            Ok(state) => ResponseData::Keyboard { state },
            Err(message) => ResponseData::Error { message },
        },
        Request::SetLighting { lighting } => outcome(
            workers
                .keyboard
                .send::<()>(|reply| KeyboardCommand::SetLighting { lighting, reply })
                .await,
        ),
```

Replace `shutdown_headset` with a version that hands both devices back concurrently:

```rust
/// Leave the devices as their owner expects them — base-station screen dark,
/// keyboard lighting back on its own profile — waiting at most [`SHUTDOWN_GRACE`].
async fn shutdown_devices(workers: &Workers) {
    let both = async {
        tokio::join!(
            workers.headset.send::<()>(HeadsetCommand::Shutdown),
            workers.keyboard.send::<()>(KeyboardCommand::Shutdown),
        )
    };
    match tokio::time::timeout(SHUTDOWN_GRACE, both).await {
        Ok((headset, keyboard)) => {
            if let Err(e) = headset {
                warn!(error = %e, "headset shutdown failed");
            }
            if let Err(e) = keyboard {
                warn!(error = %e, "keyboard shutdown failed");
            }
            debug!("devices left as found");
        }
        Err(_) => warn!("device workers did not confirm shutdown in time"),
    }
}
```

and call `shutdown_devices(&workers).await;` where `shutdown_headset` was called. Update the crate doc scope sentence to mention keyboard lighting.

- [ ] **Step 3: Run** `cargo test --workspace && cargo build --workspace --all-targets && cargo clippy --workspace --all-targets` — green, no warnings.
- [ ] **Step 4: Read-only smoke** — run the daemon, `get_keyboard_state`: `connected: true`, 87 layout keys, lighting off; the keyboard's own lighting unchanged (nothing sent while off). Stop it.
- [ ] **Step 5: Commit** `gglinuxd: keyboard lighting worker`

---

### Task 5: UI — Keyboard panel

**Files:** `ui/src/lib/types.ts`, `ui/src/lib/daemon.svelte.ts`, create `ui/src/lib/KeyboardPanel.svelte`, `ui/src/App.svelte`.

- [ ] **Step 1: Types** — add

```ts
export interface Rgb {
  r: number;
  g: number;
  b: number;
}

export type Effect =
  | { kind: "static" }
  | { kind: "breathing"; color: Rgb; period_s: number }
  | { kind: "wave"; period_s: number }
  | { kind: "spectrum"; period_s: number };

export interface KeyColor {
  code: number;
  color: Rgb;
}

export interface Lighting {
  /** false leaves the lighting to the keyboard's own profile. */
  enabled: boolean;
  effect: Effect;
  keys: KeyColor[];
  /** 0..=100 % */
  brightness: number;
}

export interface KeyInfo {
  code: number;
  label: string;
  x: number;
  y: number;
  w: number;
}

export interface KeyboardState {
  connected: boolean;
  layout: KeyInfo[];
  lighting: Lighting;
}
```

plus `Request` `get_keyboard_state`, `set_lighting {lighting}`; `ResponseData` `{ type: "keyboard"; state: KeyboardState }`; `DaemonEvent` `{ event: "keyboard_changed"; data: { state: KeyboardState } }`.

- [ ] **Step 2: Client** — `keyboard = $state<KeyboardState | null>(null)` (cleared on disconnect), `case "keyboard_changed"`, and:

```ts
  async getKeyboardState(): Promise<KeyboardState> {
    const res = await this.request({ method: "get_keyboard_state" });
    if (res.type !== "keyboard") throw new Error(`unexpected response: ${res.type}`);
    this.keyboard = res.state;
    return res.state;
  }

  async setLighting(lighting: Lighting): Promise<void> {
    const res = await this.request({ method: "set_lighting", params: { lighting } });
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getKeyboardState();
  }
```

- [ ] **Step 3: Panel** — `ui/src/lib/KeyboardPanel.svelte`:

```svelte
<script lang="ts">
  // Apex Pro TKL Gen 3 lighting: give it to gglinux or leave it to the
  // keyboard, pick an effect, and paint keys for the static one.

  import { SvelteSet } from "svelte/reactivity";
  import Icon from "./Icon.svelte";
  import { daemon } from "./daemon.svelte";
  import type { Effect, KeyColor, Lighting, Rgb } from "./types";

  const EFFECTS: { kind: Effect["kind"]; label: string }[] = [
    { kind: "static", label: "Static" },
    { kind: "breathing", label: "Breathing" },
    { kind: "wave", label: "Wave" },
    { kind: "spectrum", label: "Spectrum" },
  ];
  /** Pixels per key unit in the picture. */
  const U = 40;

  let kb = $derived(daemon.keyboard);
  let lighting = $derived(kb?.lighting ?? null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  /** Static colours being edited, by HID code; seeded once from the daemon. */
  let painted = $state<Record<number, Rgb>>({});
  let seeded = false;
  const selected = new SvelteSet<number>();
  let brush = $state("#00c8ff");

  $effect(() => {
    const l = lighting;
    if (seeded || !l) return;
    seeded = true;
    painted = Object.fromEntries(l.keys.map((k) => [k.code, k.color]));
  });

  $effect(() => {
    if (!daemon.connected) return;
    void daemon.getKeyboardState().catch((e: unknown) => {
      error = e instanceof Error ? e.message : String(e);
    });
  });

  const hex = (c: Rgb) => "#" + [c.r, c.g, c.b].map((v) => v.toString(16).padStart(2, "0")).join("");
  const rgbOf = (h: string): Rgb => ({
    r: parseInt(h.slice(1, 3), 16),
    g: parseInt(h.slice(3, 5), 16),
    b: parseInt(h.slice(5, 7), 16),
  });
  let width = $derived(kb ? Math.max(...kb.layout.map((k) => k.x + k.w)) * U : 0);
  let height = $derived(kb ? Math.max(...kb.layout.map((k) => k.y + 1)) * U : 0);
  let period = $derived(lighting && lighting.effect.kind !== "static" ? lighting.effect.period_s : 4);

  async function apply(patch: Partial<Lighting>) {
    if (!lighting) return;
    busy = true;
    error = null;
    try {
      await daemon.setLighting({ ...lighting, ...patch });
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function setEffect(kind: Effect["kind"]) {
    const effect: Effect =
      kind === "static"
        ? { kind }
        : kind === "breathing"
          ? { kind, color: rgbOf(brush), period_s: period }
          : { kind, period_s: period };
    void apply({ effect });
  }

  function setPeriod(p: number) {
    if (!lighting || lighting.effect.kind === "static") return;
    void apply({ effect: { ...lighting.effect, period_s: p } });
  }

  function toggle(code: number) {
    if (selected.has(code)) selected.delete(code);
    else selected.add(code);
  }

  function paint(codes: number[]) {
    const c = rgbOf(brush);
    painted = { ...painted, ...Object.fromEntries(codes.map((code) => [code, c])) };
  }

  function applyStatic() {
    const keys: KeyColor[] = Object.entries(painted).map(([code, color]) => ({ code: Number(code), color }));
    void apply({ effect: { kind: "static" }, keys });
  }
</script>

<article class="panel">
  <header class="top">
    <div class="glyph"><Icon name="keyboard" size={26} /></div>
    <div class="ident">
      <h3>Apex Pro TKL Gen 3</h3>
      <p class="meta">{kb?.connected ? "connected" : "not connected"}</p>
    </div>
  </header>

  {#if lighting && kb}
    <section>
      <label class="toggle">
        <input
          type="checkbox"
          checked={lighting.enabled}
          disabled={busy}
          onchange={(e) => apply({ enabled: e.currentTarget.checked })}
        />
        gglinux controls the lighting (off: the keyboard's own profile)
      </label>
    </section>

    {#if lighting.enabled}
      <section>
        <div class="head"><h4>Effect</h4></div>
        <div class="segmented">
          {#each EFFECTS as e (e.kind)}
            <button class:on={lighting.effect.kind === e.kind} disabled={busy} onclick={() => setEffect(e.kind)}>
              {e.label}
            </button>
          {/each}
        </div>
        {#if lighting.effect.kind === "breathing"}
          <div class="row">
            <span class="label">Colour</span>
            <input
              type="color"
              value={hex(lighting.effect.color)}
              onchange={(e) =>
                apply({ effect: { kind: "breathing", color: rgbOf(e.currentTarget.value), period_s: period } })}
            />
          </div>
        {/if}
        {#if lighting.effect.kind !== "static"}
          <div class="row">
            <span class="label">Speed</span>
            <input
              type="range"
              min="0.5"
              max="20"
              step="0.5"
              value={period}
              onchange={(e) => setPeriod(e.currentTarget.valueAsNumber)}
            />
            <span class="unit">{period} s</span>
          </div>
        {/if}
        <div class="row">
          <span class="label">Brightness</span>
          <input
            type="range"
            min="0"
            max="100"
            step="5"
            value={lighting.brightness}
            onchange={(e) => apply({ brightness: e.currentTarget.valueAsNumber })}
          />
          <span class="unit">{lighting.brightness}%</span>
        </div>
      </section>

      {#if lighting.effect.kind === "static"}
        <section>
          <div class="head"><h4>Keys</h4></div>
          <svg class="board" viewBox="-4 -4 {width + 8} {height + 8}" role="group" aria-label="Keyboard">
            {#each kb.layout as k (k.code)}
              <g class="key" class:selected={selected.has(k.code)}>
                <rect
                  x={k.x * U + 2}
                  y={k.y * U + 2}
                  width={k.w * U - 4}
                  height={U - 4}
                  rx="5"
                  fill={painted[k.code] ? hex(painted[k.code]) : "var(--surface-2)"}
                  role="button"
                  tabindex="-1"
                  aria-label={k.label || "Space"}
                  onclick={() => toggle(k.code)}
                  onkeydown={() => {}}
                />
                <text x={k.x * U + (k.w * U) / 2} y={k.y * U + U / 2 + 4}>{k.label}</text>
              </g>
            {/each}
          </svg>
          <div class="row">
            <input type="color" bind:value={brush} />
            <button class="ghost" disabled={selected.size === 0} onclick={() => paint([...selected])}>
              Paint selected ({selected.size})
            </button>
            <button class="ghost" onclick={() => paint(kb!.layout.map((k) => k.code))}>Paint all</button>
            <button class="ghost" onclick={() => selected.clear()}>Clear selection</button>
          </div>
          <button class="apply" disabled={busy} onclick={applyStatic}>Apply to keyboard</button>
        </section>
      {/if}
    {/if}
  {/if}

  {#if error}
    <p class="error">{error}</p>
  {/if}
</article>

<style>
  .panel { padding: 1rem 1.1rem; background: var(--surface); border: 1px solid var(--line); border-radius: 12px; }
  .top { display: flex; gap: 1rem; align-items: center; }
  .glyph { flex: none; width: 46px; height: 46px; display: grid; place-items: center; border-radius: 10px; background: var(--surface-2); color: var(--accent); }
  h3 { margin: 0; font-size: 0.98rem; font-weight: 600; }
  h4 { margin: 0; font-size: 0.8rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-dim); }
  .meta { margin: 0.25rem 0 0; font-size: 0.82rem; color: var(--text-dim); }
  section { margin-top: 1.1rem; padding-top: 1rem; border-top: 1px solid var(--line); }
  .head { display: flex; align-items: center; justify-content: space-between; gap: 0.6rem; margin-bottom: 0.6rem; }
  .toggle { display: flex; align-items: center; gap: 0.5rem; font-size: 0.85rem; cursor: pointer; }
  .row { display: flex; align-items: center; gap: 0.6rem; margin-top: 0.7rem; flex-wrap: wrap; }
  .label { width: 5.5rem; font-size: 0.82rem; color: var(--text-dim); }
  .unit { font-size: 0.78rem; color: var(--text-dim); }
  .segmented { display: inline-flex; flex-wrap: wrap; background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; overflow: hidden; }
  .segmented button { padding: 0.35rem 0.75rem; font-size: 0.82rem; color: var(--text-dim); background: none; border: none; cursor: pointer; }
  .segmented button.on { color: var(--bg); background: var(--accent); font-weight: 600; }
  .board { width: 100%; max-width: 48rem; height: auto; user-select: none; }
  .key rect { stroke: var(--line); stroke-width: 1.5; cursor: pointer; }
  .key.selected rect { stroke: var(--accent); stroke-width: 3; }
  .key text { font-size: 10px; text-anchor: middle; fill: var(--text-dim); pointer-events: none; }
  .ghost { padding: 0.25rem 0.55rem; font-size: 0.78rem; color: var(--text-dim); background: none; border: 1px solid var(--line); border-radius: 7px; cursor: pointer; }
  .ghost:disabled { opacity: 0.4; cursor: default; }
  .apply { margin-top: 0.75rem; padding: 0.4rem 0.85rem; font-size: 0.83rem; font-weight: 500; color: var(--text); background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; cursor: pointer; }
  .apply:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .apply:disabled { opacity: 0.5; cursor: default; }
  .error { margin: 0.8rem 0 0; padding: 0.5rem 0.7rem; font-size: 0.8rem; color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); border-radius: 8px; }
</style>
```

- [ ] **Step 4: Mount** — `App.svelte`: import `KeyboardPanel`; `const APEX_PID = 0x1642; let hasApex = $derived(daemon.devices.some((d) => d.product_id === APEX_PID));`; after the Nova Pro block: `{#if hasApex}<section class="controls"><KeyboardPanel /></section>{/if}`.
- [ ] **Step 5: Run** `cd ui && npm run check && npx vite build` — 0/0.
- [ ] **Step 6: Commit** `UI: keyboard lighting panel with a per-key editor`

---

### Task 6: Docs

- [ ] `PROTOCOLS.md` §3a: add "**Verified on this unit 2026-09-27**: `0x4B` init, a `0x40` frame (all red, then only G green) and `0x41` hand-back behaved as sent; `0xF5` replied `f5 00 01` (US)."
- [ ] `PLAN.md` M4: add "Progress 2026-09-27 — **M4a done in software**: `gg-device-keyboard` (allowlisted protocol: only `0x23 0x40 0x41 0x4B 0x90 0xA3 0xF5` can be framed or written; ANSI TKL layout), a keyboard worker running static per-key colours and breathing / wave / spectrum at 30 fps, handing lighting back to the keyboard when off or on exit; a Keyboard panel with a per-key editor (spec: `docs/superpowers/specs/2026-09-27-keyboard-lighting-design.md`). Next: M4b OLED (framing to confirm on-device), then reactive lighting (needs an input-access decision)."
- [ ] Commit `docs: keyboard protocol verified; M4a progress`

---

### Task 7: Hardware acceptance (owner)

- [ ] Turn gglinux lighting on (Wave) → the rainbow moves across the keys; typing unaffected.
- [ ] Breathing and Spectrum work; Speed and Brightness change them.
- [ ] Static: select keys, paint, apply → exactly those keys show those colours.
- [ ] Turn it off → the keyboard's own lighting returns. Stop the daemon while on → the same.
