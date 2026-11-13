// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apex Pro TKL Gen 3: its lighting, and what the keyboard card's panels
//! share. The switches are in [`crate::switches`], the screen in
//! [`crate::screen`], key bindings in [`crate::bindings`] and the onboard
//! slots in [`crate::onboard`].
//!
//! The lighting is a stack of zones, each a set of keys with its own effect;
//! later zones paint over earlier ones. One zone is edited at a time; picking it
//! picks its keys on the keyboard picture, where keys are painted too. The idle
//! scene and Reflect have their own sections; Reflect's switch is greyed while
//! its experimental gate is off.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use gg_ipc::{
    Effect, Experimental, Feature, IdleLighting, KeyColor, KeyInfo, KeyboardState, Lighting,
    ReactiveMode, ReactiveStatus, ReflectStatus, Request, Rgb, Zone, ZoneEffect, ZoneKeys,
    ZonePreset,
};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::client::{Client, State, TIMEOUT};
use crate::{act, act_then, sync_rows, AppWindow, Forms, Key, KeyboardUi, LightZone};

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
    /// The zone being edited, by its place in the stack (bottom first).
    pub zone: usize,
    /// The keys of the zone the next "Add zone" adds: a preset's place in
    /// [`ZonePreset::ALL`], past them the picked keys.
    pub zone_to_add: usize,
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

    /// `l` with the paint, shown: lighting from before zones becomes static, as it
    /// always did; a stack with no zone showing painted keys gets its bottom every-key
    /// zone turned into one, or a new one at the bottom.
    pub fn painted_lighting(&self, l: &Lighting) -> Lighting {
        let mut l = Lighting {
            keys: self
                .painted
                .iter()
                .map(|(code, color)| KeyColor {
                    code: *code,
                    color: *color,
                })
                .collect(),
            ..l.clone()
        };
        if l.zones.is_none() {
            l.effect = Effect::Static;
        } else if !l.stack().iter().any(|z| z.effect == ZoneEffect::PerKey) {
            let zones = l.zones_mut();
            match zones.iter().position(|z| z.keys == ZoneKeys::All) {
                Some(i) => zones[i].effect = ZoneEffect::PerKey,
                None => zones.insert(0, Zone::new("Painted", ZoneKeys::All, ZoneEffect::PerKey)),
            }
        }
        l
    }

    /// Pick the zone at `i` for editing, and its keys on the picture.
    pub fn select_zone(&mut self, i: usize, kb: &KeyboardState) {
        let Some(z) = kb.lighting.stack().get(i).cloned() else {
            return;
        };
        self.zone = i;
        self.light_selected = match &z.keys {
            ZoneKeys::All => kb.layout.iter().map(|k| k.code).collect(),
            ZoneKeys::Keys { codes } => codes.iter().copied().collect(),
        };
    }

    /// The picked keys as a zone's: every key when every key of the board is picked.
    pub fn picked_keys(&self, kb: &KeyboardState) -> Option<ZoneKeys> {
        if self.light_selected.is_empty() {
            return None;
        }
        let all = kb
            .layout
            .iter()
            .all(|k| self.light_selected.contains(&k.code));
        Some(if all {
            ZoneKeys::All
        } else {
            ZoneKeys::Keys {
                codes: self.light_selected.iter().copied().collect(),
            }
        })
    }
}

/// The zone effects, in the order the panel lists them.
pub const EFFECTS: [&str; 9] = [
    "Painted keys",
    "One colour",
    "Breathing",
    "Wave",
    "Gradient",
    "Spectrum",
    "Audio",
    "Reactive",
    "Reflect",
];

/// A zone effect's place in [`EFFECTS`].
pub fn effect_index(e: &ZoneEffect) -> i32 {
    match e {
        ZoneEffect::PerKey => 0,
        ZoneEffect::Static { .. } => 1,
        ZoneEffect::Breathing { .. } => 2,
        ZoneEffect::Wave { .. } => 3,
        ZoneEffect::Gradient { .. } => 4,
        ZoneEffect::Spectrum { .. } => 5,
        ZoneEffect::Visualizer { .. } => 6,
        ZoneEffect::Reactive { .. } => 7,
        ZoneEffect::Reflect => 8,
    }
}

fn period(e: &ZoneEffect) -> Option<f32> {
    match e {
        ZoneEffect::Breathing { period_s, .. }
        | ZoneEffect::Wave { period_s }
        | ZoneEffect::Spectrum { period_s } => Some(*period_s),
        _ => None,
    }
}

/// An effect's own colour (a gradient's first).
fn first_color(e: &ZoneEffect) -> Option<Rgb> {
    match *e {
        ZoneEffect::Static { color }
        | ZoneEffect::Breathing { color, .. }
        | ZoneEffect::Reactive { color, .. } => Some(color),
        ZoneEffect::Gradient { from, .. } => Some(from),
        _ => None,
    }
}

fn set_first_color(e: &mut ZoneEffect, c: Rgb) {
    match e {
        ZoneEffect::Static { color }
        | ZoneEffect::Breathing { color, .. }
        | ZoneEffect::Reactive { color, .. } => *color = c,
        ZoneEffect::Gradient { from, .. } => *from = c,
        _ => {}
    }
}

fn invert(c: Rgb) -> Rgb {
    Rgb {
        r: 255 - c.r,
        g: 255 - c.g,
        b: 255 - c.b,
    }
}

/// Effect `i` of [`EFFECTS`] for a zone showing `now`: its colour and speed carry over
/// where both effects have one; a zone without a colour takes the brush's.
pub fn effect_for(i: i32, now: &ZoneEffect, brush: Rgb) -> ZoneEffect {
    let color = first_color(now).unwrap_or(brush);
    let period_s = period(now).unwrap_or(4.0);
    match i {
        0 => ZoneEffect::PerKey,
        1 => ZoneEffect::Static { color },
        2 => ZoneEffect::Breathing { color, period_s },
        3 => ZoneEffect::Wave { period_s },
        4 => ZoneEffect::Gradient {
            from: color,
            to: invert(color),
        },
        5 => ZoneEffect::Spectrum { period_s },
        6 => ZoneEffect::Visualizer { sensitivity: 1.0 },
        7 => ZoneEffect::Reactive {
            mode: ReactiveMode::Fade,
            color,
            duration_s: 0.6,
        },
        _ => ZoneEffect::Reflect,
    }
}

/// A zone's keys and effect in a few words, for the list.
pub fn zone_summary(z: &Zone, layout: &[KeyInfo]) -> String {
    let keys = match &z.keys {
        ZoneKeys::All => "every key".to_owned(),
        ZoneKeys::Keys { codes } => {
            let named: Vec<String> = codes
                .iter()
                .take(4)
                .map(|c| {
                    layout
                        .iter()
                        .find(|k| k.code == *c)
                        .map_or_else(|| format!("{c:#04x}"), label)
                })
                .collect();
            let more = codes.len().saturating_sub(4);
            if more > 0 {
                format!("{} and {more} more", named.join(", "))
            } else {
                named.join(", ")
            }
        }
    };
    let effect = EFFECTS[effect_index(&z.effect) as usize].to_lowercase();
    match period(&z.effect) {
        Some(p) => format!("{keys} · {effect}, {p} s"),
        None => format!("{keys} · {effect}"),
    }
}

/// The new zone's name: `base`, or `base 2`, `base 3`… if that's taken.
fn unique_name(zones: &[Zone], base: &str) -> String {
    let mut name = base.to_owned();
    let mut n = 2;
    while zones.iter().any(|z| z.name == name) {
        name = format!("{base} {n}");
        n += 1;
    }
    name
}

/// The idle scenes the panel offers: 0 off, 1 one colour, 2 breathing, 3 wave,
/// 4 spectrum. Their slow speeds are gglinux's own choice.
pub fn idle_scene(i: i32, color: Rgb) -> Vec<Zone> {
    let effect = match i {
        1 => ZoneEffect::Static { color },
        2 => ZoneEffect::Breathing {
            color,
            period_s: 6.0,
        },
        3 => ZoneEffect::Wave { period_s: 8.0 },
        4 => ZoneEffect::Spectrum { period_s: 12.0 },
        _ => return Vec::new(),
    };
    vec![Zone::new("idle", ZoneKeys::All, effect)]
}

/// Which of the offered idle scenes `idle` is; -1 for one set up some other way.
pub fn idle_scene_index(idle: &IdleLighting) -> i32 {
    match idle.zones.as_slice() {
        [] => 0,
        [z] if z.keys == ZoneKeys::All => match z.effect {
            ZoneEffect::Static { .. } => 1,
            ZoneEffect::Breathing { .. } => 2,
            ZoneEffect::Wave { .. } => 3,
            ZoneEffect::Spectrum { .. } => 4,
            _ => -1,
        },
        _ => -1,
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

/// Keys that can carry a mute light: the labelled ones.
fn mute_keys(kb: &KeyboardState) -> Vec<&KeyInfo> {
    kb.layout.iter().filter(|k| !k.label.is_empty()).collect()
}

/// `want` as a list's model, or `None` while `now` already has it (see [`names`]).
fn strings(now: ModelRc<SharedString>, want: &[&str]) -> Option<ModelRc<SharedString>> {
    now.iter()
        .ne(want.iter().map(|&w| SharedString::from(w)))
        .then(|| ModelRc::new(VecModel::from_iter(want.iter().map(|&w| w.into()))))
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
    show_zones(&g, kb, &mut f);
    show_idle_and_reflect(&g, s, kb);
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

/// The zone list and the zone being edited.
fn show_zones(g: &KeyboardUi, kb: &KeyboardState, f: &mut KeyboardForm) {
    if let Some(m) = strings(g.get_effect_names(), &EFFECTS) {
        g.set_effect_names(m);
    }
    let presets: Vec<&str> = ZonePreset::ALL
        .iter()
        .map(|p| p.label())
        .chain(["Picked keys"])
        .collect();
    if let Some(m) = strings(g.get_preset_names(), &presets) {
        g.set_preset_names(m);
    }
    let stack = kb.lighting.stack();
    f.zone = f.zone.min(stack.len().saturating_sub(1));
    let rows: Vec<LightZone> = stack
        .iter()
        .enumerate()
        .map(|(i, z)| LightZone {
            name: z.name.clone().into(),
            summary: zone_summary(z, &kb.layout).into(),
            selected: i == f.zone,
        })
        .collect();
    sync_rows(g.get_zones(), rows, |m| g.set_zones(m));
    let Some(z) = stack.get(f.zone) else {
        g.set_zone(-1);
        g.set_zone_name(SharedString::new());
        return;
    };
    g.set_zone(f.zone as i32);
    g.set_zone_name(z.name.clone().into());
    g.set_effect(effect_index(&z.effect));
    if let Some(c) = first_color(&z.effect) {
        g.set_zone_color(color(c));
    }
    if let ZoneEffect::Gradient { to, .. } = z.effect {
        g.set_zone_color_2(color(to));
    }
    g.set_has_period(period(&z.effect).is_some());
    g.set_period(period(&z.effect).unwrap_or(4.0));
    match z.effect {
        ZoneEffect::Visualizer { sensitivity } => g.set_sensitivity(sensitivity),
        ZoneEffect::Reactive {
            mode, duration_s, ..
        } => {
            g.set_zone_mode(i32::from(mode == ReactiveMode::Ripple));
            g.set_zone_duration(duration_s);
        }
        _ => {}
    }
}

/// The idle scene's section and Reflect's, greyed with its gate's sentence while the
/// gate is off.
fn show_idle_and_reflect(g: &KeyboardUi, s: &State, kb: &KeyboardState) {
    let idle = &kb.lighting.idle;
    g.set_idle(idle.enabled);
    g.set_idle_minutes(i32::from(idle.after_minutes));
    g.set_idle_scene(idle_scene_index(idle));
    if let Some(c) = idle.zones.first().and_then(|z| first_color(&z.effect)) {
        g.set_idle_color(color(c));
    }
    g.set_idle_now(kb.lighting_idle);
    let reflect = kb
        .lighting
        .stack()
        .iter()
        .any(|z| z.effect == ZoneEffect::Reflect);
    g.set_reflect(reflect);
    let allowed = s
        .experimental
        .is_some_and(|e| e.on(Feature::LightingReflect));
    g.set_reflect_allowed(allowed);
    g.set_reflect_refusal(
        Experimental::default()
            .refusal(Feature::LightingReflect)
            .unwrap_or_default()
            .into(),
    );
    g.set_reflect_status(
        match &kb.reflect {
            ReflectStatus::Off => String::new(),
            ReflectStatus::Running => "Looking at the screen.".into(),
            ReflectStatus::Stopped { why } => why.clone(),
        }
        .into(),
    );
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
    // The zone being edited: its effect starts in its own colour, or the brush's.
    let zone = |patch: fn(&mut ZoneEffect, f32, Rgb)| {
        light(Rc::new(move |l, form, v, c| {
            if let Some(z) = l.zones_mut().get_mut(form.zone) {
                patch(&mut z.effect, v, rgb(c));
            }
        }))
    };
    let f = zone(|e, v, brush| *e = effect_for(v as i32, e, brush));
    let weak = ui.as_weak();
    g.on_set_effect(move |i| {
        let brush = weak
            .upgrade()
            .map_or(none, |ui| ui.global::<KeyboardUi>().get_brush());
        f(i as f32, brush)
    });
    let f = zone(|e, _, c| set_first_color(e, c));
    g.on_set_zone_color(move |c| f(0.0, c));
    let f = zone(|e, _, c| {
        if let ZoneEffect::Gradient { to, .. } = e {
            *to = c;
        }
    });
    g.on_set_zone_color_2(move |c| f(0.0, c));
    let f = zone(|e, v, _| match e {
        ZoneEffect::Breathing { period_s, .. }
        | ZoneEffect::Wave { period_s }
        | ZoneEffect::Spectrum { period_s } => *period_s = v,
        _ => {}
    });
    g.on_set_period(move |v| f(v, none));
    let f = zone(|e, v, _| {
        if let ZoneEffect::Visualizer { sensitivity } = e {
            *sensitivity = v;
        }
    });
    g.on_set_sensitivity(move |v| f(v, none));
    let f = zone(|e, v, _| {
        if let ZoneEffect::Reactive { mode, .. } = e {
            *mode = if v as i32 == 0 {
                ReactiveMode::Fade
            } else {
                ReactiveMode::Ripple
            };
        }
    });
    g.on_set_zone_mode(move |i| f(i as f32, none));
    let f = zone(|e, v, _| {
        if let ZoneEffect::Reactive { duration_s, .. } = e {
            *duration_s = (v * 10.0).round() / 10.0;
        }
    });
    g.on_set_zone_duration(move |v| f(v, none));
    bind_zone_list(ui, client, forms);

    // The idle scene and Reflect.
    let f = light(Rc::new(|l, _, v, _| l.idle.enabled = v != 0.0));
    g.on_set_idle(move |on| f(on as i32 as f32, none));
    let f = light(Rc::new(|l, _, v, _| {
        l.idle.after_minutes = v.clamp(0.0, 120.0) as u16
    }));
    g.on_set_idle_minutes(move |m| f(m as f32, none));
    let f = light(Rc::new(|l, _, v, c| {
        let keep = l.idle.zones.first().and_then(|z| first_color(&z.effect));
        l.idle.zones = idle_scene(v as i32, keep.unwrap_or(rgb(c)));
    }));
    let weak = ui.as_weak();
    g.on_set_idle_scene(move |i| {
        let brush = weak
            .upgrade()
            .map_or(none, |ui| ui.global::<KeyboardUi>().get_brush());
        f(i as f32, brush)
    });
    let f = light(Rc::new(|l, _, _, c| {
        if let Some(z) = l.idle.zones.first_mut() {
            set_first_color(&mut z.effect, rgb(c));
        }
    }));
    g.on_set_idle_color(move |c| f(0.0, c));
    let f = light(Rc::new(|l, _, v, _| {
        if v != 0.0 {
            l.reflect_on();
            l.enabled = true;
        } else {
            l.reflect_off();
        }
    }));
    g.on_set_reflect(move |on| f(on as i32 as f32, none));
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

/// Picking, adding, moving and removing zones, and giving one the picked keys.
fn bind_zone_list(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    let g = ui.global::<KeyboardUi>();
    let weak = ui.as_weak();
    let client2 = client.clone();
    let forms2 = forms.clone();
    g.on_select_zone(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let Some(kb) = keyboard(&client2) else { return };
        forms2.keyboard.borrow_mut().select_zone(i as usize, &kb);
        redraw(&ui, &client2, &forms2);
    });

    // A change to the stack, then what the panel edits once the daemon has it.
    type Change = fn(&mut Lighting, &KeyboardForm, &KeyboardState, Rgb) -> Result<(), String>;
    let stack = |change: Change, then: fn(&AppWindow, &Forms)| {
        let weak = ui.as_weak();
        let client = client.clone();
        let forms = forms.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(kb) = keyboard(&client) else { return };
            let brush = rgb(ui.global::<KeyboardUi>().get_brush());
            let mut lighting = kb.lighting.clone();
            if let Err(e) = change(&mut lighting, &forms.keyboard.borrow(), &kb, brush) {
                mark_light(&ui, false, e.into());
                return;
            }
            act_then(
                &ui,
                &client,
                Request::SetLighting { lighting },
                TIMEOUT,
                marker(&client, mark_light, show),
                then,
            );
        }
    };
    let add = stack(
        |l, f, kb, brush| {
            let (name, keys) = match ZonePreset::ALL.get(f.zone_to_add) {
                Some(p) => (p.label(), p.keys()),
                None => (
                    "Picked keys",
                    f.picked_keys(kb)
                        .ok_or("Pick keys on the keyboard picture first.")?,
                ),
            };
            let zones = l.zones_mut();
            let name = unique_name(zones, name);
            zones.push(Zone::new(name, keys, ZoneEffect::Static { color: brush }));
            l.enabled = true;
            Ok(())
        },
        |_, forms| forms.keyboard.borrow_mut().zone = usize::MAX, // the new one, on top
    );
    let forms2 = forms.clone();
    g.on_add_zone(move |i| {
        forms2.keyboard.borrow_mut().zone_to_add = i.max(0) as usize;
        add();
    });
    let f = stack(
        |l, f, _, _| {
            let zones = l.zones_mut();
            if f.zone >= zones.len() {
                return Err("No zone is picked.".into());
            }
            zones.remove(f.zone);
            Ok(())
        },
        |_, _| {},
    );
    g.on_remove_zone(f);
    let up = stack(
        |l, f, _, _| {
            let zones = l.zones_mut();
            if f.zone + 1 >= zones.len() {
                return Err("It's the top zone already.".into());
            }
            zones.swap(f.zone, f.zone + 1);
            Ok(())
        },
        |_, forms| forms.keyboard.borrow_mut().zone += 1,
    );
    let down = stack(
        |l, f, _, _| {
            let zones = l.zones_mut();
            if f.zone == 0 || f.zone >= zones.len() {
                return Err("It's the bottom zone already.".into());
            }
            zones.swap(f.zone, f.zone - 1);
            Ok(())
        },
        |_, forms| {
            let mut f = forms.keyboard.borrow_mut();
            f.zone = f.zone.saturating_sub(1);
        },
    );
    g.on_move_zone(move |is_up| if is_up { up() } else { down() });
    let f = stack(
        |l, f, kb, _| {
            let keys = f
                .picked_keys(kb)
                .ok_or("Pick keys on the keyboard picture first.")?;
            let z = l.zones_mut().get_mut(f.zone).ok_or("No zone is picked.")?;
            z.keys = keys;
            Ok(())
        },
        |_, _| {},
    );
    g.on_use_picked_keys(f);
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
        "keyboard.select-zone" => light.invoke_select_zone(n),
        "keyboard.add-zone" => light.invoke_add_zone(n),
        "keyboard.remove-zone" => light.invoke_remove_zone(),
        "keyboard.move-zone" => light.invoke_move_zone(n != 0),
        "keyboard.use-picked-keys" => light.invoke_use_picked_keys(),
        "keyboard.set-idle" => light.invoke_set_idle(n != 0),
        "keyboard.set-idle-scene" => light.invoke_set_idle_scene(n),
        "keyboard.set-reflect" => light.invoke_set_reflect(n != 0),
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
    #[test]
    fn every_effect_in_the_list_comes_back_to_its_place_keeping_the_zone_s_colour_and_speed() {
        let breathing = ZoneEffect::Breathing {
            color: RED,
            period_s: 3.0,
        };
        for i in 0..EFFECTS.len() as i32 {
            assert_eq!(effect_index(&effect_for(i, &breathing, BLUE)), i);
        }
        assert_eq!(
            effect_for(1, &breathing, BLUE),
            ZoneEffect::Static { color: RED }
        );
        assert_eq!(
            effect_for(3, &breathing, BLUE),
            ZoneEffect::Wave { period_s: 3.0 }
        );
        assert_eq!(
            effect_for(1, &ZoneEffect::PerKey, BLUE),
            ZoneEffect::Static { color: BLUE },
            "no colour of its own: the brush's"
        );
    }

    #[test]
    fn applying_paint_to_zones_makes_a_zone_show_it() {
        let kb = board(&[], Screen::Keyboard);
        let mut f = KeyboardForm::default();
        f.paint([0x04], GREEN);
        let w = Zone::new(
            "w",
            ZoneKeys::Keys { codes: vec![0x1A] },
            ZoneEffect::Static { color: RED },
        );
        let mut l = kb.lighting.clone();
        *l.zones_mut() = vec![
            Zone::new("all", ZoneKeys::All, ZoneEffect::Wave { period_s: 4.0 }),
            w.clone(),
        ];
        let applied = f.painted_lighting(&l);
        assert_eq!(
            applied.stack()[0].effect,
            ZoneEffect::PerKey,
            "the every-key zone"
        );
        assert_eq!(applied.stack()[1], w, "the others as they were");
        *l.zones_mut() = vec![w];
        let applied = f.painted_lighting(&l);
        assert_eq!(
            applied.stack()[0],
            Zone::new("Painted", ZoneKeys::All, ZoneEffect::PerKey),
            "none covers every key: a new one at the bottom"
        );
        assert_eq!(f.painted_lighting(&applied).stack().len(), 2, "only once");
    }

    #[test]
    fn picking_a_zone_picks_its_keys_and_every_key_picked_is_every_key() {
        let mut kb = board(&[], Screen::Keyboard);
        *kb.lighting.zones_mut() = vec![
            Zone::new("all", ZoneKeys::All, ZoneEffect::PerKey),
            Zone::new(
                "a",
                ZoneKeys::Keys { codes: vec![0x04] },
                ZoneEffect::Static { color: RED },
            ),
        ];
        let mut f = KeyboardForm::default();
        f.select_zone(1, &kb);
        assert_eq!(f.zone, 1);
        assert_eq!(f.light_selected, [0x04].into());
        f.select_zone(0, &kb);
        assert_eq!(f.light_selected, [0x29, 0x04, 0x07].into());
        assert_eq!(f.picked_keys(&kb), Some(ZoneKeys::All));
        f.toggle_light(0x29);
        assert_eq!(
            f.picked_keys(&kb),
            Some(ZoneKeys::Keys {
                codes: vec![0x04, 0x07]
            })
        );
        f.select_zone(9, &kb);
        assert_eq!(f.zone, 0, "no such zone: nothing changes");
        f.light_selected.clear();
        assert_eq!(f.picked_keys(&kb), None);
    }

    #[test]
    fn the_list_says_each_zone_s_keys_and_effect_in_words() {
        let layout = board(&[], Screen::Keyboard).layout;
        let z = Zone::new(
            "a",
            ZoneKeys::Keys {
                codes: vec![0x04, 0x07],
            },
            ZoneEffect::Wave { period_s: 4.0 },
        );
        assert_eq!(zone_summary(&z, &layout), "A, D · wave, 4 s");
        let many = Zone::new(
            "m",
            ZoneKeys::Keys {
                codes: (0x04..0x0A).collect(),
            },
            ZoneEffect::Static { color: RED },
        );
        assert_eq!(
            zone_summary(&many, &layout),
            "A, 0x05, 0x06, D and 2 more · one colour"
        );
        let screen = Zone::new("x", ZoneKeys::All, ZoneEffect::Reflect);
        assert_eq!(zone_summary(&screen, &layout), "every key · reflect");
        let zones = vec![Zone::new("WASD", ZoneKeys::All, ZoneEffect::PerKey)];
        assert_eq!(unique_name(&zones, "WASD"), "WASD 2");
        assert_eq!(unique_name(&zones, "Arrows"), "Arrows");
    }

    #[test]
    fn the_idle_scenes_offered_come_back_as_themselves() {
        for i in 0..5 {
            let idle = IdleLighting {
                zones: idle_scene(i, RED),
                ..IdleLighting::default()
            };
            assert_eq!(idle_scene_index(&idle), i);
        }
        let custom = IdleLighting {
            zones: vec![Zone::new(
                "w",
                ZonePreset::Wasd.keys(),
                ZoneEffect::Static { color: RED },
            )],
            ..IdleLighting::default()
        };
        assert_eq!(idle_scene_index(&custom), -1, "set up with ggctl");
    }
}
