// SPDX-License-Identifier: GPL-3.0-or-later
//! Packet construction for the Rival 3 Wireless Gen 2.
//!
//! Pure functions over bytes — no I/O — so the wire format is unit-testable without
//! the mouse present. Layouts ported from rivalcfg's `rival3_wireless_gen2.py` and
//! its `multidpi_range_choice_xy` / `buttons` handlers (WTFPL).

use anyhow::{bail, Result};

use crate::tables::{self, DPI_MAX, DPI_MIN, DPI_STEP, TRUEMOVE_AIR};

pub const CMD_SENSITIVITY: u8 = 0x2C;
pub const CMD_POLLING_RATE: u8 = 0x17;
pub const CMD_BUTTONS: u8 = 0x19;
pub const CMD_SAVE: u8 = 0x09;
pub const CMD_BATTERY: [u8; 2] = [0xAA, 0x01];

/// The firmware always parses five DPI slots, whatever `count` says.
pub const MAX_PRESETS: usize = 5;
/// Preset indices on the wire are 1-based (`first_preset` in the rivalcfg profile).
const FIRST_PRESET: u8 = 1;

pub const BUTTON_COUNT: usize = 6;
/// Each button gets a fixed-width field; only the first two bytes are ever used.
const BUTTON_FIELD_LEN: usize = 5;

const BTN_DISABLE: u8 = 0x00;
const BTN_KEYBOARD: u8 = 0x51;
const BTN_MULTIMEDIA: u8 = 0x61;
const BTN_DPI_SWITCH: u8 = 0x30;
const BTN_SCROLL_UP: u8 = 0x31;
const BTN_SCROLL_DOWN: u8 = 0x32;

/// The DPI the sensor runs for a requested one: clamped to its range and rounded
/// to the nearest supported stage. This is what [`dpi_code`] encodes.
pub fn nearest_dpi(dpi: u16) -> u16 {
    let clamped = dpi.clamp(DPI_MIN, DPI_MAX) as u32;
    let step = DPI_STEP as u32;
    let stage = ((clamped + step / 2) / step).max(1);
    (stage * step) as u16
}

/// Map a DPI to its TrueMove Air code, rounding to the nearest supported stage.
pub fn dpi_code(dpi: u16) -> u8 {
    // Stage 1 == DPI_MIN == index 0.
    TRUEMOVE_AIR[(nearest_dpi(dpi) / DPI_STEP - 1) as usize]
}

/// One DPI stage. X and Y can differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DpiStage {
    pub x: u16,
    pub y: u16,
}

impl DpiStage {
    pub fn uniform(dpi: u16) -> Self {
        Self { x: dpi, y: dpi }
    }
}

/// Build the sensitivity packet: `2C <count> <selected> <x1..x5> <y1..y5>`.
///
/// # The rivalcfg #273 hazard
///
/// This command uses `xxyy` ordering — all five X codes, *then* all five Y codes.
/// The Y block therefore starts at a **fixed** offset that does not depend on how
/// many stages you actually configured. Emitting a short X block (the obvious
/// encoding when you only want two stages) slides every Y value left into the X
/// region; the firmware then reads zeros for Y and **permanently disables the
/// mouse's vertical axis**.
///
/// The fix is structural here rather than a padding step that could be forgotten:
/// `xs` and `ys` are fixed-size `[u8; MAX_PRESETS]` arrays, so a short X block is
/// not representable. Unused slots stay `0x00`, which is what rivalcfg pads with —
/// they are inert because `count` tells the firmware how many stages are live.
pub fn sensitivity_packet(stages: &[DpiStage], selected: usize) -> Result<Vec<u8>> {
    if stages.is_empty() {
        bail!("at least one DPI stage is required");
    }
    if stages.len() > MAX_PRESETS {
        bail!(
            "{} DPI stages requested but the device accepts at most {}",
            stages.len(),
            MAX_PRESETS
        );
    }
    if selected >= stages.len() {
        bail!(
            "selected stage {} is out of range (only {} configured)",
            selected,
            stages.len()
        );
    }

    let mut xs = [0u8; MAX_PRESETS];
    let mut ys = [0u8; MAX_PRESETS];
    for (i, stage) in stages.iter().enumerate() {
        xs[i] = dpi_code(stage.x);
        ys[i] = dpi_code(stage.y);
    }

    let mut packet = Vec::with_capacity(3 + 2 * MAX_PRESETS);
    packet.push(CMD_SENSITIVITY);
    packet.push(stages.len() as u8);
    packet.push(FIRST_PRESET + selected as u8);
    packet.extend_from_slice(&xs);
    packet.extend_from_slice(&ys);
    Ok(packet)
}

/// Report rate. The wire codes run backwards from the frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollingRate {
    Hz1000,
    Hz500,
    Hz250,
    Hz125,
}

impl PollingRate {
    pub fn code(self) -> u8 {
        match self {
            PollingRate::Hz1000 => 0x00,
            PollingRate::Hz500 => 0x01,
            PollingRate::Hz250 => 0x02,
            PollingRate::Hz125 => 0x03,
        }
    }

    pub fn hz(self) -> u16 {
        match self {
            PollingRate::Hz1000 => 1000,
            PollingRate::Hz500 => 500,
            PollingRate::Hz250 => 250,
            PollingRate::Hz125 => 125,
        }
    }

    pub fn from_hz(hz: u16) -> Result<Self> {
        Ok(match hz {
            1000 => PollingRate::Hz1000,
            500 => PollingRate::Hz500,
            250 => PollingRate::Hz250,
            125 => PollingRate::Hz125,
            other => bail!("unsupported polling rate {other} Hz (want 1000, 500, 250 or 125)"),
        })
    }
}

pub fn polling_rate_packet(rate: PollingRate) -> Vec<u8> {
    vec![CMD_POLLING_RATE, rate.code()]
}

/// What a physical mouse button does when pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonAction {
    /// Act as physical button `n` (1-based) — the identity mapping for defaults.
    Button(u8),
    Disable,
    DpiCycle,
    ScrollUp,
    ScrollDown,
    /// HID keyboard usage code.
    Key(u8),
    /// HID consumer-page usage code.
    Multimedia(u8),
}

impl ButtonAction {
    /// Resolve a user-facing name (`"button3"`, `"dpi"`, `"volumeup"`, `"f5"`).
    ///
    /// Resolution order matches rivalcfg: physical buttons, then special actions,
    /// then multimedia, then keyboard — multimedia wins ties so that e.g. `mute`
    /// stays a media key.
    pub fn parse(name: &str) -> Result<Self> {
        let n = name.trim().to_ascii_lowercase();

        if let Some(rest) = n.strip_prefix("button") {
            if let Ok(idx) = rest.parse::<u8>() {
                if (1..=BUTTON_COUNT as u8).contains(&idx) {
                    return Ok(ButtonAction::Button(idx));
                }
                bail!("button index {idx} out of range (1..={BUTTON_COUNT})");
            }
        }

        match n.as_str() {
            "disable" | "disabled" => return Ok(ButtonAction::Disable),
            "dpi" => return Ok(ButtonAction::DpiCycle),
            "scrollup" => return Ok(ButtonAction::ScrollUp),
            "scrolldown" | "scrolldwn" | "scrolldn" => return Ok(ButtonAction::ScrollDown),
            _ => {}
        }

        if let Some(code) = tables::lookup(tables::MULTIMEDIA, &n) {
            return Ok(ButtonAction::Multimedia(code));
        }
        if let Some(code) = tables::lookup(tables::KEYBOARD, &n) {
            return Ok(ButtonAction::Key(code));
        }
        bail!("unknown button action '{name}'")
    }
}

/// The factory mapping: each button is itself, except button 6 which cycles DPI.
pub fn default_buttons() -> [ButtonAction; BUTTON_COUNT] {
    [
        ButtonAction::Button(1),
        ButtonAction::Button(2),
        ButtonAction::Button(3),
        ButtonAction::Button(4),
        ButtonAction::Button(5),
        ButtonAction::DpiCycle,
    ]
}

/// Build the button-mapping packet: `19` followed by six 5-byte fields.
pub fn buttons_packet(actions: &[ButtonAction; BUTTON_COUNT]) -> Vec<u8> {
    let mut packet = vec![0u8; 1 + BUTTON_COUNT * BUTTON_FIELD_LEN];
    packet[0] = CMD_BUTTONS;

    for (i, action) in actions.iter().enumerate() {
        let off = 1 + i * BUTTON_FIELD_LEN;
        match *action {
            ButtonAction::Button(n) => packet[off] = n,
            ButtonAction::Disable => packet[off] = BTN_DISABLE,
            ButtonAction::DpiCycle => packet[off] = BTN_DPI_SWITCH,
            ButtonAction::ScrollUp => packet[off] = BTN_SCROLL_UP,
            ButtonAction::ScrollDown => packet[off] = BTN_SCROLL_DOWN,
            ButtonAction::Key(code) => {
                packet[off] = BTN_KEYBOARD;
                packet[off + 1] = code;
            }
            ButtonAction::Multimedia(code) => {
                packet[off] = BTN_MULTIMEDIA;
                packet[off + 1] = code;
            }
        }
    }
    packet
}

pub fn save_packet() -> Vec<u8> {
    vec![CMD_SAVE]
}

pub fn battery_packet() -> Vec<u8> {
    CMD_BATTERY.to_vec()
}

/// Battery state as reported by `AA 01`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    /// Charge percentage, 1..=100.
    pub level: u8,
    pub charging: bool,
}

/// Decode an `AA 01` reply. Returns `None` when the reading isn't meaningful.
///
/// A level of `0` means the mouse is asleep or hasn't reported yet rather than
/// "empty" — a mouse at a true 0% would not be answering at all.
pub fn decode_battery(data: &[u8]) -> Option<Battery> {
    if data.len() < 3 {
        return None;
    }
    let level = data[0];
    if level == 0 || level > 100 {
        return None;
    }
    Some(Battery {
        level,
        charging: data[2] != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_codes_match_the_sensor_table() {
        assert_eq!(dpi_code(100), 0x00);
        assert_eq!(dpi_code(800), 0x09);
        assert_eq!(dpi_code(1600), 0x12);
        assert_eq!(dpi_code(18000), 0xD6);
    }

    #[test]
    fn dpi_out_of_range_is_clamped_not_wrapped() {
        assert_eq!(dpi_code(0), dpi_code(DPI_MIN));
        assert_eq!(dpi_code(50), dpi_code(DPI_MIN));
        assert_eq!(dpi_code(65535), dpi_code(DPI_MAX));
    }

    #[test]
    fn dpi_rounds_to_the_nearest_stage() {
        assert_eq!(dpi_code(1580), dpi_code(1600));
        assert_eq!(dpi_code(1610), dpi_code(1600));
        // The sensor skips codes, so neighbouring stages must stay distinct.
        assert_ne!(dpi_code(1300), dpi_code(1400));
    }

    #[test]
    fn nearest_dpi_is_the_stage_dpi_code_writes() {
        assert_eq!(nearest_dpi(1549), 1500);
        assert_eq!(nearest_dpi(1550), 1600);
        assert_eq!(nearest_dpi(0), DPI_MIN);
        assert_eq!(nearest_dpi(u16::MAX), DPI_MAX);
        for dpi in [0, 50, 149, 150, 1550, 17951, 18049, 30000, u16::MAX] {
            assert_eq!(dpi_code(nearest_dpi(dpi)), dpi_code(dpi), "{dpi}");
        }
    }

    #[test]
    fn sensitivity_packet_has_the_documented_layout() {
        let p = sensitivity_packet(&[DpiStage::uniform(800), DpiStage::uniform(1600)], 0).unwrap();
        assert_eq!(p[0], CMD_SENSITIVITY);
        assert_eq!(p[1], 2, "count");
        assert_eq!(p[2], 1, "selected preset is 1-based");
        assert_eq!(p.len(), 3 + 2 * MAX_PRESETS);
    }

    /// The regression that matters: rivalcfg #273. Fewer than five stages must
    /// still emit a full five-wide X block, so the Y block keeps its fixed offset.
    /// If this ever fails, the encoder is capable of bricking the vertical axis.
    #[test]
    fn short_preset_lists_still_pad_the_x_block() {
        let p = sensitivity_packet(&[DpiStage { x: 800, y: 1600 }], 0).unwrap();
        let (xs, ys) = (&p[3..3 + MAX_PRESETS], &p[3 + MAX_PRESETS..]);

        assert_eq!(xs.len(), MAX_PRESETS);
        assert_eq!(ys.len(), MAX_PRESETS);
        assert_eq!(xs[0], dpi_code(800));
        assert_eq!(ys[0], dpi_code(1600), "Y must not slide into the X block");
        assert!(xs[1..].iter().all(|&b| b == 0), "unused X slots are zero");
        assert!(ys[1..].iter().all(|&b| b == 0), "unused Y slots are zero");
    }

    #[test]
    fn asymmetric_stages_keep_x_and_y_separate() {
        let p = sensitivity_packet(
            &[DpiStage { x: 400, y: 800 }, DpiStage { x: 1600, y: 3200 }],
            1,
        )
        .unwrap();
        assert_eq!(p[2], 2, "second preset selected");
        assert_eq!(&p[3..5], &[dpi_code(400), dpi_code(1600)]);
        assert_eq!(&p[8..10], &[dpi_code(800), dpi_code(3200)]);
    }

    #[test]
    fn sensitivity_rejects_bad_input() {
        assert!(sensitivity_packet(&[], 0).is_err(), "empty");
        let six = vec![DpiStage::uniform(800); 6];
        assert!(sensitivity_packet(&six, 0).is_err(), "too many");
        assert!(
            sensitivity_packet(&[DpiStage::uniform(800)], 1).is_err(),
            "selected out of range"
        );
    }

    #[test]
    fn polling_rates_round_trip() {
        for hz in [1000u16, 500, 250, 125] {
            assert_eq!(PollingRate::from_hz(hz).unwrap().hz(), hz);
        }
        assert!(PollingRate::from_hz(2000).is_err());
        assert_eq!(polling_rate_packet(PollingRate::Hz1000), vec![0x17, 0x00]);
        assert_eq!(polling_rate_packet(PollingRate::Hz125), vec![0x17, 0x03]);
    }

    #[test]
    fn default_buttons_packet_matches_the_factory_mapping() {
        let p = buttons_packet(&default_buttons());
        assert_eq!(p[0], CMD_BUTTONS);
        assert_eq!(p.len(), 1 + BUTTON_COUNT * BUTTON_FIELD_LEN);
        // Buttons 1-5 are the identity mapping at their 5-byte offsets.
        for i in 0..5u8 {
            assert_eq!(p[1 + i as usize * BUTTON_FIELD_LEN], i + 1);
        }
        assert_eq!(p[1 + 5 * BUTTON_FIELD_LEN], BTN_DPI_SWITCH);
    }

    #[test]
    fn keyboard_and_multimedia_actions_use_two_bytes() {
        let mut map = default_buttons();
        map[3] = ButtonAction::parse("f5").unwrap();
        map[4] = ButtonAction::parse("volumeup").unwrap();
        let p = buttons_packet(&map);

        let off4 = 1 + 3 * BUTTON_FIELD_LEN;
        assert_eq!(p[off4], BTN_KEYBOARD);
        assert_eq!(p[off4 + 1], tables::lookup(tables::KEYBOARD, "f5").unwrap());

        let off5 = 1 + 4 * BUTTON_FIELD_LEN;
        assert_eq!(p[off5], BTN_MULTIMEDIA);
        assert_eq!(
            p[off5 + 1],
            tables::lookup(tables::MULTIMEDIA, "volumeup").unwrap()
        );
    }

    #[test]
    fn button_actions_parse_by_name() {
        assert_eq!(
            ButtonAction::parse("Button3").unwrap(),
            ButtonAction::Button(3)
        );
        assert_eq!(
            ButtonAction::parse("  DPI ").unwrap(),
            ButtonAction::DpiCycle
        );
        assert_eq!(
            ButtonAction::parse("disabled").unwrap(),
            ButtonAction::Disable
        );
        assert_eq!(
            ButtonAction::parse("scrolldn").unwrap(),
            ButtonAction::ScrollDown
        );
        assert!(matches!(
            ButtonAction::parse("vol+"),
            Ok(ButtonAction::Multimedia(_))
        ));
        assert!(matches!(
            ButtonAction::parse("esc"),
            Ok(ButtonAction::Key(_))
        ));
        assert!(ButtonAction::parse("button9").is_err());
        assert!(ButtonAction::parse("nonsense").is_err());
    }

    #[test]
    fn battery_decoding_rejects_meaningless_readings() {
        // The reply actually observed from the hardware.
        assert_eq!(
            decode_battery(&[0x10, 0x00, 0x00]),
            Some(Battery {
                level: 16,
                charging: false
            })
        );
        assert_eq!(
            decode_battery(&[0x64, 0x00, 0x01]),
            Some(Battery {
                level: 100,
                charging: true
            })
        );
        assert_eq!(decode_battery(&[0x00, 0, 0]), None, "0 means asleep");
        assert_eq!(decode_battery(&[0xFF, 0, 0]), None, "out of range");
        assert_eq!(decode_battery(&[0x10, 0x00]), None, "short reply");
    }
}
