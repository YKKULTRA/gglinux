// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake Arctis Nova Pro Wireless: settings echoed back as the device would report them.

use gg_ipc::{HeadsetChange, HeadsetControls, HeadsetState, Request, ResponseData};

use crate::log::method_name;
use crate::{error, Fake};

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    match req {
        Request::GetHeadsetState => ResponseData::Headset {
            state: headset_state(s),
        },
        Request::SetHeadsetScreen { settings } => {
            s.screen = settings;
            ResponseData::Success
        }
        Request::SetHeadset { change } => {
            fake_apply(&mut s.controls, change);
            ResponseData::Success
        }
        other => error(format!("{} is not a headset request", method_name(&other))),
    }
}

fn headset_state(s: &Fake) -> HeadsetState {
    let listed = s
        .devices
        .iter()
        .any(|d| d.kind == gg_ipc::DeviceKind::Headset);
    s.looks.headset_state(listed, s.screen, &s.controls)
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
    }
}
