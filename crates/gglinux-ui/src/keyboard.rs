// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apex Pro TKL Gen 3: lighting, switches and the screen.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use gg_ipc::{
    Effect, KeyActuation, KeyColor, KeyInfo, KeyboardState, Lighting, ReactiveMode, ReactiveStatus,
    Request, Rgb, Screen, ScreenCare, Switches, TapMode, TapPair,
};
use slint::{
    Color, ComponentHandle, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, SharedString, VecModel,
};

use crate::client::{Client, State, TIMEOUT};
use crate::{
    act, act_then, sync_rows, AppWindow, Forms, Key, KeyboardUi, ScreenUi, SwitchesUi, TapRow,
};

/// The screen: 128 × 40, one bit a pixel, 16 bytes a row.
pub const W: usize = 128;
pub const H: usize = 40;
const MAX_PAIRS: usize = 10;
const TAP_MODES: [TapMode; 4] = [
    TapMode::LastWins,
    TapMode::Neither,
    TapMode::FirstWins,
    TapMode::SecondWins,
];

/// A change to the lighting, from a control's value or colour.
type LightPatch = Rc<dyn Fn(&mut Lighting, &KeyboardForm, f32, Color)>;
/// A change to the switches, from a control's values.
type SwitchPatch = Rc<dyn Fn(&mut Switches, &KeyboardState, &KeyboardForm, [i32; 4], i32)>;

/// What the keyboard panel keeps between the daemon's answers.
#[derive(Default)]
pub struct KeyboardForm {
    /// Static colours being painted, by HID code. They follow the daemon's
    /// unless there's paint not applied yet.
    pub painted: BTreeMap<u8, Rgb>,
    pub dirty: bool,
    pub light_selected: BTreeSet<u8>,
    pub switch_selected: BTreeSet<u8>,
    /// The screen tab picked but not sent yet (text and pictures wait for "Show").
    pub picked: Option<i32>,
    pub text: String,
    /// The daemon's text last seen, so its own change shows and a refresh doesn't
    /// undo typing.
    seen_text: Option<String>,
    pub picture: Option<Picture>,
}

fn flip(set: &mut BTreeSet<u8>, code: u8) {
    if !set.remove(&code) {
        set.insert(code);
    }
}

impl KeyboardForm {
    /// Take in what the daemon has. The paint follows it unless there's paint
    /// not applied yet; the text follows only when the daemon's own changes, so
    /// a refresh doesn't undo typing.
    pub fn follow(&mut self, kb: &KeyboardState) {
        if !self.dirty {
            self.painted = kb.lighting.keys.iter().map(|k| (k.code, k.color)).collect();
        }
        if let Screen::Text { text } = &kb.screen {
            if self.seen_text.as_ref() != Some(text) {
                self.seen_text = Some(text.clone());
                self.text = text.clone();
            }
        }
    }

    /// Select a key on the lighting picture, or let it go.
    pub fn toggle_light(&mut self, code: u8) {
        flip(&mut self.light_selected, code);
    }

    /// Select a key on the switches picture, or let it go. Only keys with
    /// adjustable switches can be picked.
    pub fn toggle_switch(&mut self, kb: &KeyboardState, code: u8) {
        if kb.analog_keys.contains(&code) {
            flip(&mut self.switch_selected, code);
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

/// A picture chosen for the screen, and how it's turned into 1-bit pixels.
pub struct Picture {
    pub name: String,
    pub rgba: image::RgbaImage,
    pub threshold: u8,
    pub dither: bool,
    pub invert: bool,
    pub pixels: Vec<u8>,
}

impl Picture {
    pub fn new(name: String, rgba: image::RgbaImage) -> Picture {
        let mut p = Picture {
            name,
            rgba,
            threshold: 128,
            dither: true,
            invert: false,
            pixels: Vec::new(),
        };
        p.convert();
        p
    }

    pub fn convert(&mut self) {
        self.pixels = convert(&self.rgba, self.threshold, self.invert, self.dither);
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

const SURFACE_2: Color = Color::from_rgb_u8(28, 32, 39);
const TEXT_DIM: Color = Color::from_rgb_u8(139, 147, 159);
const TEXT: Color = Color::from_rgb_u8(230, 233, 238);

fn label(k: &KeyInfo) -> String {
    if k.label.is_empty() {
        "Space".into()
    } else {
        k.label.clone()
    }
}

fn mm(tenths: u8) -> String {
    format!("{:.1}", tenths as f32 / 10.0)
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

/// Keys with adjustable switches, in layout order.
fn analog_keys(kb: &KeyboardState) -> Vec<&KeyInfo> {
    kb.layout
        .iter()
        .filter(|k| kb.analog_keys.contains(&k.code))
        .collect()
}

fn rapid_on(sw: &Switches, code: u8) -> bool {
    sw.rapid_trigger.enabled
        && (sw.rapid_trigger.keys.is_empty() || sw.rapid_trigger.keys.contains(&code))
}

/// The keys' names for a list, or `None` while `now` already has them. A new
/// list makes a list box pick its first entry again, so it's only set on a change.
fn names(now: ModelRc<SharedString>, keys: &[&KeyInfo]) -> Option<ModelRc<SharedString>> {
    let names: Vec<SharedString> = keys.iter().map(|k| label(k).into()).collect();
    now.iter()
        .ne(names.iter().cloned())
        .then(|| ModelRc::new(VecModel::from(names)))
}

pub fn show(ui: &AppWindow, s: &State, forms: &Forms) {
    let Some(kb) = s.keyboard.as_ref() else {
        // The daemon went (or hasn't answered yet): what was being edited goes
        // with it, as the old panel's did, and so do errors from before.
        *forms.keyboard.borrow_mut() = KeyboardForm::default();
        let g = ui.global::<KeyboardUi>();
        g.set_known(false);
        g.set_connected(false);
        mark_light(ui, false, SharedString::new());
        mark_switches(ui, false, SharedString::new());
        mark_screen(ui, false, SharedString::new());
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
    show_switches(ui, kb, &f);
    show_screen(ui, kb, &f);
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

fn show_switches(ui: &AppWindow, kb: &KeyboardState, f: &KeyboardForm) {
    let sw = &kb.switches;
    let own: BTreeMap<u8, u8> = sw.keys.iter().map(|k| (k.code, k.actuation)).collect();
    let tint = Color::from_argb_u8(64, 255, 122, 41);
    let keys: Vec<Key> = kb
        .layout
        .iter()
        .map(|k| {
            let analog = kb.analog_keys.contains(&k.code);
            Key {
                x: k.x,
                y: k.y,
                w: k.w,
                label: k.label.clone().into(),
                sub: if analog {
                    mm(own.get(&k.code).copied().unwrap_or(sw.actuation)).into()
                } else {
                    SharedString::new()
                },
                fill: if analog && rapid_on(sw, k.code) {
                    tint
                } else {
                    SURFACE_2
                },
                ink: if analog { TEXT } else { TEXT_DIM },
                selected: f.switch_selected.contains(&k.code),
                fixed: !analog,
                own: own.contains_key(&k.code),
            }
        })
        .collect();
    let analog = analog_keys(kb);
    let at = |code: u8| {
        analog
            .iter()
            .position(|k| k.code == code)
            .map_or(-1, |i| i as i32)
    };
    let pairs: Vec<TapRow> = sw
        .rapid_tap
        .pairs
        .iter()
        .map(|p| TapRow {
            a: at(p.a),
            b: at(p.b),
            mode: TAP_MODES.iter().position(|m| *m == p.mode).unwrap_or(0) as i32,
            both: p.report_both,
        })
        .collect();
    let g = ui.global::<SwitchesUi>();
    g.set_enabled(sw.enabled);
    g.set_actuation(sw.actuation as i32);
    sync_rows(g.get_keys(), keys, |m| g.set_keys(m));
    g.set_selected(f.switch_selected.len() as i32);
    g.set_rapid(sw.rapid_trigger.enabled);
    g.set_rapid_distance(sw.rapid_trigger.sensitivity as i32);
    g.set_rapid_some(!sw.rapid_trigger.keys.is_empty());
    g.set_rapid_keys(
        match sw.rapid_trigger.keys.len() {
            0 => "every adjustable key".to_string(),
            1 => "1 key".to_string(),
            n => format!("{n} keys"),
        }
        .into(),
    );
    g.set_tap(sw.rapid_tap.enabled);
    if let Some(m) = names(g.get_key_names(), &analog) {
        g.set_key_names(m);
    }
    sync_rows(g.get_pairs(), pairs, |m| g.set_pairs(m));
    g.set_refused(kb.switches_error.clone().unwrap_or_default().into());
}

fn screen_kind(s: &Screen) -> i32 {
    match s {
        Screen::Keyboard => 0,
        Screen::Text { .. } => 1,
        Screen::Clock { .. } => 2,
        Screen::Image { .. } => 3,
        // Shown as a picture until the screen unit (M11 A4) gives them their own modes.
        Screen::Animation { .. } => 3,
        Screen::Monitor { .. } => 0,
    }
}

fn clock_of(s: &Screen) -> (bool, bool, bool) {
    match s {
        Screen::Clock { seconds, h24, date } => (*h24, *seconds, *date),
        _ => (true, false, true),
    }
}

/// Nothing to show but spaces.
pub fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// More than the screen's 5 lines of 16 characters.
pub fn too_long(text: &str) -> bool {
    let lines: Vec<&str> = text.split('\n').collect();
    lines.len() > Screen::MAX_LINES
        || lines
            .iter()
            .any(|l| l.chars().count() > Screen::MAX_COLUMNS)
}

fn show_screen(ui: &AppWindow, kb: &KeyboardState, f: &KeyboardForm) {
    let g = ui.global::<ScreenUi>();
    g.set_kind(f.picked.unwrap_or(screen_kind(&kb.screen)));
    if g.get_text().as_str() != f.text {
        g.set_text(f.text.clone().into());
    }
    g.set_too_long(too_long(&f.text));
    g.set_blank(blank(&f.text));
    let (h24, seconds, date) = clock_of(&kb.screen);
    g.set_h24(h24);
    g.set_seconds(seconds);
    g.set_date(date);
    show_picture(ui, kb, f);
    let c = kb.screen_care;
    g.set_care(true);
    g.set_shift(c.shift);
    g.set_away(c.dark_when_away);
    g.set_own_too(c.own_screen_too);
    g.set_resting(kb.screen_resting);
    g.set_refused(kb.screen_error.clone().unwrap_or_default().into());
}

fn show_picture(ui: &AppWindow, kb: &KeyboardState, f: &KeyboardForm) {
    let g = ui.global::<ScreenUi>();
    let bits = match (&f.picture, &kb.screen) {
        (Some(p), _) => Some(p.pixels.as_slice()),
        (None, Screen::Image { pixels }) => Some(pixels.as_slice()),
        _ => None,
    };
    g.set_has_preview(bits.is_some());
    if let Some(bits) = bits {
        g.set_preview(preview(bits));
    }
    g.set_has_source(f.picture.is_some());
    if let Some(p) = &f.picture {
        g.set_file_name(p.name.clone().into());
        g.set_threshold(p.threshold as i32);
        g.set_dither(p.dither);
        g.set_invert(p.invert);
    }
}

/// The 1-bit picture as the screen would show it.
pub fn preview(bits: &[u8]) -> slint::Image {
    let mut buf = SharedPixelBuffer::<Rgb8Pixel>::new(W as u32, H as u32);
    for (i, px) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i % W, i / W);
        let on = bits
            .get(y * 16 + x / 8)
            .is_some_and(|b| b & (0x80 >> (x % 8)) != 0);
        let v = if on { 235 } else { 10 };
        *px = Rgb8Pixel { r: v, g: v, b: v };
    }
    slint::Image::from_rgb8(buf)
}

/// A picture fitted into 128 × 40 on black, one bit a pixel, 16 bytes a row,
/// the leftmost pixel in each byte's high bit. Floyd–Steinberg dithering keeps
/// the greys.
pub fn convert(img: &image::RgbaImage, threshold: u8, invert: bool, dither: bool) -> Vec<u8> {
    let (iw, ih) = img.dimensions();
    let mut lum = vec![0f32; W * H];
    if iw > 0 && ih > 0 {
        let scale = (W as f32 / iw as f32).min(H as f32 / ih as f32);
        let (w, h) = (
            ((iw as f32 * scale).round() as u32).clamp(1, W as u32),
            ((ih as f32 * scale).round() as u32).clamp(1, H as u32),
        );
        let small = image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle);
        let (ox, oy) = ((W as u32 - w) / 2, (H as u32 - h) / 2);
        for (x, y, p) in small.enumerate_pixels() {
            // Transparent parts are on black, as the canvas drew them.
            let a = p[3] as f32 / 255.0;
            let l = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) * a;
            lum[(y + oy) as usize * W + (x + ox) as usize] = l;
        }
    }
    let mut bits = vec![0u8; Screen::IMAGE_BYTES];
    for y in 0..H {
        for x in 0..W {
            let i = y * W + x;
            let on = lum[i] >= threshold as f32;
            if dither {
                let err = lum[i] - if on { 255.0 } else { 0.0 };
                if x + 1 < W {
                    lum[i + 1] += err * 7.0 / 16.0;
                }
                if y + 1 < H {
                    if x > 0 {
                        lum[i + W - 1] += err * 3.0 / 16.0;
                    }
                    lum[i + W] += err * 5.0 / 16.0;
                    if x + 1 < W {
                        lum[i + W + 1] += err / 16.0;
                    }
                }
            }
            if on != invert {
                bits[y * 16 + (x >> 3)] |= 0x80 >> (x & 7);
            }
        }
    }
    bits
}

/// Rapid Tap's first pair: A and D (counter-strafing), else the first two
/// adjustable keys in no pair.
pub fn next_pair(analog: &[u8], pairs: &[TapPair]) -> Option<TapPair> {
    let used: BTreeSet<u8> = pairs.iter().flat_map(|p| [p.a, p.b]).collect();
    let (a, b) = if !used.contains(&0x04) && !used.contains(&0x07) {
        (0x04, 0x07)
    } else {
        let mut free = analog.iter().copied().filter(|c| !used.contains(c));
        (free.next()?, free.next()?)
    };
    Some(TapPair {
        a,
        b,
        mode: TapMode::LastWins,
        report_both: false,
    })
}

/// The selected keys get their own point (`Some`), or the default again.
pub fn with_own(sw: &Switches, selected: &BTreeSet<u8>, level: Option<u8>) -> Vec<KeyActuation> {
    let mut keys: Vec<KeyActuation> = sw
        .keys
        .iter()
        .filter(|k| !selected.contains(&k.code))
        .copied()
        .collect();
    if let Some(actuation) = level {
        keys.extend(selected.iter().map(|code| KeyActuation {
            code: *code,
            actuation,
        }));
    }
    keys
}

/// Change Rapid Tap pair `i`: its keys are at `a` and `b` in `analog` (the
/// adjustable keys, as listed), its mode at `mode` in the modes listed. A place
/// off a list keeps what the pair had.
pub fn set_pair(sw: &mut Switches, analog: &[u8], i: i32, a: i32, b: i32, mode: i32, both: bool) {
    let Some(p) = usize::try_from(i)
        .ok()
        .and_then(|i| sw.rapid_tap.pairs.get_mut(i))
    else {
        return;
    };
    let at = |v: i32| usize::try_from(v).ok();
    p.a = at(a).and_then(|a| analog.get(a)).copied().unwrap_or(p.a);
    p.b = at(b).and_then(|b| analog.get(b)).copied().unwrap_or(p.b);
    p.mode = at(mode)
        .and_then(|m| TAP_MODES.get(m))
        .copied()
        .unwrap_or(p.mode);
    p.report_both = both;
}

fn keyboard(client: &Client) -> Option<KeyboardState> {
    client.state().keyboard
}

fn mark_light(ui: &AppWindow, busy: bool, error: SharedString) {
    let g = ui.global::<KeyboardUi>();
    g.set_busy(busy);
    g.set_error(error);
}

fn mark_switches(ui: &AppWindow, busy: bool, error: SharedString) {
    let g = ui.global::<SwitchesUi>();
    g.set_busy(busy);
    g.set_error(error);
}

fn mark_screen(ui: &AppWindow, busy: bool, error: SharedString) {
    let g = ui.global::<ScreenUi>();
    g.set_busy(busy);
    g.set_error(error);
}

/// The `mark` for a panel's requests (see [`act`]). A list box keeps showing a
/// pick the daemon refused, so after a failure the card is drawn again from what
/// the daemon has.
fn marker(
    client: &Client,
    set: fn(&AppWindow, bool, SharedString),
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
                marker(&client, mark_light),
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
            marker(&client2, mark_light),
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
            marker(&client2, mark_light),
            |_, forms| {
                forms.keyboard.borrow_mut().dirty = false;
            },
        );
    });

    bind_switches(ui, client, forms);
    bind_screen(ui, client, forms);
}

fn bind_switches(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    let g = ui.global::<SwitchesUi>();
    // Like the lighting: each change sends all the switch settings, so the
    // controls wait while one is in flight.
    let switches = |patch: SwitchPatch| {
        let weak = ui.as_weak();
        let client = client.clone();
        let forms = forms.clone();
        move |args: [i32; 4], level: i32| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(kb) = keyboard(&client) else { return };
            let mut sw = kb.switches.clone();
            patch(&mut sw, &kb, &forms.keyboard.borrow(), args, level);
            act(
                &ui,
                &client,
                Request::SetSwitches { switches: sw },
                TIMEOUT,
                marker(&client, mark_switches),
            );
        }
    };
    let f = switches(Rc::new(|sw, _, _, a, _| sw.enabled = a[0] != 0));
    g.on_set_enabled(move |on| f([on as i32, 0, 0, 0], 0));
    let f = switches(Rc::new(|sw, _, _, a, _| {
        sw.actuation = a[0].clamp(1, 40) as u8
    }));
    g.on_set_actuation(move |v| f([v, 0, 0, 0], 0));
    let weak = ui.as_weak();
    let f = switches(Rc::new(|sw, _, form, a, level| {
        sw.keys = with_own(
            sw,
            &form.switch_selected,
            (a[0] != 0).then_some(level.clamp(1, 40) as u8),
        );
    }));
    g.on_set_own(move |own| {
        let level = weak
            .upgrade()
            .map_or(15, |ui| ui.global::<SwitchesUi>().get_own_level());
        f([own as i32, 0, 0, 0], level)
    });
    let f = switches(Rc::new(|sw, _, _, a, _| {
        sw.rapid_trigger.enabled = a[0] != 0
    }));
    g.on_set_rapid(move |on| f([on as i32, 0, 0, 0], 0));
    let f = switches(Rc::new(|sw, _, _, a, _| {
        sw.rapid_trigger.sensitivity = a[0].clamp(1, 20) as u8
    }));
    g.on_set_rapid_distance(move |v| f([v, 0, 0, 0], 0));
    let f = switches(Rc::new(|sw, _, form, _, _| {
        sw.rapid_trigger.keys = form.switch_selected.iter().copied().collect()
    }));
    g.on_rapid_only_selected(move || f([0; 4], 0));
    let f = switches(Rc::new(|sw, _, _, _, _| sw.rapid_trigger.keys.clear()));
    g.on_rapid_every_key(move || f([0; 4], 0));
    let f = switches(Rc::new(|sw, _, _, a, _| sw.rapid_tap.enabled = a[0] != 0));
    g.on_set_tap(move |on| f([on as i32, 0, 0, 0], 0));
    let f = switches(Rc::new(|sw, kb, _, a, both| {
        let analog: Vec<u8> = analog_keys(kb).iter().map(|k| k.code).collect();
        set_pair(sw, &analog, a[0], a[1], a[2], a[3], both != 0);
    }));
    g.on_set_pair(move |i, a, b, mode, both| f([i, a, b, mode], both as i32));
    let f = switches(Rc::new(|sw, kb, _, _, _| {
        if sw.rapid_tap.pairs.len() < MAX_PAIRS {
            if let Some(p) = next_pair(&kb.analog_keys, &sw.rapid_tap.pairs) {
                sw.rapid_tap.pairs.push(p);
            }
        }
    }));
    g.on_add_pair(move || f([0; 4], 0));
    let f = switches(Rc::new(|sw, _, _, a, _| {
        if (a[0] as usize) < sw.rapid_tap.pairs.len() {
            sw.rapid_tap.pairs.remove(a[0] as usize);
        }
    }));
    g.on_remove_pair(move |i| f([i, 0, 0, 0], 0));

    let weak = ui.as_weak();
    let client2 = client.clone();
    let forms2 = forms.clone();
    g.on_key_clicked(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let Some(kb) = keyboard(&client2) else { return };
        if let Some(k) = kb.layout.get(i as usize) {
            forms2.keyboard.borrow_mut().toggle_switch(&kb, k.code);
        }
        redraw(&ui, &client2, &forms2);
    });
    let weak = ui.as_weak();
    let client2 = client.clone();
    let forms2 = forms.clone();
    g.on_clear_selection(move || {
        forms2.keyboard.borrow_mut().switch_selected.clear();
        if let Some(ui) = weak.upgrade() {
            redraw(&ui, &client2, &forms2);
        }
    });
}

/// A picture as if it was chosen: a grey ramp behind a dark ring, so the
/// dithering shows (the shot example's step; the portal needs a desktop).
fn test_picture() -> Picture {
    let img = image::RgbaImage::from_fn(256, 80, |x, y| {
        let (dx, dy) = (x as f32 - 128.0, (y as f32 - 40.0) * 2.0);
        let r = (dx * dx + dy * dy).sqrt();
        let v = if (48.0..64.0).contains(&r) {
            0
        } else {
            x as u8
        };
        image::Rgba([v, v, v, 255])
    });
    Picture::new("ramp.png".into(), img)
}

fn bind_screen(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    let g = ui.global::<ScreenUi>();
    let send = {
        let weak = ui.as_weak();
        let client = client.clone();
        move |screen: Screen| {
            let Some(ui) = weak.upgrade() else { return };
            act_then(
                &ui,
                &client,
                Request::SetScreen { screen },
                TIMEOUT,
                marker(&client, mark_screen),
                |_, forms| {
                    forms.keyboard.borrow_mut().picked = None;
                },
            );
        }
    };
    let weak = ui.as_weak();
    let client2 = client.clone();
    let forms2 = forms.clone();
    let send2 = send.clone();
    g.on_pick(move |k| {
        let Some(ui) = weak.upgrade() else { return };
        forms2.keyboard.borrow_mut().picked = Some(k);
        redraw(&ui, &client2, &forms2);
        // The keyboard's own screen and the clock are shown at once; text and
        // pictures wait for "Show".
        let current = keyboard(&client2).map(|kb| kb.screen).unwrap_or_default();
        match k {
            0 => send2(Screen::Keyboard),
            2 => {
                let (h24, seconds, date) = clock_of(&current);
                send2(Screen::Clock { seconds, h24, date });
            }
            _ => {}
        }
    });
    let weak = ui.as_weak();
    let forms2 = forms.clone();
    g.on_text_edited(move |t| {
        forms2.keyboard.borrow_mut().text = t.to_string();
        if let Some(ui) = weak.upgrade() {
            let g = ui.global::<ScreenUi>();
            g.set_too_long(too_long(t.as_str()));
            g.set_blank(blank(t.as_str()));
        }
    });
    let forms2 = forms.clone();
    let send2 = send.clone();
    g.on_show_text(move || {
        let text = forms2.keyboard.borrow().text.clone();
        send2(Screen::Text { text });
    });
    let send2 = send.clone();
    g.on_set_clock(move |h24, seconds, date| send2(Screen::Clock { seconds, h24, date }));
    let forms2 = forms.clone();
    let send2 = send.clone();
    g.on_show_picture(move || {
        let pixels = forms2
            .keyboard
            .borrow()
            .picture
            .as_ref()
            .map(|p| p.pixels.clone());
        if let Some(pixels) = pixels {
            send2(Screen::Image { pixels });
        }
    });
    // The threshold follows the slider as it's dragged, so only the preview is
    // drawn again, and only when something changed.
    let picture = |change: fn(&mut Picture, i32)| {
        let weak = ui.as_weak();
        let client = client.clone();
        let forms = forms.clone();
        move |v: i32| {
            let Some(ui) = weak.upgrade() else { return };
            let mut f = forms.keyboard.borrow_mut();
            let Some(p) = f.picture.as_mut() else { return };
            let before = (p.threshold, p.dither, p.invert);
            change(p, v);
            if before == (p.threshold, p.dither, p.invert) {
                return;
            }
            p.convert();
            if let Some(kb) = keyboard(&client) {
                show_picture(&ui, &kb, &f);
            }
        }
    };
    g.on_set_threshold(picture(|p, v| p.threshold = v.clamp(1, 254) as u8));
    let f = picture(|p, v| p.dither = v != 0);
    g.on_set_dither(move |on| f(on as i32));
    let f = picture(|p, v| p.invert = v != 0);
    g.on_set_invert(move |on| f(on as i32));

    let weak = ui.as_weak();
    let client2 = client.clone();
    g.on_choose_picture(move || {
        let weak = weak.clone();
        client2.spawn(async move {
            let loaded = match crate::system::pick_picture().await {
                Ok(Some(path)) => tokio::task::spawn_blocking(move || {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    image::open(&path)
                        .map(|img| (name, img.to_rgba8()))
                        .map_err(|_| "That file isn't a picture this app can read.".to_string())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string())),
                Ok(None) => return,
                Err(e) => Err(e),
            };
            let _ = weak.upgrade_in_event_loop(move |ui| match loaded {
                Ok((name, rgba)) => {
                    crate::with_forms(|forms| {
                        forms.keyboard.borrow_mut().picture = Some(Picture::new(name, rgba));
                    });
                    ui.global::<ScreenUi>().set_error(SharedString::new());
                    crate::refresh(&ui);
                }
                Err(e) => ui.global::<ScreenUi>().set_error(e.into()),
            });
        });
    });

    let weak = ui.as_weak();
    let client2 = client.clone();
    g.on_set_care(move |shift, away, own| {
        let Some(ui) = weak.upgrade() else { return };
        let care = ScreenCare {
            shift,
            dark_when_away: away,
            own_screen_too: own,
        };
        act(
            &ui,
            &client2,
            Request::SetScreenCare { care },
            TIMEOUT,
            marker(&client2, mark_screen),
        );
    });
}

/// Sample text for the shot example: 0 fits, 1 is a line too long.
const SAMPLE_TEXT: [&str; 2] = ["gglinux\nhello", "gglinux\nthis line is too long"];

thread_local! {
    /// Opens the brush's picker for the shot example.
    static OPENER: slint::Timer = slint::Timer::default();
}

/// The `shot` example's scripted steps for this page (see [`crate::poke`]).
pub(crate) fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let light = ui.global::<KeyboardUi>();
    let sw = ui.global::<SwitchesUi>();
    let screen = ui.global::<ScreenUi>();
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
        "switches.set-enabled" => sw.invoke_set_enabled(n != 0),
        "switches.set-actuation" => sw.invoke_set_actuation(n),
        "switches.key" => sw.invoke_key_clicked(n),
        "switches.own-level" => sw.set_own_level(n),
        "switches.set-own" => sw.invoke_set_own(n != 0),
        "switches.clear-selection" => sw.invoke_clear_selection(),
        "switches.set-rapid" => sw.invoke_set_rapid(n != 0),
        "switches.rapid-only-selected" => sw.invoke_rapid_only_selected(),
        "switches.set-tap" => sw.invoke_set_tap(n != 0),
        "switches.add-pair" => sw.invoke_add_pair(),
        "switches.remove-pair" => sw.invoke_remove_pair(n),
        // Pair 0's mode, and "send both" on.
        "switches.pair-mode" => {
            if let Some(p) = sw.get_pairs().row_data(0) {
                sw.invoke_set_pair(0, p.a, p.b, n, true);
            }
        }
        // Both of pair 0's keys the n-th adjustable one, which the daemon refuses.
        "switches.pair-keys" => {
            if let Some(p) = sw.get_pairs().row_data(0) {
                sw.invoke_set_pair(0, n, n, p.mode, p.both);
            }
        }
        "screen.pick" => screen.invoke_pick(n),
        "screen.text" => {
            let t = SAMPLE_TEXT[(n as usize).min(1)];
            screen.set_text(t.into());
            screen.invoke_text_edited(t.into());
        }
        "screen.show-text" => screen.invoke_show_text(),
        "screen.set-clock" => screen.invoke_set_clock(true, n != 0, true),
        "screen.test-picture" => {
            crate::with_forms(|forms| forms.keyboard.borrow_mut().picture = Some(test_picture()));
            crate::refresh(ui);
        }
        "screen.set-threshold" => screen.invoke_set_threshold(n),
        "screen.set-dither" => screen.invoke_set_dither(n != 0),
        "screen.show-picture" => screen.invoke_show_picture(),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_too_long_past_five_lines_or_sixteen_columns() {
        assert!(!too_long("hello\nworld"));
        assert!(!too_long("0123456789abcdef"));
        assert!(too_long("0123456789abcdefg"));
        assert!(too_long("1\n2\n3\n4\n5\n6"));
    }

    #[test]
    fn a_white_picture_lights_every_pixel_and_inverting_clears_them() {
        let white = image::RgbaImage::from_pixel(256, 80, image::Rgba([255, 255, 255, 255]));
        assert!(convert(&white, 128, false, true).iter().all(|b| *b == 0xff));
        assert!(convert(&white, 128, true, true).iter().all(|b| *b == 0));
    }

    #[test]
    fn a_narrow_picture_is_centred_on_black() {
        // 40 × 40 white in a 128 × 40 screen: columns 44..84 lit, the rest dark.
        let white = image::RgbaImage::from_pixel(40, 40, image::Rgba([255, 255, 255, 255]));
        let bits = convert(&white, 128, false, false);
        let lit = |x: usize, y: usize| bits[y * 16 + x / 8] & (0x80 >> (x % 8)) != 0;
        assert!(!lit(0, 20) && !lit(43, 20) && lit(44, 20) && lit(83, 20) && !lit(84, 20));
        assert_eq!(bits.len(), Screen::IMAGE_BYTES);
    }

    #[test]
    fn mid_grey_dithers_to_about_half_lit() {
        let grey = image::RgbaImage::from_pixel(128, 40, image::Rgba([128, 128, 128, 255]));
        let lit: u32 = convert(&grey, 128, false, true)
            .iter()
            .map(|b| b.count_ones())
            .sum();
        let half = (W * H / 2) as u32;
        assert!(lit.abs_diff(half) < half / 10, "{lit} of {}", W * H);
    }

    #[test]
    fn the_first_pair_is_a_and_d_then_free_keys() {
        let analog = [0x04, 0x07, 0x1a, 0x16];
        let first = next_pair(&analog, &[]).unwrap();
        assert_eq!((first.a, first.b), (0x04, 0x07));
        let second = next_pair(&analog, &[first]).unwrap();
        assert_eq!((second.a, second.b), (0x1a, 0x16));
        assert!(next_pair(&analog, &[first, second]).is_none());
    }

    #[test]
    fn own_points_replace_the_selected_keys_only() {
        let sw = Switches {
            keys: vec![
                KeyActuation {
                    code: 1,
                    actuation: 5,
                },
                KeyActuation {
                    code: 2,
                    actuation: 6,
                },
            ],
            ..Switches::default()
        };
        let sel: BTreeSet<u8> = [2, 3].into();
        let set = with_own(&sw, &sel, Some(9));
        assert_eq!(
            set,
            vec![
                KeyActuation {
                    code: 1,
                    actuation: 5
                },
                KeyActuation {
                    code: 2,
                    actuation: 9
                },
                KeyActuation {
                    code: 3,
                    actuation: 9
                }
            ]
        );
        assert_eq!(
            with_own(&sw, &sel, None),
            vec![KeyActuation {
                code: 1,
                actuation: 5
            }]
        );
    }

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

    /// A, D and Esc; only A and D have adjustable switches.
    fn board(keys: &[(u8, Rgb)], screen: Screen) -> KeyboardState {
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
    fn a_second_click_lets_a_key_go_and_fixed_switches_cant_be_picked() {
        let kb = board(&[], Screen::Keyboard);
        let mut f = KeyboardForm::default();
        f.toggle_light(0x29);
        f.toggle_light(0x04);
        f.toggle_light(0x29);
        assert_eq!(f.light_selected, [0x04].into());
        f.toggle_switch(&kb, 0x29);
        f.toggle_switch(&kb, 0x07);
        assert_eq!(f.switch_selected, [0x07].into());
        f.toggle_switch(&kb, 0x07);
        assert!(f.switch_selected.is_empty());
    }

    #[test]
    fn the_text_follows_the_daemon_only_when_its_own_changes() {
        let mut f = KeyboardForm::default();
        let hello = || {
            board(
                &[],
                Screen::Text {
                    text: "hello".into(),
                },
            )
        };
        f.follow(&hello());
        assert_eq!(f.text, "hello");
        f.text = "hello there".into();
        // The same text again (any refresh) leaves the typing alone.
        f.follow(&hello());
        assert_eq!(f.text, "hello there");
        // Another screen leaves it too.
        f.follow(&board(&[], Screen::Keyboard));
        assert_eq!(f.text, "hello there");
        // New text from elsewhere (ggctl, a profile) is shown.
        f.follow(&board(&[], Screen::Text { text: "hi".into() }));
        assert_eq!(f.text, "hi");
    }

    #[test]
    fn text_of_only_spaces_is_blank() {
        assert!(blank(""));
        assert!(blank("  \n \n"));
        assert!(!blank(" a "));
        // Five full lines fit; a sixth, or a seventeenth character, doesn't.
        let full = ["0123456789abcdef"; 5].join("\n");
        assert!(!too_long(&full));
        assert!(too_long(&(full.clone() + "\nx")));
        assert!(too_long(&full.replacen('0', "00", 1)));
    }

    #[test]
    fn a_pair_takes_keys_from_the_list_and_keeps_what_isnt_on_it() {
        let analog = [0x04, 0x07, 0x1a, 0x16];
        let mut sw = Switches::default();
        sw.rapid_tap.pairs = vec![TapPair {
            a: 0x04,
            b: 0x07,
            mode: TapMode::LastWins,
            report_both: false,
        }];
        set_pair(&mut sw, &analog, 0, 2, 3, 1, true);
        assert_eq!(
            sw.rapid_tap.pairs[0],
            TapPair {
                a: 0x1a,
                b: 0x16,
                mode: TapMode::Neither,
                report_both: true,
            }
        );
        // No pick (-1), or a mode past the list: unchanged.
        set_pair(&mut sw, &analog, 0, -1, 0, 9, true);
        assert_eq!(
            (sw.rapid_tap.pairs[0].a, sw.rapid_tap.pairs[0].b),
            (0x1a, 0x04)
        );
        assert_eq!(sw.rapid_tap.pairs[0].mode, TapMode::Neither);
        // A pair that isn't there.
        let before = sw.clone();
        set_pair(&mut sw, &analog, 3, 0, 1, 0, false);
        set_pair(&mut sw, &analog, -1, 0, 1, 0, false);
        assert_eq!(sw, before);
    }

    #[test]
    fn without_dithering_the_threshold_alone_decides() {
        let grey = image::RgbaImage::from_pixel(128, 40, image::Rgba([100, 100, 100, 255]));
        let lit = |t: u8, invert: bool| -> u32 {
            convert(&grey, t, invert, false)
                .iter()
                .map(|b| b.count_ones())
                .sum()
        };
        let all = (W * H) as u32;
        assert_eq!(lit(99, false), all);
        assert_eq!(lit(101, false), 0);
        assert_eq!(lit(101, true), all);
    }

    #[test]
    fn transparent_parts_are_black() {
        let clear = image::RgbaImage::from_pixel(128, 40, image::Rgba([255, 255, 255, 0]));
        assert!(convert(&clear, 1, false, false).iter().all(|b| *b == 0));
    }

    #[test]
    fn the_preview_lights_the_pixels_the_bits_say() {
        let mut bits = vec![0u8; Screen::IMAGE_BYTES];
        // The top-left pixel, and the last one of the second row.
        bits[0] = 0x80;
        bits[16 + 15] = 0x01;
        let img = preview(&bits).to_rgb8().unwrap();
        let px = |x: usize, y: usize| img.as_slice()[y * W + x].r;
        assert_eq!((px(0, 0), px(1, 0)), (235, 10));
        assert_eq!((px(127, 1), px(126, 1)), (235, 10));
    }
}
