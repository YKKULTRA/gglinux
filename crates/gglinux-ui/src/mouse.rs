// SPDX-License-Identifier: GPL-3.0-or-later
//! The Rival 3 Wireless Gen 2 panel on the Engine page: DPI stages, polling
//! rate, buttons and the onboard save.

use std::cell::RefCell;
use std::rc::Rc;

use gg_ipc::mouse_keys::{self, KEYS, MEDIA};
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
/// The menu's two entries after [`ACTIONS`]: each opens a list to pick a key from.
pub const KEY_ITEM: usize = ACTIONS.len();
pub const MEDIA_ITEM: usize = ACTIONS.len() + 1;
/// proto::default_buttons(): each button itself, the sixth cycling DPI.
const FACTORY_BUTTONS: [&str; 6] = ["button1", "button2", "button3", "button4", "button5", "dpi"];

/// One DPI stage as typed: X is side to side, Y up and down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stage {
    pub x: i32,
    pub y: i32,
}

impl Stage {
    pub const fn both(dpi: i32) -> Stage {
        Stage { x: dpi, y: dpi }
    }
}

/// Which of a stage's numbers a box sets.
#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
}

/// Which list the key picker shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyList {
    Keys,
    Media,
}

impl KeyList {
    fn table(self) -> &'static [(&'static str, &'static str)] {
        match self {
            KeyList::Keys => KEYS,
            KeyList::Media => MEDIA,
        }
    }

    /// Its entry in the button menu.
    fn item(self) -> usize {
        match self {
            KeyList::Keys => KEY_ITEM,
            KeyList::Media => MEDIA_ITEM,
        }
    }
}

/// The open key list: under which button, which list, and the search typed.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyPicker {
    pub button: usize,
    pub list: KeyList,
    pub filter: String,
}

/// What will be written: the panel's working copy. Seeded once from what the
/// daemon applied, then it's the user's, and a refresh never overwrites it.
/// A daemon that went away takes it with it ([`MouseForm::default`] again), as
/// what it applied went with it.
#[derive(Debug, Clone, PartialEq)]
pub struct MouseForm {
    /// As typed: snapped to the mouse's steps once typing is done, and before
    /// they're sent.
    pub stages: Vec<Stage>,
    /// X and Y are set apart. While they're together a stage has one number,
    /// used for both.
    pub split: bool,
    pub active: usize,
    pub polling: u16,
    /// One `ButtonAction::parse` name per button: one of [`ACTIONS`], a key
    /// from [`KEYS`], or a name set elsewhere (ggctl), kept as it is so
    /// applying doesn't replace it.
    pub buttons: Vec<String>,
    /// The key list under a button, while it's open.
    pub picker: Option<KeyPicker>,
    seeded: bool,
}

impl Default for MouseForm {
    fn default() -> Self {
        MouseForm {
            stages: vec![Stage::both(800), Stage::both(1600)],
            split: false,
            active: 0,
            polling: 1000,
            buttons: FACTORY_BUTTONS.map(String::from).to_vec(),
            picker: None,
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
            self.stages = dpi
                .stages
                .iter()
                .map(|s| Stage {
                    x: s.x as i32,
                    y: s.y as i32,
                })
                .collect();
            // Shown together, a setup with different axes would be flattened.
            self.split = self.stages.iter().any(|s| s.x != s.y);
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

    /// A stage's number (both of them while X and Y are together).
    fn put(&mut self, i: usize, axis: Axis, dpi: i32) {
        let split = self.split;
        if let Some(s) = self.stages.get_mut(i) {
            match (split, axis) {
                (false, _) => *s = Stage::both(dpi),
                (true, Axis::X) => s.x = dpi,
                (true, Axis::Y) => s.y = dpi,
            }
        }
    }

    /// A stage while it's typed: only kept in range, so the box isn't fought.
    pub fn edit_stage(&mut self, i: usize, dpi: i32) {
        self.put(i, Axis::X, dpi.clamp(DPI_MIN, DPI_MAX));
    }

    /// A stage once typed: snapped to what the mouse takes.
    pub fn set_stage(&mut self, i: usize, dpi: i32) {
        self.put(i, Axis::X, snap(dpi));
    }

    pub fn edit_stage_y(&mut self, i: usize, dpi: i32) {
        self.put(i, Axis::Y, dpi.clamp(DPI_MIN, DPI_MAX));
    }

    pub fn set_stage_y(&mut self, i: usize, dpi: i32) {
        self.put(i, Axis::Y, snap(dpi));
    }

    /// X and Y apart, or together again: then each stage's Y becomes its X, so
    /// the one box shows all that's sent.
    pub fn set_split(&mut self, on: bool) {
        self.split = on;
        if !on {
            for s in &mut self.stages {
                s.y = s.x;
            }
        }
    }

    pub fn add_stage(&mut self) {
        if self.stages.len() < MAX_STAGES {
            self.stages
                .push(*self.stages.last().unwrap_or(&Stage::both(800)));
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

    /// A pick from a button's menu: an action is that button's name at once;
    /// "Keyboard key…" or "Media key…" opens that list under it instead.
    pub fn pick_button(&mut self, i: usize, action: usize) {
        if i >= self.buttons.len() {
            return;
        }
        let list = match action {
            KEY_ITEM => KeyList::Keys,
            MEDIA_ITEM => KeyList::Media,
            _ => {
                if let Some(a) = ACTIONS.get(action) {
                    self.buttons[i] = (*a).to_owned();
                    if self.picker.as_ref().is_some_and(|p| p.button == i) {
                        self.picker = None;
                    }
                }
                return;
            }
        };
        self.picker = Some(KeyPicker {
            button: i,
            list,
            filter: String::new(),
        });
    }

    pub fn factory_buttons(&mut self) {
        self.buttons = FACTORY_BUTTONS.map(String::from).to_vec();
        self.picker = None;
    }

    pub fn filter_keys(&mut self, text: &str) {
        if let Some(p) = &mut self.picker {
            p.filter = text.to_owned();
        }
    }

    pub fn close_keys(&mut self) {
        self.picker = None;
    }

    /// The open list's keys, as the search leaves them: `(name, label)`.
    pub fn key_choices(&self) -> Vec<(&'static str, &'static str)> {
        self.picker
            .as_ref()
            .map_or_else(Vec::new, |p| choices(p.list, &p.filter))
    }

    /// A key from the open list (by its place in [`MouseForm::key_choices`])
    /// becomes the button's name, and the list closes.
    pub fn pick_key(&mut self, index: usize) {
        let Some(p) = &self.picker else { return };
        let Some((name, _)) = choices(p.list, &p.filter).get(index).copied() else {
            return;
        };
        if let Some(b) = self.buttons.get_mut(p.button) {
            *b = name.to_owned();
        }
        self.picker = None;
    }

    /// Each button's place in the menu: an action, "Keyboard key…" for a key
    /// (and for the button whose list is open), or -1 for a name set elsewhere
    /// that the menu doesn't offer.
    pub fn button_menu(&self) -> Vec<i32> {
        self.buttons
            .iter()
            .enumerate()
            .map(|(i, b)| match &self.picker {
                Some(p) if p.button == i => p.list.item() as i32,
                _ => menu_index(b).map_or(-1, |i| i as i32),
            })
            .collect()
    }

    /// The DPI request; the stages are snapped first, so what's shown is what's sent.
    pub fn dpi_request(&mut self, save: bool) -> Request {
        for s in &mut self.stages {
            *s = Stage {
                x: snap(s.x),
                y: snap(s.y),
            };
        }
        Request::SetMouseDpi {
            stages: self
                .stages
                .iter()
                .map(|s| DpiStage {
                    x: s.x as u16,
                    y: s.y as u16,
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

/// Which key list a name is in (any case), if it's in one.
fn key_list(name: &str) -> Option<KeyList> {
    let name = name.trim().to_ascii_lowercase();
    [KeyList::Keys, KeyList::Media]
        .into_iter()
        .find(|l| l.table().iter().any(|(n, _)| *n == name))
}

/// Where a button name is in the menu: an action's own entry, or the entry of
/// the list its key is in.
fn menu_index(name: &str) -> Option<usize> {
    action_index(name).or_else(|| key_list(name).map(KeyList::item))
}

/// A list's keys whose label or name holds `filter` (any case): the exact
/// match first, then those starting with it (or with a word starting with
/// it), then the rest, each group in the list's order. Enter takes the first.
pub fn choices(list: KeyList, filter: &str) -> Vec<(&'static str, &'static str)> {
    let filter = filter.trim().to_lowercase();
    let mut found: Vec<(u8, (&'static str, &'static str))> = list
        .table()
        .iter()
        .filter_map(|&(name, label)| match_rank(&filter, name, label).map(|r| (r, (name, label))))
        .collect();
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().map(|(_, key)| key).collect()
}

/// How well a key matches a search, lowest best, or `None` for no match.
fn match_rank(filter: &str, name: &str, label: &str) -> Option<u8> {
    let label = label.to_lowercase();
    if filter.is_empty() || name == filter || label == filter {
        Some(0)
    } else if name.starts_with(filter) || label.split(' ').any(|w| w.starts_with(filter)) {
        Some(1)
    } else if name.contains(filter) || label.contains(filter) {
        Some(2)
    } else {
        None
    }
}

/// The line under a button: the key it's bound to, a name set elsewhere that
/// the menu doesn't know (kept as it is), or nothing for one of [`ACTIONS`].
pub fn button_text(name: &str) -> String {
    if action_index(name).is_some() {
        return String::new();
    }
    match (key_list(name), mouse_keys::label(name)) {
        (Some(KeyList::Keys), Some(label)) => format!("Key: {label}"),
        (Some(KeyList::Media), Some(label)) => format!("Media key: {label}"),
        _ => format!("Set elsewhere: {}", name.trim()),
    }
}

/// A stage as the summary shows it: one number, or X×Y where they differ.
fn stage_text(s: &DpiStage) -> String {
    if s.x == s.y {
        s.x.to_string()
    } else {
        format!("{}×{}", s.x, s.y)
    }
}

/// Only the parts written: the others aren't known, so they aren't shown.
pub fn summarise(a: &gg_ipc::MouseSettings) -> String {
    let mut parts = Vec::new();
    if let Some(dpi) = &a.dpi {
        let list: Vec<String> = dpi.stages.iter().map(stage_text).collect();
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
    let xs = f.stages.iter().map(|s| s.x).collect();
    sync_rows(g.get_stages(), xs, |m| g.set_stages(m));
    let ys = f.stages.iter().map(|s| s.y).collect();
    sync_rows(g.get_stages_y(), ys, |m| g.set_stages_y(m));
    g.set_split(f.split);
    g.set_active(f.active as i32);
    g.set_polling(f.polling as i32);
    g.set_buttons(ints(f.button_menu()));
    let keys: Vec<SharedString> = f.buttons.iter().map(|b| button_text(b).into()).collect();
    sync_rows(g.get_button_keys(), keys, |m| g.set_button_keys(m));
    show_picker(&g, f);
}

/// The key list: where it's open, what it shows, and the search.
fn show_picker(g: &MouseUi, f: &MouseForm) {
    let (button, media, filter, current) = match &f.picker {
        Some(p) => (
            p.button as i32,
            p.list == KeyList::Media,
            p.filter.as_str(),
            f.buttons
                .get(p.button)
                .and_then(|b| mouse_keys::label(b))
                .unwrap_or(""),
        ),
        None => (-1, false, "", ""),
    };
    g.set_key_button(button);
    g.set_key_media(media);
    // The box is bound both ways: only push a search it doesn't show yet, so
    // typing isn't disturbed.
    if g.get_key_filter() != filter {
        g.set_key_filter(filter.into());
    }
    g.set_key_current(current.into());
    let labels = f
        .key_choices()
        .into_iter()
        .map(|(_, label)| SharedString::from(label))
        .collect();
    sync_rows(g.get_key_choices(), labels, |m| g.set_key_choices(m));
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
    g.on_edit_stage_y(edit(|f, i, v| f.edit_stage_y(i as usize, v)));
    g.on_set_stage_y(edit(|f, i, v| f.set_stage_y(i as usize, v)));
    let split = edit(|f, on, _| f.set_split(on != 0));
    g.on_set_split(move |on| split(on as i32, 0));
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
    let pick_key = edit(|f, j, _| f.pick_key(j as usize));
    g.on_pick_key(move |j| pick_key(j, 0));
    let close = edit(|f, _, _| f.close_keys());
    g.on_close_keys(move || close(0, 0));
    {
        let weak = ui.as_weak();
        let form = form.clone();
        g.on_filter_keys(move |text| {
            form.borrow_mut().filter_keys(&text);
            if let Some(ui) = weak.upgrade() {
                show_mouse_form(&ui, &form.borrow());
            }
        });
    }

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
/// step with a place in a list takes it in the name (`mouse.set-stage1:3200`,
/// `mouse.stage-y1:800`; a bare `mouse.stage-y:800` is the picked stage's), and
/// a search its text after `=` (`mouse.key-filter=f1`).
pub(crate) fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let m = ui.global::<MouseUi>();
    let indexed = |prefix: &str| {
        name.strip_prefix(prefix)
            .and_then(|i| i.parse::<i32>().ok())
    };
    if let Some(i) = indexed("mouse.set-stage") {
        m.invoke_set_stage(i, n);
    } else if let Some(i) = indexed("mouse.stage-y") {
        m.invoke_set_stage_y(i, n);
    } else if let Some(i) = indexed("mouse.pick-button") {
        m.invoke_pick_button(i, n);
    } else if let Some(text) = name.strip_prefix("mouse.key-filter=") {
        m.invoke_filter_keys(text.into());
    } else {
        match name {
            "mouse.split" => m.invoke_set_split(n != 0),
            "mouse.stage-y" => m.invoke_set_stage_y(m.get_active(), n),
            // The button menu's "Keyboard key…" / "Media key…" for button `n`.
            "mouse.key-menu" => m.invoke_pick_button(n, KEY_ITEM as i32),
            "mouse.media-menu" => m.invoke_pick_button(n, MEDIA_ITEM as i32),
            "mouse.pick-key" => m.invoke_pick_key(n),
            "mouse.close-keys" => m.invoke_close_keys(),
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

    fn both(v: &[i32]) -> Vec<Stage> {
        v.iter().map(|d| Stage::both(*d)).collect()
    }

    fn applied_dpi(stages: &[(u16, u16)], active: usize) -> MouseSettings {
        MouseSettings {
            dpi: Some(MouseDpi {
                stages: stages.iter().map(|&(x, y)| DpiStage { x, y }).collect(),
                active,
            }),
            ..MouseSettings::default()
        }
    }

    #[test]
    fn a_stage_is_left_alone_while_typed_and_snapped_once_done() {
        let mut f = MouseForm::default();
        // Typing 1250 passes through 125 on the way: snapping that to 100 would
        // fight the box.
        f.edit_stage(0, 125);
        assert_eq!(f.stages[0], Stage::both(125));
        f.edit_stage(0, 1250);
        assert_eq!(f.stages[0], Stage::both(1250));
        f.set_stage(0, 1250);
        assert_eq!(f.stages[0], Stage::both(1300));
        f.set_stage(1, 50);
        assert_eq!(f.stages[1], Stage::both(DPI_MIN));
        f.set_stage(1, 99_999);
        assert_eq!(f.stages[1], Stage::both(DPI_MAX));
        f.edit_stage(1, 99_999);
        assert_eq!(
            f.stages[1],
            Stage::both(DPI_MAX),
            "even typed, a stage stays in range"
        );
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
        assert_eq!(f.stages, both(&[800, 1700]), "the form shows what was sent");
    }

    #[test]
    fn stages_come_and_go_within_bounds() {
        let mut f = MouseForm::default();
        f.set_stage(1, 3200);
        for _ in 0..10 {
            f.add_stage();
        }
        assert_eq!(f.stages, both(&[800, 3200, 3200, 3200, 3200]));
        f.pick_stage(9);
        assert_eq!(f.active, 4, "a pick past the end is the last stage");
    }

    #[test]
    fn removing_a_stage_keeps_the_pick_on_the_same_stage() {
        let mut f = MouseForm {
            stages: both(&[400, 800, 1600]),
            active: 2,
            ..MouseForm::default()
        };
        f.remove_stage(0);
        assert_eq!((f.stages.clone(), f.active), (both(&[800, 1600]), 1));
        f.remove_stage(1);
        assert_eq!((f.stages.clone(), f.active), (both(&[800]), 0));
        f.remove_stage(0);
        assert_eq!(f.stages, both(&[800]), "the last stage stays");
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
            (both(&[400]), 500, vec![1, 0, 2, 3, 4, 5])
        );
        assert!(!f.split, "equal axes keep X and Y together");
        f.set_stage(0, 3200);
        f.seed(&applied);
        assert_eq!(
            f.stages,
            both(&[3200]),
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
        assert_eq!((f.stages.clone(), f.polling), (both(&[800, 1600]), 250));
        assert_eq!(f.buttons, FACTORY_BUTTONS.map(String::from).to_vec());
    }

    #[test]
    fn a_key_set_elsewhere_is_kept_until_picked_over() {
        let mut f = MouseForm::default();
        f.seed(&MouseSettings {
            buttons: Some(
                ["button1", "Button2", "bksp", "button4", "disabled"]
                    .map(String::from)
                    .to_vec(),
            ),
            ..MouseSettings::default()
        });
        // Case and the "disabled" spelling are the daemon's too; a short list
        // gets the factory action for the rest. "bksp" is a name the daemon
        // takes that the menu and the key lists don't offer.
        assert_eq!(f.button_menu(), vec![0, 1, -1, 3, 14, 5]);
        assert_eq!(button_text(&f.buttons[2]), "Set elsewhere: bksp");
        let Request::SetMouseButtons { actions, .. } = f.buttons_request(false) else {
            panic!("not a buttons request");
        };
        assert_eq!(actions[2], "bksp", "applying doesn't replace it");
        f.pick_button(2, 8);
        assert_eq!(f.buttons[2], "playpause");
        f.factory_buttons();
        assert_eq!(f.button_menu(), vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn seeding_keeps_y_and_sets_x_and_y_apart_when_they_differ() {
        let mut f = MouseForm::default();
        f.seed(&applied_dpi(&[(400, 800), (1600, 1600)], 1));
        assert_eq!(
            (f.stages.clone(), f.active, f.split),
            (vec![Stage { x: 400, y: 800 }, Stage::both(1600)], 1, true)
        );
        let mut g = MouseForm::default();
        g.seed(&applied_dpi(&[(400, 400), (3200, 3200)], 0));
        assert!(!g.split, "equal axes keep them together");
    }

    #[test]
    fn with_x_and_y_together_an_edit_sets_both() {
        let mut f = MouseForm::default();
        f.edit_stage(0, 1250);
        assert_eq!(f.stages[0], Stage::both(1250));
        f.set_stage(0, 1250);
        assert_eq!(f.stages[0], Stage::both(1300));
        // The Y steps aren't on screen then, but they'd set both as well.
        f.set_stage_y(1, 2450);
        assert_eq!(f.stages[1], Stage::both(2500));
    }

    #[test]
    fn with_x_and_y_apart_an_edit_sets_one_snapped_on_its_own() {
        let mut f = MouseForm::default();
        f.set_split(true);
        f.edit_stage(0, 1250);
        assert_eq!(f.stages[0], Stage { x: 1250, y: 800 });
        f.set_stage(0, 1250);
        f.edit_stage_y(0, 125);
        assert_eq!(
            f.stages[0],
            Stage { x: 1300, y: 125 },
            "Y typed isn't fought"
        );
        f.set_stage_y(0, 2450);
        assert_eq!(f.stages[0], Stage { x: 1300, y: 2500 });
        f.edit_stage_y(1, 99_999);
        assert_eq!(
            f.stages[1],
            Stage {
                x: 1600,
                y: DPI_MAX
            }
        );
        f.set_stage_y(1, 50);
        assert_eq!(
            f.stages[1],
            Stage {
                x: 1600,
                y: DPI_MIN
            }
        );
    }

    #[test]
    fn putting_x_and_y_back_together_gives_y_the_x() {
        // Otherwise the one box would show X and a hidden Y would be sent.
        let mut f = MouseForm::default();
        f.seed(&applied_dpi(&[(400, 800), (1600, 3200)], 0));
        f.set_split(false);
        assert_eq!(f.stages, both(&[400, 1600]));
        f.set_split(true);
        assert_eq!(
            f.stages,
            both(&[400, 1600]),
            "setting them apart changes nothing"
        );
    }

    #[test]
    fn both_axes_are_sent_snapped() {
        let mut f = MouseForm::default();
        f.set_split(true);
        f.edit_stage(0, 420);
        f.edit_stage_y(0, 2450);
        let Request::SetMouseDpi { stages, .. } = f.dpi_request(false) else {
            panic!("not a DPI request");
        };
        assert_eq!(
            stages,
            vec![DpiStage { x: 400, y: 2500 }, DpiStage { x: 1600, y: 1600 }]
        );
        assert_eq!(
            f.stages[0],
            Stage { x: 400, y: 2500 },
            "the form shows what was sent"
        );
    }

    #[test]
    fn the_summary_shows_both_axes_only_where_they_differ() {
        assert_eq!(
            summarise(&applied_dpi(&[(400, 800), (1600, 1600)], 0)),
            "400×800 / 1600 dpi · stage 1"
        );
    }

    #[test]
    fn stages_added_and_removed_keep_both_axes_and_the_pick() {
        let mut f = MouseForm::default();
        f.seed(&applied_dpi(&[(400, 800), (800, 1600), (1600, 3200)], 2));
        f.add_stage();
        assert_eq!(
            f.stages[3],
            Stage { x: 1600, y: 3200 },
            "a new stage copies the last"
        );
        f.remove_stage(0);
        assert_eq!(f.active, 1);
        assert_eq!(f.stages[f.active], Stage { x: 1600, y: 3200 });
    }

    #[test]
    fn a_key_picked_from_the_list_becomes_the_buttons_name() {
        let mut f = MouseForm::default();
        f.pick_button(3, KEY_ITEM);
        assert_eq!(
            f.buttons[3], "button4",
            "opening the list changes nothing yet"
        );
        assert_eq!(
            f.button_menu()[3],
            KEY_ITEM as i32,
            "the menu shows the entry picked"
        );
        assert_eq!(f.key_choices().len(), KEYS.len());
        f.filter_keys("F13");
        assert_eq!(f.key_choices(), vec![("f13", "F13")]);
        f.pick_key(0);
        assert_eq!(f.buttons[3], "f13");
        assert_eq!(f.picker, None, "picking closes the list");
        assert_eq!(f.button_menu()[3], KEY_ITEM as i32);
        assert_eq!(button_text(&f.buttons[3]), "Key: F13");
        let Request::SetMouseButtons { actions, .. } = f.buttons_request(false) else {
            panic!("not a buttons request");
        };
        assert_eq!(actions[3], "f13");
    }

    #[test]
    fn a_media_key_is_picked_from_its_own_list() {
        let mut f = MouseForm::default();
        f.pick_button(5, MEDIA_ITEM);
        assert_eq!(f.button_menu()[5], MEDIA_ITEM as i32);
        assert_eq!(f.key_choices().len(), MEDIA.len());
        f.filter_keys("vol");
        assert_eq!(
            f.key_choices(),
            vec![("volumeup", "Volume up"), ("volumedown", "Volume down")]
        );
        f.pick_key(1);
        assert_eq!(f.buttons[5], "volumedown");
        // The menu has that one as an action already.
        assert_eq!(f.button_menu()[5], 13);
        assert_eq!(button_text(&f.buttons[5]), "");
    }

    #[test]
    fn a_key_name_points_the_menu_at_keyboard_key() {
        let mut f = MouseForm::default();
        f.seed(&MouseSettings {
            buttons: Some(["F13", "leftctrl", "playpause"].map(String::from).to_vec()),
            ..MouseSettings::default()
        });
        assert_eq!(
            f.button_menu(),
            vec![KEY_ITEM as i32, KEY_ITEM as i32, 8, 3, 4, 5]
        );
        assert_eq!(button_text("F13"), "Key: F13");
        assert_eq!(button_text("leftctrl"), "Key: Left Ctrl");
        assert_eq!(button_text("Button1"), "");
        assert_eq!(button_text("macro7"), "Set elsewhere: macro7");
    }

    #[test]
    fn the_search_puts_exact_and_leading_matches_first() {
        let labels = |f: &str| -> Vec<&str> {
            choices(KeyList::Keys, f)
                .into_iter()
                .map(|(_, l)| l)
                .collect()
        };
        assert_eq!(
            labels("f1"),
            vec!["F1", "F10", "F11", "F12", "F13", "F14", "F15", "F16", "F17", "F18", "F19"]
        );
        assert_eq!(labels("ALT"), vec!["Left Alt", "Right Alt"]);
        assert_eq!(labels("enter"), vec!["Enter", "Keypad Enter"]);
        assert_eq!(labels(" page "), vec!["Page Up", "Page Down"]);
        assert!(labels("zzz").is_empty());
        assert_eq!(choices(KeyList::Keys, "").len(), KEYS.len());
    }

    #[test]
    fn closing_the_list_puts_the_menu_back() {
        let mut f = MouseForm::default();
        f.pick_button(2, KEY_ITEM);
        f.filter_keys("f");
        f.pick_key(999);
        assert!(f.picker.is_some(), "a pick past the list leaves it open");
        assert_eq!(f.buttons[2], "button3");
        f.close_keys();
        assert_eq!((f.picker.clone(), f.button_menu()[2]), (None, 2));
        // Opened on another button, it moves there; an action picked on that
        // button closes it; reset to factory closes it too.
        f.pick_button(0, KEY_ITEM);
        f.pick_button(1, MEDIA_ITEM);
        assert_eq!(f.button_menu()[..2], [0, MEDIA_ITEM as i32]);
        f.pick_button(1, 6);
        assert_eq!(
            (f.picker.clone(), f.buttons[1].as_str()),
            (None, "scrollup")
        );
        f.pick_button(4, KEY_ITEM);
        f.factory_buttons();
        assert_eq!(f.picker, None);
    }

    #[test]
    fn a_name_set_elsewhere_survives_picking_keys_for_other_buttons() {
        let mut f = MouseForm::default();
        f.seed(&MouseSettings {
            buttons: Some(["button1", "bksp"].map(String::from).to_vec()),
            ..MouseSettings::default()
        });
        f.pick_button(0, KEY_ITEM);
        f.filter_keys("space");
        f.pick_key(0);
        let Request::SetMouseButtons { actions, .. } = f.buttons_request(false) else {
            panic!("not a buttons request");
        };
        assert_eq!(actions[..2], ["space", "bksp"]);
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
