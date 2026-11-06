// SPDX-License-Identifier: GPL-3.0-or-later
//! The keyboard's switches: every analog key's actuation point, Rapid Trigger and Rapid
//! Tap. They live in the keyboard's RAM, so [`SwitchesState`] sends them once each time it
//! connects, and again after a change. Off by default: gglinux leaves the keyboard's own.

use std::path::{Path, PathBuf};

use gg_device_keyboard::layout::ANSI_TKL;
use gg_device_keyboard::proto::ANALOG_KEYS;
use gg_device_keyboard::Keyboard;
use gg_ipc::{Switches, TapMode};
use tracing::{info, warn};

/// The switch settings as the keyboard worker keeps them, and what sending them gave.
pub struct SwitchesState {
    switches: Switches,
    path: PathBuf,
    /// What sending the switches to the open keyboard gave; `None`: not sent to it yet.
    sent: Option<Result<(), String>>,
}

impl SwitchesState {
    /// The saved switch settings in `config_dir`.
    pub fn load(config_dir: &Path) -> SwitchesState {
        let path = config_dir.join("switches.json");
        SwitchesState {
            switches: super::load(&path, "switch"),
            path,
            sent: None,
        }
    }

    /// New switch settings, checked and saved; the loop sends them, if they're on.
    pub fn set(&mut self, switches: Switches) -> Result<(), String> {
        switches.validate().and_then(|()| check_analog(&switches))?;
        self.switches = switches;
        super::save(&self.path, &self.switches, "switch");
        self.sent = None;
        Ok(())
    }

    /// Send the switch settings once to a keyboard that hasn't had them since it connected.
    pub fn sync(&mut self, device: Option<&Keyboard>) {
        if !self.switches.enabled || self.sent.is_some() {
            return;
        }
        let Some(device) = device else {
            return;
        };
        let s = &self.switches;
        let pairs: Vec<gg_device_keyboard::TapPair> = s
            .rapid_tap
            .pairs
            .iter()
            .map(|p| gg_device_keyboard::TapPair {
                a: p.a,
                b: p.b,
                mode: device_tap_mode(p.mode),
                report_both: p.report_both,
            })
            .collect();
        let sensitivity = s.rapid_trigger.sensitivity;
        let result = device
            .set_switches(&gg_device_keyboard::Switches {
                actuation: &|code| s.actuation_of(code),
                rapid_trigger: &|code| s.rapid_trigger_on(code),
                sensitivity: &|_| sensitivity,
                rapid_tap: s.rapid_tap.enabled,
                tap_pairs: &pairs,
            })
            .map_err(|e| format!("{e:#}"));
        match &result {
            Ok(()) => info!("keyboard switch settings sent"),
            Err(e) => warn!(error = %e, "the keyboard did not take the switch settings"),
        }
        self.sent = Some(result);
    }

    /// Forget they were sent: a keyboard that (re)connects has its own.
    pub fn forget(&mut self) {
        self.sent = None;
    }

    pub fn settings(&self) -> &Switches {
        &self.switches
    }

    /// Why the open keyboard didn't take them, if it didn't.
    pub fn error(&self) -> Option<String> {
        match &self.sent {
            Some(Err(e)) => Some(e.clone()),
            _ => None,
        }
    }
}

/// Every key a switch setting names must have an adjustable switch.
fn check_analog(switches: &Switches) -> Result<(), String> {
    let named = switches
        .keys
        .iter()
        .map(|k| k.code)
        .chain(switches.rapid_trigger.keys.iter().copied())
        .chain(switches.rapid_tap.pairs.iter().flat_map(|p| [p.a, p.b]));
    for code in named {
        if !ANALOG_KEYS.contains(&code) {
            let label = ANSI_TKL
                .iter()
                .find(|k| k.0 == code)
                .map_or_else(|| format!("{code:#04x}"), |k| k.1.to_owned());
            return Err(format!("{label} has a fixed switch: it can't be adjusted"));
        }
    }
    Ok(())
}

fn device_tap_mode(mode: TapMode) -> gg_device_keyboard::TapMode {
    use gg_device_keyboard::TapMode as D;
    match mode {
        TapMode::LastWins => D::LastWins,
        TapMode::Neither => D::Neither,
        TapMode::FirstWins => D::FirstWins,
        TapMode::SecondWins => D::SecondWins,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_device_keyboard::testing::{FakeTransport, Kind};

    #[test]
    fn switch_settings_may_only_name_keys_with_adjustable_switches() {
        let mut s = Switches::default();
        s.keys.push(gg_ipc::KeyActuation {
            code: 0x1A,
            actuation: 4,
        });
        assert!(check_analog(&s).is_ok());
        s.rapid_trigger.keys.push(0x29); // Escape
        assert_eq!(
            check_analog(&s).unwrap_err(),
            "Esc has a fixed switch: it can't be adjusted"
        );
        s.rapid_trigger.keys.clear();
        s.rapid_tap.pairs.push(gg_ipc::TapPair {
            a: 0x50,
            b: 0x4F,
            mode: TapMode::LastWins,
            report_both: false,
        });
        assert!(check_analog(&s).is_err(), "the arrows are fixed too");
    }

    fn state(switches: Switches) -> SwitchesState {
        SwitchesState {
            switches,
            path: PathBuf::new(),
            sent: None,
        }
    }

    #[test]
    fn switches_that_are_on_go_to_the_keyboard_once_a_connection() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        let mut off = state(Switches::default());
        off.sync(Some(&kb));
        assert!(fake.sent().is_empty(), "off: the keyboard keeps its own");

        let mut on = state(Switches {
            enabled: true,
            actuation: 20,
            ..Switches::default()
        });
        on.sync(None);
        on.sync(Some(&kb));
        on.sync(Some(&kb));
        let ops: Vec<(Kind, u8, u8)> = fake
            .sent()
            .iter()
            .map(|s| (s.kind, s.report[1], s.report[2]))
            .collect();
        assert_eq!(
            ops,
            [
                (Kind::Feature, 0x38, 0x61),
                (Kind::Feature, 0x38, 0x62),
                (Kind::Feature, 0x38, 0x65),
                (Kind::Output, 0x38, 0x67),
                (Kind::Output, 0x38, 0x66),
            ],
            "the three tables, the pairs and Rapid Tap's switch, once"
        );
        assert_eq!(
            fake.sent()[0].report,
            gg_device_keyboard::proto::actuation_table(|_| 20),
            "every key at 2.0 mm, on the primary layer"
        );
        assert_eq!(on.error(), None);
        on.forget();
        on.sync(Some(&kb));
        assert_eq!(fake.sent().len(), 10, "sent again after a reconnect");
    }

    #[test]
    fn a_keyboard_that_refuses_them_is_reported_and_not_asked_again_until_a_change() {
        let fake = FakeTransport::new();
        fake.fail_send(1);
        let kb = fake.keyboard();
        let mut s = state(Switches {
            enabled: true,
            ..Switches::default()
        });
        s.sync(Some(&kb));
        assert!(s.error().unwrap().contains("0x38"), "{:?}", s.error());
        s.sync(Some(&kb));
        assert!(fake.sent().is_empty(), "not tried every loop");
    }
}
