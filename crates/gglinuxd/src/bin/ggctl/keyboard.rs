// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl keyboard` and `ggctl switches`: the keyboard as the daemon has it, and its
//! switch settings.

use anyhow::{bail, Context, Result};
use gg_ipc::{KeyboardState, Request, Switches};

use crate::Action;

/// What `ggctl keyboard` prints: whether it's there, its switches and its screen.
pub fn print(state: &KeyboardState) {
    let mm = |t: u8| format!("{:.1} mm", f32::from(t) / 10.0);
    println!("connected: {}", if state.connected { "yes" } else { "no" });
    let s = &state.switches;
    if s.enabled {
        let own = if s.keys.is_empty() {
            String::new()
        } else {
            format!(" ({} keys with their own)", s.keys.len())
        };
        let rt = &s.rapid_trigger;
        let rapid = if rt.enabled {
            let keys = if rt.keys.is_empty() {
                "every key".to_owned()
            } else {
                format!("{} keys", rt.keys.len())
            };
            format!("on, {}, {keys}", mm(rt.sensitivity))
        } else {
            "off".to_owned()
        };
        let tap = if s.rapid_tap.enabled {
            format!("on, {} pairs", s.rapid_tap.pairs.len())
        } else {
            "off".to_owned()
        };
        let protection = match (s.protection.enabled, s.protection.keys.len()) {
            (false, _) => "off".to_owned(),
            (true, 0) => "on, every key".to_owned(),
            (true, n) => format!("on, {n} keys"),
        };
        println!(
            "switches:  actuation {}{own}; Rapid Trigger {rapid}; Rapid Tap {tap}; Protection Mode {protection}",
            mm(s.actuation)
        );
    } else {
        println!("switches:  left to the keyboard");
    }
    if let Some(e) = &state.switches_error {
        println!("           the keyboard refused them: {e}");
    }
    crate::screen::print(state);
}

/// One change to the switch settings.
#[derive(Debug, PartialEq)]
pub enum SwitchEdit {
    Enable(bool),
    /// Every key's actuation point, in tenths of a millimetre; per-key points are dropped.
    Actuation(u8),
    RapidTrigger {
        on: bool,
        sensitivity: Option<u8>,
    },
    /// Protection Mode, on every adjustable key.
    Protection(bool),
    /// The same settings off and on again, so the daemon sends all five reports
    /// ([`resend`]); changes nothing in them.
    Resend,
}

impl SwitchEdit {
    pub fn apply(self, s: &mut Switches) {
        match self {
            SwitchEdit::Enable(on) => s.enabled = on,
            SwitchEdit::Resend => {}
            SwitchEdit::Actuation(tenths) => {
                s.enabled = true;
                s.actuation = tenths;
                s.keys.clear();
            }
            SwitchEdit::Protection(on) => {
                s.enabled |= on;
                s.protection.enabled = on;
                s.protection.keys.clear();
            }
            SwitchEdit::RapidTrigger { on, sensitivity } => {
                s.enabled |= on;
                s.rapid_trigger.enabled = on;
                if let Some(d) = sensitivity {
                    s.rapid_trigger.sensitivity = d;
                }
            }
        }
    }
}

/// `1.5` (millimetres) as tenths of a millimetre.
fn tenths(mm: &str) -> Result<u8> {
    let v: f32 = mm
        .parse()
        .with_context(|| format!("'{mm}' is not a distance in mm"))?;
    let t = (v * 10.0).round();
    if !(1.0..=40.0).contains(&t) {
        bail!("{mm} mm is out of range (0.1 to 4.0)");
    }
    Ok(t as u8)
}

/// `ggctl switches resend`: gglinux's switch settings `on`, sent off and then on again
/// through `set`, unchanged. A change that turns them on makes the daemon send all five
/// switch reports, so this puts gglinux's settings back on a keyboard that may hold
/// others now (a config picked on the keyboard itself, or GG).
pub fn resend(on: Switches, set: impl Fn(Switches) -> Result<()>) -> Result<()> {
    if !on.enabled {
        bail!(
            "gglinux's switch settings are off, so the keyboard keeps its own: there's nothing \
             to send again (`ggctl switches on` sends gglinux's)"
        );
    }
    set(Switches {
        enabled: false,
        ..on.clone()
    })?;
    set(on).context(
        "gglinux's switch settings were turned off but not on again: run `ggctl switches on`",
    )?;
    println!("ok: all five switch reports go to the keyboard again");
    Ok(())
}

pub fn switches_action(rest: &[&str]) -> Result<Action> {
    Ok(match rest {
        [] => Action::Send(Request::GetKeyboardState),
        ["on"] => Action::EditSwitches(SwitchEdit::Enable(true)),
        ["off"] => Action::EditSwitches(SwitchEdit::Enable(false)),
        ["resend"] => Action::EditSwitches(SwitchEdit::Resend),
        ["actuation", mm] => Action::EditSwitches(SwitchEdit::Actuation(tenths(mm)?)),
        ["protection", "on"] => Action::EditSwitches(SwitchEdit::Protection(true)),
        ["protection", "off"] => Action::EditSwitches(SwitchEdit::Protection(false)),
        ["rapid-trigger", "on"] => Action::EditSwitches(SwitchEdit::RapidTrigger {
            on: true,
            sensitivity: None,
        }),
        ["rapid-trigger", "on", mm] => {
            let d = tenths(mm)?;
            if d > 20 {
                bail!("the Rapid Trigger distance goes up to 2.0 mm");
            }
            Action::EditSwitches(SwitchEdit::RapidTrigger {
                on: true,
                sensitivity: Some(d),
            })
        }
        ["rapid-trigger", "off"] => Action::EditSwitches(SwitchEdit::RapidTrigger {
            on: false,
            sensitivity: None,
        }),
        _ => bail!(
            "usage: ggctl switches on|off|resend | actuation 1.5 | rapid-trigger on [0.2]|off | protection on|off"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{args, parse, sends};

    fn edits(line: &str) -> SwitchEdit {
        match parse(&args(line)).unwrap() {
            Action::EditSwitches(e) => e,
            _ => panic!("{line} is not a switch edit"),
        }
    }

    #[test]
    fn switch_commands_edit_what_the_daemon_has() {
        assert_eq!(edits("switches on"), SwitchEdit::Enable(true));
        assert_eq!(
            edits("switches protection on"),
            SwitchEdit::Protection(true)
        );
        let mut s = Switches::default();
        s.protection.keys.push(0x04);
        SwitchEdit::Protection(true).apply(&mut s);
        assert!(s.enabled && s.protection.enabled && s.protection.keys.is_empty());
        SwitchEdit::Protection(false).apply(&mut s);
        assert!(s.enabled && !s.protection.enabled);
        assert!(parse(&args("switches protection maybe")).is_err());
        assert_eq!(edits("switches actuation 1.5"), SwitchEdit::Actuation(15));
        assert_eq!(
            edits("switches rapid-trigger on 0.2"),
            SwitchEdit::RapidTrigger {
                on: true,
                sensitivity: Some(2)
            }
        );
        assert!(parse(&args("switches actuation 4.5")).is_err());
        assert!(parse(&args("switches rapid-trigger on 2.5")).is_err());
        assert!(matches!(sends("switches"), Request::GetKeyboardState));

        let mut s = Switches {
            keys: vec![gg_ipc::KeyActuation {
                code: 0x04,
                actuation: 3,
            }],
            ..Switches::default()
        };
        SwitchEdit::Actuation(20).apply(&mut s);
        assert!(s.enabled && s.actuation == 20 && s.keys.is_empty());
        SwitchEdit::RapidTrigger {
            on: true,
            sensitivity: Some(5),
        }
        .apply(&mut s);
        assert!(s.rapid_trigger.enabled && s.rapid_trigger.sensitivity == 5);
    }

    #[test]
    fn resend_turns_the_switch_settings_off_and_on_again_unchanged() {
        assert_eq!(edits("switches resend"), SwitchEdit::Resend);
        let on = Switches {
            enabled: true,
            actuation: 18,
            ..Switches::default()
        };
        let mut s = on.clone();
        SwitchEdit::Resend.apply(&mut s);
        assert_eq!(s, on, "nothing changed in them");

        // Off and then on again, the same settings: the daemon sends all five.
        let sent = std::cell::RefCell::new(Vec::new());
        let record = |s: Switches| {
            sent.borrow_mut().push(s);
            Ok(())
        };
        resend(on.clone(), record).unwrap();
        let off = Switches {
            enabled: false,
            ..on.clone()
        };
        assert_eq!(*sent.borrow(), [off, on.clone()]);

        // Off: the keyboard runs its own, nothing to send again and nothing sent.
        sent.borrow_mut().clear();
        let e = resend(Switches::default(), record).unwrap_err();
        assert!(e.to_string().contains("ggctl switches on"), "{e}");
        assert!(sent.borrow().is_empty());

        // Turned off but not on again: it says how to get them back.
        let e = resend(on, |s| {
            if s.enabled {
                bail!("the daemon went away")
            }
            Ok(())
        })
        .unwrap_err();
        let e = format!("{e:#}");
        assert!(
            e.contains("turned off but not on again") && e.contains("ggctl switches on"),
            "{e}"
        );
    }
}
