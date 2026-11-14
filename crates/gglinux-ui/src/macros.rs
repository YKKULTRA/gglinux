// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard macros, from [`State::macros`]: a panel at the end of the keyboard card. The
//! list of macros (name, keys, steps as words); picking one opens it in the editor: its
//! name, its steps as the words `ggctl macro set` takes, the keys that play it (clicked on
//! the keyboard picture) and, a row per key, how it plays it: GG's play modes, with the
//! number of times and the wait between plays where the mode takes them. "Write to slot
//! N" puts them into an onboard slot: slot 5 (the spare one) unless another is picked for
//! that one write (review R2, K2), named on the button ("Write into Config 5"), with the
//! sentence that it rewrites that slot's macros, keeps a backup and doesn't load it.
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
/// picked for the next write (`None`: slot 5). A pick lasts for one write.
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
                MacroMode::Times => format!("{key} ({}×)", b.times),
                MacroMode::WhileHeld => format!("{key} (while held)"),
                MacroMode::Toggle => format!("{key} (toggle)"),
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
/// other plays it, in the way its first other key does (once per press if none).
/// Checked as the daemon checks.
pub fn clicked(lib: &MacroLibrary, name: &str, code: u8) -> Result<MacroLibrary, String> {
    let mut new = lib.clone();
    let had = new
        .bindings
        .iter()
        .any(|b| b.code == code && b.name == name);
    new.bindings.retain(|b| b.code != code);
    if !had {
        let like = lib.bindings.iter().find(|b| b.name == name);
        new.bindings.push(MacroBinding {
            code,
            ..like
                .cloned()
                .unwrap_or_else(|| MacroBinding::new(code, name, MacroMode::Once))
        });
    }
    new.validate()?;
    Ok(new)
}

/// The keys that play `name`, in the library's order: the editor's rows.
pub fn key_rows(lib: &MacroLibrary, name: &str) -> Vec<MacroBinding> {
    lib.bindings
        .iter()
        .filter(|b| b.name == name)
        .cloned()
        .collect()
}

/// A change to one key's way of playing its macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Play {
    /// The mode, by its place in [`MacroMode::ALL`]; the numbers it doesn't use go back
    /// to GG's.
    Mode(usize),
    Times(i32),
    Delay(i32),
}

/// The library with row `row` of `name`'s keys ([`key_rows`]) changed. Checked as the
/// daemon checks: a number out of range, or one the mode doesn't take, is refused.
pub fn played(
    lib: &MacroLibrary,
    name: &str,
    row: usize,
    change: Play,
) -> Result<MacroLibrary, String> {
    let mut new = lib.clone();
    let b = new
        .bindings
        .iter_mut()
        .filter(|b| b.name == name)
        .nth(row)
        .ok_or("that key plays this macro no more")?;
    match change {
        Play::Mode(i) => b.set_mode(*MacroMode::ALL.get(i).ok_or("not a way to play it")?),
        Play::Times(n) => {
            b.times = u8::try_from(n).map_err(|_| "a macro plays 1 to 255 times".to_owned())?
        }
        Play::Delay(ms) => {
            b.repeat_delay_ms = u16::try_from(ms)
                .map_err(|_| "the wait between plays is 1 to 65535 ms".to_owned())?
        }
    }
    new.validate()?;
    Ok(new)
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

/// The slot the panel writes into: the one picked for this write, else slot 5 (the spare
/// one), never just the running one (review R2, K2).
pub fn slot(form: &MacrosForm) -> u8 {
    form.slot.unwrap_or(ONBOARD_SLOTS - 1)
}

impl MacrosForm {
    /// The slot for a write that starts now; the pick is used up, so the next write goes
    /// to slot 5 again unless another is picked.
    pub fn take_slot(&mut self) -> u8 {
        let target = slot(self);
        self.slot = None;
        target
    }
}

/// The write button's words: the target slot by its name when known ("Write into
/// Config 5"), marked when it's the one the keyboard runs.
pub fn write_label(form: &MacrosForm, onboard: Option<&OnboardState>) -> String {
    let target = slot(form);
    let name = onboard
        .and_then(|o| o.slots.iter().find(|s| s.index == target))
        .and_then(|s| s.name.clone())
        .unwrap_or_else(|| format!("slot {}", target + 1));
    let running = if onboard.and_then(|o| o.active) == Some(target) {
        " (running)"
    } else {
        ""
    };
    format!("Write into {name}{running}")
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
    }
    let rows = match &form.picked {
        Picked::Named(name) => key_rows(lib, name),
        _ => Vec::new(),
    };
    let names = rows
        .iter()
        .map(|b| SharedString::from(MacroBinding::key_name(b.code).to_uppercase()))
        .collect();
    sync_rows(g.get_key_names(), names, |m| g.set_key_names(m));
    let modes = rows
        .iter()
        .map(|b| {
            MacroMode::ALL
                .iter()
                .position(|m| *m == b.mode)
                .map_or(-1, |i| i as i32)
        })
        .collect();
    sync_rows(g.get_key_modes(), modes, |m| g.set_key_modes(m));
    let times = rows.iter().map(|b| i32::from(b.times)).collect();
    sync_rows(g.get_key_times(), times, |m| g.set_key_times(m));
    let delays = rows.iter().map(|b| i32::from(b.repeat_delay_ms)).collect();
    sync_rows(g.get_key_delays(), delays, |m| g.set_key_delays(m));
    let names: Vec<SharedString> = slot_names(s.onboard.as_ref())
        .into_iter()
        .map(SharedString::from)
        .collect();
    // A new list makes a list box pick its first entry again: only on a change.
    if g.get_slots().iter().ne(names.iter().cloned()) {
        g.set_slots(ModelRc::new(VecModel::from(names)));
    }
    g.set_slot(i32::from(slot(form)));
    g.set_write_label(write_label(form, s.onboard.as_ref()).into());
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
    let play = |change: fn(i32) -> Play| {
        let weak = ui.as_weak();
        let c = client.clone();
        move |row: i32, value: i32| {
            let Some(ui) = weak.upgrade() else { return };
            let lib = library(&c);
            let picked = crate::with_forms(|f| f.macros.borrow().picked.clone());
            let Picked::Named(name) = picked else { return };
            match played(&lib, &name, row.max(0) as usize, change(value)) {
                Ok(library) => ask(&ui, &c, Request::SetMacros { library }, TIMEOUT),
                Err(e) => ui.global::<MacrosUi>().set_error(e.into()),
            }
        }
    };
    g.on_set_mode(play(|i| Play::Mode(i.max(0) as usize)));
    g.on_set_times(play(Play::Times));
    g.on_set_delay(play(Play::Delay));
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
        let slot = crate::with_forms(|f| f.macros.borrow_mut().take_slot());
        ask(&ui, &c, Request::WriteMacros { slot }, WRITE_TIMEOUT);
        crate::refresh(&ui);
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

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]). A key row's
/// step takes the row in its name: `macros.mode0:1` (row 0 plays it a number of times),
/// `macros.times0:3`, `macros.delay0:100`.
pub fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let g = ui.global::<MacrosUi>();
    let row = |prefix: &str| {
        name.strip_prefix(prefix)
            .and_then(|r| r.parse::<i32>().ok())
    };
    if let Some(r) = row("macros.mode") {
        g.invoke_set_mode(r, n);
        return true;
    }
    if let Some(r) = row("macros.times") {
        g.invoke_set_times(r, n);
        return true;
    }
    if let Some(r) = row("macros.delay") {
        g.invoke_set_delay(r, n);
        return true;
    }
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
            bindings: vec![MacroBinding::new(0x13, "copy", MacroMode::WhileHeld)],
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
        assert_eq!(removed(&l, "copy").bindings, []);
    }

    /// GG's apex-30 from the editor: O toggles copy with 200 ms between plays, P plays it
    /// three times, 100 ms apart. A new key takes the first key's way of playing.
    #[test]
    fn each_key_row_sets_its_mode_and_the_numbers_the_mode_takes() {
        let l = played(&lib(), "copy", 0, Play::Mode(1)).unwrap();
        let l = played(&l, "copy", 0, Play::Times(3)).unwrap();
        let l = played(&l, "copy", 0, Play::Delay(100)).unwrap();
        let p = &l.bindings[0];
        assert_eq!(
            (p.mode, p.times, p.repeat_delay_ms),
            (MacroMode::Times, 3, 100)
        );
        assert_eq!(rows(&l)[0].2, "on P (3×)");
        let two = clicked(&l, "copy", 0x12).unwrap();
        assert_eq!(key_rows(&two, "copy")[1], {
            let mut o = p.clone();
            o.code = 0x12;
            o
        });
        let two = played(&two, "copy", 1, Play::Mode(3)).unwrap();
        let two = played(&two, "copy", 1, Play::Delay(200)).unwrap();
        let o = &key_rows(&two, "copy")[1];
        assert_eq!(
            (o.mode, o.times, o.repeat_delay_ms),
            (MacroMode::Toggle, 1, 200)
        );
        assert_eq!(rows(&two)[0].2, "on P (3×), O (toggle)");
        // A number the mode doesn't take, or out of range, is refused with the reason.
        for (row, change, why) in [
            (1, Play::Times(3), "takes no number of times"),
            (0, Play::Times(0), "1 to 255"),
            (0, Play::Times(256), "1 to 255"),
            (0, Play::Delay(0), "1 to 65535"),
            (0, Play::Delay(70_000), "1 to 65535"),
            (0, Play::Mode(9), "not a way"),
            (2, Play::Mode(0), "no more"),
        ] {
            let e = played(&two, "copy", row, change).unwrap_err();
            assert!(e.contains(why), "{change:?}: {e}");
        }
        // Back to once per press: GG's numbers again.
        let once = played(&two, "copy", 0, Play::Mode(0)).unwrap();
        assert_eq!(
            key_rows(&once, "copy")[0],
            MacroBinding::new(0x13, "copy", MacroMode::Once)
        );
        let held = played(&two, "copy", 1, Play::Mode(2)).unwrap();
        assert_eq!(
            key_rows(&held, "copy")[1].repeat_delay_ms,
            15,
            "while held keeps GG's"
        );
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
        assert_eq!(slot(&form), 4, "the spare slot");
        form.slot = Some(2);
        assert_eq!(slot(&form), 2, "the one picked");
    }

    /// Review R2, K2: slot 5 unless another is picked, even while slot 1 runs; a pick
    /// lasts for one write; the button names the target.
    #[test]
    fn the_panel_writes_into_slot_5_unless_another_is_picked_for_this_write() {
        let onboard = OnboardState {
            slots: (0..5)
                .map(|index| OnboardSlot {
                    index,
                    name: Some(format!("Config {}", index + 1)),
                    ..OnboardSlot::default()
                })
                .collect(),
            active: Some(0),
            ..OnboardState::default()
        };
        let mut form = MacrosForm::default();
        assert_eq!(slot(&form), 4, "not the running one");
        assert_eq!(write_label(&form, Some(&onboard)), "Write into Config 5");
        assert_eq!(write_label(&form, None), "Write into slot 5");
        form.slot = Some(0);
        assert_eq!(
            write_label(&form, Some(&onboard)),
            "Write into Config 1 (running)"
        );
        assert_eq!(form.take_slot(), 0);
        assert_eq!(slot(&form), 4, "the next write goes to slot 5 again");
        assert_eq!(form.take_slot(), 4);
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
