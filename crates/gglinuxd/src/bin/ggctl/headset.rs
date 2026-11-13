// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl headset`: the headset and its base station, as the daemon has them, and
//! one setting at a time. The base station's own settings (Bluetooth, gain, line
//! out, home screen, dim timeout) need `ggctl experimental headset_extras on`.

use anyhow::{bail, Context, Result};
use gg_ipc::{
    BluetoothCall, BluetoothLink, HeadsetChange, HeadsetControls, HeadsetGain, HeadsetPairing,
    HeadsetPower, HeadsetState, HomeScreen, LineOut, LineOutMix, NoiseMode, RadioMode, Request,
};

/// The EQ presets by name, in the base station's order (`4` is Custom).
const EQ_PRESETS: [&str; 4] = ["flat", "bass", "focus", "smiley"];
const SIDETONE: [&str; 4] = ["off", "low", "medium", "high"];

pub fn request(words: &[&str]) -> Result<Request> {
    Ok(match words {
        [] => Request::GetHeadsetState,
        [setting, values @ ..] => Request::SetHeadset {
            change: change(setting, values)?,
        },
    })
}

/// What `ggctl headset <setting> <value>` changes.
fn change(setting: &str, values: &[&str]) -> Result<HeadsetChange> {
    let [value] = values else {
        bail!("usage: ggctl headset {setting} <value>   (see ggctl --help)");
    };
    let value = *value;
    let number = |what: &str| -> Result<u8> {
        value
            .parse()
            .with_context(|| format!("{what} is a number, not '{value}'"))
    };
    let pick = |choices: &[&str]| -> Result<usize> {
        choices
            .iter()
            .position(|c| *c == value)
            .with_context(|| format!("{setting} is one of: {}", choices.join(", ")))
    };
    Ok(match setting {
        "anc" => HeadsetChange::NoiseMode(
            [NoiseMode::Off, NoiseMode::Transparency, NoiseMode::Anc]
                [pick(&["off", "transparency", "on"])?],
        ),
        "transparency" => HeadsetChange::Transparency(number("the transparency level (1 to 10)")?),
        "sidetone" => HeadsetChange::Sidetone(match pick(&SIDETONE) {
            Ok(i) => i as u8,
            Err(_) => number("sidetone (off, low, medium, high, or 0 to 3)")?,
        }),
        "mic-volume" => HeadsetChange::MicVolume(number("the mic volume (1 to 10)")?),
        "mic-led" => HeadsetChange::MicLed(number("the mute light (1 to 10)")?),
        "auto-off" => HeadsetChange::AutoOffMinutes(minutes(value, &["never"])?),
        "radio" => {
            HeadsetChange::RadioMode([RadioMode::Speed, RadioMode::Range][pick(&["speed", "range"])?])
        }
        "eq" => match pick(&EQ_PRESETS) {
            Ok(i) => HeadsetChange::EqPreset(i as u8),
            Err(_) => bail!(
                "eq is one of: {}; for your own curve: ggctl headset eq-custom 0,0,0,0,0,0,0,0,0,1.5",
                EQ_PRESETS.join(", ")
            ),
        },
        "eq-custom" => HeadsetChange::EqBands(
            value
                .split(',')
                .map(|g| {
                    g.trim()
                        .parse::<f32>()
                        .with_context(|| format!("'{g}' is not a gain in dB"))
                })
                .collect::<Result<_>>()?,
        ),
        "bluetooth-at-power-on" => HeadsetChange::BluetoothAtPowerOn(pick(&["off", "on"])? == 1),
        "bluetooth-call" => HeadsetChange::BluetoothCall(
            [BluetoothCall::Nothing, BluetoothCall::LowerGame, BluetoothCall::MuteGame]
                [pick(&["nothing", "lower", "mute"])?],
        ),
        "gain" => {
            HeadsetChange::Gain([HeadsetGain::Low, HeadsetGain::High][pick(&["low", "high"])?])
        }
        "line-out" => HeadsetChange::LineOut(
            [LineOut::Speakers, LineOut::Stream][pick(&["speakers", "stream"])?],
        ),
        "stream-mix" => {
            let levels = value
                .split(',')
                .map(|v| {
                    v.trim()
                        .parse::<u8>()
                        .with_context(|| format!("'{v}' is not a level from 0 to 100"))
                })
                .collect::<Result<Vec<_>>>()?;
            let [main, aux, mic] = levels[..] else {
                bail!("usage: ggctl headset stream-mix MAIN,AUX,MIC   (each 0 to 100)");
            };
            HeadsetChange::LineOutMix(LineOutMix { main, aux, mic })
        }
        "home-screen" => HeadsetChange::HomeScreen(
            [HomeScreen::Detailed, HomeScreen::Simple][pick(&["detailed", "simple"])?],
        ),
        "screen-dim" => HeadsetChange::ScreenDimMinutes(minutes(value, &["never", "off"])?),
        other => bail!(
            "unknown headset setting '{other}' (try: anc, transparency, sidetone, mic-volume, \
             mic-led, auto-off, radio, eq, eq-custom, bluetooth-at-power-on, bluetooth-call, gain, \
             line-out, stream-mix, home-screen, screen-dim)"
        ),
    })
}

/// A number of minutes, `5` or `5min`; any of `never` is 0.
fn minutes(value: &str, never: &[&str]) -> Result<u16> {
    if never.contains(&value) {
        return Ok(0);
    }
    value
        .trim_end_matches("min")
        .parse()
        .with_context(|| format!("'{value}' is not a number of minutes"))
}

/// What `ggctl headset` prints.
pub fn print(state: &HeadsetState) {
    for (label, value) in lines(state) {
        println!("{:<15}{value}", format!("{label}:"));
    }
}

/// The lines `ggctl headset` prints, label and value.
fn lines(state: &HeadsetState) -> Vec<(&'static str, String)> {
    let unknown = || "unknown".to_string();
    let c = &state.controls;
    let mut out = vec![(
        "base station",
        if state.connected {
            "connected"
        } else {
            "not connected"
        }
        .to_string(),
    )];
    let battery = state
        .headset_battery
        .map_or(String::new(), |b| format!(", battery {b}%"));
    out.push((
        "headset",
        match state.power {
            HeadsetPower::Online => format!("on{battery}"),
            HeadsetPower::Charging => format!("charging{battery}"),
            HeadsetPower::Offline => "off".into(),
            HeadsetPower::Unknown => unknown(),
        },
    ));
    out.push((
        "spare battery",
        state
            .spare_battery
            .map_or("none".into(), |b| format!("{b}%")),
    ));
    let s = &state.screen;
    out.push((
        "screen",
        format!(
            "burn-in protection {}, lit {} s after events, brightness {}{}",
            if s.protect { "on" } else { "off" },
            s.lit_seconds,
            s.brightness
                .map_or("left as it is".into(), |b| b.to_string()),
            if state.screen_dark { ", dark now" } else { "" }
        ),
    ));
    out.push(("noise control", noise(c)));
    out.push(("microphone", microphone(c)));
    out.push((
        "auto-off",
        c.auto_off_minutes.map_or_else(unknown, |m| match m {
            0 => "never".into(),
            m => format!("{m} min"),
        }),
    ));
    out.push(("2.4 GHz", radio(c)));
    out.push(("equalizer", equalizer(c)));
    out.push(("bluetooth", bluetooth(c)));
    out.push((
        "gain",
        c.gain.map_or_else(unknown, |g| match g {
            HeadsetGain::Low => "low".into(),
            HeadsetGain::High => "high".into(),
        }),
    ));
    out.push(("line out", line_out(c)));
    let not_read = " (not read back: what was set here)";
    out.push((
        "home screen",
        c.home_screen.map_or_else(unknown, |h| {
            let home = match h {
                HomeScreen::Detailed => "detailed",
                HomeScreen::Simple => "simple",
            };
            format!("{home}{not_read}")
        }),
    ));
    out.push((
        "screen dim",
        c.screen_dim_minutes.map_or_else(unknown, |m| {
            let when = match m {
                0 => "never".to_string(),
                m => format!("after {m} min"),
            };
            when + not_read
        }),
    ));
    out
}

fn noise(c: &HeadsetControls) -> String {
    match c.noise_mode {
        None => "unknown".into(),
        Some(NoiseMode::Off) => "off".into(),
        Some(NoiseMode::Anc) => "anc".into(),
        Some(NoiseMode::Transparency) => format!(
            "transparency, level {}",
            c.transparency.map_or("unknown".into(), |t| t.to_string())
        ),
    }
}

fn microphone(c: &HeadsetControls) -> String {
    let or_unknown = |v: Option<u8>| v.map_or("unknown".into(), |v| v.to_string());
    format!(
        "{}, sidetone {}, volume {}, mute light {}",
        match c.mic_muted {
            Some(true) => "muted",
            Some(false) => "live",
            None => "mute unknown",
        },
        c.sidetone
            .and_then(|s| SIDETONE.get(usize::from(s)))
            .map_or("unknown", |s| *s),
        or_unknown(c.mic_volume),
        or_unknown(c.mic_led),
    )
}

fn radio(c: &HeadsetControls) -> String {
    format!(
        "{}; headset link: {}",
        match c.radio_mode {
            Some(RadioMode::Speed) => "speed",
            Some(RadioMode::Range) => "range",
            None => "mode unknown",
        },
        match c.pairing {
            Some(HeadsetPairing::Connected) => "connected",
            Some(HeadsetPairing::PairedOffline) => "paired, headset off or out of reach",
            Some(HeadsetPairing::NotPaired) => "not paired",
            None => "unknown",
        }
    )
}

fn equalizer(c: &HeadsetControls) -> String {
    match c.eq_preset {
        None => "unknown".into(),
        Some(4) => format!(
            "custom: {}",
            c.eq_bands.as_ref().map_or("gains unknown".into(), |bands| {
                bands
                    .iter()
                    .map(|g| format!("{g:+}"))
                    .collect::<Vec<_>>()
                    .join(" ")
                    + " dB"
            })
        ),
        Some(p) => EQ_PRESETS
            .get(usize::from(p))
            .map_or_else(|| format!("preset {p}"), |name| name.to_string()),
    }
}

fn bluetooth(c: &HeadsetControls) -> String {
    let now = match (c.bluetooth_on, c.bluetooth_link) {
        (_, Some(BluetoothLink::Connected)) => "on, a phone is connected",
        (_, Some(BluetoothLink::NotConnected)) => "on, nothing connected",
        (Some(false), _) | (_, Some(BluetoothLink::Off)) => "off",
        (Some(true), None) => "on",
        (None, None) => "unknown",
    };
    format!(
        "{now}; on at power-on: {}; during a call: {}",
        match c.bluetooth_at_power_on {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        },
        match c.bluetooth_call {
            Some(BluetoothCall::Nothing) => "nothing",
            Some(BluetoothCall::LowerGame) => "game 12 dB lower",
            Some(BluetoothCall::MuteGame) => "game muted",
            None => "unknown",
        }
    )
}

fn line_out(c: &HeadsetControls) -> String {
    match c.line_out {
        None => "unknown".into(),
        Some(LineOut::Speakers) => "speakers".into(),
        Some(LineOut::Stream) => format!(
            "stream ({})",
            c.line_out_mix.map_or("mix unknown".into(), |m| format!(
                "main {}, aux {}, mic {}",
                m.main, m.aux, m.mic
            ))
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::ScreenSettings;

    fn set(line: &str) -> HeadsetChange {
        let words: Vec<&str> = line.split_whitespace().collect();
        match request(&words).unwrap() {
            Request::SetHeadset { change } => change,
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn bare_headset_reads_the_state() {
        assert!(matches!(request(&[]).unwrap(), Request::GetHeadsetState));
    }

    #[test]
    fn the_base_station_settings_map_to_their_changes() {
        assert_eq!(
            set("bluetooth-at-power-on on"),
            HeadsetChange::BluetoothAtPowerOn(true)
        );
        assert_eq!(
            set("bluetooth-at-power-on off"),
            HeadsetChange::BluetoothAtPowerOn(false)
        );
        assert_eq!(
            set("bluetooth-call lower"),
            HeadsetChange::BluetoothCall(BluetoothCall::LowerGame)
        );
        assert_eq!(
            set("bluetooth-call mute"),
            HeadsetChange::BluetoothCall(BluetoothCall::MuteGame)
        );
        assert_eq!(
            set("bluetooth-call nothing"),
            HeadsetChange::BluetoothCall(BluetoothCall::Nothing)
        );
        assert_eq!(set("gain high"), HeadsetChange::Gain(HeadsetGain::High));
        assert_eq!(
            set("line-out stream"),
            HeadsetChange::LineOut(LineOut::Stream)
        );
        assert_eq!(
            set("stream-mix 50,100,0"),
            HeadsetChange::LineOutMix(LineOutMix {
                main: 50,
                aux: 100,
                mic: 0
            })
        );
        assert_eq!(
            set("home-screen simple"),
            HeadsetChange::HomeScreen(HomeScreen::Simple)
        );
        assert_eq!(set("screen-dim 5"), HeadsetChange::ScreenDimMinutes(5));
        assert_eq!(set("screen-dim off"), HeadsetChange::ScreenDimMinutes(0));
        assert_eq!(set("screen-dim 30min"), HeadsetChange::ScreenDimMinutes(30));
    }

    #[test]
    fn the_first_milestones_settings_map_too() {
        assert_eq!(set("anc on"), HeadsetChange::NoiseMode(NoiseMode::Anc));
        assert_eq!(
            set("anc transparency"),
            HeadsetChange::NoiseMode(NoiseMode::Transparency)
        );
        assert_eq!(set("transparency 7"), HeadsetChange::Transparency(7));
        assert_eq!(set("sidetone medium"), HeadsetChange::Sidetone(2));
        assert_eq!(set("sidetone 3"), HeadsetChange::Sidetone(3));
        assert_eq!(set("mic-volume 10"), HeadsetChange::MicVolume(10));
        assert_eq!(set("mic-led 1"), HeadsetChange::MicLed(1));
        assert_eq!(set("auto-off never"), HeadsetChange::AutoOffMinutes(0));
        assert_eq!(set("auto-off 30"), HeadsetChange::AutoOffMinutes(30));
        assert_eq!(set("radio range"), HeadsetChange::RadioMode(RadioMode::Range));
        assert_eq!(set("eq flat"), HeadsetChange::EqPreset(0));
        assert_eq!(set("eq smiley"), HeadsetChange::EqPreset(3));
        assert_eq!(
            set("eq-custom 0,0,0,0,0,0,0,0,0,1.5"),
            HeadsetChange::EqBands(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.5])
        );
    }

    #[test]
    fn mistakes_are_errors() {
        for bad in [
            "gain medium",
            "gain",
            "gain low high",
            "stream-mix 100,100",
            "stream-mix 100,100,300",
            "line-out headphones",
            "screen-dim soon",
            "bluetooth-call maybe",
            "anc max",
            "eq custom",
            "frobnicate 1",
        ] {
            let words: Vec<&str> = bad.split_whitespace().collect();
            assert!(request(&words).is_err(), "{bad}");
        }
    }

    fn state(controls: HeadsetControls) -> HeadsetState {
        HeadsetState {
            connected: true,
            power: HeadsetPower::Offline,
            headset_battery: None,
            spare_battery: Some(100),
            screen: ScreenSettings::default(),
            screen_dark: true,
            controls,
        }
    }

    fn line(state: &HeadsetState, label: &str) -> String {
        lines(state)
            .into_iter()
            .find(|(l, _)| *l == label)
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("no {label} line"))
    }

    #[test]
    fn nothing_known_prints_unknown() {
        let s = state(HeadsetControls::default());
        for label in [
            "noise control",
            "auto-off",
            "equalizer",
            "gain",
            "line out",
            "home screen",
            "screen dim",
        ] {
            assert_eq!(line(&s, label), "unknown", "{label}");
        }
        assert!(
            line(&s, "bluetooth").starts_with("unknown;"),
            "{}",
            line(&s, "bluetooth")
        );
        assert_eq!(line(&s, "headset"), "off");
        assert_eq!(line(&s, "spare battery"), "100%");
    }

    #[test]
    fn what_is_known_prints_plainly() {
        let s = state(HeadsetControls {
            bluetooth_on: Some(false),
            bluetooth_link: Some(BluetoothLink::Off),
            bluetooth_at_power_on: Some(false),
            bluetooth_call: Some(BluetoothCall::LowerGame),
            pairing: Some(HeadsetPairing::PairedOffline),
            radio_mode: Some(RadioMode::Speed),
            gain: Some(HeadsetGain::Low),
            line_out: Some(LineOut::Stream),
            line_out_mix: Some(LineOutMix {
                main: 50,
                aux: 100,
                mic: 0,
            }),
            screen_dim_minutes: Some(5),
            eq_preset: Some(0),
            ..HeadsetControls::default()
        });
        assert_eq!(
            line(&s, "bluetooth"),
            "off; on at power-on: no; during a call: game 12 dB lower"
        );
        assert_eq!(
            line(&s, "2.4 GHz"),
            "speed; headset link: paired, headset off or out of reach"
        );
        assert_eq!(line(&s, "line out"), "stream (main 50, aux 100, mic 0)");
        assert_eq!(line(&s, "gain"), "low");
        assert_eq!(line(&s, "equalizer"), "flat");
        assert!(line(&s, "screen dim").starts_with("after 5 min (not read back"));
    }

    #[test]
    fn the_usage_lists_the_headset() {
        assert!(crate::USAGE.contains("ggctl headset "));
        assert!(crate::USAGE.contains("ggctl headset screen-dim"));
    }
}
