// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apex Pro TKL Gen 3: its lighting, and what the keyboard card's panels
//! share. The switches are in [`crate::switches`], the screen in
//! [`crate::screen`], key bindings in [`crate::bindings`] and the onboard
//! slots in [`crate::onboard`].

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use gg_ipc::{
    Effect, KeyColor, KeyInfo, KeyboardState, Lighting, ReactiveMode, ReactiveStatus, Request, Rgb,
};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::client::{Client, State, TIMEOUT};
use crate::{act, act_then, sync_rows, AppWindow, Forms, Key, KeyboardUi};

/// A change to the lighting, from a control's value or colour.
type LightPatch = Rc<dyn Fn(&mut Lighting, &KeyboardForm, f32, Color)>;

/// What the lighting panel keeps between the daemon's answers.
#[derive(Default)]
pub struct KeyboardForm {
    /// Static colours being painted, by HID code. They follow the daemon's
    /// unless there's paint not applied yet.
    pub painted: BTreeMap<u8, Rgb>,
    pub dirty: bool,
    pub light_selected: BTreeSet<u8>,
}

impl KeyboardForm {
    /// Take in what the daemon has. The paint follows it unless there's paint
    /// not applied yet.
    pub fn follow(&mut self, kb: &KeyboardState) {
        if !self.dirty {
            self.painted = kb.lighting.keys.iter().map(|k| (k.code, k.color)).collect();
        }
    }

    /// Select a key on the lighting picture, or let it go.
    pub fn toggle_light(&mut self, code: u8) {
        if !self.light_selected.remove(&code) {
            self.light_selected.insert(code);
        }
    }

    /// Paint keys in the brush's colour, to apply later.
    pub fn paint(&mut self, codes: impl IntoIterator<Item = u8>, brush: Rgb) {
        self.painted.extend(codes.into_iter().map(|c| (c, brush)));
        self.dirty = true;
    }

    /// `l` as a static effect with the paint.
    pub fn painted_lighting(&self, l: &Lighting) -> Lighting {
        Lighting {
            effect: Effect::Static,
            keys: self
                .painted
                .iter()
                .map(|(code, color)| KeyColor {
                    code: *code,
                    color: *color,
                })
                .collect(),
            ..l.clone()
        }
    }
}

pub fn rgb(c: Color) -> Rgb {
    Rgb {
        r: c.red(),
        g: c.green(),
        b: c.blue(),
    }
}

pub fn color(c: Rgb) -> Color {
    Color::from_rgb_u8(c.r, c.g, c.b)
}

/// Black or white, whichever reads on the colour.
fn ink(c: Rgb) -> Color {
    let lum = 0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32;
    if lum > 140.0 {
        Color::from_rgb_u8(12, 14, 17)
    } else {
        Color::from_rgb_u8(230, 233, 238)
    }
}

/// The keys' colours on a keyboard picture (Theme.surface-2, text-dim, text).
pub(crate) const SURFACE_2: Color = Color::from_rgb_u8(28, 32, 39);
pub(crate) const TEXT_DIM: Color = Color::from_rgb_u8(139, 147, 159);
pub(crate) const TEXT: Color = Color::from_rgb_u8(230, 233, 238);

/// A key's name in a list.
pub(crate) fn label(k: &KeyInfo) -> String {
    if k.label.is_empty() {
        "Space".into()
    } else {
        k.label.clone()
    }
}

fn effect_index(e: &Effect) -> i32 {
    match e {
        Effect::Static => 0,
        Effect::Breathing { .. } => 1,
        Effect::Wave { .. } => 2,
        Effect::Spectrum { .. } => 3,
        Effect::Visualizer { .. } => 4,
    }
}

fn period(e: &Effect) -> Option<f32> {
    match e {
        Effect::Breathing { period_s, .. }
        | Effect::Wave { period_s }
        | Effect::Spectrum { period_s } => Some(*period_s),
        _ => None,
    }
}

/// Keys that can carry a mute light: the labelled ones.
fn mute_keys(kb: &KeyboardState) -> Vec<&KeyInfo> {
    kb.layout.iter().filter(|k| !k.label.is_empty()).collect()
}

/// The keys' names for a list, or `None` while `now` already has them. A new
/// list makes a list box pick its first entry again, so it's only set on a change.
pub(crate) fn names(
    now: ModelRc<SharedString>,
    keys: &[&KeyInfo],
) -> Option<ModelRc<SharedString>> {
    let names: Vec<SharedString> = keys.iter().map(|k| label(k).into()).collect();
    now.iter()
        .ne(names.iter().cloned())
        .then(|| ModelRc::new(VecModel::from(names)))
}

pub fn show(ui: &AppWindow, s: &State, forms: &Forms) {
    let Some(kb) = s.keyboard.as_ref() else {
        // The daemon went (or hasn't answered yet): what was being edited goes
        // with it, as the old panel's did, and so do errors from before.
        reset(forms);
        let g = ui.global::<KeyboardUi>();
        g.set_known(false);
        g.set_connected(false);
        mark_light(ui, false, SharedString::new());
        return;
    };
    let mut f = forms.keyboard.borrow_mut();
    f.follow(kb);
    let l = &kb.lighting;
    let g = ui.global::<KeyboardUi>();
    g.set_known(true);
    g.set_connected(kb.connected);
    let cols = kb.layout.iter().map(|k| k.x + k.w).fold(0.0, f32::max);
    let rows = kb.layout.iter().map(|k| k.y + 1.0).fold(0.0, f32::max);
    g.set_cols(cols.max(1.0));
    g.set_rows(rows.max(1.0));
    g.set_enabled(l.enabled);
    g.set_effect(effect_index(&l.effect));
    if let Effect::Breathing { color: c, .. } = l.effect {
        g.set_breathing_color(color(c));
    }
    g.set_has_period(period(&l.effect).is_some());
    g.set_period(period(&l.effect).unwrap_or(4.0));
    if let Effect::Visualizer { sensitivity } = l.effect {
        g.set_sensitivity(sensitivity);
    }
    g.set_brightness(l.brightness as i32);
    g.set_reactive(l.reactive.enabled);
    g.set_reactive_mode(match l.reactive.mode {
        ReactiveMode::Fade => 0,
        ReactiveMode::Ripple => 1,
    });
    g.set_reactive_color(color(l.reactive.color));
    g.set_duration(l.reactive.duration_s);
    g.set_reactive_status(
        match kb.reactive {
            ReactiveStatus::Off => "waiting for the keyboard",
            ReactiveStatus::Reading => "on",
            ReactiveStatus::NoAccess => "no access",
        }
        .into(),
    );
    g.set_no_access(kb.reactive == ReactiveStatus::NoAccess);
    let mute = mute_keys(kb);
    if let Some(m) = names(g.get_key_names(), &mute) {
        g.set_key_names(m);
    }
    g.set_mute(l.mic_mute.enabled);
    g.set_mute_key(
        mute.iter()
            .position(|k| k.code == l.mic_mute.key)
            .map_or(-1, |i| i as i32),
    );
    g.set_mute_color(color(l.mic_mute.color));
    show_light_keys(ui, kb, &f);
}

fn show_light_keys(ui: &AppWindow, kb: &KeyboardState, f: &KeyboardForm) {
    let keys: Vec<Key> = kb
        .layout
        .iter()
        .map(|k| {
            let paint = f.painted.get(&k.code).copied();
            Key {
                x: k.x,
                y: k.y,
                w: k.w,
                label: k.label.clone().into(),
                sub: SharedString::new(),
                fill: paint.map_or(SURFACE_2, color),
                ink: paint.map_or(TEXT_DIM, ink),
                selected: f.light_selected.contains(&k.code),
                fixed: false,
                own: false,
            }
        })
        .collect();
    let g = ui.global::<KeyboardUi>();
    sync_rows(g.get_keys(), keys, |m| g.set_keys(m));
    g.set_selected(f.light_selected.len() as i32);
    g.set_dirty(f.dirty);
}

/// The daemon's keyboard state, if it has said.
pub(crate) fn keyboard(client: &Client) -> Option<KeyboardState> {
    client.state().keyboard
}

fn mark_light(ui: &AppWindow, busy: bool, error: SharedString) {
    let g = ui.global::<KeyboardUi>();
    g.set_busy(busy);
    g.set_error(error);
}

/// The `mark` for a panel's requests (see [`act`]): `set` marks the panel. A
/// list box keeps showing a pick the daemon refused, so after a failure the
/// panel is drawn again (`show`) from what the daemon has.
pub(crate) fn marker(
    client: &Client,
    set: fn(&AppWindow, bool, SharedString),
    show: fn(&AppWindow, &State, &Forms),
) -> impl Fn(&AppWindow, bool, SharedString) + Send + Clone + 'static {
    let client = client.clone();
    move |ui: &AppWindow, busy: bool, error: SharedString| {
        let failed = !busy && !error.is_empty();
        set(ui, busy, error);
        if failed {
            crate::with_forms(|forms| show(ui, &client.state(), forms));
        }
    }
}

fn redraw(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    let s = client.state();
    show(ui, &s, forms);
}

pub fn bind(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    let g = ui.global::<KeyboardUi>();

    // Each change sends the whole lighting, built from the daemon's: a change made
    // from a copy older than the last answer would undo that one, so the controls
    // wait while one is in flight.
    let light = |patch: LightPatch| {
        let weak = ui.as_weak();
        let client = client.clone();
        let forms = forms.clone();
        move |v: f32, c: Color| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(mut lighting) = keyboard(&client).map(|k| k.lighting) else {
                return;
            };
            patch(&mut lighting, &forms.keyboard.borrow(), v, c);
            act(
                &ui,
                &client,
                Request::SetLighting { lighting },
                TIMEOUT,
                marker(&client, mark_light, show),
            );
        }
    };
    let none = Color::default();
    let f = light(Rc::new(|l, _, v, _| l.enabled = v != 0.0));
    g.on_set_enabled(move |on| f(on as i32 as f32, none));
    // A breathing effect starts in the brush's colour.
    let f = light(Rc::new(|l, _, v, brush| {
        let p = period(&l.effect).unwrap_or(4.0);
        l.effect = match v as i32 {
            0 => Effect::Static,
            1 => Effect::Breathing {
                color: rgb(brush),
                period_s: p,
            },
            2 => Effect::Wave { period_s: p },
            3 => Effect::Spectrum { period_s: p },
            _ => Effect::Visualizer { sensitivity: 1.0 },
        };
    }));
    let weak = ui.as_weak();
    g.on_set_effect(move |i| {
        let brush = weak
            .upgrade()
            .map_or(none, |ui| ui.global::<KeyboardUi>().get_brush());
        f(i as f32, brush)
    });
    let f = light(Rc::new(|l, _, _, c| {
        let p = period(&l.effect).unwrap_or(4.0);
        l.effect = Effect::Breathing {
            color: rgb(c),
            period_s: p,
        };
    }));
    g.on_set_breathing_color(move |c| f(0.0, c));
    let f = light(Rc::new(|l, _, v, _| match &mut l.effect {
        Effect::Breathing { period_s, .. }
        | Effect::Wave { period_s }
        | Effect::Spectrum { period_s } => *period_s = v,
        _ => {}
    }));
    g.on_set_period(move |v| f(v, none));
    let f = light(Rc::new(|l, _, v, _| {
        l.effect = Effect::Visualizer { sensitivity: v }
    }));
    g.on_set_sensitivity(move |v| f(v, none));
    let f = light(Rc::new(|l, _, v, _| {
        l.brightness = v.clamp(0.0, 100.0) as u8
    }));
    g.on_set_brightness(move |v| f(v as f32, none));
    let f = light(Rc::new(|l, _, v, _| l.reactive.enabled = v != 0.0));
    g.on_set_reactive(move |on| f(on as i32 as f32, none));
    let f = light(Rc::new(|l, _, v, _| {
        l.reactive.mode = if v as i32 == 0 {
            ReactiveMode::Fade
        } else {
            ReactiveMode::Ripple
        }
    }));
    g.on_set_reactive_mode(move |i| f(i as f32, none));
    let f = light(Rc::new(|l, _, _, c| l.reactive.color = rgb(c)));
    g.on_set_reactive_color(move |c| f(0.0, c));
    let f = light(Rc::new(|l, _, v, _| {
        l.reactive.duration_s = (v * 10.0).round() / 10.0
    }));
    g.on_set_duration(move |v| f(v, none));
    let f = light(Rc::new(|l, _, v, _| l.mic_mute.enabled = v != 0.0));
    g.on_set_mute(move |on| f(on as i32 as f32, none));
    let weak = ui.as_weak();
    let client2 = client.clone();
    g.on_set_mute_key(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let Some(kb) = keyboard(&client2) else { return };
        let Some(code) = mute_keys(&kb).get(i as usize).map(|k| k.code) else {
            return;
        };
        let mut lighting = kb.lighting;
        lighting.mic_mute.key = code;
        act(
            &ui,
            &client2,
            Request::SetLighting { lighting },
            TIMEOUT,
            marker(&client2, mark_light, show),
        );
    });
    let f = light(Rc::new(|l, _, _, c| l.mic_mute.color = rgb(c)));
    g.on_set_mute_color(move |c| f(0.0, c));

    // Painting: a working copy until "Apply".
    let edit = |change: fn(&mut KeyboardForm, &KeyboardState, i32, Rgb)| {
        let weak = ui.as_weak();
        let client = client.clone();
        let forms = forms.clone();
        move |i: i32| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(kb) = keyboard(&client) else { return };
            let brush = rgb(ui.global::<KeyboardUi>().get_brush());
            change(&mut forms.keyboard.borrow_mut(), &kb, i, brush);
            redraw(&ui, &client, &forms);
        }
    };
    let f = edit(|f, kb, i, _| {
        if let Some(k) = kb.layout.get(i as usize) {
            f.toggle_light(k.code);
        }
    });
    g.on_key_clicked(f);
    let f = edit(|f, _, _, brush| {
        let codes: Vec<u8> = f.light_selected.iter().copied().collect();
        f.paint(codes, brush);
    });
    g.on_paint_selected(move || f(0));
    let f = edit(|f, kb, _, brush| f.paint(kb.layout.iter().map(|k| k.code), brush));
    g.on_paint_all(move || f(0));
    let f = edit(|f, _, _, _| f.light_selected.clear());
    g.on_clear_selection(move || f(0));
    let weak = ui.as_weak();
    let client2 = client.clone();
    let forms2 = forms.clone();
    g.on_apply_static(move || {
        let Some(ui) = weak.upgrade() else { return };
        let Some(kb) = keyboard(&client2) else { return };
        let lighting = forms2.keyboard.borrow().painted_lighting(&kb.lighting);
        act_then(
            &ui,
            &client2,
            Request::SetLighting { lighting },
            TIMEOUT,
            marker(&client2, mark_light, show),
            |_, forms| {
                forms.keyboard.borrow_mut().dirty = false;
            },
        );
    });
}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.keyboard.borrow_mut() = KeyboardForm::default();
}

thread_local! {
    /// Opens the brush's picker for the shot example.
    static OPENER: slint::Timer = slint::Timer::default();
}

/// The `shot` example's scripted steps for the lighting (see [`crate::poke`]).
pub(crate) fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let light = ui.global::<KeyboardUi>();
    match name {
        "keyboard.set-enabled" => light.invoke_set_enabled(n != 0),
        "keyboard.set-effect" => light.invoke_set_effect(n),
        "keyboard.set-reactive" => light.invoke_set_reactive(n != 0),
        "keyboard.set-mute" => light.invoke_set_mute(n != 0),
        "keyboard.key" => light.invoke_key_clicked(n),
        "keyboard.paint-selected" => light.invoke_paint_selected(),
        "keyboard.paint-all" => light.invoke_paint_all(),
        "keyboard.clear-selection" => light.invoke_clear_selection(),
        "keyboard.apply-static" => light.invoke_apply_static(),
        // The brush: n as 0xRRGGBB.
        "keyboard.brush" => {
            light.set_brush(Color::from_rgb_u8((n >> 16) as u8, (n >> 8) as u8, n as u8))
        }
        "keyboard.open-brush" => OPENER.with(|t| {
            // A popup asked for before the window is up isn't drawn, so wait for it.
            let weak = ui.as_weak();
            t.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(1),
                move || {
                    let Some(ui) = weak.upgrade() else { return };
                    if ui.window().is_visible() {
                        let g = ui.global::<KeyboardUi>();
                        g.set_open_brush(g.get_open_brush() + 1);
                        OPENER.with(slint::Timer::stop);
                    }
                },
            );
        }),
        _ => return false,
    }
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use gg_ipc::{Screen, ScreenCare, Switches};

    const RED: Rgb = Rgb { r: 255, g: 0, b: 0 };
    const GREEN: Rgb = Rgb { r: 0, g: 255, b: 0 };
    const BLUE: Rgb = Rgb { r: 0, g: 0, b: 255 };

    fn key(code: u8, label: &str, x: f32) -> KeyInfo {
        KeyInfo {
            code,
            label: label.into(),
            x,
            y: 0.0,
            w: 1.0,
        }
    }

    /// A, D and Esc; only A and D have adjustable switches. The switches' and
    /// the screen's tests use it too.
    pub(crate) fn board(keys: &[(u8, Rgb)], screen: Screen) -> KeyboardState {
        KeyboardState {
            connected: true,
            layout: vec![
                key(0x29, "Esc", 0.0),
                key(0x04, "A", 1.0),
                key(0x07, "D", 2.0),
            ],
            lighting: Lighting {
                enabled: true,
                effect: Effect::Static,
                keys: keys
                    .iter()
                    .map(|&(code, color)| KeyColor { code, color })
                    .collect(),
                brightness: 40,
                ..Lighting::default()
            },
            reactive: ReactiveStatus::Off,
            lighting_idle: false,
            reflect: Default::default(),
            analog_keys: vec![0x04, 0x07],
            switches: Switches::default(),
            switches_error: None,
            screen,
            screen_error: None,
            screen_care: ScreenCare::default(),
            screen_resting: false,
        }
    }

    #[test]
    fn the_paint_follows_the_daemon_until_keys_are_painted() {
        let mut f = KeyboardForm::default();
        f.follow(&board(&[(0x04, RED)], Screen::Keyboard));
        assert_eq!(f.painted, [(0x04, RED)].into());
        f.paint([0x07], BLUE);
        assert!(f.dirty);
        // A change from elsewhere doesn't undo paint not applied yet.
        f.follow(&board(&[(0x04, GREEN)], Screen::Keyboard));
        assert_eq!(f.painted, [(0x04, RED), (0x07, BLUE)].into());
        // Once applied, the daemon's colours are the ones shown again.
        f.dirty = false;
        f.follow(&board(&[(0x04, GREEN)], Screen::Keyboard));
        assert_eq!(f.painted, [(0x04, GREEN)].into());
    }

    #[test]
    fn painting_over_a_key_replaces_its_colour() {
        let mut f = KeyboardForm::default();
        f.paint([0x04, 0x07], RED);
        f.paint([0x07], BLUE);
        assert_eq!(f.painted, [(0x04, RED), (0x07, BLUE)].into());
    }

    #[test]
    fn applying_paint_makes_the_effect_static_and_keeps_the_rest() {
        let kb = board(&[], Screen::Keyboard);
        let mut lighting = kb.lighting.clone();
        lighting.effect = Effect::Wave { period_s: 3.0 };
        let mut f = KeyboardForm::default();
        f.paint([0x29, 0x04], GREEN);
        let applied = f.painted_lighting(&lighting);
        assert_eq!(applied.effect, Effect::Static);
        assert_eq!(
            applied.keys,
            vec![
                KeyColor {
                    code: 0x04,
                    color: GREEN
                },
                KeyColor {
                    code: 0x29,
                    color: GREEN
                },
            ]
        );
        assert_eq!(applied.brightness, 40);
        assert_eq!(applied.reactive, lighting.reactive);
    }

    #[test]
    fn a_second_click_lets_a_key_go() {
        let mut f = KeyboardForm::default();
        f.toggle_light(0x29);
        f.toggle_light(0x04);
        f.toggle_light(0x29);
        assert_eq!(f.light_selected, [0x04].into());
    }
}
