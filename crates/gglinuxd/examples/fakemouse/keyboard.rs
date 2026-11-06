// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake Apex Pro TKL Gen 3: the keyboard state, lighting and switches as last set. Its
//! screen, key bindings and onboard slots each have their own module.

use gg_device_keyboard::layout::ANSI_TKL;
use gg_ipc::{KeyInfo, KeyboardState, Lighting, ReactiveStatus, Request, ResponseData};

use crate::{keyboard_bindings, keyboard_onboard, keyboard_screen, Fake};

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    match req {
        Request::GetKeyboardState => ResponseData::Keyboard {
            state: s.keyboard.clone(),
        },
        Request::SetLighting { lighting } => {
            // Like a machine without the optional udev rule, so the UI's hint shows.
            s.keyboard.reactive = if lighting.enabled && lighting.reactive.enabled {
                ReactiveStatus::NoAccess
            } else {
                ReactiveStatus::Off
            };
            s.keyboard.lighting = lighting;
            ResponseData::Success
        }
        Request::SetSwitches { switches } => match switches.validate() {
            Ok(()) => {
                s.keyboard.switches = switches;
                ResponseData::Success
            }
            Err(message) => ResponseData::Error { message },
        },
        req @ (Request::SetScreen { .. } | Request::SetScreenCare { .. }) => {
            keyboard_screen::handle(req, s)
        }
        req @ (Request::GetBindings | Request::ReadBindings | Request::SetBindings { .. }) => {
            keyboard_bindings::handle(req, s)
        }
        req @ (Request::GetOnboard
        | Request::ReadOnboard { .. }
        | Request::LoadOnboard { .. }
        | Request::WriteOnboard { .. }) => keyboard_onboard::handle(req, s),
        other => crate::error(format!(
            "{} is not a keyboard request",
            crate::log::method_name(&other)
        )),
    }
}

/// The Apex as the daemon reports it, lighting off.
pub fn fake_keyboard() -> KeyboardState {
    KeyboardState {
        connected: true,
        layout: ANSI_TKL
            .iter()
            .map(|&(code, label, x, y, w)| KeyInfo {
                code,
                label: label.to_owned(),
                x,
                y,
                w,
            })
            .collect(),
        lighting: Lighting::default(),
        reactive: ReactiveStatus::Off,
        analog_keys: gg_device_keyboard::proto::ANALOG_KEYS.to_vec(),
        switches: Default::default(),
        switches_error: None,
        screen: Default::default(),
        screen_error: None,
        screen_care: Default::default(),
        screen_resting: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_keyboard_has_every_key_the_daemon_sends_for_drawing() {
        // The Keyboard panel draws, paints and picks the mic-mute key from this.
        let layout = fake_keyboard().layout;
        assert_eq!(layout.len(), ANSI_TKL.len());
        assert!(layout.iter().any(|k| k.label == "Esc"), "{layout:?}");
    }
}
