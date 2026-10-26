//! Headset settings between the IPC and the device: mapping a client's change
//! onto the device's typed setting, and remembering what the device cannot
//! report back.

use gg_device_headset::{AutoOff, NoiseMode as DevNoise, Setting, WirelessMode};
use gg_ipc::{HeadsetChange, NoiseMode, RadioMode};

/// Map a client's change onto the device setting. Ranges are the device
/// crate's to judge (see [`validated`]); only the auto-off conversion, which
/// has no device meaning outside its levels, is checked here.
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
    })
}

/// A setting that is safe to send: mapped, and every packet it needs encodes.
/// Checked before the device is touched, so a bad value never half-applies.
pub fn validated(change: &HeadsetChange) -> Result<Setting, String> {
    let setting = setting_for(change)?;
    setting.packets().map_err(|e| format!("{e:#}"))?;
    Ok(setting)
}

/// Settings the device cannot report, as gglinux last wrote them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WriteOnly {
    pub sidetone: Option<u8>,
    pub mic_volume: Option<u8>,
    pub eq_preset: Option<u8>,
    pub eq_bands: Option<Vec<f32>>,
}

/// Remember an applied change if the device cannot report it back.
pub fn record(written: &mut WriteOnly, change: &HeadsetChange) {
    match change {
        HeadsetChange::Sidetone(level) => written.sidetone = Some(*level),
        HeadsetChange::MicVolume(volume) => written.mic_volume = Some(*volume),
        HeadsetChange::EqPreset(preset) => written.eq_preset = Some(*preset),
        HeadsetChange::EqBands(gains) => {
            written.eq_bands = Some(gains.clone());
            written.eq_preset = Some(gg_device_headset::EQ_CUSTOM_PRESET);
        }
        _ => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use gg_device_headset::NoiseMode as DevNoise;

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
        ] {
            assert!(validated(&change).is_err(), "{change:?}");
        }
        assert!(validated(&HeadsetChange::MicVolume(10)).is_ok());
    }

    #[test]
    fn recording_custom_bands_marks_the_preset_custom() {
        let mut w = WriteOnly::default();
        record(&mut w, &HeadsetChange::EqBands(vec![1.0; 10]));
        assert_eq!(w.eq_preset, Some(4));
        assert_eq!(w.eq_bands, Some(vec![1.0; 10]));
        record(&mut w, &HeadsetChange::Sidetone(3));
        assert_eq!(w.sidetone, Some(3));
        record(&mut w, &HeadsetChange::NoiseMode(NoiseMode::Off));
        assert_eq!(w.mic_volume, None, "read-back settings are not recorded");
    }
}
