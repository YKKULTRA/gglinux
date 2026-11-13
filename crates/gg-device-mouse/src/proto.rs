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
/// Set the sleep timer. Not in rivalcfg: from our own GG 123 captures (2026-11-08,
/// `docs/PROTOCOLS.md`), where GG sends it only when saving, followed by [`CMD_SAVE`].
pub const CMD_SLEEP_TIMER: u8 = 0x2B;
/// Read the sleep timer back (same captures); the answer is laid out as [`CMD_SLEEP_TIMER`]'s
/// arguments.
pub const CMD_READ_SLEEP_TIMER: u8 = 0xAB;
/// The sleep timers GG offers, in whole minutes: the only values seen on the wire.
pub const SLEEP_MINUTES: std::ops::RangeInclusive<u8> = 1..=20;
/// Where the timer's seconds (`u16`, little-endian) sit, counting from the command byte.
const SLEEP_OFFSET: usize = 5;
/// Set how the mouse talks to its dongle (GG's High-Efficiency Mode and Wireless
/// Stability). Same captures; GG sends it at once, without a save.
pub const CMD_RADIO: u8 = 0x2E;
/// Read it back: the answer is [`CMD_RADIO`]'s two arguments.
pub const CMD_READ_RADIO: u8 = 0xAE;
/// Read the DPI stages back: the answer is [`CMD_SENSITIVITY`]'s arguments.
pub const CMD_READ_SENSITIVITY: u8 = 0xAC;
/// Read the polling rate back: the answer is [`CMD_POLLING_RATE`]'s code.
pub const CMD_READ_POLLING_RATE: u8 = 0x97;
/// Set what the scroll wheel does: `1A <up entry> <down entry>`, each entry five bytes like
/// a field of the buttons table [`CMD_BUTTONS`]. Not in rivalcfg: from our own GG 123
/// capture rival-5 (2026-11-13, `docs/PROTOCOLS.md` §1 "Rival: more from GG 123"), where GG
/// sends it on Save, followed by [`CMD_SAVE`]. Nothing reads it back.
pub const CMD_WHEEL: u8 = 0x1A;

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

pub fn read_polling_rate_packet() -> Vec<u8> {
    vec![CMD_READ_POLLING_RATE]
}

/// The answer to `97`: the rate's code. `None` for a code no rate has. A mouse that
/// isn't answering reads as all zeros, which is also 1000 Hz's code: the caller decides
/// whether the mouse answered at all (see [`decode_sensitivity`], which can tell).
pub fn decode_polling_rate(data: &[u8]) -> Option<PollingRate> {
    [
        PollingRate::Hz1000,
        PollingRate::Hz500,
        PollingRate::Hz250,
        PollingRate::Hz125,
    ]
    .into_iter()
    .find(|r| Some(&r.code()) == data.first())
}

pub fn read_sensitivity_packet() -> Vec<u8> {
    vec![CMD_READ_SENSITIVITY]
}

/// The DPI of a TrueMove Air code, or `None` for a code the sensor table doesn't have.
pub fn dpi_of_code(code: u8) -> Option<u16> {
    let stage = TRUEMOVE_AIR.iter().position(|&c| c == code)?;
    Some((stage as u16 + 1) * DPI_STEP)
}

/// The answer to `AC`: `<count> <selected, from 1> <x1..x5> <y1..y5>`, the arguments of
/// `2C`. Gives the live stages and which one is active (from 0). `None` unless the count,
/// the selection and every live code make sense: all zeros (no answer) is `None`.
pub fn decode_sensitivity(data: &[u8]) -> Option<(Vec<DpiStage>, usize)> {
    let count = usize::from(*data.first()?);
    let selected = usize::from(*data.get(1)?);
    if !(1..=MAX_PRESETS).contains(&count) || !(1..=count).contains(&selected) {
        return None;
    }
    let xs = data.get(2..2 + MAX_PRESETS)?;
    let ys = data.get(2 + MAX_PRESETS..2 + 2 * MAX_PRESETS)?;
    let stages = (0..count)
        .map(|i| {
            Some(DpiStage {
                x: dpi_of_code(xs[i])?,
                y: dpi_of_code(ys[i])?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some((stages, selected - usize::from(FIRST_PRESET)))
}

/// How the mouse talks to its dongle. GG shows it as two switches, High-Efficiency Mode
/// and Wireless Stability; these three are every state it sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Radio {
    /// GG's High-Efficiency Mode on: `2E 00`.
    HighEfficiency,
    /// Both off: `2E 02 00`.
    Normal,
    /// GG's Wireless Stability on: `2E 02 01`.
    Stable,
}

impl Radio {
    const ALL: [Radio; 3] = [Radio::HighEfficiency, Radio::Normal, Radio::Stable];

    pub(crate) fn bytes(self) -> [u8; 2] {
        match self {
            Radio::HighEfficiency => [0x00, 0x00],
            Radio::Normal => [0x02, 0x00],
            Radio::Stable => [0x02, 0x01],
        }
    }
}

pub fn radio_packet(radio: Radio) -> Vec<u8> {
    let [mode, stability] = radio.bytes();
    vec![CMD_RADIO, mode, stability]
}

pub fn read_radio_packet() -> Vec<u8> {
    vec![CMD_READ_RADIO]
}

/// The answer to `AE`: `<mode> <stability>`, as `2E` takes them. `None` for any pair GG
/// never sent. All zeros is High-Efficiency Mode, and also a mouse that isn't answering:
/// as with [`decode_polling_rate`], the caller decides whether it answered.
pub fn decode_radio(data: &[u8]) -> Option<Radio> {
    let pair = data.get(..2)?;
    Radio::ALL.into_iter().find(|r| r.bytes() == pair)
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

/// One wheel action a capture proves: the action, the byte GG sent for it, and where it
/// was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WheelAction {
    pub action: ButtonAction,
    pub code: u8,
    pub seen: &'static str,
}

/// The only actions sent for the wheel: those a capture shows. The buttons table's other
/// actions (other buttons, keys, media keys, DPI, disabled) look alike but were never seen
/// on the wheel, so they aren't sent. Add a row only with the capture that proves it.
pub const WHEEL_ACTIONS: [WheelAction; 3] = [
    WheelAction {
        action: ButtonAction::ScrollUp,
        code: 0x31,
        seen: "rival-5.pcap: GG's default wheel, 1a 31 00 00 00 00 32",
    },
    WheelAction {
        action: ButtonAction::ScrollDown,
        code: 0x32,
        seen: "rival-5.pcap: GG's default wheel, 1a 31 00 00 00 00 32",
    },
    WheelAction {
        action: ButtonAction::Button(4),
        code: 0x04,
        seen: "rival-5.pcap: Scroll Up = Button 4, 1a 04 00 00 00 00 32",
    },
];

/// The wheel as it comes from the factory, and as GG sends it by default: up scrolls up,
/// down scrolls down.
pub const DEFAULT_WHEEL: [ButtonAction; 2] = [ButtonAction::ScrollUp, ButtonAction::ScrollDown];

/// The byte the wheel takes for `action`, or `None` for an action no capture shows on it.
pub fn wheel_code(action: ButtonAction) -> Option<u8> {
    WHEEL_ACTIONS
        .iter()
        .find(|w| w.action == action)
        .map(|w| w.code)
}

/// Build the wheel packet: `1A <up> 00 00 00 00 <down> 00 00 00 00`, as GG sends it.
/// Refuses an action [`WHEEL_ACTIONS`] doesn't have.
pub fn wheel_packet(up: ButtonAction, down: ButtonAction) -> Result<Vec<u8>> {
    let mut packet = vec![0u8; 1 + 2 * BUTTON_FIELD_LEN];
    packet[0] = CMD_WHEEL;
    for (i, action) in [up, down].into_iter().enumerate() {
        let Some(code) = wheel_code(action) else {
            bail!("{action:?} is not known for the wheel yet: no capture shows its bytes");
        };
        packet[1 + i * BUTTON_FIELD_LEN] = code;
    }
    Ok(packet)
}

pub fn save_packet() -> Vec<u8> {
    vec![CMD_SAVE]
}

pub fn battery_packet() -> Vec<u8> {
    CMD_BATTERY.to_vec()
}

/// `2B 00 00 00 00 <seconds u16 LE>`: the mouse sleeps after `minutes` without use.
///
/// Only GG's own range is taken (1 to 20 minutes): no other value has been seen on the
/// wire, so what the mouse does with one is unknown.
pub fn sleep_timer_packet(minutes: u8) -> Result<Vec<u8>> {
    if !SLEEP_MINUTES.contains(&minutes) {
        bail!(
            "the sleep timer takes {} to {} minutes, not {minutes}",
            SLEEP_MINUTES.start(),
            SLEEP_MINUTES.end()
        );
    }
    let mut packet = vec![0u8; SLEEP_OFFSET + 2];
    packet[0] = CMD_SLEEP_TIMER;
    packet[SLEEP_OFFSET..].copy_from_slice(&(u16::from(minutes) * 60).to_le_bytes());
    Ok(packet)
}

pub fn read_sleep_timer_packet() -> Vec<u8> {
    vec![CMD_READ_SLEEP_TIMER]
}

/// The answer to `AB`: `00 00 00 00 <seconds u16 LE>`, the arguments of `2B` without its
/// command byte. `None` for a short answer or a zero (no timer GG would set).
pub fn decode_sleep_timer(data: &[u8]) -> Option<u16> {
    let at = SLEEP_OFFSET - 1;
    let secs = u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?);
    (secs != 0).then_some(secs)
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

    /// Byte for byte as GG 123 sent them on Save (rival-5 capture): Scroll Up as Button 4,
    /// then back to GG's default. The whole report is these eleven bytes and zeros.
    #[test]
    fn the_wheel_goes_out_as_gg_sends_it() {
        use ButtonAction::*;
        assert_eq!(
            wheel_packet(Button(4), ScrollDown).unwrap(),
            [0x1A, 0x04, 0, 0, 0, 0, 0x32, 0, 0, 0, 0]
        );
        assert_eq!(
            wheel_packet(ScrollUp, ScrollDown).unwrap(),
            [0x1A, 0x31, 0, 0, 0, 0, 0x32, 0, 0, 0, 0]
        );
        assert_eq!(
            wheel_packet(DEFAULT_WHEEL[0], DEFAULT_WHEEL[1]).unwrap(),
            wheel_packet(ScrollUp, ScrollDown).unwrap()
        );
        // Each entry has the buttons table's layout: the same byte the button field takes.
        for w in WHEEL_ACTIONS {
            let mut buttons = default_buttons();
            buttons[0] = w.action;
            assert_eq!(buttons_packet(&buttons)[1], w.code, "{:?}", w.action);
            assert!(w.seen.contains(".pcap"), "{:?} has its capture", w.action);
        }
    }

    /// Only what a capture shows goes to the wheel: everything else in the buttons table is
    /// refused there, however alike it looks.
    #[test]
    fn an_action_no_capture_shows_on_the_wheel_is_refused() {
        use ButtonAction::*;
        for action in [
            Button(1),
            Button(2),
            Button(3),
            Button(5),
            Disable,
            DpiCycle,
            Key(0x3E),
            Multimedia(0xCD),
        ] {
            assert_eq!(wheel_code(action), None, "{action:?}");
            let up = wheel_packet(action, ScrollDown).unwrap_err().to_string();
            assert!(up.contains("not known for the wheel yet"), "{up}");
            assert!(wheel_packet(ScrollUp, action).is_err(), "{action:?} down");
        }
        assert_eq!(
            WHEEL_ACTIONS.map(|w| (w.action, w.code)),
            [(ScrollUp, 0x31), (ScrollDown, 0x32), (Button(4), 0x04)]
        );
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

    /// Byte for byte as GG 123 sent them on each switch (rival-1 capture), and the mouse's
    /// answers to `AE` there.
    #[test]
    fn the_radio_modes_go_out_and_read_back_as_in_ggs_capture() {
        assert_eq!(radio_packet(Radio::HighEfficiency), [0x2E, 0x00, 0x00]);
        assert_eq!(radio_packet(Radio::Normal), [0x2E, 0x02, 0x00]);
        assert_eq!(radio_packet(Radio::Stable), [0x2E, 0x02, 0x01]);
        assert_eq!(read_radio_packet(), [0xAE]);
        let mut answer = [0u8; 64];
        assert_eq!(decode_radio(&answer), Some(Radio::HighEfficiency));
        answer[0] = 0x02;
        assert_eq!(decode_radio(&answer), Some(Radio::Normal));
        answer[1] = 0x01;
        assert_eq!(decode_radio(&answer), Some(Radio::Stable));
        for unseen in [[0x00, 0x01], [0x01, 0x00], [0x02, 0x02], [0x03, 0x00]] {
            assert_eq!(decode_radio(&unseen), None, "{unseen:02X?}");
        }
        assert_eq!(decode_radio(&[0x02]), None, "short");
    }

    /// The mouse's answers to `AC` in the captures, next to the `2C` GG had just sent.
    #[test]
    fn the_dpi_stages_read_back_from_the_mouses_answer() {
        assert_eq!(read_sensitivity_packet(), [0xAC]);
        let mut answer = [0u8; 64];
        // GG's defaults, stage 3 active: 400, 800, 1600, 2400, 3200.
        answer[..12].copy_from_slice(&[
            5, 3, 0x04, 0x09, 0x12, 0x1B, 0x26, 0x04, 0x09, 0x12, 0x1B, 0x26,
        ]);
        let (stages, active) = decode_sensitivity(&answer).unwrap();
        assert_eq!(stages, [400, 800, 1600, 2400, 3200].map(DpiStage::uniform));
        assert_eq!(active, 2);
        // Stage 1 at X 500, Y 400, and active.
        answer[..12].copy_from_slice(&[
            5, 1, 0x05, 0x09, 0x12, 0x1B, 0x26, 0x04, 0x09, 0x12, 0x1B, 0x26,
        ]);
        let (stages, active) = decode_sensitivity(&answer).unwrap();
        assert_eq!((stages[0], active), (DpiStage { x: 500, y: 400 }, 0));
        // What comes back is what would be sent.
        let sent = sensitivity_packet(&stages, active).unwrap();
        assert_eq!(sent[1..], answer[..12]);
        // Two stages: only the live codes are read.
        answer[..12].copy_from_slice(&[2, 2, 0x09, 0x12, 0, 0, 0, 0x09, 0x12, 0, 0, 0]);
        let (stages, active) = decode_sensitivity(&answer).unwrap();
        assert_eq!((stages.len(), active), (2, 1));
    }

    #[test]
    fn a_dpi_answer_that_makes_no_sense_reads_as_nothing() {
        assert_eq!(decode_sensitivity(&[0u8; 64]), None, "no answer");
        let ok = [
            5, 3, 0x04, 0x09, 0x12, 0x1B, 0x26, 0x04, 0x09, 0x12, 0x1B, 0x26,
        ];
        assert!(decode_sensitivity(&ok).is_some());
        for (at, bad, why) in [
            (0, 6, "six stages"),
            (1, 0, "no stage active"),
            (1, 6, "a stage past the count"),
            (2, 0x01, "a code the sensor has none for"),
        ] {
            let mut answer = ok;
            answer[at] = bad;
            assert_eq!(decode_sensitivity(&answer), None, "{why}");
        }
        assert_eq!(decode_sensitivity(&ok[..11]), None, "short");
        // Every code the table has comes back as its DPI.
        for dpi in (DPI_MIN..=DPI_MAX).step_by(usize::from(DPI_STEP)) {
            assert_eq!(dpi_of_code(dpi_code(dpi)), Some(dpi));
        }
    }

    /// The mouse's answers to `97` in the captures: `03` before (125 Hz), `01` after 500 Hz
    /// was saved, `00` after 1000 Hz.
    #[test]
    fn the_polling_rate_reads_back_from_the_mouses_answer() {
        assert_eq!(read_polling_rate_packet(), [0x97]);
        for (code, rate) in [
            (0x00, PollingRate::Hz1000),
            (0x01, PollingRate::Hz500),
            (0x02, PollingRate::Hz250),
            (0x03, PollingRate::Hz125),
        ] {
            assert_eq!(decode_polling_rate(&[code, 0, 0]), Some(rate));
        }
        assert_eq!(decode_polling_rate(&[0x04]), None);
        assert_eq!(decode_polling_rate(&[]), None);
    }

    /// Byte for byte as GG 123 sent them when saving 20 and 10 minutes (rival-2/-3b captures).
    #[test]
    fn the_sleep_timer_goes_out_as_gg_sends_it() {
        assert_eq!(
            sleep_timer_packet(20).unwrap(),
            [0x2B, 0x00, 0x00, 0x00, 0x00, 0xB0, 0x04]
        );
        assert_eq!(
            sleep_timer_packet(10).unwrap(),
            [0x2B, 0x00, 0x00, 0x00, 0x00, 0x58, 0x02]
        );
        assert_eq!(sleep_timer_packet(1).unwrap()[5..], [0x3C, 0x00]);
    }

    #[test]
    fn a_sleep_timer_outside_ggs_range_is_refused() {
        for minutes in [0, 21, 255] {
            let e = sleep_timer_packet(minutes).unwrap_err().to_string();
            assert!(e.contains("1 to 20 minutes"), "{minutes}: {e}");
        }
    }

    /// The mouse's answers to `AB` in the captures: 5, 10 and 20 minutes.
    #[test]
    fn the_sleep_timer_reads_back_from_the_mouses_answer() {
        assert_eq!(read_sleep_timer_packet(), [0xAB]);
        let mut answer = [0u8; 64];
        for (bytes, secs) in [
            ([0x2C, 0x01], 300),
            ([0x58, 0x02], 600),
            ([0xB0, 0x04], 1200),
        ] {
            answer[4..6].copy_from_slice(&bytes);
            assert_eq!(decode_sleep_timer(&answer), Some(secs));
        }
        assert_eq!(decode_sleep_timer(&[0, 0, 0, 0, 0x2C]), None, "short");
        assert_eq!(decode_sleep_timer(&[0u8; 64]), None, "zero");
    }
}
