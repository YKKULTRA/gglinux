// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard macros, from [`State::macros`]: a panel at the end of the keyboard card. The
//! list of macros (name, keys, steps as words); picking one opens it in the editor: its
//! name, its steps as the words `ggctl macro set` takes, the keys that play it (clicked on
//! the keyboard picture) and whether they play it once per press or while held. "Write to
//! slot N" puts them into an onboard slot (names from [`State::onboard`], the running one
//! marked), with the sentence that it rewrites that slot's macros and keeps a backup.
//!
//! Each change sends the whole library (`SetMacros`), checked here first with the same
//! checks the daemon makes; the daemon answers with the macros as they are after it.
//! Everything is greyed, with the reason, while `keyboard_macros` is off; the write also
//! while `keyboard_onboard` is off.

use std::rc::Rc;
use std::time::Duration;

use gg_ipc::{
    Experimental, Feature, KeyboardState, Macro, MacroBinding, MacroLibrary, MacroMode,
    MacrosState, OnboardState, Request, ONBOARD_SLOTS,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::client::{lost_connection, Client, State, TIMEOUT};
use crate::keyboard::{label, SURFACE_2, TEXT, TEXT_DIM};

/// Whether a key on the picture can have a macro: the keyboard keys. Not the Fn key (it
/// keeps its job) nor the media button beside the screen (hard-wired). The daemon checks
/// each key against the keyboard's own key order too.
fn can_have(code: u8) -> bool {
    (0x04..=0xE7).contains(&code)
}
use crate::{sync_rows, AppWindow, Forms, Key, MacroRow, MacrosUi};

/// A write takes about 2 s, but the erase may take up to 6, and a failed one is written
/// back, up to three times; as ggctl waits.
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);

/// Which macro the editor has open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Picked {
    #[default]
    None,
    New,
    /// The macro with this name.
    Named(String),
}

/// What the panel keeps between the daemon's answers: which macro is open, and the slot
/// picked to write into (`None`: the running one, or slot 5).
#[derive(Debug, Default)]
pub struct MacrosForm {
    pub picked: Picked,
    pub slot: Option<u8>,
}

/// The keys that play `name`, in words; "no key plays it" for none.
fn keys_of(lib: &MacroLibrary, name: &str) -> String {
    let keys: Vec<String> = lib
        .bindings
        .iter()
        .filter(|b| b.name == name)
        .map(|b| {
            let key = MacroBinding::key_name(b.code).to_uppercase();
            match b.mode {
                MacroMode::Once => key,
                MacroMode::WhileHeld => format!("{key} (while held)"),
            }
        })
        .collect();
    if keys.is_empty() {
        "no key plays it".into()
    } else {
        format!("on {}", keys.join(", "))
    }
}

/// The list's rows.
pub fn rows(lib: &MacroLibrary) -> Vec<(String, String, String)> {
    lib.macros
        .iter()
        .map(|m| {
            (
                m.name.clone(),
                Macro::steps_text(&m.events),
                keys_of(lib, &m.name),
            )
        })
        .collect()
}

/// The library with the editor's macro saved: `name` and `steps` (as words), replacing
/// the open one (`picked`), its keys following a new name. Checked as the daemon checks.
pub fn saved(
    lib: &MacroLibrary,
    picked: &Picked,
    name: &str,
    steps: &str,
) -> Result<MacroLibrary, String> {
    let name = name.trim();
    let events = Macro::parse_steps(steps)?;
    let mut new = lib.clone();
    match picked {
        Picked::Named(old) if new.find(old).is_some() => {
            for m in new.macros.iter_mut().filter(|m| &m.name == old) {
                m.name = name.to_owned();
                m.events.clone_from(&events);
            }
            for b in new.bindings.iter_mut().filter(|b| &b.name == old) {
                b.name = name.to_owned();
            }
        }
        _ => new.macros.push(Macro {
            name: name.to_owned(),
            events,
        }),
    }
    new.validate()?;
    Ok(new)
}

/// The library without `name` and its keys.
pub fn removed(lib: &MacroLibrary, name: &str) -> MacroLibrary {
    let mut new = lib.clone();
    new.macros.retain(|m| m.name != name);
    new.bindings.retain(|b| b.name != name);
    new
}

/// A click on key `code` for macro `name`: a key that plays it types itself again; any
/// other plays it, in the way its other keys do. Checked as the daemon checks.
pub fn clicked(lib: &MacroLibrary, name: &str, code: u8) -> Result<MacroLibrary, String> {
    let mut new = lib.clone();
    let had = new
        .bindings
        .iter()
        .any(|b| b.code == code && b.name == name);
    new.bindings.retain(|b| b.code != code);
    if !had {
        new.bindings.push(MacroBinding {
            code,
            name: name.to_owned(),
            mode: mode_of(lib, name),
        });
    }
    new.validate()?;
    Ok(new)
}

/// How `name`'s keys play it: while held if any of them does.
fn mode_of(lib: &MacroLibrary, name: &str) -> MacroMode {
    if lib
        .bindings
        .iter()
        .any(|b| b.name == name && b.mode == MacroMode::WhileHeld)
    {
        MacroMode::WhileHeld
    } else {
        MacroMode::Once
    }
}

/// The library with every key of `name` playing it while held, or once per press.
pub fn with_hold(lib: &MacroLibrary, name: &str, hold: bool) -> MacroLibrary {
    let mut new = lib.clone();
    for b in new.bindings.iter_mut().filter(|b| b.name == name) {
        b.mode = if hold {
            MacroMode::WhileHeld
        } else {
            MacroMode::Once
        };
    }
    new
}

/// The five slots by name, "running" after the one the keyboard runs.
pub fn slot_names(onboard: Option<&OnboardState>) -> Vec<String> {
    (0..ONBOARD_SLOTS)
        .map(|i| {
            let s = onboard.and_then(|o| o.slots.iter().find(|s| s.index == i));
            let mut text = format!("Slot {}", i + 1);
            if let Some(name) = s.and_then(|s| s.name.as_deref()) {
                text.push_str(&format!(": {name}"));
            }
            if onboard.and_then(|o| o.active) == Some(i) {
                text.push_str(" (running)");
            }
            text
        })
        .collect()
}

/// The slot the panel writes into: the one picked, else the running one, else slot 5
/// (the spare one).
pub fn slot(form: &MacrosForm, onboard: Option<&OnboardState>) -> u8 {
    form.slot
        .or_else(|| onboard.and_then(|o| o.active))
        .unwrap_or(ONBOARD_SLOTS - 1)
}

/// Why the macros can't be changed, and why they can't be written, if not.
fn refusals(exp: Option<Experimental>) -> (Option<String>, Option<String>) {
    let exp = exp.unwrap_or_default();
    let change = exp.refusal(Feature::KeyboardMacros);
    let write = change
        .clone()
        .or_else(|| exp.refusal(Feature::KeyboardOnboard));
    (change, write)
}

/// Where the macros were last written, in words.
fn written(state: &MacrosState) -> String {
    match state.written_slot {
        Some(s) => format!("Last written into slot {}.", u16::from(s) + 1),
        None => "Not written into a slot yet.".into(),
    }
}

/// The keyboard picture for macro `name`: its keys outlined, other macros' keys named,
/// keys that can't have a macro grey.
fn board(kb: &KeyboardState, lib: &MacroLibrary, name: &str, gated: bool) -> Vec<Key> {
    kb.layout
        .iter()
        .map(|k| {
            let can = can_have(k.code);
            let bound = lib.bindings.iter().find(|b| b.code == k.code);
            Key {
                x: k.x,
                y: k.y,
                w: k.w,
                label: k.label.clone().into(),
                sub: bound.map(|b| short(&b.name)).unwrap_or_default().into(),
                fill: SURFACE_2,
                ink: if can && gated { TEXT } else { TEXT_DIM },
                selected: false,
                fixed: !can,
                own: bound.is_some_and(|b| b.name == name),
            }
        })
        .collect()
}

/// A macro's name, cut to fit under a key.
fn short(name: &str) -> String {
    let mut s: String = name.chars().take(6).collect();
    if name.chars().count() > 6 {
        s.push('…');
    }
    s
}

/// `text` with its first letter made a capital, to stand alone as a sentence.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Show the daemon's macros.
pub fn show(ui: &AppWindow, s: &State) {
    let g = ui.global::<MacrosUi>();
    let Some(state) = s.macros.as_ref() else {
        g.set_shown(false);
        return;
    };
    crate::with_forms(|forms| draw(ui, s, state, &mut forms.macros.borrow_mut()));
}

fn draw(ui: &AppWindow, s: &State, state: &MacrosState, form: &mut MacrosForm) {
    let g = ui.global::<MacrosUi>();
    let (change, write) = refusals(s.experimental);
    g.set_shown(true);
    g.set_gated(change.is_none());
    g.set_refusal(change.unwrap_or_default().into());
    g.set_can_write(write.is_none());
    g.set_write_refusal(write.unwrap_or_default().into());
    let lib = &state.library;
    let rows: Vec<MacroRow> = rows(lib)
        .into_iter()
        .map(|(name, steps, keys)| MacroRow {
            name: name.into(),
            steps: steps.into(),
            keys: keys.into(),
        })
        .collect();
    sync_rows(g.get_rows(), rows, |m| g.set_rows(m));
    // A macro removed (here or elsewhere) closes; a renamed one is found by its row.
    if let Picked::Named(name) = &form.picked {
        if lib.find(name).is_none() {
            form.picked = Picked::None;
        }
    }
    let picked = match &form.picked {
        Picked::None => -2,
        Picked::New => -1,
        Picked::Named(name) => lib
            .macros
            .iter()
            .position(|m| &m.name == name)
            .map_or(-2, |i| i as i32),
    };
    g.set_picked(picked);
    if let (Picked::Named(name), Some(kb)) = (&form.picked, s.keyboard.as_ref()) {
        let keys = board(kb, lib, name, g.get_gated());
        sync_rows(g.get_keys(), keys, |m| g.set_keys(m));
        g.set_hold(mode_of(lib, name) == MacroMode::WhileHeld);
    }
    let names: Vec<SharedString> = slot_names(s.onboard.as_ref())
        .into_iter()
        .map(SharedString::from)
        .collect();
    // A new list makes a list box pick its first entry again: only on a change.
    if g.get_slots().iter().ne(names.iter().cloned()) {
        g.set_slots(ModelRc::new(VecModel::from(names)));
    }
    g.set_slot(i32::from(slot(form, s.onboard.as_ref())));
    g.set_written(written(state).into());
    g.set_busy(state.busy);
    g.set_note(
        state
            .note
            .as_deref()
            .map(sentence)
            .unwrap_or_default()
            .into(),
    );
    if let Some(e) = &state.error {
        g.set_error(e.into());
    }
}

/// The macros as the daemon said last.
fn library(client: &Client) -> MacroLibrary {
    client.state().macros.map(|s| s.library).unwrap_or_default()
}

/// Open `picked` in the editor: its name and steps, or empty ones for a new macro.
fn open(ui: &AppWindow, lib: &MacroLibrary, picked: Picked) {
    let g = ui.global::<MacrosUi>();
    let (name, steps) = match &picked {
        Picked::Named(n) => lib
            .find(n)
            .map(|m| (m.name.clone(), Macro::steps_text(&m.events)))
            .unwrap_or_default(),
        _ => (String::new(), String::new()),
    };
    g.set_name(name.into());
    g.set_steps(steps.into());
    g.set_error(SharedString::new());
    crate::with_forms(|forms| forms.macros.borrow_mut().picked = picked);
    crate::refresh(ui);
}

/// How a button changes the library: the new one, and which macro is open after.
type Change = fn(&AppWindow, &MacroLibrary, &Picked) -> Result<(MacroLibrary, Picked), String>;

/// Answer the panel's buttons.
pub fn bind(ui: &AppWindow, client: &Client, _forms: &Rc<Forms>) {
    let g = ui.global::<MacrosUi>();
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_pick(move |row| {
        let Some(ui) = weak.upgrade() else { return };
        let lib = library(&c);
        let picked = match row {
            -1 => Picked::New,
            r if r >= 0 => lib
                .macros
                .get(r as usize)
                .map_or(Picked::None, |m| Picked::Named(m.name.clone())),
            _ => Picked::None,
        };
        open(&ui, &lib, picked);
    });
    let edit = |make: Change| {
        let weak = ui.as_weak();
        let client = client.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let lib = library(&client);
            let picked = crate::with_forms(|f| f.macros.borrow().picked.clone());
            match make(&ui, &lib, &picked) {
                Ok((library, picked)) => {
                    crate::with_forms(|f| f.macros.borrow_mut().picked = picked);
                    ask(&ui, &client, Request::SetMacros { library }, TIMEOUT);
                }
                Err(e) => ui.global::<MacrosUi>().set_error(e.into()),
            }
        }
    };
    g.on_save(edit(|ui, lib, picked| {
        let g = ui.global::<MacrosUi>();
        let name = g.get_name().trim().to_owned();
        let new = saved(lib, picked, &name, &g.get_steps())?;
        Ok((new, Picked::Named(name)))
    }));
    g.on_remove(edit(|_, lib, picked| match picked {
        Picked::Named(name) => Ok((removed(lib, name), Picked::None)),
        _ => Err("no macro is open".into()),
    }));
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_key_clicked(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let s = c.state();
        let (Some(kb), Some(state)) = (s.keyboard.as_ref(), s.macros.as_ref()) else {
            return;
        };
        let Some(code) = kb.layout.get(i as usize).map(|k| k.code) else {
            return;
        };
        let picked = crate::with_forms(|f| f.macros.borrow().picked.clone());
        let Picked::Named(name) = picked else { return };
        if !can_have(code) {
            let key = kb.layout[i as usize].clone();
            ui.global::<MacrosUi>()
                .set_error(format!("{} can't play a macro", label(&key)).into());
            return;
        }
        match clicked(&state.library, &name, code) {
            Ok(library) => ask(&ui, &c, Request::SetMacros { library }, TIMEOUT),
            Err(e) => ui.global::<MacrosUi>().set_error(e.into()),
        }
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_hold(move |hold| {
        let Some(ui) = weak.upgrade() else { return };
        let lib = library(&c);
        let picked = crate::with_forms(|f| f.macros.borrow().picked.clone());
        if let Picked::Named(name) = picked {
            let library = with_hold(&lib, &name, hold);
            ask(&ui, &c, Request::SetMacros { library }, TIMEOUT);
        }
    });
    let weak = ui.as_weak();
    g.on_pick_slot(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        crate::with_forms(|f| f.macros.borrow_mut().slot = Some(i.clamp(0, 4) as u8));
        crate::refresh(&ui);
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_write(move || {
        let Some(ui) = weak.upgrade() else { return };
        let onboard = c.state().onboard;
        let slot = crate::with_forms(|f| slot(&f.macros.borrow(), onboard.as_ref()));
        ask(&ui, &c, Request::WriteMacros { slot }, WRITE_TIMEOUT);
    });
}

/// Send one request: the panel is busy until the answer, which brings the macros as they
/// are after it, or the error.
fn ask(ui: &AppWindow, client: &Client, req: Request, timeout: Duration) {
    let g = ui.global::<MacrosUi>();
    g.set_busy(true);
    g.set_error(SharedString::new());
    let weak = ui.as_weak();
    client.request(req, timeout, move |answer| {
        let error: SharedString = match answer {
            Err(e) if !lost_connection(&e) => e.into(),
            _ => SharedString::new(),
        };
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let g = ui.global::<MacrosUi>();
            g.set_busy(false);
            g.set_error(error);
        });
    });
}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.macros.borrow_mut() = MacrosForm::default();
}

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]).
pub fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let g = ui.global::<MacrosUi>();
    match name {
        // Open row n in the editor (-1: a new macro).
        "macros.pick" => g.invoke_pick(n),
        "macros.key" => g.invoke_key_clicked(n),
        "macros.save" => g.invoke_save(),
        "macros.slot" => g.invoke_pick_slot(n),
        "macros.write" => g.invoke_write(),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::OnboardSlot;

    fn lib() -> MacroLibrary {
        MacroLibrary {
            macros: vec![
                Macro {
                    name: "copy".into(),
                    events: Macro::parse_steps("ctrl+c").unwrap(),
                },
                Macro {
                    name: "hi".into(),
                    events: Macro::parse_steps("text:hi").unwrap(),
                },
            ],
            bindings: vec![MacroBinding {
                code: 0x13,
                name: "copy".into(),
                mode: MacroMode::WhileHeld,
            }],
        }
    }

    #[test]
    fn each_macro_shows_its_steps_as_words_and_its_keys() {
        assert_eq!(
            rows(&lib()),
            [
                ("copy".into(), "ctrl+c".into(), "on P (while held)".into()),
                ("hi".into(), "h i".into(), "no key plays it".into()),
            ]
        );
    }

    #[test]
    fn saving_the_editor_changes_or_adds_and_keys_follow_a_new_name() {
        let named = Picked::Named("copy".into());
        let new = saved(&lib(), &named, "copy it", "ctrl+insert").unwrap();
        assert_eq!(new.macros[0].name, "copy it");
        assert_eq!(Macro::steps_text(&new.macros[0].events), "ctrl+insert");
        assert_eq!(new.bindings[0].name, "copy it");
        let added = saved(&lib(), &Picked::New, "paste", "ctrl+v").unwrap();
        assert_eq!(added.macros.len(), 3);
        // Refused with the reason, as the daemon would.
        let e = saved(&lib(), &Picked::New, "hi", "a").unwrap_err();
        assert!(e.contains("two macros are called 'hi'"), "{e}");
        let e = saved(&lib(), &Picked::New, "x", "click:2").unwrap_err();
        assert!(e.contains("left mouse button"), "{e}");
    }

    #[test]
    fn a_key_click_binds_or_lets_go_and_follows_the_macros_way_of_playing() {
        let l = lib();
        // O joins copy, held like P.
        let two = clicked(&l, "copy", 0x12).unwrap();
        assert_eq!(two.bindings.len(), 2);
        assert!(two.bindings.iter().all(|b| b.mode == MacroMode::WhileHeld));
        // A third key is one too many.
        let e = clicked(&two, "hi", 0x14).unwrap_err();
        assert!(e.contains("at most 2"), "{e}");
        // P clicked again types itself.
        let one = clicked(&two, "copy", 0x13).unwrap();
        assert_eq!(one.bindings.len(), 1);
        // A key of another macro moves to this one.
        let moved = clicked(&l, "hi", 0x13).unwrap();
        assert_eq!(moved.bindings[0].name, "hi");
        assert_eq!(moved.bindings[0].mode, MacroMode::Once);
        assert_eq!(
            with_hold(&moved, "hi", true).bindings[0].mode,
            MacroMode::WhileHeld
        );
        assert_eq!(removed(&l, "copy").bindings, []);
    }

    #[test]
    fn the_slots_are_named_with_the_running_one_marked_and_slot_5_by_default() {
        let onboard = OnboardState {
            slots: vec![OnboardSlot {
                index: 0,
                name: Some("Config 1".into()),
                ..OnboardSlot::default()
            }],
            active: Some(0),
            ..OnboardState::default()
        };
        let names = slot_names(Some(&onboard));
        assert_eq!(names[0], "Slot 1: Config 1 (running)");
        assert_eq!(names[4], "Slot 5");
        assert_eq!(slot_names(None).len(), 5);
        let mut form = MacrosForm::default();
        assert_eq!(slot(&form, None), 4, "the spare slot");
        assert_eq!(slot(&form, Some(&onboard)), 0, "the running one");
        form.slot = Some(2);
        assert_eq!(slot(&form, Some(&onboard)), 2, "the one picked");
    }

    #[test]
    fn the_fn_key_and_the_media_button_cant_have_a_macro() {
        assert!(can_have(0x13) && can_have(0xE7));
        assert!(!can_have(0xF0) && !can_have(0xFB));
    }

    #[test]
    fn the_controls_wait_for_their_gates() {
        let (change, write) = refusals(None);
        assert!(change.unwrap().contains("Keyboard: macros"));
        assert!(write.unwrap().contains("Keyboard: macros"));
        let mut e = Experimental::default();
        e.set(Feature::KeyboardMacros, true);
        let (change, write) = refusals(Some(e));
        assert_eq!(change, None);
        assert!(write.unwrap().contains("onboard profile slots"));
        e.set(Feature::KeyboardOnboard, true);
        assert_eq!(refusals(Some(e)), (None, None));
    }
}
