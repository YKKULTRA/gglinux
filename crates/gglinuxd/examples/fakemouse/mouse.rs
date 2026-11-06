// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake Rival 3: settings echoed back as applied, a battery that drifts.

use gg_ipc::{DeviceKind, MouseDpi, MouseSettings, MouseState, Request, ResponseData};

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
        other => error(format!("{} is not a mouse request", method_name(&other))),
    }
}

fn note_save(save: bool) {
    if save {
        eprintln!(">>   (would also commit to onboard memory)");
    }
}

fn mouse_state(s: &Fake) -> MouseState {
    MouseState {
        connected: s.devices.iter().any(|d| d.kind == DeviceKind::Mouse),
        battery: s.battery,
        applied: s.applied.clone(),
    }
}
