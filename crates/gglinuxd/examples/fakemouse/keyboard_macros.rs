// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake keyboard's macros: a library in memory, two examples to start with, and five
//! slots like factory ones behind the real slot code. `write_macros` runs the daemon's
//! checked transaction with the real codec against memory, so it takes about as long as on
//! the keyboard. Refused while `keyboard_macros` (and, to write, `keyboard_onboard`) is
//! off, as the daemon refuses.
//!
//! Like a device, it lags: the first `get_macros` after a change still answers with the
//! state from before it. The change's own answer is the state after it.

use gg_device_keyboard::macros::{self as slot, Event as SlotEvent, Mode, SlotMacro};
use gg_device_keyboard::onboard::fake::FakeSlots;
use gg_device_keyboard::onboard::{Timing, Written, SLOTS};
use gg_device_keyboard::{Families, Keyboard};
use gg_ipc::{
    check_slot, Feature, Macro, MacroBinding, MacroEvent, MacroLibrary, MacroMode, MacrosState,
    Request, ResponseData,
};

use crate::log::method_name;
use crate::{error, Fake};

/// Where the made-up backups seem to be.
const BACKUPS: &str = "/home/you/.config/gglinux/onboard-backups";

/// The fake's macros: its one field in [`Fake`].
pub struct Macros {
    state: MacrosState,
    /// What the next `get_macros` answers: the state from before the last change.
    before: Option<MacrosState>,
    /// The slots, made on the first write.
    keyboard: Option<Keyboard>,
    backups: usize,
}

impl Default for Macros {
    /// Two examples, one on a key.
    fn default() -> Macros {
        let steps = |words: &str| Macro::parse_steps(words).expect("an example that parses");
        Macros {
            state: MacrosState {
                library: MacroLibrary {
                    macros: vec![
                        Macro {
                            name: "greeting".into(),
                            events: steps(r#"text:"Hello!" enter"#),
                        },
                        Macro {
                            name: "copy and paste".into(),
                            events: steps("ctrl+c wait:50 ctrl+v"),
                        },
                    ],
                    bindings: vec![MacroBinding {
                        code: 0x13,
                        name: "greeting".into(),
                        mode: MacroMode::Once,
                    }],
                },
                ..MacrosState::default()
            },
            before: None,
            keyboard: None,
            backups: 0,
        }
    }
}

/// Five slots like factory ones, behind a keyboard with the onboard family on (the gates
/// are checked before anything gets here; the allowlist is the keyboard's).
fn keyboard() -> Keyboard {
    let slots = FakeSlots::new();
    for s in 0..SLOTS {
        let name = format!("Config {}", s + 1);
        slots.set_slot(s, slot::factory_like(&name).bytes());
    }
    let keyboard = slots.keyboard();
    keyboard.set_families(Families {
        bindings: false,
        onboard: true,
        protection: false,
    });
    keyboard
}

/// The daemon's checks: the library's own, and a place for each key.
fn check(library: &MacroLibrary) -> Result<(), String> {
    library.validate()?;
    match library
        .bindings
        .iter()
        .find(|b| slot::place(b.code).is_none())
    {
        Some(b) => Err(format!(
            "key {} can't play a macro",
            MacroBinding::key_name(b.code)
        )),
        None => Ok(()),
    }
}

fn slot_macros(library: &MacroLibrary) -> Vec<SlotMacro> {
    library
        .bindings
        .iter()
        .filter_map(|b| {
            let m = library.find(&b.name)?;
            Some(SlotMacro {
                key: b.code,
                mode: match b.mode {
                    MacroMode::Once => Mode::Once,
                    MacroMode::WhileHeld => Mode::WhileHeld,
                },
                events: m
                    .events
                    .iter()
                    .map(|e| match *e {
                        MacroEvent::Key { code, down } => SlotEvent::Key { usage: code, down },
                        MacroEvent::Button { button, down } => SlotEvent::Button { button, down },
                        MacroEvent::Delay { ms } => SlotEvent::Delay { ms },
                    })
                    .collect(),
            })
        })
        .collect()
}

impl Macros {
    fn write(&mut self, slot: u8) -> Result<(), String> {
        check(&self.state.library)?;
        let macros = slot_macros(&self.state.library);
        let n = self.backups;
        let kb = self.keyboard.get_or_insert_with(keyboard);
        let mut backed_up = false;
        let result = kb.write_slot(
            slot,
            &Timing::KEYBOARD,
            |old| {
                backed_up = true;
                Ok(old.serialize())
            },
            |old| slot::write(old, &macros),
        );
        if backed_up {
            self.backups += 1;
            self.state.backup = Some(format!(
                "{BACKUPS}/slot{}-20261113T12{:02}{:02}Z.bin",
                slot + 1,
                n / 60 % 60,
                n % 60
            ));
        }
        let written = result.map_err(|e| e.to_string())?;
        self.state.written_slot = Some(slot);
        self.state.note = match written {
            Written::Unchanged => None,
            Written::Written(check) => Some(check.warning().unwrap_or_else(|| {
                format!(
                    "the keyboard plays these macros once it runs slot {}: `ggctl onboard load {}` (or the slot's Load button in the app)",
                    slot + 1,
                    slot + 1
                )
            })),
        };
        Ok(())
    }
}

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    if let Request::GetMacros = req {
        let m = &mut s.macros;
        let state = m.before.take().unwrap_or_else(|| m.state.clone());
        return ResponseData::Macros { state };
    }
    let gates: &[Feature] = match req {
        Request::WriteMacros { .. } => &[Feature::KeyboardMacros, Feature::KeyboardOnboard],
        _ => &[Feature::KeyboardMacros],
    };
    for &feature in gates {
        if let Some(why) = s.experimental.refusal(feature) {
            return error(why);
        }
    }
    let m = &mut s.macros;
    let before = m.state.clone();
    let result = match req {
        Request::SetMacros { library } => check(&library).map(|()| m.state.library = library),
        Request::WriteMacros { slot } => {
            if let Err(e) = check_slot(slot) {
                return error(e);
            }
            m.state.error = None;
            m.state.note = None;
            let result = m.write(slot);
            m.state.error = result.as_ref().err().cloned();
            result
        }
        other => Err(format!("{} is not a macros request", method_name(&other))),
    };
    match result {
        Ok(()) => {
            m.before = Some(before);
            ResponseData::Macros {
                state: m.state.clone(),
            }
        }
        Err(message) => error(message),
    }
}
