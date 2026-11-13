// SPDX-License-Identifier: GPL-3.0-or-later
//! Headset settings between the IPC and the device: mapping a client's change
//! onto the device's typed setting, checking it against the experimental gates,
//! remembering what gglinux wrote, and putting what the base station reports
//! together with that.

use gg_device_headset::{
    AudioSettings, AutoOff, BluetoothCall as DevCall, BluetoothLink as DevLink, DimTimeout,
    Gain as DevGain, Headset, HomeScreen as DevHome, LineOut as DevLineOut, NoiseMode as DevNoise,
    Pairing as DevPairing, Port, Setting, Status, StreamMix, UxSettings, WirelessMode,
};
use gg_ipc::{
    BluetoothCall, BluetoothLink, Experimental, HeadsetChange, HeadsetControls, HeadsetGain,
    HeadsetPairing, HomeScreen, LineOut, LineOutMix, NoiseMode, RadioMode,
};

/// Map a client's change onto the device setting. Ranges are the device
/// crate's to judge (see [`validated`]); only the minute conversions, which
/// have no device meaning outside their levels, are checked here.
pub fn setting_for(change: &HeadsetChange) -> Result<Setting, String> {
    Ok(match change {
        HeadsetChange::NoiseMode(mode) => Setting::NoiseMode(match mode {
            NoiseMode::Off => DevNoise::Off,
            NoiseMode::Transparency => DevNoise::Transparency,
            NoiseMode::Anc => DevNoise::Anc,
        }),
        HeadsetChange::Transparency(level) => Setting::Transparency(*level),
        HeadsetChange::Sidetone(level) => Setting::Sidetone(*level),
        HeadsetChange::MicVolume(volume) => Setting::MicVolume(*volume),
        HeadsetChange::MicLed(level) => Setting::MicLed(*level),
        HeadsetChange::AutoOffMinutes(minutes) => {
            Setting::AutoOff(AutoOff::from_minutes(*minutes).map_err(|e| e.to_string())?)
        }
        HeadsetChange::RadioMode(mode) => Setting::WirelessMode(match mode {
            RadioMode::Speed => WirelessMode::Speed,
            RadioMode::Range => WirelessMode::Range,
        }),
        HeadsetChange::EqPreset(preset) => Setting::EqPreset(*preset),
        HeadsetChange::EqBands(gains) => Setting::EqBands(gains.clone()),
        HeadsetChange::BluetoothAtPowerOn(on) => Setting::BluetoothAtPowerOn(*on),
        HeadsetChange::BluetoothCall(call) => Setting::BluetoothCall(match call {
            BluetoothCall::Nothing => DevCall::Nothing,
            BluetoothCall::LowerGame => DevCall::LowerGame,
            BluetoothCall::MuteGame => DevCall::MuteGame,
        }),
        HeadsetChange::Gain(gain) => Setting::Gain(match gain {
            HeadsetGain::Low => DevGain::Low,
            HeadsetGain::High => DevGain::High,
        }),
        HeadsetChange::LineOut(line_out) => Setting::LineOut(match line_out {
            LineOut::Speakers => DevLineOut::Speakers,
            LineOut::Stream => DevLineOut::Stream,
        }),
        HeadsetChange::LineOutMix(mix) => Setting::StreamMix(StreamMix {
            main: mix.main,
            aux: mix.aux,
            mic: mix.mic,
        }),
        HeadsetChange::HomeScreen(home) => Setting::HomeScreen(match home {
            HomeScreen::Detailed => DevHome::Detailed,
            HomeScreen::Simple => DevHome::Simple,
        }),
        HeadsetChange::ScreenDimMinutes(minutes) => {
            Setting::ScreenDim(DimTimeout::from_minutes(*minutes).map_err(|e| e.to_string())?)
        }
    })
}

/// A setting that is safe to send: mapped, and every packet it needs encodes.
/// Checked before the device is touched, so a bad value never half-applies.
pub fn validated(change: &HeadsetChange) -> Result<Setting, String> {
    let setting = setting_for(change)?;
    setting.packets().map_err(|e| format!("{e:#}"))?;
    Ok(setting)
}

/// Why a change didn't reach the base station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failed {
    /// Refused before anything was sent: its gate is off, a value is out of range,
    /// or there is no base station.
    Refused(String),
    /// The write itself failed: the device should be opened again.
    Write(String),
}

/// Send one change: its gate first, then its values, then the base station.
/// Nothing is sent unless all three pass.
pub fn send_change<P: Port>(
    device: Option<&Headset<P>>,
    gates: &Experimental,
    change: &HeadsetChange,
) -> Result<(), Failed> {
    if let Some(why) = change.feature().and_then(|f| gates.refusal(f)) {
        return Err(Failed::Refused(why));
    }
    let setting = validated(change).map_err(Failed::Refused)?;
    let device = device.ok_or_else(|| Failed::Refused("base station not connected".into()))?;
    device
        .apply(&setting)
        .map_err(|e| Failed::Write(format!("{e:#}")))
}

/// What gglinux wrote this session. Shown where the base station reports nothing:
/// not yet, or (the home screen and the dim timeout) while `headset_extras` is off,
/// as their `80` reply is asked for only while it is on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Written {
    pub sidetone: Option<u8>,
    pub mic_volume: Option<u8>,
    pub eq_preset: Option<u8>,
    pub eq_bands: Option<Vec<f32>>,
    pub bluetooth_at_power_on: Option<bool>,
    pub bluetooth_call: Option<BluetoothCall>,
    pub gain: Option<HeadsetGain>,
    pub line_out: Option<LineOut>,
    pub line_out_mix: Option<LineOutMix>,
    pub home_screen: Option<HomeScreen>,
    pub screen_dim_minutes: Option<u16>,
}

/// Remember an applied change. The settings the `b0` reply carries (noise
/// control, transparency, mute light, auto-off, radio) are read every second
/// and not kept here.
pub fn record(written: &mut Written, change: &HeadsetChange) {
    match change {
        HeadsetChange::Sidetone(level) => written.sidetone = Some(*level),
        HeadsetChange::MicVolume(volume) => written.mic_volume = Some(*volume),
        HeadsetChange::EqPreset(preset) => written.eq_preset = Some(*preset),
        HeadsetChange::EqBands(gains) => {
            written.eq_bands = Some(gains.clone());
            written.eq_preset = Some(gg_device_headset::EQ_CUSTOM_PRESET);
        }
        HeadsetChange::BluetoothAtPowerOn(on) => written.bluetooth_at_power_on = Some(*on),
        HeadsetChange::BluetoothCall(call) => written.bluetooth_call = Some(*call),
        HeadsetChange::Gain(gain) => written.gain = Some(*gain),
        HeadsetChange::LineOut(line_out) => written.line_out = Some(*line_out),
        HeadsetChange::LineOutMix(mix) => written.line_out_mix = Some(*mix),
        HeadsetChange::HomeScreen(home) => written.home_screen = Some(*home),
        HeadsetChange::ScreenDimMinutes(minutes) => written.screen_dim_minutes = Some(*minutes),
        HeadsetChange::NoiseMode(_)
        | HeadsetChange::Transparency(_)
        | HeadsetChange::MicLed(_)
        | HeadsetChange::AutoOffMinutes(_)
        | HeadsetChange::RadioMode(_) => {}
    }
}

/// The settings as clients see them: what the base station reports wins over
/// what gglinux wrote, wherever it reports something.
pub fn merged(
    status: Option<&Status>,
    audio: Option<&AudioSettings>,
    ux: Option<&UxSettings>,
    written: &Written,
) -> HeadsetControls {
    let read = |pick: fn(&AudioSettings) -> Option<u8>| audio.and_then(pick);
    HeadsetControls {
        noise_mode: status.and_then(|s| s.noise_mode()).map(noise_mode),
        transparency: status
            .map(|s| s.transparency_raw)
            .filter(|v| (1..=10).contains(v)),
        mic_led: status.map(|s| s.mic_led_brightness),
        mic_muted: status.and_then(|s| s.mic_muted()),
        auto_off_minutes: status.and_then(|s| s.auto_off).map(|a| a.minutes()),
        radio_mode: status.and_then(|s| s.wireless_mode).map(radio_mode),
        sidetone: read(|a| a.sidetone).or(written.sidetone),
        mic_volume: read(|a| a.mic_volume).or(written.mic_volume),
        eq_preset: read(|a| a.eq_preset).or(written.eq_preset),
        eq_bands: audio
            .and_then(|a| a.eq_bands.clone())
            .or_else(|| written.eq_bands.clone()),
        bluetooth_at_power_on: status
            .and_then(|s| s.bluetooth_at_power_on)
            .or(written.bluetooth_at_power_on),
        bluetooth_call: status
            .and_then(|s| s.bluetooth_call)
            .map(bluetooth_call)
            .or(written.bluetooth_call),
        bluetooth_on: status.and_then(|s| s.bluetooth_on),
        bluetooth_link: status.and_then(|s| s.bluetooth_link).map(bluetooth_link),
        pairing: status.and_then(|s| s.pairing).map(pairing),
        gain: audio
            .and_then(|a| a.gain)
            .map(|g| match g {
                DevGain::Low => HeadsetGain::Low,
                DevGain::High => HeadsetGain::High,
            })
            .or(written.gain),
        line_out: audio
            .and_then(|a| a.line_out)
            .map(|l| match l {
                DevLineOut::Speakers => LineOut::Speakers,
                DevLineOut::Stream => LineOut::Stream,
            })
            .or(written.line_out),
        line_out_mix: audio
            .and_then(|a| a.stream_mix)
            .map(|m| LineOutMix {
                main: m.main,
                aux: m.aux,
                mic: m.mic,
            })
            .or(written.line_out_mix),
        home_screen: ux
            .and_then(|u| u.home_screen)
            .map(|h| match h {
                DevHome::Detailed => HomeScreen::Detailed,
                DevHome::Simple => HomeScreen::Simple,
            })
            .or(written.home_screen),
        screen_dim_minutes: ux
            .and_then(|u| u.dim_timeout)
            .map(|d| d.minutes())
            .or(written.screen_dim_minutes),
    }
}

pub fn noise_mode(mode: DevNoise) -> NoiseMode {
    match mode {
        DevNoise::Off => NoiseMode::Off,
        DevNoise::Transparency => NoiseMode::Transparency,
        DevNoise::Anc => NoiseMode::Anc,
    }
}

pub fn radio_mode(mode: WirelessMode) -> RadioMode {
    match mode {
        WirelessMode::Speed => RadioMode::Speed,
        WirelessMode::Range => RadioMode::Range,
    }
}

fn bluetooth_call(call: DevCall) -> BluetoothCall {
    match call {
        DevCall::Nothing => BluetoothCall::Nothing,
        DevCall::LowerGame => BluetoothCall::LowerGame,
        DevCall::MuteGame => BluetoothCall::MuteGame,
    }
}

fn bluetooth_link(link: DevLink) -> BluetoothLink {
    match link {
        DevLink::Off => BluetoothLink::Off,
        DevLink::Connected => BluetoothLink::Connected,
        DevLink::NotConnected => BluetoothLink::NotConnected,
    }
}

fn pairing(p: DevPairing) -> HeadsetPairing {
    match p {
        DevPairing::NotPaired => HeadsetPairing::NotPaired,
        DevPairing::PairedOffline => HeadsetPairing::PairedOffline,
        DevPairing::Connected => HeadsetPairing::Connected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_device_headset::testing::FakeBase;
    use gg_device_headset::NoiseMode as DevNoise;
    use gg_ipc::Feature;

    #[test]
    fn changes_map_onto_device_settings() {
        assert_eq!(
            setting_for(&HeadsetChange::NoiseMode(NoiseMode::Anc)).unwrap(),
            Setting::NoiseMode(DevNoise::Anc)
        );
        assert_eq!(
            setting_for(&HeadsetChange::RadioMode(RadioMode::Range)).unwrap(),
            Setting::WirelessMode(WirelessMode::Range)
        );
        assert_eq!(
            setting_for(&HeadsetChange::AutoOffMinutes(0)).unwrap(),
            Setting::AutoOff(AutoOff::Never)
        );
        assert_eq!(
            setting_for(&HeadsetChange::Sidetone(2)).unwrap(),
            Setting::Sidetone(2)
        );
        assert_eq!(
            setting_for(&HeadsetChange::ScreenDimMinutes(0)).unwrap(),
            Setting::ScreenDim(DimTimeout::Never)
        );
    }

    #[test]
    fn auto_off_minutes_must_be_a_device_level() {
        assert!(
            setting_for(&HeadsetChange::AutoOffMinutes(45)).is_err(),
            "no rounding"
        );
    }

    #[test]
    fn invalid_changes_are_rejected_before_the_device() {
        for change in [
            HeadsetChange::MicVolume(160),
            HeadsetChange::MicLed(0),
            HeadsetChange::Transparency(11),
            HeadsetChange::EqBands(vec![0.0; 3]),
            HeadsetChange::ScreenDimMinutes(45),
            HeadsetChange::LineOutMix(LineOutMix {
                main: 101,
                aux: 0,
                mic: 0,
            }),
        ] {
            assert!(validated(&change).is_err(), "{change:?}");
        }
        assert!(validated(&HeadsetChange::MicVolume(10)).is_ok());
    }

    fn extras_on() -> Experimental {
        let mut e = Experimental::default();
        e.set(Feature::HeadsetExtras, true);
        e
    }

    /// Every base-station setting, with GG's bytes for it.
    fn base_station_changes() -> Vec<(HeadsetChange, Vec<u8>)> {
        vec![
            (
                HeadsetChange::BluetoothAtPowerOn(true),
                vec![0x06, 0xb2, 0x01],
            ),
            (
                HeadsetChange::BluetoothAtPowerOn(false),
                vec![0x06, 0xb2, 0x00],
            ),
            (
                HeadsetChange::BluetoothCall(BluetoothCall::LowerGame),
                vec![0x06, 0xb3, 0x01],
            ),
            (
                HeadsetChange::BluetoothCall(BluetoothCall::MuteGame),
                vec![0x06, 0xb3, 0x02],
            ),
            (
                HeadsetChange::BluetoothCall(BluetoothCall::Nothing),
                vec![0x06, 0xb3, 0x00],
            ),
            (
                HeadsetChange::Gain(HeadsetGain::High),
                vec![0x06, 0x27, 0x02],
            ),
            (
                HeadsetChange::Gain(HeadsetGain::Low),
                vec![0x06, 0x27, 0x01],
            ),
            (
                HeadsetChange::LineOut(LineOut::Stream),
                vec![0x06, 0x43, 0x02],
            ),
            (
                HeadsetChange::LineOut(LineOut::Speakers),
                vec![0x06, 0x43, 0x01],
            ),
            (
                HeadsetChange::LineOutMix(LineOutMix {
                    main: 50,
                    aux: 100,
                    mic: 100,
                }),
                vec![0x06, 0x47, 0x32, 0x00, 0x64, 0x64],
            ),
            (
                HeadsetChange::HomeScreen(HomeScreen::Simple),
                vec![0x06, 0x89, 0x01],
            ),
            (
                HeadsetChange::HomeScreen(HomeScreen::Detailed),
                vec![0x06, 0x89, 0x00],
            ),
            (HeadsetChange::ScreenDimMinutes(0), vec![0x06, 0x83, 0x00]),
            (HeadsetChange::ScreenDimMinutes(1), vec![0x06, 0x83, 0x01]),
            (HeadsetChange::ScreenDimMinutes(5), vec![0x06, 0x83, 0x02]),
            (HeadsetChange::ScreenDimMinutes(10), vec![0x06, 0x83, 0x03]),
            (HeadsetChange::ScreenDimMinutes(15), vec![0x06, 0x83, 0x04]),
            (HeadsetChange::ScreenDimMinutes(30), vec![0x06, 0x83, 0x05]),
            (HeadsetChange::ScreenDimMinutes(60), vec![0x06, 0x83, 0x06]),
        ]
    }

    #[test]
    fn with_its_gate_off_a_base_station_setting_sends_nothing() {
        let fake = FakeBase::new();
        let device = Headset::with_port(fake.clone());
        for (change, _) in base_station_changes() {
            match send_change(Some(&device), &Experimental::default(), &change) {
                Err(Failed::Refused(why)) => assert!(
                    why.contains("hasn't been tried on your hardware yet")
                        && why.contains("Headset: Bluetooth, gain and line out"),
                    "{why}"
                ),
                other => panic!("{change:?}: {other:?}"),
            }
        }
        assert!(
            fake.reports().is_empty(),
            "nothing reached the base station"
        );
        // The gate is checked first: a missing base station doesn't hide it.
        let none: Option<&Headset<FakeBase>> = None;
        let why = send_change(
            none,
            &Experimental::default(),
            &HeadsetChange::ScreenDimMinutes(5),
        );
        assert!(matches!(why, Err(Failed::Refused(m)) if m.contains("Settings → Experimental")));
    }

    #[test]
    fn with_its_gate_on_each_goes_out_as_gg_sends_it_and_alone() {
        for (change, bytes) in base_station_changes() {
            let fake = FakeBase::new();
            let device = Headset::with_port(fake.clone());
            send_change(Some(&device), &extras_on(), &change).unwrap();
            assert_eq!(fake.commands(), vec![bytes], "{change:?}: no save after it");
        }
    }

    #[test]
    fn the_first_milestones_settings_need_no_gate_and_still_save() {
        let fake = FakeBase::new();
        let device = Headset::with_port(fake.clone());
        send_change(
            Some(&device),
            &Experimental::default(),
            &HeadsetChange::Sidetone(1),
        )
        .unwrap();
        assert_eq!(
            fake.commands(),
            vec![vec![0x06, 0x39, 0x01], vec![0x06, 0x09]]
        );
    }

    #[test]
    fn a_bad_value_or_no_base_station_sends_nothing_even_with_the_gate_on() {
        let fake = FakeBase::new();
        let device = Headset::with_port(fake.clone());
        for change in [
            HeadsetChange::ScreenDimMinutes(2),
            HeadsetChange::LineOutMix(LineOutMix {
                main: 100,
                aux: 120,
                mic: 100,
            }),
        ] {
            assert!(matches!(
                send_change(Some(&device), &extras_on(), &change),
                Err(Failed::Refused(_))
            ));
        }
        assert!(fake.reports().is_empty());
        let none: Option<&Headset<FakeBase>> = None;
        assert_eq!(
            send_change(none, &extras_on(), &HeadsetChange::Gain(HeadsetGain::High)),
            Err(Failed::Refused("base station not connected".into()))
        );
    }

    #[test]
    fn recording_custom_bands_marks_the_preset_custom() {
        let mut w = Written::default();
        record(&mut w, &HeadsetChange::EqBands(vec![1.0; 10]));
        assert_eq!(w.eq_preset, Some(4));
        assert_eq!(w.eq_bands, Some(vec![1.0; 10]));
        record(&mut w, &HeadsetChange::Sidetone(3));
        assert_eq!(w.sidetone, Some(3));
        record(&mut w, &HeadsetChange::NoiseMode(NoiseMode::Off));
        assert_eq!(w.mic_volume, None, "read-back settings are not recorded");
        record(&mut w, &HeadsetChange::ScreenDimMinutes(5));
        record(&mut w, &HeadsetChange::HomeScreen(HomeScreen::Simple));
        assert_eq!(
            (w.screen_dim_minutes, w.home_screen),
            (Some(5), Some(HomeScreen::Simple))
        );
    }

    /// What the fake base station answers to a poll and an `80` query: GG's
    /// defaults, as GG read them in the captures.
    fn read() -> (Status, AudioSettings, UxSettings) {
        let device = Headset::with_port(FakeBase::new());
        let p = device.poll().unwrap();
        let ux = device.ux_settings().unwrap().decoded.unwrap();
        (p.status.unwrap(), p.audio.unwrap(), ux)
    }

    #[test]
    fn what_the_base_station_reports_wins_over_what_was_written() {
        let (status, audio, ux) = read();
        let mut w = Written::default();
        for change in [
            HeadsetChange::HomeScreen(HomeScreen::Simple),
            HeadsetChange::ScreenDimMinutes(30),
            HeadsetChange::Gain(HeadsetGain::High),
            HeadsetChange::LineOut(LineOut::Stream),
            HeadsetChange::Sidetone(3),
            HeadsetChange::BluetoothCall(BluetoothCall::MuteGame),
            HeadsetChange::BluetoothAtPowerOn(true),
        ] {
            record(&mut w, &change);
        }
        let c = merged(Some(&status), Some(&audio), Some(&ux), &w);
        assert_eq!(c.home_screen, Some(HomeScreen::Detailed));
        assert_eq!(c.screen_dim_minutes, Some(1));
        assert_eq!(c.gain, Some(HeadsetGain::Low));
        assert_eq!(c.line_out, Some(LineOut::Speakers));
        assert_eq!(c.sidetone, Some(0));
        assert_eq!(c.mic_volume, Some(10));
        assert_eq!(c.eq_preset, Some(0));
        assert_eq!(c.eq_bands, Some(vec![0.0; 10]));
        assert_eq!(c.bluetooth_call, Some(BluetoothCall::Nothing));
        assert_eq!(c.bluetooth_at_power_on, Some(false));
        assert_eq!(
            c.line_out_mix,
            Some(LineOutMix {
                main: 100,
                aux: 100,
                mic: 100
            })
        );
        // Read only, from the `b0` reply GG got.
        assert_eq!(c.bluetooth_on, Some(false));
        assert_eq!(c.bluetooth_link, Some(BluetoothLink::Off));
        assert_eq!(c.pairing, Some(HeadsetPairing::PairedOffline));
    }

    #[test]
    fn what_was_written_shows_where_nothing_is_reported() {
        let mut w = Written::default();
        record(&mut w, &HeadsetChange::Gain(HeadsetGain::High));
        record(&mut w, &HeadsetChange::HomeScreen(HomeScreen::Simple));
        record(&mut w, &HeadsetChange::ScreenDimMinutes(10));
        let unread = merged(None, None, None, &w);
        assert_eq!(unread.gain, Some(HeadsetGain::High));
        assert_eq!(unread.pairing, None, "read only: never written");
        let (status, audio, _) = read();
        let c = merged(Some(&status), Some(&audio), None, &w);
        assert_eq!(
            (c.home_screen, c.screen_dim_minutes),
            (Some(HomeScreen::Simple), Some(10)),
            "without an 80 reply (gate off), the screen settings are what was written"
        );
        assert_eq!(
            merged(None, None, None, &Written::default()),
            HeadsetControls::default()
        );
    }
}
