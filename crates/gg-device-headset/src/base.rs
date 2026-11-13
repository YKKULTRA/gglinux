// SPDX-License-Identifier: GPL-3.0-or-later
//! The base station's own settings that GG 123 has and the first milestones left
//! out (Bluetooth at power-on and during calls, gain, line out and its stream mix,
//! the home screen, the screen's dim timeout), and the replies that report them.
//!
//! Every write here was seen going out from GG 123, both ways, in our captures
//! `arctis-1` … `arctis-3` and `arctis-5` (60 min dim) of 2026-11-13
//! (`GG-CAPTURES-2026-11-13.md`), and matches Arctis Sound Manager's device file
//! (ea410e0, `settings` and `device_init`); the dim timeout only the captures name.
//! GG sends each one alone: no save (`09`) follows it, so neither do we.
//!
//! Reply offsets count the report id as byte 0, as [`crate::proto`] does. They are
//! Arctis Sound Manager's (`settings_readback`, `probe_queries`, `status`), checked
//! against the `20`, `80` and `b0` replies GG got in `arctis-5` (headset on), whose
//! values match the settings GG pushed right after.

use anyhow::{bail, ensure, Result};

// --- opcodes ------------------------------------------------------------------

const CMD_AUDIO_SETTINGS: u8 = 0x20;
const CMD_GAIN: u8 = 0x27;
const CMD_LINE_OUT: u8 = 0x43;
const CMD_STREAM_MIX: u8 = 0x47;
const CMD_UX_SETTINGS: u8 = 0x80;
const CMD_SCREEN_DIM: u8 = 0x83;
const CMD_HOME_SCREEN: u8 = 0x89;
const CMD_BLUETOOTH_AT_POWER_ON: u8 = 0xb2;
const CMD_BLUETOOTH_CALL: u8 = 0xb3;

/// Each stream-mix level is a percentage.
pub const STREAM_LEVEL_MAX: u8 = 100;

// --- the settings ---------------------------------------------------------------

/// What a Bluetooth call does to the game sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BluetoothCall {
    Nothing,
    /// Game sound 12 dB lower while the call lasts.
    LowerGame,
    MuteGame,
}

impl BluetoothCall {
    pub fn code(self) -> u8 {
        match self {
            BluetoothCall::Nothing => 0,
            BluetoothCall::LowerGame => 1,
            BluetoothCall::MuteGame => 2,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => BluetoothCall::Nothing,
            1 => BluetoothCall::LowerGame,
            2 => BluetoothCall::MuteGame,
            _ => return None,
        })
    }
}

/// The base station's output gain (GG's "Gain" on the Audio tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gain {
    Low,
    High,
}

impl Gain {
    pub fn code(self) -> u8 {
        match self {
            Gain::Low => 1,
            Gain::High => 2,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Gain::Low),
            2 => Some(Gain::High),
            _ => None,
        }
    }
}

/// What the base station's line out carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOut {
    /// The same sound as the headset, for speakers.
    Speakers,
    /// A stream mix of main, aux and mic, each at its own level.
    Stream,
}

impl LineOut {
    pub fn code(self) -> u8 {
        match self {
            LineOut::Speakers => 1,
            LineOut::Stream => 2,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(LineOut::Speakers),
            2 => Some(LineOut::Stream),
            _ => None,
        }
    }
}

/// The levels of the line-out stream mix, 0–100 each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamMix {
    pub main: u8,
    pub aux: u8,
    pub mic: u8,
}

/// The base station's home screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeScreen {
    Detailed,
    Simple,
}

impl HomeScreen {
    pub fn code(self) -> u8 {
        match self {
            HomeScreen::Detailed => 0,
            HomeScreen::Simple => 1,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(HomeScreen::Detailed),
            1 => Some(HomeScreen::Simple),
            _ => None,
        }
    }
}

/// How long the base station's screen stays bright before it dims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimTimeout {
    Never,
    Min1,
    Min5,
    Min10,
    Min15,
    Min30,
    /// Captured in `arctis-5` (`83 06`).
    Min60,
}

impl DimTimeout {
    const ALL: [DimTimeout; 7] = [
        DimTimeout::Never,
        DimTimeout::Min1,
        DimTimeout::Min5,
        DimTimeout::Min10,
        DimTimeout::Min15,
        DimTimeout::Min30,
        DimTimeout::Min60,
    ];

    pub fn code(self) -> u8 {
        match self {
            DimTimeout::Never => 0,
            DimTimeout::Min1 => 1,
            DimTimeout::Min5 => 2,
            DimTimeout::Min10 => 3,
            DimTimeout::Min15 => 4,
            DimTimeout::Min30 => 5,
            DimTimeout::Min60 => 6,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.code() == code)
    }

    pub fn minutes(self) -> u16 {
        match self {
            DimTimeout::Never => 0,
            DimTimeout::Min1 => 1,
            DimTimeout::Min5 => 5,
            DimTimeout::Min10 => 10,
            DimTimeout::Min15 => 15,
            DimTimeout::Min30 => 30,
            DimTimeout::Min60 => 60,
        }
    }

    pub fn from_minutes(minutes: u16) -> Result<Self> {
        match Self::ALL.into_iter().find(|d| d.minutes() == minutes) {
            Some(dim) => Ok(dim),
            None => bail!(
                "unsupported dim timeout {minutes} min (want 0 for never, 1, 5, 10, 15, 30 or 60)"
            ),
        }
    }
}

// --- packets ------------------------------------------------------------------

/// Bluetooth on (or not) when the headset powers on.
pub fn bluetooth_at_power_on_packet(on: bool) -> Vec<u8> {
    vec![CMD_BLUETOOTH_AT_POWER_ON, u8::from(on)]
}

/// What a Bluetooth call does to the game sound.
pub fn bluetooth_call_packet(call: BluetoothCall) -> Vec<u8> {
    vec![CMD_BLUETOOTH_CALL, call.code()]
}

/// Output gain, low or high.
pub fn gain_packet(gain: Gain) -> Vec<u8> {
    vec![CMD_GAIN, gain.code()]
}

/// What the line out carries.
pub fn line_out_packet(line_out: LineOut) -> Vec<u8> {
    vec![CMD_LINE_OUT, line_out.code()]
}

/// The stream mix as GG sends it: `47 <main> 00 <aux> <mic>`. Arctis Sound
/// Manager sends main twice; GG sends `00` in that place, and we copy GG.
pub fn stream_mix_packet(mix: StreamMix) -> Result<Vec<u8>> {
    for (name, level) in [("main", mix.main), ("aux", mix.aux), ("mic", mix.mic)] {
        ensure!(
            level <= STREAM_LEVEL_MAX,
            "stream mix {name} level {level} out of range (0..={STREAM_LEVEL_MAX})"
        );
    }
    Ok(vec![CMD_STREAM_MIX, mix.main, 0x00, mix.aux, mix.mic])
}

/// The home screen.
pub fn home_screen_packet(home: HomeScreen) -> Vec<u8> {
    vec![CMD_HOME_SCREEN, home.code()]
}

/// The screen's dim timeout.
pub fn screen_dim_packet(dim: DimTimeout) -> Vec<u8> {
    vec![CMD_SCREEN_DIM, dim.code()]
}

/// Ask for the screen settings (`06 80`), decoded by [`UxSettings::decode`].
pub fn ux_settings_packet() -> Vec<u8> {
    vec![CMD_UX_SETTINGS]
}

// --- what the base station reports ------------------------------------------------

/// Bluetooth's link to a phone, as the `06 b0` reply says (byte 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BluetoothLink {
    Off,
    Connected,
    /// On, with nothing connected.
    NotConnected,
}

impl BluetoothLink {
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => BluetoothLink::Off,
            1 => BluetoothLink::Connected,
            2 => BluetoothLink::NotConnected,
            _ => return None,
        })
    }
}

/// The 2.4 GHz link between base station and headset, as the `06 b0` reply says
/// (byte 14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pairing {
    NotPaired,
    /// Paired, but the headset is off or out of reach.
    PairedOffline,
    Connected,
}

impl Pairing {
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Pairing::NotPaired,
            4 => Pairing::PairedOffline,
            8 => Pairing::Connected,
            _ => return None,
        })
    }
}

/// Byte offsets into the `06 20` reply (`settings_readback` and
/// `probe_queries.audio_settings`, whose field numbers count from the opcode).
mod audio_at {
    pub const VOLUME: usize = 3;
    pub const GAIN: usize = 4;
    pub const EQ_PRESET: usize = 6;
    pub const EQ_BANDS: usize = 7;
    pub const MIC_VOLUME: usize = 0x11;
    pub const SIDETONE: usize = 0x12;
    pub const LINE_OUT: usize = 0x13;
    /// One main level, then an unused byte, then aux and mic.
    pub const STREAM_MAIN: usize = 0x16;
    pub const STREAM_AUX: usize = 0x18;
    pub const STREAM_MIC: usize = 0x19;
}

/// A decoded `06 20` reply: the base station's audio settings. A field is `None`
/// when its byte isn't one of the values that setting takes, so a wrong offset
/// shows as "unknown" rather than as a made-up setting.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSettings {
    /// Attenuation, 0 loudest to 56 silent.
    pub volume: Option<u8>,
    pub gain: Option<Gain>,
    /// `0..=4` (Flat, Bass Boost, Focus, Smiley, Custom). GG's game presets,
    /// numbered after these, read as `None`.
    pub eq_preset: Option<u8>,
    /// The Custom slot's ten gains in dB.
    pub eq_bands: Option<Vec<f32>>,
    pub mic_volume: Option<u8>,
    pub sidetone: Option<u8>,
    pub line_out: Option<LineOut>,
    pub stream_mix: Option<StreamMix>,
}

impl AudioSettings {
    /// Decode a reply; `None` if it isn't a `20` reply.
    pub fn decode(report: &[u8]) -> Option<Self> {
        use audio_at::*;
        if report.get(1) != Some(&CMD_AUDIO_SETTINGS) {
            return None;
        }
        let at = |i: usize| report.get(i).copied();
        let level = |i: usize| at(i).filter(|v| *v <= STREAM_LEVEL_MAX);
        let eq_bands = report
            .get(EQ_BANDS..EQ_BANDS + 10)
            .filter(|bands| bands.iter().all(|b| *b <= 40))
            .map(|bands| bands.iter().map(|b| (f32::from(*b) - 20.0) / 2.0).collect());
        let stream_mix = match (level(STREAM_MAIN), level(STREAM_AUX), level(STREAM_MIC)) {
            (Some(main), Some(aux), Some(mic)) => Some(StreamMix { main, aux, mic }),
            _ => None,
        };
        Some(AudioSettings {
            volume: at(VOLUME).filter(|v| *v <= crate::proto::VOLUME_ATTEN_MAX),
            gain: at(GAIN).and_then(Gain::from_code),
            eq_preset: at(EQ_PRESET).filter(|p| *p <= crate::proto::EQ_PRESET_MAX),
            eq_bands,
            mic_volume: at(MIC_VOLUME).filter(|v| (1..=crate::proto::MIC_VOLUME_MAX).contains(v)),
            sidetone: at(SIDETONE).filter(|v| *v <= 3),
            line_out: at(LINE_OUT).and_then(LineOut::from_code),
            stream_mix,
        })
    }
}

/// Byte offsets into the `06 80` reply (`probe_queries.ux_settings`).
mod ux_at {
    pub const DIM_TIMEOUT: usize = 2;
    pub const BRIGHTNESS: usize = 3;
    pub const HOME_SCREEN: usize = 5;
}

/// A decoded `06 80` reply: the screen settings, per Arctis Sound Manager's
/// `probe_queries.ux_settings`. GG's one read in `arctis-5`, `06 80 01 0a 01 00`,
/// says dim after 1 min, brightness 10, home screen detailed: what GG pushed right
/// after (`83 01`, `85 0a`, `89 00`). No capture shows a changed value coming back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UxSettings {
    pub dim_timeout: Option<DimTimeout>,
    pub brightness: Option<u8>,
    pub home_screen: Option<HomeScreen>,
}

impl UxSettings {
    /// Decode a reply; `None` if it isn't an `80` reply.
    pub fn decode(report: &[u8]) -> Option<Self> {
        if report.get(1) != Some(&CMD_UX_SETTINGS) {
            return None;
        }
        let at = |i: usize| report.get(i).copied();
        Some(UxSettings {
            dim_timeout: at(ux_at::DIM_TIMEOUT).and_then(DimTimeout::from_code),
            brightness: at(ux_at::BRIGHTNESS).filter(|b| {
                (crate::proto::OLED_BRIGHTNESS_MIN..=crate::proto::OLED_BRIGHTNESS_MAX).contains(b)
            }),
            home_screen: at(ux_at::HOME_SCREEN).and_then(HomeScreen::from_code),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every write as GG 123 sent it in arctis-1 … arctis-3, both ways.
    #[test]
    fn writes_match_the_captured_bytes() {
        assert_eq!(bluetooth_at_power_on_packet(true), vec![0xb2, 0x01]);
        assert_eq!(bluetooth_at_power_on_packet(false), vec![0xb2, 0x00]);
        assert_eq!(
            bluetooth_call_packet(BluetoothCall::Nothing),
            vec![0xb3, 0x00]
        );
        assert_eq!(
            bluetooth_call_packet(BluetoothCall::LowerGame),
            vec![0xb3, 0x01]
        );
        assert_eq!(
            bluetooth_call_packet(BluetoothCall::MuteGame),
            vec![0xb3, 0x02]
        );
        assert_eq!(gain_packet(Gain::Low), vec![0x27, 0x01]);
        assert_eq!(gain_packet(Gain::High), vec![0x27, 0x02]);
        assert_eq!(line_out_packet(LineOut::Speakers), vec![0x43, 0x01]);
        assert_eq!(line_out_packet(LineOut::Stream), vec![0x43, 0x02]);
        assert_eq!(home_screen_packet(HomeScreen::Detailed), vec![0x89, 0x00]);
        assert_eq!(home_screen_packet(HomeScreen::Simple), vec![0x89, 0x01]);
        let dims = [
            (DimTimeout::Never, 0x00),
            (DimTimeout::Min1, 0x01),
            (DimTimeout::Min5, 0x02),
            (DimTimeout::Min10, 0x03),
            (DimTimeout::Min15, 0x04),
            (DimTimeout::Min30, 0x05),
            (DimTimeout::Min60, 0x06),
        ];
        for (dim, code) in dims {
            assert_eq!(screen_dim_packet(dim), vec![0x83, code], "{dim:?}");
        }
        assert_eq!(ux_settings_packet(), vec![0x80]);
    }

    /// GG's connect push sent `47 64 00 64 64`; its main at 50 % was `32`.
    #[test]
    fn the_stream_mix_goes_out_as_gg_sends_it() {
        let all = StreamMix {
            main: 100,
            aux: 100,
            mic: 100,
        };
        assert_eq!(
            stream_mix_packet(all).unwrap(),
            vec![0x47, 0x64, 0x00, 0x64, 0x64]
        );
        let half = StreamMix { main: 50, ..all };
        assert_eq!(
            stream_mix_packet(half).unwrap(),
            vec![0x47, 0x32, 0x00, 0x64, 0x64]
        );
        let quiet = StreamMix {
            main: 0,
            aux: 7,
            mic: 0,
        };
        assert_eq!(
            stream_mix_packet(quiet).unwrap(),
            vec![0x47, 0x00, 0x00, 0x07, 0x00]
        );
    }

    #[test]
    fn a_stream_level_past_100_is_refused() {
        let ok = StreamMix {
            main: 100,
            aux: 100,
            mic: 100,
        };
        for bad in [
            StreamMix { main: 101, ..ok },
            StreamMix { aux: 101, ..ok },
            StreamMix { mic: 255, ..ok },
        ] {
            assert!(stream_mix_packet(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_dim_timeout_takes_only_the_captured_choices() {
        for (minutes, dim) in [
            (0, DimTimeout::Never),
            (1, DimTimeout::Min1),
            (5, DimTimeout::Min5),
            (10, DimTimeout::Min10),
            (15, DimTimeout::Min15),
            (30, DimTimeout::Min30),
            (60, DimTimeout::Min60),
        ] {
            assert_eq!(DimTimeout::from_minutes(minutes).unwrap(), dim);
            assert_eq!(dim.minutes(), minutes);
            assert_eq!(DimTimeout::from_code(dim.code()), Some(dim));
        }
        let two = DimTimeout::from_minutes(2).unwrap_err().to_string();
        assert!(two.contains("1, 5, 10, 15, 30 or 60"), "{two}");
        assert!(DimTimeout::from_minutes(45).is_err());
        assert_eq!(DimTimeout::from_code(0x07), None);
    }

    #[test]
    fn codes_read_back_as_written() {
        for call in [
            BluetoothCall::Nothing,
            BluetoothCall::LowerGame,
            BluetoothCall::MuteGame,
        ] {
            assert_eq!(BluetoothCall::from_code(call.code()), Some(call));
        }
        assert_eq!(BluetoothCall::from_code(3), None);
        for gain in [Gain::Low, Gain::High] {
            assert_eq!(Gain::from_code(gain.code()), Some(gain));
        }
        assert_eq!(Gain::from_code(0), None);
        for line_out in [LineOut::Speakers, LineOut::Stream] {
            assert_eq!(LineOut::from_code(line_out.code()), Some(line_out));
        }
        assert_eq!(LineOut::from_code(0), None);
        for home in [HomeScreen::Detailed, HomeScreen::Simple] {
            assert_eq!(HomeScreen::from_code(home.code()), Some(home));
        }
        assert_eq!(HomeScreen::from_code(2), None);
    }

    #[test]
    fn link_and_pairing_codes_are_arctis_sound_managers() {
        assert_eq!(BluetoothLink::from_code(0), Some(BluetoothLink::Off));
        assert_eq!(BluetoothLink::from_code(1), Some(BluetoothLink::Connected));
        assert_eq!(
            BluetoothLink::from_code(2),
            Some(BluetoothLink::NotConnected)
        );
        assert_eq!(BluetoothLink::from_code(3), None);
        assert_eq!(Pairing::from_code(1), Some(Pairing::NotPaired));
        assert_eq!(Pairing::from_code(4), Some(Pairing::PairedOffline));
        assert_eq!(Pairing::from_code(8), Some(Pairing::Connected));
        assert_eq!(Pairing::from_code(0), None);
    }

    /// The `06 20` reply GG got in arctis-5 (headset on), zero-padded to 64 bytes.
    /// GG then pushed gain `01`, preset `00`, bands `14`, mic volume `0a`, sidetone
    /// `00`, line out `01` and stream `64 00 64 64`: the same values.
    fn audio_reply() -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..26].copy_from_slice(&[
            0x06, 0x20, 0x00, 0x03, 0x01, 0x00, 0x00, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14,
            0x14, 0x14, 0x14, 0x0a, 0x00, 0x01, 0x64, 0x64, 0x64, 0x00, 0x64, 0x64,
        ]);
        r
    }

    #[test]
    fn the_captured_audio_reply_decodes_at_arctis_sound_managers_offsets() {
        let a = AudioSettings::decode(&audio_reply()).unwrap();
        assert_eq!(a.volume, Some(3));
        assert_eq!(a.gain, Some(Gain::Low));
        assert_eq!(a.eq_preset, Some(0));
        assert_eq!(a.eq_bands, Some(vec![0.0; 10]));
        assert_eq!(a.mic_volume, Some(10));
        assert_eq!(a.sidetone, Some(0));
        assert_eq!(a.line_out, Some(LineOut::Speakers));
        assert_eq!(
            a.stream_mix,
            Some(StreamMix {
                main: 100,
                aux: 100,
                mic: 100
            })
        );

        let mut r = audio_reply();
        r[audio_at::GAIN] = 0x02;
        r[audio_at::EQ_PRESET] = 0x04;
        r[audio_at::EQ_BANDS] = 40; // +10 dB
        r[audio_at::EQ_BANDS + 9] = 0; // -10 dB
        r[audio_at::EQ_BANDS + 1] = 21; // +0.5 dB
        r[audio_at::SIDETONE] = 3;
        r[audio_at::LINE_OUT] = 0x02;
        r[audio_at::STREAM_MAIN] = 0x32;
        r[0x17] = 0x99; // the unused byte between main and aux
        r[audio_at::STREAM_AUX] = 0x00;
        r[audio_at::STREAM_MIC] = 0x07;
        let a = AudioSettings::decode(&r).unwrap();
        assert_eq!(a.gain, Some(Gain::High));
        assert_eq!(a.eq_preset, Some(4));
        let bands = a.eq_bands.unwrap();
        assert_eq!((bands[0], bands[1], bands[9]), (10.0, 0.5, -10.0));
        assert_eq!(a.sidetone, Some(3));
        assert_eq!(a.line_out, Some(LineOut::Stream));
        assert_eq!(
            a.stream_mix,
            Some(StreamMix {
                main: 50,
                aux: 0,
                mic: 7
            })
        );
    }

    /// A byte outside a setting's values is unknown, never a guess.
    #[test]
    fn audio_bytes_outside_their_values_read_as_unknown() {
        let mut r = audio_reply();
        r[audio_at::VOLUME] = 57;
        r[audio_at::GAIN] = 0x00;
        r[audio_at::EQ_PRESET] = 0x05; // one of GG's game presets
        r[audio_at::EQ_BANDS + 3] = 41;
        r[audio_at::MIC_VOLUME] = 0x00;
        r[audio_at::SIDETONE] = 4;
        r[audio_at::LINE_OUT] = 0x03;
        r[audio_at::STREAM_MIC] = 101;
        let a = AudioSettings::decode(&r).unwrap();
        assert_eq!(a.volume, None);
        assert_eq!(a.gain, None);
        assert_eq!(a.eq_preset, None);
        assert_eq!(a.eq_bands, None, "one bad band spoils the curve");
        assert_eq!(a.mic_volume, None);
        assert_eq!(a.sidetone, None);
        assert_eq!(a.line_out, None);
        assert_eq!(a.stream_mix, None);
    }

    #[test]
    fn a_short_or_foreign_reply_decodes_only_what_it_holds() {
        // The 2026-09-27 read, as far as it was written down.
        let short = [0x06, 0x20, 0x00, 0x06, 0x01, 0x00, 0x00, 0x14, 0x14];
        let a = AudioSettings::decode(&short).unwrap();
        assert_eq!(
            (a.volume, a.gain, a.eq_preset),
            (Some(6), Some(Gain::Low), Some(0))
        );
        assert_eq!((a.eq_bands, a.line_out, a.stream_mix), (None, None, None));
        assert_eq!(AudioSettings::decode(&[0x06, 0xb0, 0x00, 0x06]), None);
        assert_eq!(AudioSettings::decode(&[0x06]), None);
    }

    /// GG's read in arctis-5: dim after 1 min, brightness 10, home screen detailed,
    /// as GG pushed right after (`83 01`, `85 0a`, `89 00`).
    #[test]
    fn the_captured_ux_reply_decodes_at_arctis_sound_managers_offsets() {
        let mut r = vec![0u8; 64];
        r[..6].copy_from_slice(&[0x06, 0x80, 0x01, 0x0a, 0x01, 0x00]);
        let ux = UxSettings::decode(&r).unwrap();
        assert_eq!(ux.dim_timeout, Some(DimTimeout::Min1));
        assert_eq!(ux.brightness, Some(10));
        assert_eq!(ux.home_screen, Some(HomeScreen::Detailed));
        r[..6].copy_from_slice(&[0x06, 0x80, 0x02, 0x0a, 0x03, 0x01]);
        let ux = UxSettings::decode(&r).unwrap();
        assert_eq!(ux.dim_timeout, Some(DimTimeout::Min5));
        assert_eq!(ux.brightness, Some(10));
        assert_eq!(ux.home_screen, Some(HomeScreen::Simple));
        r[ux_at::DIM_TIMEOUT] = 0x07;
        r[ux_at::BRIGHTNESS] = 0;
        r[ux_at::HOME_SCREEN] = 9;
        let odd = UxSettings::decode(&r).unwrap();
        assert_eq!(
            (odd.dim_timeout, odd.brightness, odd.home_screen),
            (None, None, None)
        );
        assert_eq!(UxSettings::decode(&[0x06, 0x20, 0x01]), None);
    }
}
