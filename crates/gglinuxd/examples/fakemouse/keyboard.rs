// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake Apex Pro TKL Gen 3: the keyboard state, lighting and switches as last set. Its
//! screen, key bindings and onboard slots each have their own module.
//!
//! Lighting is checked as the daemon checks it, Reflect zones included: refused while
//! `lighting_reflect` is off, unless the lighting had them already. With the gate on,
//! Reflect stops at once, as the daemon's does while its screen capture isn't built.

use gg_device_keyboard::layout::US;
use gg_ipc::{
    Experimental, Feature, KeyInfo, KeyboardState, Lighting, ReactiveStatus, ReflectStatus,
    Request, ResponseData,
};

use crate::{keyboard_bindings, keyboard_macros, keyboard_onboard, keyboard_screen, Fake};

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    match req {
        Request::GetKeyboardState => ResponseData::Keyboard {
            state: s.keyboard.clone(),
        },
        Request::SetLighting { lighting } => {
            if let Err(message) = check_lighting(&lighting, &s.keyboard.lighting, &s.experimental) {
                return ResponseData::Error { message };
            }
            // Like a machine without the optional udev rule, so the UI's hint shows.
            s.keyboard.reactive = if lighting.enabled && lighting.reactive.enabled {
                ReactiveStatus::NoAccess
            } else {
                ReactiveStatus::Off
            };
            s.keyboard.reflect = reflect_status(&lighting, &s.experimental);
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
        req @ (Request::GetMacros | Request::SetMacros { .. } | Request::WriteMacros { .. }) => {
            keyboard_macros::handle(req, s)
        }
        other => crate::error(format!(
            "{} is not a keyboard request",
            crate::log::method_name(&other)
        )),
    }
}

/// The daemon's checks: ranges, then Reflect's gate for lighting that brings Reflect in.
fn check_lighting(new: &Lighting, now: &Lighting, gates: &Experimental) -> Result<(), String> {
    new.validate()?;
    match gates.refusal(Feature::LightingReflect) {
        Some(why) if new.uses_reflect() && !now.uses_reflect() => Err(why),
        _ => Ok(()),
    }
}

/// What the daemon reports for Reflect with this lighting: stopped, as its capture isn't
/// built, or by the gate.
fn reflect_status(lighting: &Lighting, gates: &Experimental) -> ReflectStatus {
    let drawn = lighting.enabled
        && lighting
            .stack()
            .iter()
            .any(|z| z.effect == gg_ipc::ZoneEffect::Reflect);
    match (drawn, gates.refusal(Feature::LightingReflect)) {
        (false, _) => ReflectStatus::Off,
        (true, Some(why)) => ReflectStatus::Stopped { why },
        (true, None) => ReflectStatus::Stopped {
            why: ReflectStatus::NOT_BUILT.into(),
        },
    }
}

/// The Apex as the daemon reports it, lighting off.
pub fn fake_keyboard() -> KeyboardState {
    KeyboardState {
        connected: true,
        layout: US
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
        lighting_idle: false,
        reflect: ReflectStatus::Off,
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
        assert_eq!(layout.len(), US.len());
        assert!(layout.iter().any(|k| k.label == "Esc"), "{layout:?}");
    }

    #[test]
    fn the_fake_keyboard_is_the_us_gen_3_board_with_its_mute_key_on_it() {
        let kb = fake_keyboard();
        assert!(
            kb.layout.iter().all(|k| !(0x46..=0x48).contains(&k.code)),
            "no Print Screen, Scroll Lock or Pause"
        );
        assert!(
            kb.layout.iter().any(|k| k.code == 0xFB),
            "the screen button"
        );
        let mute = kb.lighting.mic_mute.key;
        assert!(kb.layout.iter().any(|k| k.code == mute), "{mute:#04x}");
    }

    #[test]
    fn reflect_is_refused_while_its_gate_is_off_and_stops_as_not_built_once_it_is_on() {
        let mut lit = Lighting {
            enabled: true,
            ..Lighting::default()
        };
        lit.reflect_on();
        let off = Experimental::default();
        let none = Lighting::default();
        let why = check_lighting(&lit, &none, &off).unwrap_err();
        assert!(why.contains("Reflect"), "{why}");
        assert!(
            check_lighting(&lit, &lit, &off).is_ok(),
            "it had Reflect already"
        );
        let mut on = off;
        on.set(Feature::LightingReflect, true);
        assert!(check_lighting(&lit, &none, &on).is_ok());
        assert_eq!(
            reflect_status(&lit, &on),
            ReflectStatus::Stopped {
                why: ReflectStatus::NOT_BUILT.into()
            }
        );
        assert_eq!(reflect_status(&none, &on), ReflectStatus::Off);
        let bad = Lighting {
            brightness: 120,
            ..Lighting::default()
        };
        assert!(check_lighting(&bad, &none, &on).is_err(), "ranges too");
    }
}
