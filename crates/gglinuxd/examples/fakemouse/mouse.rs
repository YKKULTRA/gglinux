// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake Rival 3: settings echoed back as applied, a battery that drifts.

use gg_ipc::{
    DeviceKind, DpiStage, Feature, MouseDpi, MouseRadio, MouseSettings, MouseState, Request,
    ResponseData, MOUSE_SLEEP_MINUTES,
};

use crate::log::method_name;
use crate::{error, Fake};

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    match req {
        Request::GetMouseState => ResponseData::Mouse {
            state: mouse_state(s),
        },
        Request::SetMouseDpi {
            stages,
            active,
            save,
        } => {
            s.applied.get_or_insert_with(MouseSettings::default).dpi =
                Some(MouseDpi { stages, active });
            note_save(save);
            ResponseData::Success
        }
        Request::SetMousePolling { hz, save } => {
            s.applied
                .get_or_insert_with(MouseSettings::default)
                .polling_hz = Some(hz);
            note_save(save);
            ResponseData::Success
        }
        Request::SetMouseButtons { actions, save } => {
            s.applied.get_or_insert_with(MouseSettings::default).buttons = Some(actions);
            note_save(save);
            ResponseData::Success
        }
        Request::SaveMouse => {
            eprintln!(">>   (would commit to onboard memory)");
            ResponseData::Success
        }
        // Gated like the daemon, so the app's greyed state and refusal can be drawn.
        Request::SetMouseSleep { minutes } => {
            if let Some(why) = s.experimental.refusal(Feature::MousePower) {
                return error(why);
            }
            if !MOUSE_SLEEP_MINUTES.contains(&minutes) {
                return error(format!(
                    "the sleep timer takes 1 to 20 minutes, not {minutes}"
                ));
            }
            s.applied
                .get_or_insert_with(MouseSettings::default)
                .sleep_minutes = Some(minutes);
            eprintln!(">>   (would also commit to onboard memory)");
            ResponseData::Success
        }
        Request::SetMouseRadio { radio, save } => {
            if let Some(why) = s.experimental.refusal(Feature::MousePower) {
                return error(why);
            }
            s.applied.get_or_insert_with(MouseSettings::default).radio = Some(radio);
            note_save(save);
            ResponseData::Success
        }
        other => error(format!("{} is not a mouse request", method_name(&other))),
    }
}

fn note_save(save: bool) {
    if save {
        eprintln!(">>   (would also commit to onboard memory)");
    }
}

fn mouse_state(s: &Fake) -> MouseState {
    let power = s.experimental.on(Feature::MousePower);
    let applied = s.applied.as_ref();
    MouseState {
        connected: s.devices.iter().any(|d| d.kind == DeviceKind::Mouse),
        battery: s.battery,
        applied: s.applied.clone(),
        // The fake mouse "reports" what was last written, GG's defaults before that, while
        // the gate is on: as the daemon reads it.
        sleep_secs: power.then(|| {
            let minutes = applied.and_then(|a| a.sleep_minutes).unwrap_or(5);
            u16::from(minutes) * 60
        }),
        radio: power.then(|| applied.and_then(|a| a.radio).unwrap_or(MouseRadio::Normal)),
        reported_dpi: power.then(|| {
            applied.and_then(|a| a.dpi.clone()).unwrap_or(MouseDpi {
                stages: [400, 800, 1600, 2400, 3200]
                    .map(|d| DpiStage { x: d, y: d })
                    .to_vec(),
                active: 2,
            })
        }),
        reported_polling_hz: power.then(|| applied.and_then(|a| a.polling_hz).unwrap_or(1000)),
    }
}
