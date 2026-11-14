// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake Arctis Nova Pro Wireless: settings echoed back as the base station
//! would report them, one read late.
//!
//! As in the daemon, the base station's own settings (Bluetooth, gain, line out,
//! home screen, dim timeout) are refused while `headset_extras` is off. A change
//! shows from the second read after it: the first still gets the settings from
//! before, as a real base station's first reply after a write may. With
//! `--reported` the base station reports GG's defaults (the owner's settings since
//! the captures) for everything its replies carry; the home screen and the dim
//! timeout only while `headset_extras` is on, as the daemon asks for them only then.

use std::sync::Mutex;

use gg_ipc::{
    BluetoothCall, BluetoothLink, Feature, HeadsetChange, HeadsetControls, HeadsetGain,
    HeadsetPairing, HeadsetPower, HeadsetState, HomeScreen, LineOut, LineOutMix, Request,
    ResponseData,
};

use crate::log::method_name;
use crate::{error, Fake};

/// The settings from before the last change, for the one read that still gets them.
static BEFORE: Mutex<Option<HeadsetControls>> = Mutex::new(None);

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    match req {
        Request::GetHeadsetState => {
            let before = BEFORE.lock().unwrap_or_else(|e| e.into_inner()).take();
            ResponseData::Headset {
                state: headset_state(s, before.as_ref().unwrap_or(&s.controls)),
            }
        }
        Request::SetHeadsetScreen { settings } => {
            s.screen = settings;
            ResponseData::Success
        }
        Request::SetHeadset { change } => {
            if let Some(why) = change.feature().and_then(|f| s.experimental.refusal(f)) {
                return error(why);
            }
            BEFORE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert_with(|| s.controls.clone());
            fake_apply(&mut s.controls, change);
            ResponseData::Success
        }
        other => error(format!("{} is not a headset request", method_name(&other))),
    }
}

/// Whether the fake was started with `--reported`.
fn reported() -> bool {
    std::env::args().any(|a| a == "--reported")
}

fn headset_state(s: &Fake, controls: &HeadsetControls) -> HeadsetState {
    let listed = s
        .devices
        .iter()
        .any(|d| d.kind == gg_ipc::DeviceKind::Headset);
    let mut state = s.looks.headset_state(listed, s.screen, controls);
    if state.connected {
        if reported() {
            let extras = s.experimental.on(Feature::HeadsetExtras);
            reported_defaults(&mut state.controls, state.power, extras);
        }
    } else {
        // As the daemon: with no base station, what was written still shows.
        let c = &mut state.controls;
        c.bluetooth_at_power_on = controls.bluetooth_at_power_on;
        c.bluetooth_call = controls.bluetooth_call;
        c.gain = controls.gain;
        c.line_out = controls.line_out;
        c.line_out_mix = controls.line_out_mix;
        c.home_screen = controls.home_screen;
        c.screen_dim_minutes = controls.screen_dim_minutes;
    }
    state
}

/// What a base station on GG's defaults reports, where nothing was set here. The
/// home screen and the dim timeout come from the `80` reply, read only with
/// `headset_extras` on.
fn reported_defaults(c: &mut HeadsetControls, power: HeadsetPower, extras: bool) {
    c.sidetone.get_or_insert(0);
    c.mic_volume.get_or_insert(10);
    c.eq_preset.get_or_insert(0);
    c.eq_bands.get_or_insert_with(|| vec![0.0; 10]);
    c.bluetooth_at_power_on.get_or_insert(false);
    c.bluetooth_call.get_or_insert(BluetoothCall::Nothing);
    c.bluetooth_on.get_or_insert(false);
    c.bluetooth_link.get_or_insert(BluetoothLink::Off);
    c.pairing.get_or_insert(match power {
        HeadsetPower::Online | HeadsetPower::Charging => HeadsetPairing::Connected,
        HeadsetPower::Offline | HeadsetPower::Unknown => HeadsetPairing::PairedOffline,
    });
    c.gain.get_or_insert(HeadsetGain::Low);
    c.line_out.get_or_insert(LineOut::Speakers);
    c.line_out_mix.get_or_insert(LineOutMix {
        main: 100,
        aux: 100,
        mic: 100,
    });
    if extras {
        c.home_screen.get_or_insert(HomeScreen::Detailed);
        c.screen_dim_minutes.get_or_insert(1);
    }
}

/// Echo a change into the fake's controls, as the real device would report it.
fn fake_apply(c: &mut HeadsetControls, change: HeadsetChange) {
    match change {
        HeadsetChange::NoiseMode(m) => c.noise_mode = Some(m),
        HeadsetChange::Transparency(v) => c.transparency = Some(v),
        HeadsetChange::Sidetone(v) => c.sidetone = Some(v),
        HeadsetChange::MicVolume(v) => c.mic_volume = Some(v),
        HeadsetChange::MicLed(v) => c.mic_led = Some(v),
        HeadsetChange::AutoOffMinutes(v) => c.auto_off_minutes = Some(v),
        HeadsetChange::RadioMode(m) => c.radio_mode = Some(m),
        HeadsetChange::EqPreset(p) => c.eq_preset = Some(p),
        HeadsetChange::EqBands(b) => {
            c.eq_bands = Some(b);
            c.eq_preset = Some(4);
        }
        HeadsetChange::BluetoothAtPowerOn(on) => c.bluetooth_at_power_on = Some(on),
        HeadsetChange::BluetoothCall(call) => c.bluetooth_call = Some(call),
        HeadsetChange::Gain(g) => c.gain = Some(g),
        HeadsetChange::LineOut(l) => c.line_out = Some(l),
        HeadsetChange::LineOutMix(m) => c.line_out_mix = Some(m),
        HeadsetChange::HomeScreen(h) => c.home_screen = Some(h),
        HeadsetChange::ScreenDimMinutes(m) => c.screen_dim_minutes = Some(m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_change_is_echoed_where_the_daemon_shows_it() {
        let mut c = HeadsetControls::default();
        fake_apply(&mut c, HeadsetChange::Gain(HeadsetGain::High));
        fake_apply(&mut c, HeadsetChange::LineOut(LineOut::Stream));
        fake_apply(
            &mut c,
            HeadsetChange::LineOutMix(LineOutMix {
                main: 50,
                aux: 0,
                mic: 7,
            }),
        );
        fake_apply(&mut c, HeadsetChange::ScreenDimMinutes(5));
        assert_eq!(c.gain, Some(HeadsetGain::High));
        assert_eq!(c.line_out, Some(LineOut::Stream));
        assert_eq!(c.line_out_mix.map(|m| m.main), Some(50));
        assert_eq!(c.screen_dim_minutes, Some(5));
    }

    #[test]
    fn a_reported_base_station_fills_only_what_wasnt_set() {
        let mut c = HeadsetControls {
            gain: Some(HeadsetGain::High),
            ..HeadsetControls::default()
        };
        reported_defaults(&mut c, HeadsetPower::Offline, false);
        assert_eq!(c.gain, Some(HeadsetGain::High));
        assert_eq!(c.line_out, Some(LineOut::Speakers));
        assert_eq!(c.pairing, Some(HeadsetPairing::PairedOffline));
        assert_eq!((c.home_screen, c.screen_dim_minutes), (None, None));
        reported_defaults(&mut c, HeadsetPower::Offline, true);
        assert_eq!(
            (c.home_screen, c.screen_dim_minutes),
            (Some(HomeScreen::Detailed), Some(1))
        );
    }
}
