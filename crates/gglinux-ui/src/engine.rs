// SPDX-License-Identifier: GPL-3.0-or-later
//! The Engine page: the device list, the mouse and the headset.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use gg_ipc::{
    DeviceInfo, DpiStage, HeadsetChange, HeadsetControls, HeadsetPower, NoiseMode, RadioMode,
    Request,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::client::{Client, State, TIMEOUT};
use crate::{act, sync_rows, AppWindow, Device, EngineUi, HeadsetUi, Iface, MouseUi};

pub const RIVAL3: u16 = 0x1872;
pub const NOVA_PRO: u16 = 0x12e0;
pub const APEX: u16 = 0x1642;

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

/// Auto-off choices, in minutes (`HeadsetUi.auto-off-names`).
const AUTO_OFF: [u16; 7] = [0, 1, 5, 10, 15, 30, 60];
const EQ_CUSTOM: i32 = 4;
const EQ_BANDS: usize = 10;

pub fn ints(v: impl IntoIterator<Item = i32>) -> ModelRc<i32> {
    ModelRc::new(VecModel::from(v.into_iter().collect::<Vec<_>>()))
}

pub fn floats(v: impl IntoIterator<Item = f32>) -> ModelRc<f32> {
    ModelRc::new(VecModel::from(v.into_iter().collect::<Vec<_>>()))
}

// --- the device cards ----------------------------------------------------------

/// Which cards have their interface table open, by device id: kept across
/// refreshes of the list, and forgotten once the device goes.
#[derive(Debug, Default)]
pub struct OpenCards(BTreeSet<String>);

impl OpenCards {
    pub fn toggle(&mut self, id: &str) {
        if !self.0.remove(id) {
            self.0.insert(id.to_owned());
        }
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.0.contains(id)
    }

    /// Forget the cards of devices no longer listed.
    pub fn keep(&mut self, devices: &[DeviceInfo]) {
        self.0.retain(|id| devices.iter().any(|d| &d.id == id));
    }
}

thread_local! {
    static OPEN: RefCell<OpenCards> = const { RefCell::new(OpenCards(BTreeSet::new())) };
}

pub fn device_row(d: &DeviceInfo, open: bool) -> Device {
    let kind = serde_json::to_value(d.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut ids = format!("{:04x}:{:04x}  ·  {kind}", d.vendor_id, d.product_id);
    if let Some(sn) = &d.serial {
        ids.push_str(&format!("  ·  sn {sn}"));
    }
    let vendor = d
        .interfaces
        .iter()
        .filter(|i| i.usage_page >= 0xff00)
        .count();
    let interfaces: Vec<Iface> = d
        .interfaces
        .iter()
        .map(|i| Iface {
            number: i.interface_number,
            usage_page: format!("0x{:04x}", i.usage_page).into(),
            usage: format!("0x{:02x}", i.usage).into(),
            path: i.path.clone().into(),
            vendor: i.usage_page >= 0xff00,
        })
        .collect();
    Device {
        id: d.id.clone().into(),
        name: d.name.clone().into(),
        kind: kind.into(),
        ids: ids.into(),
        connected: d.connected,
        summary: format!("{} HID interfaces · {vendor} vendor", d.interfaces.len()).into(),
        open,
        interfaces: ModelRc::new(VecModel::from(interfaces)),
    }
}

pub fn show_devices(ui: &AppWindow, s: &State) {
    let g = ui.global::<EngineUi>();
    let parts = s.running();
    let has = |pid| s.devices.iter().any(|d| d.product_id == pid);
    g.set_mouse(parts.mouse && has(RIVAL3));
    g.set_headset(parts.headset && has(NOVA_PRO));
    g.set_keyboard(parts.keyboard && has(APEX));
    let rows: Vec<Device> = OPEN.with(|open| {
        let mut open = open.borrow_mut();
        open.keep(&s.devices);
        s.devices
            .iter()
            .map(|d| device_row(d, open.is_open(&d.id)))
            .collect()
    });
    sync_rows(g.get_devices(), rows, |m| g.set_devices(m));
}

pub fn bind_devices(ui: &AppWindow, client: &Client) {
    let weak = ui.as_weak();
    let client = client.clone();
    ui.global::<EngineUi>().on_toggle(move |id| {
        OPEN.with(|open| open.borrow_mut().toggle(&id));
        if let Some(ui) = weak.upgrade() {
            show_devices(&ui, &client.state());
        }
    });
}

// --- the mouse ---------------------------------------------------------------

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

// --- the headset -------------------------------------------------------------

/// The custom EQ curve being edited, seeded once from what was last applied;
/// gone with the daemon, like the mouse's form.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadsetForm {
    pub bands: Vec<f32>,
    pub editing_custom: bool,
    seeded: bool,
}

impl Default for HeadsetForm {
    fn default() -> Self {
        HeadsetForm {
            bands: vec![0.0; EQ_BANDS],
            editing_custom: false,
            seeded: false,
        }
    }
}

impl HeadsetForm {
    /// As [`MouseForm::follow`]: the curve is seeded once from what was applied,
    /// and starts over, Custom closed, while there's no state.
    pub fn follow(&mut self, headset: Option<&gg_ipc::HeadsetState>) {
        match headset {
            None => *self = HeadsetForm::default(),
            Some(h) => self.seed(&h.controls),
        }
    }

    pub fn seed(&mut self, c: &HeadsetControls) {
        if let (false, Some(bands)) = (self.seeded, &c.eq_bands) {
            self.seeded = true;
            self.bands = bands.clone();
            self.bands.resize(EQ_BANDS, 0.0);
        }
    }

    /// A band's gain, in the half-dB steps the base station takes.
    pub fn set_band(&mut self, i: usize, db: f32) {
        if let Some(b) = self.bands.get_mut(i) {
            *b = ((db * 2.0).round() / 2.0).clamp(-10.0, 10.0);
        }
    }

    /// Custom only opens the curve, to be applied with its own button; any
    /// other preset is sent at once.
    pub fn pick_preset(&mut self, i: i32) -> Option<HeadsetChange> {
        self.editing_custom = i == EQ_CUSTOM;
        (i != EQ_CUSTOM).then(|| HeadsetChange::EqPreset(i.clamp(0, 3) as u8))
    }
}

/// The auto-off choice shown, or -1 for not known (or not one of the choices).
fn auto_off_index(minutes: Option<u16>) -> i32 {
    minutes
        .and_then(|m| AUTO_OFF.iter().position(|a| *a == m))
        .map_or(-1, |i| i as i32)
}

fn show_headset_form(ui: &AppWindow, f: &HeadsetForm) {
    let g = ui.global::<HeadsetUi>();
    sync_rows(g.get_bands(), f.bands.clone(), |m| g.set_bands(m));
    g.set_editing_custom(f.editing_custom);
}

pub fn show_headset(ui: &AppWindow, s: &State, form: &RefCell<HeadsetForm>) {
    let g = ui.global::<HeadsetUi>();
    form.borrow_mut().follow(s.headset.as_ref());
    show_headset_form(ui, &form.borrow());
    let Some(h) = s.headset.as_ref() else {
        g.set_known(false);
        g.set_connected(false);
        g.set_controls(false);
        return;
    };
    g.set_known(true);
    g.set_connected(h.connected);
    g.set_power(
        match h.power {
            HeadsetPower::Online => "headset on",
            HeadsetPower::Charging => "headset charging",
            HeadsetPower::Offline => "headset off",
            HeadsetPower::Unknown => "headset state unknown",
        }
        .into(),
    );
    g.set_battery(h.headset_battery.map_or(-1, i32::from));
    g.set_spare(h.spare_battery.map_or(-1, i32::from));
    g.set_protect(h.screen.protect);
    g.set_lit_seconds(h.screen.lit_seconds as i32);
    g.set_brightness(h.screen.brightness.map_or(-1, i32::from));
    g.set_screen_dark(h.screen_dark);
    g.set_controls(true);
    let c = &h.controls;
    g.set_noise(c.noise_mode.map_or(-1, |m| match m {
        NoiseMode::Off => 0,
        NoiseMode::Transparency => 1,
        NoiseMode::Anc => 2,
    }));
    g.set_transparency(c.transparency.map_or(-1, i32::from));
    g.set_mic_muted(c.mic_muted.map_or(-1, i32::from));
    g.set_sidetone(c.sidetone.map_or(-1, i32::from));
    g.set_mic_volume(c.mic_volume.map_or(-1, i32::from));
    g.set_mic_led(c.mic_led.map_or(-1, i32::from));
    g.set_auto_off(auto_off_index(c.auto_off_minutes));
    g.set_radio(c.radio_mode.map_or(-1, |r| match r {
        RadioMode::Speed => 0,
        RadioMode::Range => 1,
    }));
    g.set_eq_preset(c.eq_preset.map_or(-1, i32::from));
}

pub fn bind_headset(ui: &AppWindow, client: &Client, form: Rc<RefCell<HeadsetForm>>) {
    let g = ui.global::<HeadsetUi>();
    let mark = |ui: &AppWindow, busy: bool, error: SharedString| {
        let g = ui.global::<HeadsetUi>();
        g.set_busy(busy);
        g.set_error(error);
    };
    // Each control applies at once: the changes are cheap and reversible, and the
    // screen shows the result.
    let screen = |patch: fn(&mut gg_ipc::ScreenSettings, i32)| {
        let weak = ui.as_weak();
        let client = client.clone();
        move |v: i32| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(mut settings) = client.state().headset.map(|h| h.screen) else {
                return;
            };
            patch(&mut settings, v);
            act(
                &ui,
                &client,
                Request::SetHeadsetScreen { settings },
                TIMEOUT,
                mark,
            );
        }
    };
    let protect = screen(|s, v| s.protect = v != 0);
    g.on_set_protect(move |on| protect(on as i32));
    g.on_set_lit_seconds(screen(|s, v| s.lit_seconds = v.clamp(3, 600) as u16));
    g.on_set_brightness(screen(|s, v| s.brightness = Some(v.clamp(1, 10) as u8)));

    let change = |make: fn(i32) -> Option<HeadsetChange>| {
        let weak = ui.as_weak();
        let client = client.clone();
        move |v: i32| {
            let Some(ui) = weak.upgrade() else { return };
            if let Some(change) = make(v) {
                act(&ui, &client, Request::SetHeadset { change }, TIMEOUT, mark);
            }
        }
    };
    g.on_set_noise(change(|i| {
        Some(HeadsetChange::NoiseMode(match i {
            0 => NoiseMode::Off,
            1 => NoiseMode::Transparency,
            _ => NoiseMode::Anc,
        }))
    }));
    g.on_set_transparency(change(|v| {
        Some(HeadsetChange::Transparency(v.clamp(1, 10) as u8))
    }));
    g.on_set_sidetone(change(|v| {
        Some(HeadsetChange::Sidetone(v.clamp(0, 3) as u8))
    }));
    g.on_set_mic_volume(change(|v| {
        Some(HeadsetChange::MicVolume(v.clamp(1, 10) as u8))
    }));
    g.on_set_mic_led(change(|v| {
        Some(HeadsetChange::MicLed(v.clamp(1, 10) as u8))
    }));
    g.on_set_auto_off(change(|i| {
        AUTO_OFF
            .get(i as usize)
            .map(|m| HeadsetChange::AutoOffMinutes(*m))
    }));
    g.on_set_radio(change(|i| {
        Some(HeadsetChange::RadioMode(if i == 0 {
            RadioMode::Speed
        } else {
            RadioMode::Range
        }))
    }));

    let weak = ui.as_weak();
    let c = client.clone();
    let f = form.clone();
    g.on_pick_preset(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let change = f.borrow_mut().pick_preset(i);
        show_headset_form(&ui, &f.borrow());
        if let Some(change) = change {
            act(&ui, &c, Request::SetHeadset { change }, TIMEOUT, mark);
        }
    });
    let weak = ui.as_weak();
    let f = form.clone();
    g.on_set_band(move |i, db| {
        f.borrow_mut().set_band(i as usize, db);
        if let Some(ui) = weak.upgrade() {
            show_headset_form(&ui, &f.borrow());
        }
    });
    let weak = ui.as_weak();
    let client = client.clone();
    g.on_apply_bands(move || {
        let Some(ui) = weak.upgrade() else { return };
        let bands = form.borrow().bands.clone();
        act(
            &ui,
            &client,
            Request::SetHeadset {
                change: HeadsetChange::EqBands(bands),
            },
            TIMEOUT,
            mark,
        );
    });
}

/// The `shot` example's scripted steps for this page (see [`crate::poke`]). A
/// step with a place in a list takes it in the name (`mouse.set-stage1:3200`,
/// `headset.set-band3:6` for +6 dB on the fourth band).
pub(crate) fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let m = ui.global::<MouseUi>();
    let h = ui.global::<HeadsetUi>();
    let indexed = |prefix: &str| {
        name.strip_prefix(prefix)
            .and_then(|i| i.parse::<i32>().ok())
    };
    if let Some(i) = indexed("mouse.set-stage") {
        m.invoke_set_stage(i, n);
    } else if let Some(i) = indexed("mouse.pick-button") {
        m.invoke_pick_button(i, n);
    } else if let Some(i) = indexed("headset.set-band") {
        h.invoke_set_band(i, n as f32);
    } else {
        match name {
            "devices.toggle" => {
                let id = ui.global::<EngineUi>().get_devices().row_data(n as usize);
                if let Some(d) = id {
                    ui.global::<EngineUi>().invoke_toggle(d.id);
                }
            }
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
            "headset.set-protect" => h.invoke_set_protect(n != 0),
            "headset.set-lit-seconds" => h.invoke_set_lit_seconds(n),
            "headset.set-brightness" => h.invoke_set_brightness(n),
            "headset.set-noise" => h.invoke_set_noise(n),
            "headset.set-transparency" => h.invoke_set_transparency(n),
            "headset.set-sidetone" => h.invoke_set_sidetone(n),
            "headset.set-mic-volume" => h.invoke_set_mic_volume(n),
            "headset.set-mic-led" => h.invoke_set_mic_led(n),
            "headset.set-auto-off" => h.invoke_set_auto_off(n),
            "headset.set-radio" => h.invoke_set_radio(n),
            "headset.pick-preset" => h.invoke_pick_preset(n),
            "headset.apply-bands" => h.invoke_apply_bands(),
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::{DeviceKind, MouseDpi, MouseSettings};

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

    #[test]
    fn the_eq_curve_starts_over_with_the_daemon() {
        let state = |bands: Option<Vec<f32>>| gg_ipc::HeadsetState {
            connected: true,
            power: HeadsetPower::Online,
            headset_battery: Some(80),
            spare_battery: None,
            screen: gg_ipc::ScreenSettings::default(),
            screen_dark: false,
            controls: HeadsetControls {
                eq_bands: bands,
                ..HeadsetControls::default()
            },
        };
        let mut f = HeadsetForm::default();
        f.follow(Some(&state(Some(vec![2.0; EQ_BANDS]))));
        f.pick_preset(EQ_CUSTOM);
        f.follow(None);
        assert_eq!(f, HeadsetForm::default(), "Custom closed, the curve flat");
        f.follow(Some(&state(Some(vec![-1.0; EQ_BANDS]))));
        assert_eq!(f.bands, vec![-1.0; EQ_BANDS]);
    }

    #[test]
    fn the_eq_curve_is_seeded_once_and_kept_in_half_db_steps() {
        let mut f = HeadsetForm::default();
        assert_eq!(f.bands, vec![0.0; EQ_BANDS]);
        let mut c = HeadsetControls {
            eq_bands: Some(vec![1.5; EQ_BANDS]),
            ..HeadsetControls::default()
        };
        f.seed(&c);
        f.set_band(0, 3.3);
        f.set_band(1, -14.0);
        c.eq_bands = Some(vec![0.0; EQ_BANDS]);
        f.seed(&c);
        assert_eq!(
            &f.bands[..3],
            &[3.5, -10.0, 1.5],
            "a later state leaves the edit alone"
        );
    }

    #[test]
    fn custom_opens_the_curve_and_a_preset_is_sent() {
        let mut f = HeadsetForm::default();
        assert_eq!(f.pick_preset(EQ_CUSTOM), None);
        assert!(f.editing_custom);
        assert_eq!(f.pick_preset(1), Some(HeadsetChange::EqPreset(1)));
        assert!(!f.editing_custom);
    }

    #[test]
    fn auto_off_shows_only_its_choices() {
        assert_eq!(auto_off_index(None), -1);
        assert_eq!(auto_off_index(Some(0)), 0);
        assert_eq!(auto_off_index(Some(30)), 5);
        assert_eq!(auto_off_index(Some(7)), -1);
    }

    #[test]
    fn an_open_card_stays_open_until_its_device_goes() {
        let device = |id: &str| DeviceInfo {
            id: id.into(),
            kind: DeviceKind::Mouse,
            vendor_id: 0x1038,
            product_id: RIVAL3,
            name: "Rival 3 Wireless Gen 2".into(),
            serial: None,
            connected: true,
            interfaces: vec![],
        };
        let mut open = OpenCards::default();
        open.toggle("1038:1872");
        open.keep(&[device("1038:12e0"), device("1038:1872")]);
        assert!(open.is_open("1038:1872"));
        assert!(!open.is_open("1038:12e0"));
        open.keep(&[device("1038:12e0")]);
        open.keep(&[device("1038:12e0"), device("1038:1872")]);
        assert!(!open.is_open("1038:1872"), "back again, it starts closed");
        open.toggle("1038:12e0");
        open.toggle("1038:12e0");
        assert!(!open.is_open("1038:12e0"));
    }
}
