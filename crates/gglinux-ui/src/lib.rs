// SPDX-License-Identifier: GPL-3.0-or-later
//! The gglinux desktop app: a native (Slint) client of `gglinuxd`.
//!
//! The window is `ui/app.slint`. Each page's state lives in a Slint global
//! (`MouseUi`, `SonarUi`, ...): this side fills it from what the daemon says
//! ([`show`]) and answers its callbacks with requests ([`wire`]). Where a panel
//! keeps a working copy (a form the user edits before applying), the copy is
//! here, in [`Forms`], so it's plain Rust and tested as such.

slint::include_modules!();

pub mod actions;
pub mod bindings;
pub mod client;
pub mod engine;
pub mod eq;
pub mod experimental;
pub mod keyboard;
pub mod mouse;
pub mod onboard;
pub mod pages;
pub mod screen;
pub mod shell;
pub mod sonar;
pub mod switches;
pub mod system;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gg_ipc::Request;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use client::{Changed, Client, State};

/// The panels' working copies: one for each module that draws a panel.
#[derive(Default)]
pub struct Forms {
    pub mouse: Rc<RefCell<mouse::MouseForm>>,
    pub headset: Rc<RefCell<engine::HeadsetForm>>,
    pub keyboard: RefCell<keyboard::KeyboardForm>,
    pub switches: RefCell<switches::SwitchesForm>,
    pub screen: RefCell<screen::ScreenForm>,
    pub bindings: RefCell<bindings::BindingsForm>,
    pub onboard: RefCell<onboard::OnboardForm>,
    pub sonar: Rc<RefCell<sonar::SonarForm>>,
    pub parts: Rc<RefCell<pages::PartsForm>>,
    pub actions: RefCell<actions::ActionsForm>,
    pub experimental: RefCell<experimental::ExperimentalForm>,
}

impl Forms {
    /// Start every working copy afresh. The old app drew its panels anew when
    /// the daemon came back, and a copy made against a daemon process that's gone
    /// would show settings the new one never applied. Each module resets its own.
    fn reset(&self) {
        mouse::reset(self);
        engine::reset(self);
        keyboard::reset(self);
        switches::reset(self);
        screen::reset(self);
        bindings::reset(self);
        onboard::reset(self);
        *self.sonar.borrow_mut() = Default::default();
        *self.parts.borrow_mut() = Default::default();
        actions::reset(self);
        experimental::reset(self);
    }
}

/// Put `rows` in a repeater's model in place, row by row, when the model is one
/// of ours: a new model makes the repeater build every row again, which ends a
/// drag, closes an open list box and drops what's being typed, each time the
/// daemon says anything.
pub(crate) fn sync_rows<T: Clone + PartialEq + 'static>(
    current: ModelRc<T>,
    rows: Vec<T>,
    set: impl FnOnce(ModelRc<T>),
) {
    let Some(model) = current.as_any().downcast_ref::<VecModel<T>>() else {
        set(ModelRc::new(VecModel::from(rows)));
        return;
    };
    for (i, row) in rows.iter().enumerate() {
        if i >= model.row_count() {
            model.push(row.clone());
        } else if model.row_data(i).as_ref() != Some(row) {
            model.set_row_data(i, row.clone());
        }
    }
    while model.row_count() > rows.len() {
        model.remove(model.row_count() - 1);
    }
}

thread_local! {
    static FORMS: RefCell<Option<Rc<Forms>>> = const { RefCell::new(None) };
    static CLIENT: RefCell<Option<Client>> = const { RefCell::new(None) };
}

/// The window's working copies (on the UI thread).
pub fn with_forms<R>(f: impl FnOnce(&Rc<Forms>) -> R) -> R {
    let forms = FORMS.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(Default::default)
            .clone()
    });
    f(&forms)
}

/// Redraw the window from the last state, after a change of its own.
pub fn refresh(ui: &AppWindow) {
    if let Some(s) = CLIENT.with(|c| c.borrow().as_ref().map(Client::state)) {
        show_all(ui, &s);
    }
}

/// Connect to the daemon and wire the window to it.
pub fn start(ui: &AppWindow) -> Client {
    let weak = ui.as_weak();
    let client = Client::start(move |what, state| {
        let _ = weak.upgrade_in_event_loop(move |ui| show(&ui, what, &state));
    });
    let forms = with_forms(Rc::clone);
    wire(ui, &client, &forms);
    CLIENT.with(|c| *c.borrow_mut() = Some(client.clone()));
    client
}

/// Answer the window's callbacks.
pub fn wire(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    engine::bind_devices(ui, client);
    mouse::bind_mouse(ui, client, forms.mouse.clone());
    engine::bind_headset(ui, client, forms.headset.clone());
    keyboard::bind(ui, client, forms);
    switches::bind(ui, client, forms);
    screen::bind(ui, client, forms);
    bindings::bind(ui, client, forms);
    onboard::bind(ui, client, forms);
    sonar::bind(ui, client, forms.sonar.clone());
    pages::bind_games(ui, client);
    pages::bind_moments(ui, client);
    pages::bind_parts(ui, client, forms.parts.clone());
    pages::bind_profiles(ui, client);
    pages::bind_settings(ui, client);
    actions::bind(ui, client, forms);
    experimental::bind(ui, client, forms);
    // Start at login and backups work without the daemon.
    pages::show_settings(ui);
}

/// Show a change in the window.
pub fn show(ui: &AppWindow, what: Changed, s: &State) {
    let forms = with_forms(Rc::clone);
    let gg = ui.global::<Gg>();
    match what {
        Changed::Connection => {
            if gg.get_connected() && !s.connected {
                forms.reset();
            }
            gg.set_connected(s.connected);
        }
        Changed::Parts => {
            let p = s.running();
            gg.set_parts(Parts {
                keyboard: p.keyboard,
                headset: p.headset,
                mouse: p.mouse,
                sonar: p.sonar,
                gamesense: s.parts.unwrap_or_default().gamesense,
                profiles: p.profiles,
                clips: p.clips,
            });
            gg.set_restarting(s.restarting);
            // A page whose part went off goes; the device panels follow the parts too.
            if !gg.invoke_shown(gg.get_page()) {
                gg.set_page(Page::Engine);
            }
            engine::show_devices(ui, s);
            pages::show_parts(ui, s, &forms.parts);
        }
        Changed::Devices => engine::show_devices(ui, s),
        Changed::Mouse => mouse::show_mouse(ui, s, &forms.mouse),
        Changed::Headset => engine::show_headset(ui, s, &forms.headset),
        // One card, three panels.
        Changed::Keyboard => {
            keyboard::show(ui, s, &forms);
            switches::show(ui, s, &forms);
            screen::show(ui, s, &forms);
            bindings::show(ui, s);
        }
        Changed::Sonar => sonar::show(ui, s, &forms.sonar),
        Changed::Gamesense => pages::show_games(ui, s),
        Changed::Profiles => pages::show_profiles(ui, s),
        Changed::Clips => pages::show_moments(ui, s),
        Changed::Experimental => {
            experimental::show(ui, s);
            // Panels whose controls are greyed while their gate is off.
            bindings::show(ui, s);
            switches::show(ui, s, &forms);
        }
        Changed::Bindings => bindings::show(ui, s),
        Changed::Onboard => onboard::show(ui, s),
        Changed::Actions => actions::show(ui, s),
    }
}

/// Show everything, as after a connect.
pub fn show_all(ui: &AppWindow, s: &State) {
    for what in Changed::ALL {
        show(ui, what, s);
    }
}

/// [`act`], then `then` on the window and its working copies if it worked.
pub fn act_then(
    ui: &AppWindow,
    client: &Client,
    req: Request,
    timeout: Duration,
    mark: impl Fn(&AppWindow, bool, SharedString) + Send + Clone + 'static,
    then: fn(&AppWindow, &Forms),
) {
    mark(ui, true, SharedString::new());
    let weak = ui.as_weak();
    client.command(req, timeout, move |result| {
        let ok = result.is_ok();
        let error = panel_error(result);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if ok {
                with_forms(|forms| then(&ui, forms));
                refresh(&ui);
            }
            mark(&ui, false, error);
        });
    });
}

/// Send a change, marking the panel busy until the answer and showing an error
/// if it failed. `mark(ui, busy, error)` sets the panel's two properties.
pub fn act(
    ui: &AppWindow,
    client: &Client,
    req: Request,
    timeout: Duration,
    mark: impl Fn(&AppWindow, bool, SharedString) + Send + Clone + 'static,
) {
    mark(ui, true, SharedString::new());
    let weak = ui.as_weak();
    client.command(req, timeout, move |result| {
        let error = panel_error(result);
        let _ = weak.upgrade_in_event_loop(move |ui| mark(&ui, false, error));
    });
}

/// The error a panel shows for a change. None when the daemon went away: the
/// window shows the offline view instead, and when the daemon is back the panel
/// is as it says, as when the old app drew its panels anew.
fn panel_error(result: Result<(), String>) -> SharedString {
    match result {
        Err(e) if !client::lost_connection(&e) => e.into(),
        _ => SharedString::new(),
    }
}

/// Run one of the window's callbacks by name, as a click would: for the `shot`
/// example's scripted steps (`global.callback[:arg]`).
#[doc(hidden)]
pub fn poke(ui: &AppWindow, step: &str) {
    let (name, arg) = step.split_once(':').unwrap_or((step, ""));
    let n: i32 = arg.parse().unwrap_or(0);
    // Each page module answers for its own steps.
    let known = engine::poke(ui, name, n)
        || mouse::poke(ui, name, n)
        || keyboard::poke(ui, name, n)
        || switches::poke(ui, name, n)
        || screen::poke(ui, name, n)
        || bindings::poke(ui, name, n)
        || onboard::poke(ui, name, n)
        || sonar::poke(ui, name, n)
        || pages::poke(ui, name, n)
        || actions::poke(ui, name, n)
        || experimental::poke(ui, name, n);
    if !known {
        match name {
            "page" => ui.global::<Gg>().set_page(match n {
                1 => Page::Sonar,
                2 => Page::Games,
                3 => Page::Moments,
                4 => Page::Settings,
                _ => Page::Engine,
            }),
            // The offline views, as if the daemon had gone (for a moment, after a
            // change of parts).
            "offline" | "restarting" => {
                let gg = ui.global::<Gg>();
                gg.set_connected(false);
                gg.set_restarting(name == "restarting");
            }
            other => eprintln!("poke: no such step {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_updated_in_place() {
        let first = ModelRc::new(VecModel::from(vec![1, 2, 3]));
        let mut replaced = None;
        sync_rows(first.clone(), vec![1, 5], |m| replaced = Some(m));
        assert!(replaced.is_none(), "the same model, so its rows stay built");
        assert_eq!(first.iter().collect::<Vec<_>>(), [1, 5]);
        sync_rows(first.clone(), vec![1, 5, 6, 7], |m| replaced = Some(m));
        assert!(replaced.is_none());
        assert_eq!(first.iter().collect::<Vec<_>>(), [1, 5, 6, 7]);
        sync_rows(ModelRc::default(), vec![9], |m| replaced = Some(m));
        assert_eq!(replaced.unwrap().iter().collect::<Vec<_>>(), [9]);
    }

    #[test]
    fn a_lost_connection_starts_every_working_copy_afresh() {
        let forms = Forms::default();
        forms.mouse.borrow_mut().add_stage();
        forms.headset.borrow_mut().pick_preset(4);
        forms.keyboard.borrow_mut().toggle_light(0x04);
        forms.switches.borrow_mut().selected.insert(0x04);
        forms.screen.borrow_mut().picked = Some(1);
        forms.reset();
        assert_eq!(*forms.mouse.borrow(), mouse::MouseForm::default());
        assert_eq!(*forms.headset.borrow(), engine::HeadsetForm::default());
        assert!(forms.keyboard.borrow().light_selected.is_empty());
        assert!(forms.switches.borrow().selected.is_empty());
        assert_eq!(forms.screen.borrow().picked, None);
    }

    #[test]
    fn a_panel_shows_errors_but_not_the_daemon_going_away() {
        assert_eq!(panel_error(Ok(())), "");
        assert_eq!(
            panel_error(Err("no mouse connected".into())),
            "no mouse connected"
        );
        assert_eq!(panel_error(Err(client::GONE.into())), "");
        assert_eq!(panel_error(Err(client::AWAY.into())), "");
    }
}
