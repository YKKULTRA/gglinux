// SPDX-License-Identifier: GPL-3.0-or-later
//! The Rival 3 Wireless Gen 2 panel on the Engine page: DPI stages, polling
//! rate, buttons and the onboard save.

use std::cell::RefCell;
use std::rc::Rc;

use gg_ipc::{DpiStage, Request};
use slint::{ComponentHandle, SharedString};

use crate::client::{Client, State, TIMEOUT};
use crate::engine::ints;
use crate::{act, sync_rows, AppWindow, Forms, MouseUi};

/// Mirrors gg-device-mouse: DPI_MIN, DPI_MAX, DPI_STEP, MAX_PRESETS.
const DPI_MIN: i32 = 100;
const DPI_MAX: i32 = 18000;
const DPI_STEP: i32 = 100;
const MAX_STAGES: usize = 5;

/// The `ButtonAction::parse` names offered, in the menu's order (`MouseUi.actions`).
pub const ACTIONS: [&str; 15] = [
    "button1",
    "button2",
    "button3",
    "button4",
    "button5",
    "dpi",
    "scrollup",
    "scrolldown",
    "playpause",
    "next",
    "previous",
    "mute",
    "volumeup",
    "volumedown",
    "disable",
];
/// proto::default_buttons(): each button itself, the sixth cycling DPI.
const FACTORY_BUTTONS: [&str; 6] = ["button1", "button2", "button3", "button4", "button5", "dpi"];

/// What will be written: the panel's working copy. Seeded once from what the
/// daemon applied, then it's the user's, and a refresh never overwrites it.
/// A daemon that went away takes it with it ([`MouseForm::default`] again), as
/// what it applied went with it.
#[derive(Debug, Clone, PartialEq)]
pub struct MouseForm {
    /// As typed: snapped to the mouse's steps once typing is done, and before
    /// they're sent.
    pub stages: Vec<i32>,
    pub active: usize,
    pub polling: u16,
    /// One `ButtonAction::parse` name per button: one of [`ACTIONS`], or a key
    /// set elsewhere (ggctl), kept as it is so applying doesn't replace it.
    pub buttons: Vec<String>,
    seeded: bool,
}

impl Default for MouseForm {
    fn default() -> Self {
        MouseForm {
            stages: vec![800, 1600],
            active: 0,
            polling: 1000,
            buttons: FACTORY_BUTTONS.map(String::from).to_vec(),
            seeded: false,
        }
    }
}

/// A DPI the mouse takes: in range, on a step.
fn snap(dpi: i32) -> i32 {
    (((dpi as f32) / DPI_STEP as f32).round() as i32 * DPI_STEP).clamp(DPI_MIN, DPI_MAX)
}

impl MouseForm {
    /// Keep the form in step with the daemon: seeded once from what it applied,
    /// and started over while there's no state (not read yet, or the daemon went
    /// away, and what it applied went with it), as the old panel did.
    pub fn follow(&mut self, mouse: Option<&gg_ipc::MouseState>) {
        match mouse {
            None => *self = MouseForm::default(),
            Some(m) => {
                if let Some(a) = &m.applied {
                    self.seed(a);
                }
            }
        }
    }

    pub fn seed(&mut self, applied: &gg_ipc::MouseSettings) {
        if self.seeded {
            return;
        }
        self.seeded = true;
        if let Some(dpi) = &applied.dpi {
            self.stages = dpi.stages.iter().map(|s| s.x as i32).collect();
            self.active = dpi.active.min(self.stages.len().saturating_sub(1));
        }
        if let Some(hz) = applied.polling_hz {
            self.polling = hz;
        }
        if let Some(buttons) = &applied.buttons {
            self.buttons = FACTORY_BUTTONS
                .iter()
                .enumerate()
                .map(|(i, factory)| buttons.get(i).map_or(*factory, String::as_str).to_owned())
                .collect();
        }
    }

    /// A stage while it's typed: only kept in range, so the box isn't fought.
    pub fn edit_stage(&mut self, i: usize, dpi: i32) {
        if let Some(s) = self.stages.get_mut(i) {
            *s = dpi.clamp(DPI_MIN, DPI_MAX);
        }
    }

    /// A stage once typed: snapped to what the mouse takes.
    pub fn set_stage(&mut self, i: usize, dpi: i32) {
        if let Some(s) = self.stages.get_mut(i) {
            *s = snap(dpi);
        }
    }

    pub fn add_stage(&mut self) {
        if self.stages.len() < MAX_STAGES {
            self.stages.push(*self.stages.last().unwrap_or(&800));
        }
    }

    pub fn remove_stage(&mut self, i: usize) {
        if self.stages.len() <= 1 || i >= self.stages.len() {
            return;
        }
        self.stages.remove(i);
        // The picked stage moves down a place when one before it goes, and the
        // daemon refuses a pick past the end.
        if i < self.active {
            self.active -= 1;
        } else if self.active >= self.stages.len() {
            self.active = self.stages.len() - 1;
        }
    }

    pub fn pick_stage(&mut self, i: usize) {
        self.active = i.min(self.stages.len().saturating_sub(1));
    }

    pub fn pick_button(&mut self, i: usize, action: usize) {
        if let (Some(b), Some(a)) = (self.buttons.get_mut(i), ACTIONS.get(action)) {
            *b = (*a).to_owned();
        }
    }

    pub fn factory_buttons(&mut self) {
        self.buttons = FACTORY_BUTTONS.map(String::from).to_vec();
    }

    /// Each button's place in the menu, or -1 for a key the menu doesn't offer.
    pub fn button_menu(&self) -> Vec<i32> {
        self.buttons
            .iter()
            .map(|b| action_index(b).map_or(-1, |i| i as i32))
            .collect()
    }

    /// The DPI request; the stages are snapped first, so what's shown is what's sent.
    pub fn dpi_request(&mut self, save: bool) -> Request {
        for s in &mut self.stages {
            *s = snap(*s);
        }
        Request::SetMouseDpi {
            stages: self
                .stages
                .iter()
                .map(|x| DpiStage {
                    x: *x as u16,
                    y: *x as u16,
                })
                .collect(),
            active: self.active,
            save,
        }
    }

    pub fn polling_request(&mut self, save: bool) -> Request {
        Request::SetMousePolling {
            hz: self.polling,
            save,
        }
    }

    pub fn buttons_request(&mut self, save: bool) -> Request {
        Request::SetMouseButtons {
            actions: self.buttons.clone(),
            save,
        }
    }
}

/// Where a button name is in the menu. The daemon reads names without regard
/// to case, and takes "disabled" as well.
fn action_index(name: &str) -> Option<usize> {
    let name = name.trim().to_ascii_lowercase();
    let name = if name == "disabled" { "disable" } else { &name };
    ACTIONS.iter().position(|a| *a == name)
}

/// Only the parts written: the others aren't known, so they aren't shown.
pub fn summarise(a: &gg_ipc::MouseSettings) -> String {
    let mut parts = Vec::new();
    if let Some(dpi) = &a.dpi {
        let list: Vec<String> = dpi.stages.iter().map(|s| s.x.to_string()).collect();
        parts.push(format!(
            "{} dpi · stage {}",
            list.join(" / "),
            dpi.active + 1
        ));
    }
    if let Some(hz) = a.polling_hz {
        parts.push(format!("{hz} Hz"));
    }
    if a.buttons.is_some() {
        parts.push("button layout".into());
    }
    parts.join(" · ")
}

fn show_mouse_form(ui: &AppWindow, f: &MouseForm) {
    let g = ui.global::<MouseUi>();
    sync_rows(g.get_stages(), f.stages.clone(), |m| g.set_stages(m));
    g.set_active(f.active as i32);
    g.set_polling(f.polling as i32);
    g.set_buttons(ints(f.button_menu()));
    let keys: Vec<SharedString> = f
        .buttons
        .iter()
        .map(|b| match action_index(b) {
            Some(_) => SharedString::new(),
            None => b.as_str().into(),
        })
        .collect();
    sync_rows(g.get_button_keys(), keys, |m| g.set_button_keys(m));
}

pub fn show_mouse(ui: &AppWindow, s: &State, form: &RefCell<MouseForm>) {
    let g = ui.global::<MouseUi>();
    g.set_known(s.mouse.is_some());
    form.borrow_mut().follow(s.mouse.as_ref());
    let Some(m) = s.mouse.as_ref() else {
        // Onboard writes start off again too: they can't be undone.
        g.set_persist(false);
        g.set_connected(false);
        g.set_battery(-1);
        g.set_charging(false);
        g.set_applied(SharedString::new());
        show_mouse_form(ui, &form.borrow());
        return;
    };
    g.set_connected(m.connected);
    g.set_battery(m.battery.map_or(-1, |b| b.level as i32));
    g.set_charging(m.battery.is_some_and(|b| b.charging));
    g.set_applied(m.applied.as_ref().map(summarise).unwrap_or_default().into());
    show_mouse_form(ui, &form.borrow());
}

pub fn bind_mouse(ui: &AppWindow, client: &Client, form: Rc<RefCell<MouseForm>>) {
    let g = ui.global::<MouseUi>();
    let edit = |f: fn(&mut MouseForm, i32, i32)| {
        let weak = ui.as_weak();
        let form = form.clone();
        move |a: i32, b: i32| {
            f(&mut form.borrow_mut(), a, b);
            if let Some(ui) = weak.upgrade() {
                show_mouse_form(&ui, &form.borrow());
            }
        }
    };
    g.on_edit_stage(edit(|f, i, v| f.edit_stage(i as usize, v)));
    g.on_set_stage(edit(|f, i, v| f.set_stage(i as usize, v)));
    let add = edit(|f, _, _| f.add_stage());
    g.on_add_stage(move || add(0, 0));
    let remove = edit(|f, i, _| f.remove_stage(i as usize));
    g.on_remove_stage(move |i| remove(i, 0));
    let pick = edit(|f, i, _| f.pick_stage(i as usize));
    g.on_pick_stage(move |i| pick(i, 0));
    let polling = edit(|f, hz, _| f.polling = hz as u16);
    g.on_pick_polling(move |hz| polling(hz, 0));
    g.on_pick_button(edit(|f, i, a| f.pick_button(i as usize, a as usize)));
    let factory = edit(|f, _, _| f.factory_buttons());
    g.on_factory_buttons(move || factory(0, 0));

    let send = |tag: &'static str, make: fn(&mut MouseForm, bool) -> Request| {
        let weak = ui.as_weak();
        let client = client.clone();
        let form = form.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let save = ui.global::<MouseUi>().get_persist();
            let req = make(&mut form.borrow_mut(), save);
            show_mouse_form(&ui, &form.borrow());
            act(&ui, &client, req, TIMEOUT, move |ui, busy, error| {
                let g = ui.global::<MouseUi>();
                g.set_busy(if busy {
                    tag.into()
                } else {
                    SharedString::new()
                });
                g.set_error(error);
            });
        }
    };
    g.on_apply_dpi(send("dpi", MouseForm::dpi_request));
    g.on_apply_polling(send("polling", MouseForm::polling_request));
    g.on_apply_buttons(send("buttons", MouseForm::buttons_request));
    g.on_save(send("save", |_, _| Request::SaveMouse));
}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.mouse.borrow_mut() = MouseForm::default();
}

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]). A
/// step with a place in a list takes it in the name (`mouse.set-stage1:3200`).
pub(crate) fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let m = ui.global::<MouseUi>();
    let indexed = |prefix: &str| {
        name.strip_prefix(prefix)
            .and_then(|i| i.parse::<i32>().ok())
    };
    if let Some(i) = indexed("mouse.set-stage") {
        m.invoke_set_stage(i, n);
    } else if let Some(i) = indexed("mouse.pick-button") {
        m.invoke_pick_button(i, n);
    } else {
        match name {
            "mouse.add-stage" => m.invoke_add_stage(),
            "mouse.remove-stage" => m.invoke_remove_stage(n),
            "mouse.pick-stage" => m.invoke_pick_stage(n),
            "mouse.pick-polling" => m.invoke_pick_polling(n),
            "mouse.factory-buttons" => m.invoke_factory_buttons(),
            "mouse.persist" => m.set_persist(n != 0),
            "mouse.apply-dpi" => m.invoke_apply_dpi(),
            "mouse.apply-polling" => m.invoke_apply_polling(),
            "mouse.apply-buttons" => m.invoke_apply_buttons(),
            "mouse.save" => m.invoke_save(),
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::{MouseDpi, MouseSettings};

    #[test]
    fn a_stage_is_left_alone_while_typed_and_snapped_once_done() {
        let mut f = MouseForm::default();
        // Typing 1250 passes through 125 on the way: snapping that to 100 would
        // fight the box.
        f.edit_stage(0, 125);
        assert_eq!(f.stages[0], 125);
        f.edit_stage(0, 1250);
        assert_eq!(f.stages[0], 1250);
        f.set_stage(0, 1250);
        assert_eq!(f.stages[0], 1300);
        f.set_stage(1, 50);
        assert_eq!(f.stages[1], DPI_MIN);
        f.set_stage(1, 99_999);
        assert_eq!(f.stages[1], DPI_MAX);
        f.edit_stage(1, 99_999);
        assert_eq!(f.stages[1], DPI_MAX, "even typed, a stage stays in range");
    }

    #[test]
    fn what_is_sent_is_snapped_and_shown_so() {
        // Apply without leaving the box: the typed number is snapped on the way out.
        let mut f = MouseForm::default();
        f.edit_stage(1, 1650);
        let Request::SetMouseDpi {
            stages,
            active,
            save,
        } = f.dpi_request(true)
        else {
            panic!("not a DPI request");
        };
        let sent: Vec<u16> = stages.iter().map(|s| s.x).collect();
        assert_eq!((sent, active, save), (vec![800, 1700], 0, true));
        assert!(stages.iter().all(|s| s.x == s.y));
        assert_eq!(f.stages, vec![800, 1700], "the form shows what was sent");
    }

    #[test]
    fn stages_come_and_go_within_bounds() {
        let mut f = MouseForm::default();
        f.set_stage(1, 3200);
        for _ in 0..10 {
            f.add_stage();
        }
        assert_eq!(f.stages, vec![800, 3200, 3200, 3200, 3200]);
        f.pick_stage(9);
        assert_eq!(f.active, 4, "a pick past the end is the last stage");
    }

    #[test]
    fn removing_a_stage_keeps_the_pick_on_the_same_stage() {
        let mut f = MouseForm {
            stages: vec![400, 800, 1600],
            active: 2,
            ..MouseForm::default()
        };
        f.remove_stage(0);
        assert_eq!((f.stages.clone(), f.active), (vec![800, 1600], 1));
        f.remove_stage(1);
        assert_eq!((f.stages.clone(), f.active), (vec![800], 0));
        f.remove_stage(0);
        assert_eq!(f.stages, vec![800], "the last stage stays");
    }

    #[test]
    fn the_form_is_seeded_once() {
        let applied = MouseSettings {
            dpi: Some(MouseDpi {
                stages: vec![DpiStage { x: 400, y: 400 }],
                active: 0,
            }),
            polling_hz: Some(500),
            buttons: Some(
                ["button2", "button1", "button3", "button4", "button5", "dpi"]
                    .map(String::from)
                    .to_vec(),
            ),
        };
        let mut f = MouseForm::default();
        f.seed(&applied);
        assert_eq!(
            (f.stages.clone(), f.polling, f.button_menu()),
            (vec![400], 500, vec![1, 0, 2, 3, 4, 5])
        );
        f.set_stage(0, 3200);
        f.seed(&applied);
        assert_eq!(
            f.stages,
            vec![3200],
            "a later refresh leaves the edit alone"
        );
        assert_eq!(
            summarise(&applied),
            "400 dpi · stage 1 · 500 Hz · button layout"
        );
    }

    #[test]
    fn a_part_never_written_keeps_the_forms_own() {
        let mut f = MouseForm::default();
        f.seed(&MouseSettings {
            polling_hz: Some(250),
            ..MouseSettings::default()
        });
        assert_eq!((f.stages.clone(), f.polling), (vec![800, 1600], 250));
        assert_eq!(f.buttons, FACTORY_BUTTONS.map(String::from).to_vec());
    }

    #[test]
    fn a_key_set_elsewhere_is_kept_until_picked_over() {
        let mut f = MouseForm::default();
        f.seed(&MouseSettings {
            buttons: Some(
                ["button1", "Button2", "f13", "button4", "disabled"]
                    .map(String::from)
                    .to_vec(),
            ),
            ..MouseSettings::default()
        });
        // Case and the "disabled" spelling are the daemon's too; a short list
        // gets the factory action for the rest.
        assert_eq!(f.button_menu(), vec![0, 1, -1, 3, 14, 5]);
        let Request::SetMouseButtons { actions, .. } = f.buttons_request(false) else {
            panic!("not a buttons request");
        };
        assert_eq!(actions[2], "f13", "applying doesn't replace it");
        f.pick_button(2, 8);
        assert_eq!(f.buttons[2], "playpause");
        f.factory_buttons();
        assert_eq!(f.button_menu(), vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_daemon_gone_takes_the_working_copy_with_it() {
        let state = |applied: Option<MouseSettings>| gg_ipc::MouseState {
            connected: true,
            battery: None,
            applied,
        };
        let first = MouseSettings {
            polling_hz: Some(500),
            ..MouseSettings::default()
        };
        let mut f = MouseForm::default();
        f.follow(Some(&state(Some(first))));
        f.add_stage();
        // A battery notification, or the answer after a write: the edit stays.
        f.follow(Some(&state(None)));
        assert_eq!((f.polling, f.stages.len()), (500, 3));
        // The daemon went away; the one that comes back applied nothing yet.
        f.follow(None);
        assert_eq!(f, MouseForm::default());
        f.follow(Some(&state(None)));
        assert_eq!(f, MouseForm::default());
        // And what it applies later seeds the form again.
        f.follow(Some(&state(Some(MouseSettings {
            polling_hz: Some(125),
            ..MouseSettings::default()
        }))));
        assert_eq!(f.polling, 125);
    }
}
