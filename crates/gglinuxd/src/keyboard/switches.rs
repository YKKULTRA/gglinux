// SPDX-License-Identifier: GPL-3.0-or-later
//! The keyboard's switches: every analog key's actuation point, Rapid Trigger and Rapid
//! Tap. They live in the keyboard's RAM, so [`SwitchesState`] sends them once each time it
//! connects, and again after a change. Off by default: gglinux leaves the keyboard's own.
//!
//! Keys may also have a second (deep-press) actuation point. Its table goes after the
//! others, and only while the keyboard lets the bindings family out (`keyboard_bindings`
//! on); with the gate off exactly the same reports go out as without deep-press points.
//! Removing the last one sends the table once more, with none for every key, and a deep
//! press must reach the keyboard deeper than the key's first press ([`check_deeper`]).

use std::path::{Path, PathBuf};

use gg_device_keyboard::layout::US;
use gg_device_keyboard::proto::{self, ANALOG_KEYS};
use gg_device_keyboard::Keyboard;
use gg_ipc::{Switches, TapMode};
use tracing::{info, warn};

/// The switch settings as the keyboard worker keeps them, and what sending them gave.
pub struct SwitchesState {
    switches: Switches,
    path: PathBuf,
    /// What sending the switches to the open keyboard gave; `None`: not sent to it yet.
    sent: Option<Result<(), String>>,
    /// With which bindings family the open keyboard last had its deep-press points
    /// dealt with: sent (`true`), held back (`false`), or not yet.
    second: Option<bool>,
    /// Whether the keyboard let Protection Mode out when the settings last went: they go
    /// again when that changes, with or without the protected keys.
    protection: Option<bool>,
    /// The open keyboard holds deep-press points of gglinux's: a table with some went out
    /// since it connected. They stay there until a table without them replaces them.
    second_out: bool,
}

impl SwitchesState {
    /// The saved switch settings in `config_dir`.
    pub fn load(config_dir: &Path) -> SwitchesState {
        let path = config_dir.join("switches.json");
        SwitchesState {
            switches: super::load(&path, "switch"),
            path,
            sent: None,
            second: None,
            protection: None,
            second_out: false,
        }
    }

    /// New switch settings, checked and saved; the loop sends them, if they're on.
    pub fn set(&mut self, switches: Switches) -> Result<(), String> {
        switches
            .validate()
            .and_then(|()| check_analog(&switches))
            .and_then(|()| check_deeper(&switches))?;
        self.switches = switches;
        super::save(&self.path, &self.switches, "switch");
        // Sent again, to the keyboard that has the old ones (and maybe deep-press points).
        self.sent = None;
        self.second = None;
        Ok(())
    }

    /// Send the switch settings once to a keyboard that hasn't had them since it connected.
    pub fn sync(&mut self, device: Option<&Keyboard>) {
        if !self.switches.enabled {
            return;
        }
        let Some(device) = device else {
            return;
        };
        // Again when the protection gate changes, so its keys follow it at once.
        let protection = device.families().protection;
        let follows = !self.switches.protection.enabled || self.protection == Some(protection);
        if self.sent.is_none() || !follows {
            self.send(device);
        }
        self.sync_second(device);
    }

    /// The actuation points, Rapid Trigger and Rapid Tap.
    fn send(&mut self, device: &Keyboard) {
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
        // Protection Mode only while its gate lets it out; without, the keys go unprotected.
        let protection = device.families().protection;
        self.protection = Some(protection);
        let result = device
            .set_switches(&gg_device_keyboard::Switches {
                actuation: &|code| s.actuation_of(code),
                rapid_trigger: &|code| s.rapid_trigger_on(code),
                protection: &|code| protection && s.protection_on(code),
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
        self.second = None;
    }

    /// The deep-press points, once the others went through: only while the keyboard lets
    /// the bindings family out, and again when that comes on. With none left, a table of
    /// "none" for every key takes back points of gglinux's the keyboard still holds.
    fn sync_second(&mut self, device: &Keyboard) {
        let on = device.families().bindings;
        let s = &self.switches;
        if !matches!(self.sent, Some(Ok(()))) || self.second == Some(on) {
            return;
        }
        if s.second.is_empty() && !self.second_out {
            return;
        }
        self.second = Some(on);
        if !on {
            return;
        }
        let result = device
            .set_second_actuation(|code| {
                s.second
                    .iter()
                    .find(|k| k.code == code)
                    .map(|k| k.actuation)
            })
            .map_err(|e| format!("{e:#}"));
        match result {
            Ok(()) => {
                info!("keyboard deep-press points sent");
                self.second_out = !s.second.is_empty();
            }
            Err(e) => {
                warn!(error = %e, "the keyboard did not take the deep-press points");
                self.sent = Some(Err(e));
            }
        }
    }

    /// Forget they were sent: a keyboard that (re)connects, or that just loaded an
    /// onboard slot, has its own.
    pub fn forget(&mut self) {
        self.sent = None;
        self.second = None;
        self.protection = None;
        self.second_out = false;
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

/// A key's deep press must go to the keyboard deeper than its first press. The keyboard
/// gets thresholds, not tenths, and some tenths share one (0.1 and 0.2 mm; 3.8, 3.9 and
/// 4.0 mm): two points with the same one would fire together.
fn check_deeper(switches: &Switches) -> Result<(), String> {
    let mm = |tenths: u8| format!("{}.{} mm", tenths / 10, tenths % 10);
    for k in &switches.second {
        let first = switches.actuation_of(k.code);
        if proto::thresholds(k.actuation).0 <= proto::thresholds(first).0 {
            return Err(format!(
                "{}'s deep press at {} reaches the keyboard at the same depth as its first \
                 press at {}, so both would fire together. Leave more room between them: the \
                 keyboard can't tell 0.1 from 0.2 mm, or 3.8 from 3.9 or 4.0 mm",
                label(k.code),
                mm(k.actuation),
                mm(first)
            ));
        }
    }
    Ok(())
}

/// A key's label on the US board; keys with a fixed switch have the same one on every board.
fn label(code: u8) -> String {
    US.iter()
        .find(|k| k.0 == code)
        .map_or_else(|| format!("{code:#04x}"), |k| k.1.to_owned())
}

/// Every key a switch setting names must have an adjustable switch.
fn check_analog(switches: &Switches) -> Result<(), String> {
    let named = switches
        .keys
        .iter()
        .map(|k| k.code)
        .chain(switches.rapid_trigger.keys.iter().copied())
        .chain(switches.protection.keys.iter().copied())
        .chain(switches.rapid_tap.pairs.iter().flat_map(|p| [p.a, p.b]))
        .chain(switches.second.iter().map(|k| k.code));
    for code in named {
        if !ANALOG_KEYS.contains(&code) {
            return Err(format!(
                "{} has a fixed switch: it can't be adjusted",
                label(code)
            ));
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
    use crate::scratch::Scratch;
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
        s.rapid_tap.pairs.clear();
        s.second.push(gg_ipc::KeyActuation {
            code: 0x29,
            actuation: 30,
        });
        assert!(check_analog(&s).is_err(), "no deep press on Esc");
    }

    fn state(switches: Switches) -> SwitchesState {
        SwitchesState {
            switches,
            path: PathBuf::new(),
            sent: None,
            second: None,
            protection: None,
            second_out: false,
        }
    }

    /// The release-mode tables (`38 62`) among what was sent, as each key's mode.
    fn release_tables(fake: &FakeTransport) -> Vec<Vec<u8>> {
        fake.sent()
            .iter()
            .filter(|s| s.report[1..3] == [0x38, 0x62])
            .map(|s| s.report[4..4 + 68 * 2].chunks(2).map(|p| p[1]).collect())
            .collect()
    }

    #[test]
    fn protection_mode_goes_out_only_while_its_gate_is_on_and_follows_it() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        let mut s = state(Switches {
            enabled: true,
            protection: gg_ipc::Protection {
                enabled: true,
                keys: vec![0x04],
            },
            rapid_trigger: gg_ipc::RapidTrigger {
                enabled: true,
                keys: vec![0x04, 0x05],
                sensitivity: 2,
            },
            ..Switches::default()
        });
        // Gate off: A and B have Rapid Trigger, nothing is protected, nothing is refused.
        s.sync(Some(&kb));
        let tables = release_tables(&fake);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0][..3], [2, 2, 0], "A, B, C");
        assert!(matches!(s.sent, Some(Ok(()))));
        s.sync(Some(&kb));
        assert_eq!(
            release_tables(&fake).len(),
            1,
            "not sent again while nothing changed"
        );
        // Gate on: sent again at once, A now Rapid Trigger with Protection (04).
        kb.set_families(gg_device_keyboard::Families {
            protection: true,
            ..gg_device_keyboard::Families::default()
        });
        s.sync(Some(&kb));
        let tables = release_tables(&fake);
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[1][..3], [4, 2, 0]);
        s.sync(Some(&kb));
        assert_eq!(release_tables(&fake).len(), 2);
        // Gate off again: the protected key goes back to Rapid Trigger alone.
        kb.set_families(gg_device_keyboard::Families::default());
        s.sync(Some(&kb));
        assert_eq!(release_tables(&fake).last().unwrap()[..3], [2, 2, 0]);
        // Protection on a key with a fixed switch is refused like any other switch setting.
        let mut bad = Switches::default();
        bad.protection.keys.push(0x29);
        assert!(check_analog(&bad).is_err());
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

    fn deep(enabled: bool) -> Switches {
        Switches {
            enabled,
            actuation: 15,
            second: vec![gg_ipc::KeyActuation {
                code: 0x1A,
                actuation: 30,
            }],
            ..Switches::default()
        }
    }

    fn bindings_on() -> gg_device_keyboard::Families {
        gg_device_keyboard::Families {
            bindings: true,
            onboard: false,
            protection: false,
        }
    }

    #[test]
    fn deep_press_points_change_nothing_sent_while_the_gate_is_off() {
        let plain = FakeTransport::new();
        state(Switches {
            second: Vec::new(),
            ..deep(true)
        })
        .sync(Some(&plain.keyboard()));
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        let mut s = state(deep(true));
        s.sync(Some(&kb));
        s.sync(Some(&kb));
        assert_eq!(fake.sent(), plain.sent(), "byte for byte the same");
        assert_eq!(fake.sent().len(), 5);
        assert_eq!(s.error(), None);
    }

    #[test]
    fn with_the_gate_on_the_deep_press_table_follows_the_others_once_a_connection() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        kb.set_families(bindings_on());
        let mut s = state(deep(true));
        s.sync(Some(&kb));
        s.sync(Some(&kb));
        let sent = fake.sent();
        assert_eq!(sent.len(), 6);
        assert_eq!((sent[5].kind, sent[5].report[1]), (Kind::Feature, 0x38));
        assert_eq!(
            sent[5].report,
            gg_device_keyboard::bindings::second_actuation_table(|c| (c == 0x1A).then_some(30))
        );
        assert_eq!(sent[0].report[4], 0, "the primary table first");
        s.forget();
        s.sync(Some(&kb));
        assert_eq!(fake.sent().len(), 12, "sent again after a reconnect");

        // Off, nothing at all; and no deep-press points, no extra table.
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        kb.set_families(bindings_on());
        state(deep(false)).sync(Some(&kb));
        state(Switches::default()).sync(Some(&kb));
        assert!(fake.sent().is_empty());
        state(Switches {
            second: Vec::new(),
            ..deep(true)
        })
        .sync(Some(&kb));
        assert_eq!(fake.sent().len(), 5);
    }

    #[test]
    fn turning_the_gate_on_sends_the_deep_press_table_alone_and_a_refusal_is_reported() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        let mut s = state(deep(true));
        s.sync(Some(&kb));
        assert_eq!(fake.sent().len(), 5);
        kb.set_families(bindings_on());
        s.sync(Some(&kb));
        s.sync(Some(&kb));
        let sent = fake.sent();
        assert_eq!(sent.len(), 6, "only the deep-press table");
        assert_eq!(sent[5].report[..5], [0x00, 0x38, 0x61, 0x44, 0x02]);

        let fake = FakeTransport::new();
        fake.fail_send(6);
        let kb = fake.keyboard();
        kb.set_families(bindings_on());
        let mut s = state(deep(true));
        s.sync(Some(&kb));
        assert!(s.error().unwrap().contains("0x38"), "{:?}", s.error());
        s.sync(Some(&kb));
        assert_eq!(fake.sent().len(), 5, "not tried every loop");
    }

    #[test]
    fn removing_the_last_deep_press_sends_one_table_with_none_for_every_key() {
        let d = Scratch::new("switches-deep-cleared");
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        kb.set_families(bindings_on());
        let mut s = SwitchesState::load(&d.0);
        let none = || Switches {
            second: Vec::new(),
            ..deep(true)
        };
        s.set(deep(true)).unwrap();
        s.sync(Some(&kb));
        assert_eq!(fake.sent().len(), 6);

        fake.clear_sent();
        s.set(none()).unwrap();
        s.sync(Some(&kb));
        s.sync(Some(&kb));
        let sent = fake.sent();
        assert_eq!(sent.len(), 6, "the switches, then one deep-press table");
        assert_eq!(
            sent[5].report,
            gg_device_keyboard::bindings::second_actuation_table(|_| None)
        );

        // Once: the keyboard has none now.
        fake.clear_sent();
        s.set(Switches {
            actuation: 16,
            ..none()
        })
        .unwrap();
        s.sync(Some(&kb));
        assert_eq!(fake.sent().len(), 5);

        // Removed while the gate is off: nothing then, the table once it's back on.
        s.set(deep(true)).unwrap();
        s.sync(Some(&kb));
        kb.set_families(gg_device_keyboard::Families::default());
        s.set(none()).unwrap();
        fake.clear_sent();
        s.sync(Some(&kb));
        assert_eq!(fake.sent().len(), 5, "the switches only");
        kb.set_families(bindings_on());
        s.sync(Some(&kb));
        s.sync(Some(&kb));
        let sent = fake.sent();
        assert_eq!(sent.len(), 6);
        assert_eq!(
            sent[5].report,
            gg_device_keyboard::bindings::second_actuation_table(|_| None)
        );

        // A keyboard that reconnects has its own: nothing to take back there.
        s.set(deep(true)).unwrap();
        s.sync(Some(&kb));
        s.forget();
        s.set(none()).unwrap();
        fake.clear_sent();
        s.sync(Some(&kb));
        assert_eq!(fake.sent().len(), 5);
    }

    #[test]
    fn a_deep_press_must_reach_the_keyboard_deeper_than_the_first_press() {
        let d = Scratch::new("switches-deeper");
        let mut s = SwitchesState::load(&d.0);
        let at = |first, deep| Switches {
            enabled: true,
            actuation: first,
            second: vec![gg_ipc::KeyActuation {
                code: 0x1A,
                actuation: deep,
            }],
            ..Switches::default()
        };
        // 3.8, 3.9 and 4.0 mm send the same threshold; so do 0.1 and 0.2 mm.
        assert_eq!(
            s.set(at(38, 39)).unwrap_err(),
            "W's deep press at 3.9 mm reaches the keyboard at the same depth as its first \
             press at 3.8 mm, so both would fire together. Leave more room between them: the \
             keyboard can't tell 0.1 from 0.2 mm, or 3.8 from 3.9 or 4.0 mm"
        );
        for (first, deep) in [(38, 40), (39, 40), (1, 2)] {
            let e = s.set(at(first, deep)).unwrap_err();
            assert!(e.contains("same depth"), "{first} {deep}: {e}");
        }
        // A key's own first point counts, not every key's.
        let mut own = at(10, 39);
        own.keys.push(gg_ipc::KeyActuation {
            code: 0x1A,
            actuation: 38,
        });
        assert!(s.set(own).unwrap_err().contains("same depth"));
        assert!(!d.0.join("switches.json").exists(), "nothing saved");
        for (first, deep) in [(37, 38), (2, 3), (15, 30), (1, 40)] {
            s.set(at(first, deep)).unwrap();
        }
    }

    #[tokio::test]
    async fn after_a_slot_load_gglinuxs_switches_go_to_the_keyboard_again() {
        use super::super::{onboard, spawn_fake, Command};
        let d = Scratch::new("switches-slot-load");
        std::fs::write(
            d.0.join("switches.json"),
            r#"{"enabled":true,"actuation":20}"#,
        )
        .unwrap();
        let fake = FakeTransport::new();
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let exp = gg_ipc::Experimental {
            keyboard_onboard: true,
            ..gg_ipc::Experimental::default()
        };
        let kb = spawn_fake(fake.keyboard(), d.0.clone(), tx, exp);
        kb.send(Command::GetState).await.unwrap();
        kb.send(Command::GetState).await.unwrap();
        assert_eq!(fake.sent().len(), 5, "sent once on connect");

        fake.clear_sent();
        let load = |slot| move |reply| Command::Onboard(onboard::Cmd::Load { slot, reply });
        kb.send(load(1)).await.unwrap();
        kb.send(Command::GetState).await.unwrap();
        let ops: Vec<(Kind, u8, u8)> = fake
            .sent()
            .iter()
            .map(|s| (s.kind, s.report[1], s.report[2]))
            .collect();
        assert_eq!(
            ops,
            [
                (Kind::Output, 0xB2, 0x01),
                (Kind::Feature, 0x38, 0x61),
                (Kind::Feature, 0x38, 0x62),
                (Kind::Feature, 0x38, 0x65),
                (Kind::Output, 0x38, 0x67),
                (Kind::Output, 0x38, 0x66),
            ],
            "the slot runs its own switches: gglinux's go again"
        );

        // A load that fails leaves the keyboard as it was: nothing goes again.
        fake.clear_sent();
        fake.fail_send(12);
        assert!(kb.send(load(2)).await.is_err());
        kb.send(Command::GetState).await.unwrap();
        assert!(fake.sent().is_empty());
        kb.send::<()>(Command::Shutdown).await.unwrap();
    }
}
