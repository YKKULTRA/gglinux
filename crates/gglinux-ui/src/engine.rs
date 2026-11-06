// SPDX-License-Identifier: GPL-3.0-or-later
//! The Engine page: the device list and the headset. The mouse panel is in
//! [`crate::mouse`], the keyboard card in [`crate::keyboard`].

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use gg_ipc::{
    DeviceInfo, HeadsetChange, HeadsetControls, HeadsetPower, NoiseMode, RadioMode, Request,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::client::{Client, State, TIMEOUT};
use crate::{act, sync_rows, AppWindow, Device, EngineUi, Forms, HeadsetUi, Iface};

pub const RIVAL3: u16 = 0x1872;
pub const NOVA_PRO: u16 = 0x12e0;
pub const APEX: u16 = 0x1642;

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

// --- the headset -------------------------------------------------------------

/// The custom EQ curve being edited, seeded once from what was last applied;
/// gone with the daemon, like the mouse's form ([`crate::mouse::MouseForm`]).
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
    /// As [`crate::mouse::MouseForm::follow`]: the curve is seeded once from
    /// what was applied, and starts over, Custom closed, while there's no state.
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

/// Start the headset's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.headset.borrow_mut() = HeadsetForm::default();
}

/// The `shot` example's scripted steps for this page (see [`crate::poke`]). A
/// step with a place in a list takes it in the name (`headset.set-band3:6` for
/// +6 dB on the fourth band).
pub(crate) fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let h = ui.global::<HeadsetUi>();
    if let Some(i) = name
        .strip_prefix("headset.set-band")
        .and_then(|i| i.parse::<i32>().ok())
    {
        h.invoke_set_band(i, n as f32);
        return true;
    }
    match name {
        "devices.toggle" => {
            let id = ui.global::<EngineUi>().get_devices().row_data(n as usize);
            if let Some(d) = id {
                ui.global::<EngineUi>().invoke_toggle(d.id);
            }
        }
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
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::DeviceKind;

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
