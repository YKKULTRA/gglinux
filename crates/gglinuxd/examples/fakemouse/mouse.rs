// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake Rival 3: settings echoed back as applied, a battery that drifts.
//!
//! With `--asleep` it behaves as the daemon does with a sleeping mouse: no battery, none of
//! the mouse's own settings shown, and every write refused with [`MOUSE_ASLEEP`]. The
//! wheel is checked as the daemon checks it: any button name, and anything but the factory
//! pair only with `mouse_wheel` on.

use gg_device_mouse::{proto, ButtonAction, BUTTON_COUNT, DEFAULT_WHEEL};
use gg_ipc::{
    DeviceKind, DpiStage, Experimental, Feature, MouseDpi, MouseRadio, MouseSettings, MouseState,
    Request, ResponseData, MOUSE_ASLEEP, MOUSE_SLEEP_MINUTES,
};

use crate::log::method_name;
use crate::{error, Fake};

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    if let Request::GetMouseState = req {
        return ResponseData::Mouse {
            state: mouse_state(s),
        };
    }
    // Gated like the daemon, so the app's greyed state and refusal can be drawn.
    if let Some(why) = refusal(&req, &s.experimental) {
        return error(why);
    }
    if connected(s) && s.battery.is_none() {
        return error(MOUSE_ASLEEP.into());
    }
    match req {
        Request::SetMouseDpi {
            stages,
            active,
            save,
        } => {
            s.applied.get_or_insert_with(MouseSettings::default).dpi =
                Some(MouseDpi { stages, active });
            note_save(save);
        }
        Request::SetMousePolling { hz, save } => {
            s.applied
                .get_or_insert_with(MouseSettings::default)
                .polling_hz = Some(hz);
            note_save(save);
        }
        Request::SetMouseButtons { actions, save } => {
            let applied = s.applied.get_or_insert_with(MouseSettings::default);
            applied.buttons = Some(recorded(
                applied.buttons.as_deref(),
                &actions,
                s.experimental.on(Feature::MouseWheel),
            ));
            note_save(save);
        }
        Request::SaveMouse => eprintln!(">>   (would commit to onboard memory)"),
        Request::SetMouseSleep { minutes } => {
            s.applied
                .get_or_insert_with(MouseSettings::default)
                .sleep_minutes = Some(minutes);
            eprintln!(">>   (would also commit to onboard memory)");
        }
        Request::SetMouseRadio { radio, save } => {
            s.applied.get_or_insert_with(MouseSettings::default).radio = Some(radio);
            note_save(save);
        }
        other => return error(format!("{} is not a mouse request", method_name(&other))),
    }
    ResponseData::Success
}

/// Why the daemon would refuse `req` before asking the mouse anything, or `None`.
fn refusal(req: &Request, experimental: &Experimental) -> Option<String> {
    match req {
        Request::SetMouseSleep { minutes } => {
            experimental.refusal(Feature::MousePower).or_else(|| {
                (!MOUSE_SLEEP_MINUTES.contains(minutes))
                    .then(|| format!("the sleep timer takes 1 to 20 minutes, not {minutes}"))
            })
        }
        Request::SetMouseRadio { .. } => experimental.refusal(Feature::MousePower),
        Request::SetMouseButtons { actions, .. } => buttons_refusal(actions, experimental),
        _ => None,
    }
}

/// The daemon's checks of a buttons request: six names, or eight with the wheel's two; the
/// wheel only with actions a capture shows, and other than the factory pair only with its
/// gate on.
fn buttons_refusal(actions: &[String], experimental: &Experimental) -> Option<String> {
    let wheel = match actions.len() {
        BUTTON_COUNT => return None,
        n if n == BUTTON_COUNT + 2 => &actions[BUTTON_COUNT..],
        n => {
            return Some(format!(
                "expected 6 button actions, or 8 with the wheel's two, got {n}"
            ))
        }
    };
    let mut picked = Vec::new();
    for name in wheel {
        match ButtonAction::parse(name) {
            Ok(action) if proto::wheel_takes(action) => picked.push(action),
            _ => return Some(format!("the wheel doesn't take “{name}”")),
        }
    }
    if picked == DEFAULT_WHEEL {
        return None;
    }
    experimental.refusal(Feature::MouseWheel)
}

/// The buttons as the daemon records them: the wheel's two only once they've gone out
/// (gate on), else the ones recorded before, if any.
fn recorded(before: Option<&[String]>, actions: &[String], wheel_on: bool) -> Vec<String> {
    let mut names = actions[..BUTTON_COUNT.min(actions.len())].to_vec();
    if wheel_on && actions.len() == BUTTON_COUNT + 2 {
        names.extend_from_slice(&actions[BUTTON_COUNT..]);
    } else if let Some(old) = before.filter(|b| b.len() == BUTTON_COUNT + 2) {
        names.extend_from_slice(&old[BUTTON_COUNT..]);
    }
    names
}

fn connected(s: &Fake) -> bool {
    s.devices.iter().any(|d| d.kind == DeviceKind::Mouse)
}

fn note_save(save: bool) {
    if save {
        eprintln!(">>   (would also commit to onboard memory)");
    }
}

fn mouse_state(s: &Fake) -> MouseState {
    // Read only with the gate on, and dropped while the mouse sleeps, as the daemon does.
    let read = s.experimental.on(Feature::MousePower) && s.battery.is_some();
    let applied = s.applied.as_ref();
    MouseState {
        connected: connected(s),
        battery: s.battery,
        applied: s.applied.clone(),
        // The fake mouse "reports" what was last written, GG's defaults before that.
        sleep_secs: read.then(|| {
            let minutes = applied.and_then(|a| a.sleep_minutes).unwrap_or(5);
            u16::from(minutes) * 60
        }),
        radio: read.then(|| applied.and_then(|a| a.radio).unwrap_or(MouseRadio::Normal)),
        reported_dpi: read.then(|| {
            applied.and_then(|a| a.dpi.clone()).unwrap_or(MouseDpi {
                stages: [400, 800, 1600, 2400, 3200]
                    .map(|d| DpiStage { x: d, y: d })
                    .to_vec(),
                active: 2,
            })
        }),
        reported_polling_hz: read.then(|| applied.and_then(|a| a.polling_hz).unwrap_or(1000)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    const SIX: [&str; 6] = ["button1", "button2", "button3", "button4", "button5", "dpi"];

    #[test]
    fn the_wheel_is_checked_as_the_daemon_checks_it() {
        let off = Experimental::default();
        let mut on = off;
        on.set(Feature::MouseWheel, true);
        let eight = |up: &str, down: &str| {
            let mut n = names(&SIX);
            n.extend(names(&[up, down]));
            n
        };
        assert_eq!(buttons_refusal(&names(&SIX), &off), None);
        assert_eq!(
            buttons_refusal(&eight("scrollup", "scrolldown"), &off),
            None
        );
        let gated = buttons_refusal(&eight("button4", "scrolldown"), &off).unwrap();
        assert!(gated.contains("scroll wheel"), "{gated}");
        assert_eq!(buttons_refusal(&eight("button4", "scrolldown"), &on), None);
        // Any button name goes on the wheel; a name no button takes doesn't.
        for name in ["f5", "dpi", "button6", "volumeup", "disable"] {
            assert_eq!(buttons_refusal(&eight(name, "scrolldown"), &on), None, "{name}");
        }
        let unknown = buttons_refusal(&eight("f99", "scrolldown"), &on).unwrap();
        assert!(unknown.contains("the wheel doesn't take"), "{unknown}");
        assert!(buttons_refusal(&names(&SIX[..5]), &on).is_some());

        // Recorded as the daemon records them.
        let first = recorded(None, &eight("button4", "scrolldown"), true);
        assert_eq!(first[6..], ["button4", "scrolldown"]);
        let again = recorded(Some(first.as_slice()), &names(&SIX), true);
        assert_eq!(again, first, "six buttons keep the wheel's record");
        assert_eq!(
            recorded(None, &eight("scrollup", "scrolldown"), false).len(),
            6
        );
    }
}
