//! Packet builders and reply decoding for the Arctis Nova Pro Wireless.
//!
//! Commands are written as report `0x06`. The opcode is the byte *after* the
//! report id, so the tables in `docs/PROTOCOLS.md` that read `06 39 <lvl>`
//! correspond to `sidetone_packet` emitting `[0x39, lvl]` — the `0x06` is added by
//! the transport, not here.
//!
//! **Replies come back on report `0x06` too**, echoing the opcode, which is not
//! what the reference implementations describe (they document `07 b0`). Report
//! `0x07` carries the *unsolicited* stream instead: `07 25 <attenuation>` whenever
//! the volume wheel moves — and only then — plus other device-side changes. The
//! steady ~16 ms `07 25` stream seen on 2026-08-12 was a wheel in motion, not a
//! periodic report. Verified on hardware; see `docs/PROTOCOLS.md`.
//!
//! Decoding therefore keys on the opcode rather than the report id, and accepts
//! either id: matching on arrival order instead would decode an ambient interrupt
//! as whatever reply was being waited for.

use anyhow::{bail, ensure, Result};

// --- opcodes ------------------------------------------------------------------

const CMD_SAVE: u8 = 0x09;
const CMD_STATUS: u8 = 0xb0;
const CMD_AUDIO_SETTINGS: u8 = 0x20;
const CMD_VOLUME: u8 = 0x25;
const CMD_ANALOG_GAIN: u8 = 0x27;
const CMD_EQ_PRESET: u8 = 0x2e;
const CMD_EQ_BANDS: u8 = 0x33;
const CMD_MIC_VOLUME: u8 = 0x37;
const CMD_SIDETONE: u8 = 0x39;
const CMD_CHATMIX: u8 = 0x49;
const CMD_OLED_BRIGHTNESS: u8 = 0x85;
const CMD_OLED_FRAME: u8 = 0x93;
const CMD_OLED_RETURN_TO_UI: u8 = 0x95;
const CMD_TRANSPARENCY: u8 = 0xb9;
const CMD_ANC: u8 = 0xbd;
const CMD_MIC_LED: u8 = 0xbf;
const CMD_AUTO_OFF: u8 = 0xc1;
const CMD_WIRELESS_MODE: u8 = 0xc3;

/// Number of EQ bands the custom slot holds.
pub const EQ_BANDS: usize = 10;

/// The custom EQ slot. Bands only take effect once this preset is selected.
pub const EQ_CUSTOM_PRESET: u8 = 4;

/// Highest selectable EQ preset (`0..=3` are factory, `4` is custom).
pub const EQ_PRESET_MAX: u8 = EQ_CUSTOM_PRESET;

/// Mic volume steps: 1 (muted) to 10 (100%). loteran's shipped slider uses this
/// scale; an older comment claiming `0xa0` for 100% is contradicted by it.
pub const MIC_VOLUME_MAX: u8 = 10;

/// Mic-mute LED and transparency levels share a 1–10 scale; 1 is the minimum
/// (HeadsetControl's "off" is 1), there is no 0.
const LEVEL_MIN: u8 = 1;
const LEVEL_MAX: u8 = 10;

/// Volume is an *attenuation*: 0 is loudest, 56 is silent.
pub const VOLUME_ATTEN_MAX: u8 = 56;

/// The battery gauge reports eighths, not percent.
const BATTERY_STEPS: u8 = 8;

/// Report id of the unsolicited event stream (commands and replies use `0x06`).
pub const REPORT_EVENT: u8 = 0x07;

/// Bytes in one OLED frame report after its report id. Frame reports are
/// 1024-byte *feature* reports, unlike the 64-byte output reports commands use.
pub const OLED_FRAME_PAYLOAD: usize = 1023;

/// Widest strip one frame report can carry, and the panel height.
const OLED_STRIP_WIDTH: u8 = 64;
const OLED_HEIGHT: u8 = 64;

/// OLED brightness range. There is no "off": 0 is refused by ggoled and behaves
/// like 1 on this unit (verified 2026-09-27) — blank with a black frame instead.
pub const OLED_BRIGHTNESS_MIN: u8 = 1;
pub const OLED_BRIGHTNESS_MAX: u8 = 10;

// --- setters ------------------------------------------------------------------

/// Commit the current settings to the base station's flash.
///
/// HeadsetControl sends this after every setter (`sendCommandWithSave`); we keep
/// it separate so a caller can batch changes and persist once.
pub fn save_packet() -> Vec<u8> {
    vec![CMD_SAVE]
}

/// Request the status block decoded by [`Status::decode`].
pub fn status_packet() -> Vec<u8> {
    vec![CMD_STATUS]
}

/// Sidetone level, 0 (off) to 3 (high).
pub fn sidetone_packet(level: u8) -> Result<Vec<u8>> {
    ensure!(level <= 3, "sidetone level {level} out of range (0..=3)");
    Ok(vec![CMD_SIDETONE, level])
}

/// Select an EQ preset: `0..=3` factory, [`EQ_CUSTOM_PRESET`] for the custom slot.
pub fn eq_preset_packet(preset: u8) -> Result<Vec<u8>> {
    ensure!(
        preset <= EQ_PRESET_MAX,
        "EQ preset {preset} out of range (0..={EQ_PRESET_MAX})"
    );
    Ok(vec![CMD_EQ_PRESET, preset])
}

/// Ten band gains in dB, each −10.0..=+10.0 in 0.5 dB steps.
///
/// Writing bands does not switch to the custom slot — send [`eq_preset_packet`]
/// with [`EQ_CUSTOM_PRESET`] as well, or the change stays inaudible.
pub fn eq_bands_packet(gains_db: &[f32]) -> Result<Vec<u8>> {
    ensure!(
        gains_db.len() == EQ_BANDS,
        "expected {EQ_BANDS} EQ bands, got {}",
        gains_db.len()
    );
    let mut packet = Vec::with_capacity(1 + EQ_BANDS);
    packet.push(CMD_EQ_BANDS);
    for (i, &db) in gains_db.iter().enumerate() {
        packet.push(encode_gain(db, i)?);
    }
    Ok(packet)
}

/// `0x14` is 0 dB and each step is half a decibel, so the byte is `20 + 2*dB`.
fn encode_gain(db: f32, band: usize) -> Result<u8> {
    if !db.is_finite() || !(-10.0..=10.0).contains(&db) {
        bail!("EQ band {band} gain {db} dB out of range (-10.0..=10.0)");
    }
    let steps = (db * 2.0).round();
    // Exact after the range check above, but clamp rather than trust the float:
    // a wrapped byte here is a loud, arbitrary EQ curve in someone's ears.
    Ok((20.0 + steps).clamp(0.0, 40.0) as u8)
}

/// Microphone volume, 1 (muted) to [`MIC_VOLUME_MAX`].
pub fn mic_volume_packet(volume: u8) -> Result<Vec<u8>> {
    ensure!(
        (1..=MIC_VOLUME_MAX).contains(&volume),
        "mic volume {volume} out of range (1..={MIC_VOLUME_MAX})"
    );
    Ok(vec![CMD_MIC_VOLUME, volume])
}

/// Hardware mic gain. This is an analog (hi-Z) gain toggle, not a sample rate.
pub fn analog_gain_packet(high: bool) -> Vec<u8> {
    vec![CMD_ANALOG_GAIN, if high { 2 } else { 1 }]
}

/// Mic-mute LED brightness, 1..=10.
pub fn mic_led_packet(brightness: u8) -> Result<Vec<u8>> {
    ensure!(
        (LEVEL_MIN..=LEVEL_MAX).contains(&brightness),
        "mic LED brightness {brightness} out of range ({LEVEL_MIN}..={LEVEL_MAX})"
    );
    Ok(vec![CMD_MIC_LED, brightness])
}

/// Inactivity shutdown timer.
pub fn auto_off_packet(timer: AutoOff) -> Vec<u8> {
    vec![CMD_AUTO_OFF, timer.code()]
}

/// 2.4 GHz radio profile.
pub fn wireless_mode_packet(mode: WirelessMode) -> Vec<u8> {
    vec![CMD_WIRELESS_MODE, mode.code()]
}

/// Enable the ChatMix dial. While disabled the dial sends no interrupt reports.
pub fn chatmix_packet(enabled: bool) -> Vec<u8> {
    vec![CMD_CHATMIX, u8::from(enabled)]
}

/// Transparency (passthrough) level, 1..=10.
pub fn transparency_packet(level: u8) -> Result<Vec<u8>> {
    ensure!(
        (LEVEL_MIN..=LEVEL_MAX).contains(&level),
        "transparency level {level} out of range ({LEVEL_MIN}..={LEVEL_MAX})"
    );
    Ok(vec![CMD_TRANSPARENCY, level])
}

/// Noise control mode — what the headset's ANC button cycles through.
pub fn noise_mode_packet(mode: NoiseMode) -> Vec<u8> {
    vec![CMD_ANC, mode.code()]
}

/// Master volume, expressed as attenuation: 0 is loudest, 56 is silent.
pub fn volume_packet(attenuation: u8) -> Result<Vec<u8>> {
    ensure!(
        attenuation <= VOLUME_ATTEN_MAX,
        "volume attenuation {attenuation} out of range (0..={VOLUME_ATTEN_MAX})"
    );
    Ok(vec![CMD_VOLUME, attenuation])
}

/// Ask for the audio settings, whose reply carries the master volume.
pub fn audio_settings_packet() -> Vec<u8> {
    vec![CMD_AUDIO_SETTINGS]
}

/// Master volume from a `06 20` reply, as attenuation (0 loudest, 56 silent).
///
/// Byte 2 is the audio input; byte 3 is the volume, on the same scale as the
/// `07 25` event, so it is known before the wheel first moves.
pub fn decode_volume(report: &[u8]) -> Option<u8> {
    match report {
        [_, CMD_AUDIO_SETTINGS, _, atten, ..] if *atten <= VOLUME_ATTEN_MAX => Some(*atten),
        _ => None,
    }
}

/// The volume carried by a `07 25 <attenuation>` event.
pub fn decode_volume_event(report: &[u8]) -> Option<u8> {
    match report {
        [REPORT_EVENT, CMD_VOLUME, atten, ..] => Some(*atten),
        _ => None,
    }
}

/// OLED brightness, [`OLED_BRIGHTNESS_MIN`]..=[`OLED_BRIGHTNESS_MAX`].
pub fn oled_brightness_packet(level: u8) -> Result<Vec<u8>> {
    ensure!(
        (OLED_BRIGHTNESS_MIN..=OLED_BRIGHTNESS_MAX).contains(&level),
        "screen brightness {level} out of range ({OLED_BRIGHTNESS_MIN}..={OLED_BRIGHTNESS_MAX}) \
         — the lowest level is still lit, so there is no brightness that turns it off"
    );
    Ok(vec![CMD_OLED_BRIGHTNESS, level])
}

/// Hand the screen back to the firmware's own display.
pub fn oled_return_to_ui_packet() -> Vec<u8> {
    vec![CMD_OLED_RETURN_TO_UI]
}

/// The two strips of an all-black frame, each the payload of one frame report.
///
/// Frames go out in strips at most 64 px wide — `93 <x> 00 <width> <height>`, then
/// the pixels column-major, LSB at the top — and here every pixel is off, which is
/// the point: an unlit OLED pixel does not wear. One frame holds until something
/// makes the firmware redraw (a volume change does).
pub fn oled_blank_packets() -> [Vec<u8>; 2] {
    [0, OLED_STRIP_WIDTH].map(|x| {
        let mut packet = vec![0u8; OLED_FRAME_PAYLOAD];
        packet[..5].copy_from_slice(&[CMD_OLED_FRAME, x, 0, OLED_STRIP_WIDTH, OLED_HEIGHT]);
        packet
    })
}

/// Whether a report came from the unsolicited event stream.
pub fn is_event(report: &[u8]) -> bool {
    report.len() >= 2 && report[0] == REPORT_EVENT
}

/// Sort one report read while waiting for the reply to `awaiting`.
///
/// Returns the report if it is that reply. Otherwise an event is appended to
/// `events` and anything else — a late reply to an earlier request — is dropped,
/// so it can neither be mistaken for this reply nor for something that happened
/// on the device.
pub fn sort_report(
    report: Vec<u8>,
    awaiting: Option<u8>,
    events: &mut Vec<Vec<u8>>,
) -> Option<Vec<u8>> {
    if awaiting.is_some() && report.get(1) == awaiting.as_ref() {
        return Some(report);
    }
    if is_event(&report) {
        events.push(report);
    }
    None
}

/// The init sequence loteran sends before applying settings.
///
/// Each entry is one command. They are undocumented beyond "the vendor tool sends
/// these", so they are reproduced verbatim rather than interpreted.
pub fn init_sequence() -> Vec<Vec<u8>> {
    vec![
        vec![0x20],
        vec![0x10],
        vec![0x80],
        vec![0x3b],
        vec![0xb2],
        vec![0xb3],
        vec![0xb7],
        vec![0x83, 0x01],
        vec![0x69, 0x00],
    ]
}

// --- enums --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoOff {
    /// Never switch off.
    Never,
    Min1,
    Min5,
    Min10,
    Min15,
    Min30,
    Min60,
}

impl AutoOff {
    pub fn code(self) -> u8 {
        match self {
            AutoOff::Never => 0,
            AutoOff::Min1 => 1,
            AutoOff::Min5 => 2,
            AutoOff::Min10 => 3,
            AutoOff::Min15 => 4,
            AutoOff::Min30 => 5,
            AutoOff::Min60 => 6,
        }
    }

    pub fn minutes(self) -> u16 {
        match self {
            AutoOff::Never => 0,
            AutoOff::Min1 => 1,
            AutoOff::Min5 => 5,
            AutoOff::Min10 => 10,
            AutoOff::Min15 => 15,
            AutoOff::Min30 => 30,
            AutoOff::Min60 => 60,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => AutoOff::Never,
            1 => AutoOff::Min1,
            2 => AutoOff::Min5,
            3 => AutoOff::Min10,
            4 => AutoOff::Min15,
            5 => AutoOff::Min30,
            6 => AutoOff::Min60,
            _ => return None,
        })
    }

    pub fn from_minutes(minutes: u16) -> Result<Self> {
        Ok(match minutes {
            0 => AutoOff::Never,
            1 => AutoOff::Min1,
            5 => AutoOff::Min5,
            10 => AutoOff::Min10,
            15 => AutoOff::Min15,
            30 => AutoOff::Min30,
            60 => AutoOff::Min60,
            other => bail!(
                "unsupported auto-off timer {other} min (want 0 for never, 1, 5, 10, 15, 30 or 60)"
            ),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WirelessMode {
    /// Lower latency, shorter range.
    Speed,
    /// Longer range, higher latency.
    Range,
}

impl WirelessMode {
    pub fn code(self) -> u8 {
        match self {
            WirelessMode::Speed => 0,
            WirelessMode::Range => 1,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(WirelessMode::Speed),
            1 => Some(WirelessMode::Range),
            _ => None,
        }
    }
}

/// Noise control. Status byte 10 and the `07 bd` event carry the same codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseMode {
    Off,
    Transparency,
    Anc,
}

impl NoiseMode {
    pub fn code(self) -> u8 {
        match self {
            NoiseMode::Off => 0,
            NoiseMode::Transparency => 1,
            NoiseMode::Anc => 2,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => NoiseMode::Off,
            1 => NoiseMode::Transparency,
            2 => NoiseMode::Anc,
            _ => return None,
        })
    }
}

/// One user-facing setting, as a client changes it.
#[derive(Debug, Clone, PartialEq)]
pub enum Setting {
    NoiseMode(NoiseMode),
    Transparency(u8),
    Sidetone(u8),
    MicVolume(u8),
    MicLed(u8),
    AutoOff(AutoOff),
    WirelessMode(WirelessMode),
    EqPreset(u8),
    EqBands(Vec<f32>),
}

impl Setting {
    /// Every command this setting needs, validated, in send order. Custom bands
    /// stay inaudible until the custom slot is selected, so that goes first.
    pub fn packets(&self) -> Result<Vec<Vec<u8>>> {
        Ok(match self {
            Setting::NoiseMode(mode) => vec![noise_mode_packet(*mode)],
            Setting::Transparency(level) => vec![transparency_packet(*level)?],
            Setting::Sidetone(level) => vec![sidetone_packet(*level)?],
            Setting::MicVolume(volume) => vec![mic_volume_packet(*volume)?],
            Setting::MicLed(brightness) => vec![mic_led_packet(*brightness)?],
            Setting::AutoOff(timer) => vec![auto_off_packet(*timer)],
            Setting::WirelessMode(mode) => vec![wireless_mode_packet(*mode)],
            Setting::EqPreset(preset) => vec![eq_preset_packet(*preset)?],
            Setting::EqBands(gains) => {
                vec![eq_preset_packet(EQ_CUSTOM_PRESET)?, eq_bands_packet(gains)?]
            }
        })
    }
}

/// Where the headset is, as reported by status byte 15.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerStatus {
    /// Powered down or out of range — battery readings are meaningless.
    Offline,
    /// Sitting on the charge cable.
    Charging,
    Online,
    /// Anything we have not seen, preserved rather than guessed at.
    Unknown(u8),
}

impl PowerStatus {
    fn decode(byte: u8) -> Self {
        match byte {
            0x01 => PowerStatus::Offline,
            0x02 => PowerStatus::Charging,
            0x08 => PowerStatus::Online,
            other => PowerStatus::Unknown(other),
        }
    }

    /// Whether a battery reading taken alongside this status means anything.
    pub fn battery_is_meaningful(self) -> bool {
        matches!(self, PowerStatus::Charging | PowerStatus::Online)
    }
}

// --- status readback ----------------------------------------------------------

/// Byte offsets into the `07 b0` reply, counting the report id as byte 0.
mod offset {
    pub const OPCODE: usize = 1;
    pub const HEADSET_BATTERY: usize = 6;
    pub const SPARE_BATTERY: usize = 7;
    pub const TRANSPARENCY: usize = 8;
    pub const MIC_STATUS: usize = 9;
    pub const ANC: usize = 10;
    pub const MIC_LED: usize = 11;
    pub const AUTO_OFF: usize = 12;
    pub const WIRELESS_MODE: usize = 13;
    pub const POWER: usize = 15;
    /// Shortest reply we can decode every field from.
    pub const MIN_LEN: usize = POWER + 1;
}

/// A decoded `06 b0` reply.
///
/// Fields the reference implementations agree on are typed; the ones whose value
/// ranges are still unconfirmed are left as raw bytes rather than given an enum
/// that would imply more certainty than we have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// Headset charge, 0..=100, or `None` when the headset is offline.
    pub headset_battery: Option<u8>,
    /// Charge of the spare pack in the base station, if one is seated.
    pub spare_battery: Option<u8>,
    pub power: PowerStatus,
    pub auto_off: Option<AutoOff>,
    pub wireless_mode: Option<WirelessMode>,
    pub mic_led_brightness: u8,
    /// Raw status byte; see [`Status::mic_muted`].
    pub mic_status_raw: u8,
    /// Raw status byte; see [`Status::noise_mode`].
    pub anc_raw: u8,
    /// Raw status byte, 1..=10 when meaningful.
    pub transparency_raw: u8,
}

impl Status {
    /// Decode a reply. Returns `None` if this is not a `b0` status report.
    pub fn decode(report: &[u8]) -> Option<Self> {
        // Reject anything that is not the reply we asked for. Without this a
        // ChatMix or volume interrupt that arrived first would be decoded as a
        // status block and produce plausible-looking nonsense.
        if report.len() < offset::MIN_LEN || report.get(offset::OPCODE) != Some(&CMD_STATUS) {
            return None;
        }
        let power = PowerStatus::decode(report[offset::POWER]);
        Some(Status {
            headset_battery: power
                .battery_is_meaningful()
                .then(|| battery_percent(report[offset::HEADSET_BATTERY])),
            spare_battery: spare_percent(report[offset::SPARE_BATTERY]),
            power,
            auto_off: AutoOff::from_code(report[offset::AUTO_OFF]),
            wireless_mode: WirelessMode::from_code(report[offset::WIRELESS_MODE]),
            mic_led_brightness: report[offset::MIC_LED],
            mic_status_raw: report[offset::MIC_STATUS],
            anc_raw: report[offset::ANC],
            transparency_raw: report[offset::TRANSPARENCY],
        })
    }

    /// Decoded noise mode; `None` for a byte outside the known codes.
    pub fn noise_mode(&self) -> Option<NoiseMode> {
        NoiseMode::from_code(self.anc_raw)
    }

    /// Whether the mic is muted (status byte 9: 0 live, 1 muted).
    pub fn mic_muted(&self) -> Option<bool> {
        match self.mic_status_raw {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

/// The gauge counts eighths; scale to percent, saturating above full.
fn battery_percent(raw: u8) -> u8 {
    let steps = raw.min(BATTERY_STEPS) as u16;
    (steps * 100 / BATTERY_STEPS as u16) as u8
}

/// An empty charge slot reads as 0, which is "no pack" rather than "flat pack".
fn spare_percent(raw: u8) -> Option<u8> {
    (raw > 0).then(|| battery_percent(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the wire bytes for the setters whose encoding is documented.
    #[test]
    fn setters_match_the_documented_bytes() {
        assert_eq!(save_packet(), vec![0x09]);
        assert_eq!(status_packet(), vec![0xb0]);
        assert_eq!(sidetone_packet(2).unwrap(), vec![0x39, 0x02]);
        assert_eq!(eq_preset_packet(4).unwrap(), vec![0x2e, 0x04]);
        assert_eq!(mic_volume_packet(10).unwrap(), vec![0x37, 0x0a]);
        assert_eq!(analog_gain_packet(true), vec![0x27, 0x02]);
        assert_eq!(analog_gain_packet(false), vec![0x27, 0x01]);
        assert_eq!(mic_led_packet(10).unwrap(), vec![0xbf, 0x0a]);
        assert_eq!(auto_off_packet(AutoOff::Min30), vec![0xc1, 0x05]);
        assert_eq!(wireless_mode_packet(WirelessMode::Range), vec![0xc3, 0x01]);
        assert_eq!(chatmix_packet(true), vec![0x49, 0x01]);
        assert_eq!(volume_packet(0).unwrap(), vec![0x25, 0x00]);
    }

    #[test]
    fn setters_reject_out_of_range_values() {
        assert!(sidetone_packet(4).is_err());
        assert!(eq_preset_packet(5).is_err());
        assert!(
            mic_volume_packet(0x00).is_err(),
            "0x00 is below the mute floor of 0x01"
        );
        assert!(mic_volume_packet(0xa1).is_err());
        assert!(mic_led_packet(11).is_err());
        assert!(volume_packet(57).is_err());
        assert!(AutoOff::from_minutes(45).is_err());
    }

    /// 0x14 is 0 dB and each step is half a decibel.
    #[test]
    fn eq_gains_encode_around_the_0x14_midpoint() {
        let flat = eq_bands_packet(&[0.0; EQ_BANDS]).unwrap();
        assert_eq!(flat[0], 0x33);
        assert!(flat[1..].iter().all(|&b| b == 0x14), "{flat:02x?}");

        let boosted =
            eq_bands_packet(&[10.0, -10.0, 0.5, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]).unwrap();
        assert_eq!(&boosted[1..5], &[40, 0, 21, 19]);
    }

    #[test]
    fn eq_rejects_bad_input() {
        assert!(eq_bands_packet(&[0.0; 9]).is_err(), "wrong band count");
        assert!(eq_bands_packet(&[0.0; 11]).is_err(), "wrong band count");
        assert!(eq_bands_packet(&[10.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]).is_err());
        assert!(eq_bands_packet(&[f32::NAN, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]).is_err());
    }

    fn status_reply(power: u8, headset: u8, spare: u8) -> Vec<u8> {
        let mut r = vec![0u8; 20];
        r[0] = 0x07;
        r[1] = 0xb0;
        r[offset::HEADSET_BATTERY] = headset;
        r[offset::SPARE_BATTERY] = spare;
        r[offset::AUTO_OFF] = 5;
        r[offset::WIRELESS_MODE] = 1;
        r[offset::MIC_LED] = 7;
        r[offset::POWER] = power;
        r
    }

    #[test]
    fn status_decodes_the_documented_byte_map() {
        let s = Status::decode(&status_reply(0x08, 4, 8)).unwrap();
        assert_eq!(s.headset_battery, Some(50), "4 of 8 steps is half");
        assert_eq!(s.spare_battery, Some(100));
        assert_eq!(s.power, PowerStatus::Online);
        assert_eq!(s.auto_off, Some(AutoOff::Min30));
        assert_eq!(s.wireless_mode, Some(WirelessMode::Range));
        assert_eq!(s.mic_led_brightness, 7);
    }

    /// An offline headset still reports *some* battery byte; it just means nothing.
    #[test]
    fn offline_headset_reports_no_battery() {
        let s = Status::decode(&status_reply(0x01, 3, 0)).unwrap();
        assert_eq!(s.power, PowerStatus::Offline);
        assert_eq!(s.headset_battery, None);
        assert_eq!(
            s.spare_battery, None,
            "an empty charge slot is absent, not flat"
        );
    }

    /// The protocol has no correlation id, so a stray interrupt must not be
    /// mistaken for the status block we asked for.
    #[test]
    fn non_status_reports_are_rejected() {
        let mut chatmix = vec![0u8; 20];
        chatmix[0] = 0x07;
        chatmix[1] = 0x45; // ChatMix dial, not status
        assert!(Status::decode(&chatmix).is_none());
        assert!(Status::decode(&[]).is_none());
        assert!(Status::decode(&[0x07, 0xb0]).is_none(), "truncated reply");
    }

    #[test]
    fn battery_scale_saturates() {
        assert_eq!(battery_percent(0), 0);
        assert_eq!(battery_percent(8), 100);
        assert_eq!(
            battery_percent(200),
            100,
            "a bogus gauge must not exceed 100%"
        );
    }

    #[test]
    fn oled_brightness_is_one_to_ten() {
        assert_eq!(oled_brightness_packet(1).unwrap(), vec![0x85, 0x01]);
        assert_eq!(oled_brightness_packet(10).unwrap(), vec![0x85, 0x0a]);
        assert!(
            oled_brightness_packet(0).is_err(),
            "0 does not turn the screen off"
        );
        assert!(oled_brightness_packet(11).is_err());
        assert_eq!(oled_return_to_ui_packet(), vec![0x95]);
        assert_eq!(audio_settings_packet(), vec![0x20]);
    }

    /// Two 64-px strips; with the report id each is a 1024-byte feature report.
    #[test]
    fn oled_blank_is_two_all_black_strips() {
        let [left, right] = oled_blank_packets();
        for (strip, x) in [(&left, 0x00), (&right, 0x40)] {
            assert_eq!(strip.len(), OLED_FRAME_PAYLOAD);
            assert_eq!(&strip[..5], &[0x93, x, 0x00, 0x40, 0x40]);
            assert!(strip[5..].iter().all(|&b| b == 0), "every pixel off");
        }
        assert_eq!(OLED_FRAME_PAYLOAD + 1, 1024);
    }

    #[test]
    fn volume_decodes_from_both_the_reply_and_the_event() {
        // Captured on this unit 2026-09-27: 06 20 <input> <volume> ...
        let reply = [0x06, 0x20, 0x00, 0x06, 0x01, 0x00, 0x00, 0x14, 0x14];
        assert_eq!(decode_volume(&reply), Some(6));
        assert_eq!(
            decode_volume(&[0x06, 0x20, 0x00, 57]),
            None,
            "past silent is not a volume"
        );
        assert_eq!(decode_volume(&[0x06, 0xb0, 0x00, 0x06]), None);
        assert_eq!(decode_volume_event(&[0x07, 0x25, 0x07]), Some(7));
        assert_eq!(
            decode_volume_event(&[0x06, 0x25, 0x07]),
            None,
            "a reply, not an event"
        );
    }

    #[test]
    fn events_that_arrive_before_a_reply_are_kept() {
        let mut events = Vec::new();
        let wheel = vec![0x07, 0x25, 0x06];
        let status = vec![0x06, 0xb0, 0x00, 0x00, 0x01];
        assert_eq!(sort_report(wheel.clone(), Some(0xb0), &mut events), None);
        assert_eq!(
            sort_report(status.clone(), Some(0xb0), &mut events),
            Some(status)
        );
        assert_eq!(events, vec![wheel]);
    }

    #[test]
    fn a_late_reply_to_another_request_is_dropped() {
        let mut events = Vec::new();
        let stale_status = vec![0x06, 0xb0, 0x00, 0x00, 0x01];
        assert_eq!(sort_report(stale_status, Some(0x20), &mut events), None);
        assert!(events.is_empty(), "a stale reply must not wake the screen");
    }

    #[test]
    fn queued_reports_sort_with_nothing_awaited() {
        let mut events = Vec::new();
        assert_eq!(
            sort_report(vec![0x06, 0x20, 0x00, 0x06], None, &mut events),
            None
        );
        assert_eq!(sort_report(vec![0x07, 0xbd, 0x01], None, &mut events), None);
        assert_eq!(
            sort_report(vec![0x07], None, &mut events),
            None,
            "too short to be an event"
        );
        assert_eq!(events, vec![vec![0x07, 0xbd, 0x01]]);
    }

    #[test]
    fn every_setting_encodes_to_its_documented_commands() {
        let cases: Vec<(Setting, Vec<Vec<u8>>)> = vec![
            (Setting::NoiseMode(NoiseMode::Off), vec![vec![0xbd, 0]]),
            (
                Setting::NoiseMode(NoiseMode::Transparency),
                vec![vec![0xbd, 1]],
            ),
            (Setting::NoiseMode(NoiseMode::Anc), vec![vec![0xbd, 2]]),
            (Setting::Transparency(10), vec![vec![0xb9, 10]]),
            (Setting::Sidetone(3), vec![vec![0x39, 3]]),
            (Setting::MicVolume(1), vec![vec![0x37, 1]]),
            (Setting::MicLed(1), vec![vec![0xbf, 1]]),
            (Setting::AutoOff(AutoOff::Never), vec![vec![0xc1, 0]]),
            (
                Setting::WirelessMode(WirelessMode::Speed),
                vec![vec![0xc3, 0]],
            ),
            (Setting::EqPreset(1), vec![vec![0x2e, 1]]),
        ];
        for (setting, expected) in cases {
            assert_eq!(setting.packets().unwrap(), expected, "{setting:?}");
        }
        let bands = Setting::EqBands(vec![0.0; EQ_BANDS]).packets().unwrap();
        assert_eq!(
            bands[0],
            vec![0x2e, 0x04],
            "custom slot first, or the bands stay inaudible"
        );
        assert_eq!(bands[1][0], 0x33);
        assert_eq!(bands.len(), 2);
    }

    #[test]
    fn settings_reject_values_the_references_do_not_use() {
        assert!(Setting::Transparency(0).packets().is_err());
        assert!(Setting::Transparency(11).packets().is_err());
        assert!(Setting::MicVolume(0).packets().is_err());
        assert!(
            Setting::MicVolume(11).packets().is_err(),
            "the old 0xa0 scale is gone"
        );
        assert!(
            Setting::MicLed(0).packets().is_err(),
            "1 is the dimmest; 0 is not a level"
        );
        assert!(Setting::MicLed(11).packets().is_err());
        assert!(Setting::Sidetone(4).packets().is_err());
        assert!(Setting::EqPreset(5).packets().is_err());
        assert!(Setting::EqBands(vec![0.0; 9]).packets().is_err());
    }

    #[test]
    fn auto_off_never_is_code_zero() {
        assert_eq!(AutoOff::Never.code(), 0);
        assert_eq!(AutoOff::Never.minutes(), 0);
        assert_eq!(AutoOff::from_code(0), Some(AutoOff::Never));
        assert_eq!(AutoOff::from_minutes(0).unwrap(), AutoOff::Never);
        let mut r = status_reply(0x08, 8, 8);
        r[offset::AUTO_OFF] = 0;
        assert_eq!(Status::decode(&r).unwrap().auto_off, Some(AutoOff::Never));
    }

    #[test]
    fn status_reports_noise_mode_and_mute() {
        let mut r = status_reply(0x08, 8, 8);
        r[offset::ANC] = 1;
        r[offset::MIC_STATUS] = 1;
        let s = Status::decode(&r).unwrap();
        assert_eq!(s.noise_mode(), Some(NoiseMode::Transparency));
        assert_eq!(s.mic_muted(), Some(true));
        r[offset::ANC] = 9;
        r[offset::MIC_STATUS] = 7;
        let odd = Status::decode(&r).unwrap();
        assert_eq!(
            odd.noise_mode(),
            None,
            "unknown byte is unknown, not a mode"
        );
        assert_eq!(odd.mic_muted(), None);
    }
}
