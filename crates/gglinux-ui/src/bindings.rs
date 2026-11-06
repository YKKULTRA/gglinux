// SPDX-License-Identifier: GPL-3.0-or-later
//! Key bindings for the Apex Pro TKL Gen 3: remapping, the Fn layer and the
//! deep-press layer, from [`State::bindings`]. A panel in the keyboard card,
//! after the switches.
//!
//! The panel shows once the daemon has said what its bindings are. While
//! `keyboard_bindings` is off in Settings → Experimental every control is greyed and
//! the contract's sentence says why. A key is picked on the picture, its action chosen
//! in the editor below it, and "Apply" sends all the bindings with that one changed.
//! The deep-press point itself is set in the switches panel.

use std::rc::Rc;

use gg_ipc::{
    Bindings, BindingsState, Experimental, Feature, KeyAction, KeyBinding, KeyboardState, Layer,
    Request,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::client::{Client, State, TIMEOUT};
use crate::keyboard::{label, marker, SURFACE_2, TEXT, TEXT_DIM};
use crate::{sync_rows, AppWindow, BindingsUi, Forms, Key};

/// The layers, in the order of the panel's tabs.
const LAYERS: [Layer; 3] = [Layer::Normal, Layer::Fn, Layer::Second];
/// Keys on the picture with no binding on this keyboard: Print Screen, Scroll Lock and
/// Pause aren't on the board.
const UNBINDABLE: [u8; 3] = [0x46, 0x47, 0x48];
/// Ctrl, Shift, Alt and Super, the editor's toggles: the left-hand ones.
const MODIFIERS: [u8; 4] = [0xE0, 0xE1, 0xE2, 0xE3];
const MODIFIER_NAMES: [(u8, &str); 8] = [
    (0xE0, "Ctrl"),
    (0xE1, "Shift"),
    (0xE2, "Alt"),
    (0xE3, "Super"),
    (0xE4, "Right Ctrl"),
    (0xE5, "Right Shift"),
    (0xE6, "AltGr"),
    (0xE7, "Right Super"),
];
/// The media keys the editor offers, in its list's order, and short names for the picture.
const MEDIA: [(u16, &str); 6] = [
    (205, "Play"),
    (181, "Next"),
    (182, "Prev"),
    (226, "Mute"),
    (233, "Vol+"),
    (234, "Vol−"),
];
/// The editor's kinds of action, in its list's order (`BindingsUi.kinds`).
const KINDS: usize = 10;

/// The keys an action can send besides the modifiers, as the editor lists them.
const USAGES: [(&str, u8); 87] = [
    ("A", 0x04),
    ("B", 0x05),
    ("C", 0x06),
    ("D", 0x07),
    ("E", 0x08),
    ("F", 0x09),
    ("G", 0x0A),
    ("H", 0x0B),
    ("I", 0x0C),
    ("J", 0x0D),
    ("K", 0x0E),
    ("L", 0x0F),
    ("M", 0x10),
    ("N", 0x11),
    ("O", 0x12),
    ("P", 0x13),
    ("Q", 0x14),
    ("R", 0x15),
    ("S", 0x16),
    ("T", 0x17),
    ("U", 0x18),
    ("V", 0x19),
    ("W", 0x1A),
    ("X", 0x1B),
    ("Y", 0x1C),
    ("Z", 0x1D),
    ("1", 0x1E),
    ("2", 0x1F),
    ("3", 0x20),
    ("4", 0x21),
    ("5", 0x22),
    ("6", 0x23),
    ("7", 0x24),
    ("8", 0x25),
    ("9", 0x26),
    ("0", 0x27),
    ("Enter", 0x28),
    ("Esc", 0x29),
    ("Backspace", 0x2A),
    ("Tab", 0x2B),
    ("Space", 0x2C),
    ("-", 0x2D),
    ("=", 0x2E),
    ("[", 0x2F),
    ("]", 0x30),
    ("\\", 0x31),
    (";", 0x33),
    ("'", 0x34),
    ("`", 0x35),
    (",", 0x36),
    (".", 0x37),
    ("/", 0x38),
    ("Caps Lock", 0x39),
    ("F1", 0x3A),
    ("F2", 0x3B),
    ("F3", 0x3C),
    ("F4", 0x3D),
    ("F5", 0x3E),
    ("F6", 0x3F),
    ("F7", 0x40),
    ("F8", 0x41),
    ("F9", 0x42),
    ("F10", 0x43),
    ("F11", 0x44),
    ("F12", 0x45),
    ("Print Screen", 0x46),
    ("Scroll Lock", 0x47),
    ("Pause", 0x48),
    ("Insert", 0x49),
    ("Home", 0x4A),
    ("Page Up", 0x4B),
    ("Delete", 0x4C),
    ("End", 0x4D),
    ("Page Down", 0x4E),
    ("→", 0x4F),
    ("←", 0x50),
    ("↓", 0x51),
    ("↑", 0x52),
    ("Menu", 0x65),
    ("F13", 0x68),
    ("F14", 0x69),
    ("F15", 0x6A),
    ("F16", 0x6B),
    ("F17", 0x6C),
    ("F18", 0x6D),
    ("F19", 0x6E),
    ("F20", 0x6F),
];

/// What the panel keeps between the daemon's answers.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BindingsForm {
    /// The tab shown: 0 normal, 1 with Fn held, 2 deep press.
    pub layer: usize,
    /// The key picked on the picture, by HID code.
    pub picked: Option<u8>,
}

impl BindingsForm {
    pub fn layer(&self) -> Layer {
        LAYERS[self.layer.min(LAYERS.len() - 1)]
    }

    /// Pick a key, or let it go with a second click. Only keys with a binding on the
    /// tab shown can be picked.
    pub fn toggle(&mut self, kb: &KeyboardState, code: u8) {
        self.picked = match self.picked {
            Some(c) if c == code => None,
            _ if bindable(kb, self.layer(), code) => Some(code),
            other => other,
        };
    }

    /// Show another tab; the picked key stays if it has a binding there too.
    pub fn show_layer(&mut self, kb: Option<&KeyboardState>, layer: usize) {
        self.layer = layer.min(LAYERS.len() - 1);
        let keep = self
            .picked
            .zip(kb)
            .is_some_and(|(c, kb)| bindable(kb, self.layer(), c));
        if !keep {
            self.picked = None;
        }
    }
}

/// Whether `code` has a binding on `layer` of this keyboard.
pub fn bindable(kb: &KeyboardState, layer: Layer, code: u8) -> bool {
    match layer {
        Layer::Second => kb.analog_keys.contains(&code),
        Layer::Normal | Layer::Fn => {
            kb.layout.iter().any(|k| k.code == code) && !UNBINDABLE.contains(&code)
        }
    }
}

fn usage_name(code: u8) -> String {
    MODIFIER_NAMES
        .iter()
        .find(|m| m.0 == code)
        .map(|m| m.1)
        .or_else(|| USAGES.iter().find(|u| u.1 == code).map(|u| u.0))
        .map_or_else(|| format!("{code:#04x}"), str::to_owned)
}

/// What an action shows on its key in the picture: a few characters.
pub fn short(action: &KeyAction) -> String {
    match action {
        KeyAction::Default => String::new(),
        KeyAction::Keys { codes } => codes
            .iter()
            .map(|&c| usage_name(c))
            .collect::<Vec<_>>()
            .join("+"),
        KeyAction::Consumer { usage } => MEDIA
            .iter()
            .find(|m| m.0 == *usage)
            .map_or_else(|| "Media".to_owned(), |m| m.1.to_owned()),
        KeyAction::Mouse { button } => format!("M{button}"),
        KeyAction::Cpi => "CPI".into(),
        KeyAction::WheelUp => "Wh↑".into(),
        KeyAction::WheelDown => "Wh↓".into(),
        KeyAction::PanLeft => "Wh←".into(),
        KeyAction::PanRight => "Wh→".into(),
        KeyAction::Firmware { .. } => "own".into(),
        KeyAction::Macro { slot } => format!("Mac{slot}"),
        KeyAction::Unbound => "—".into(),
    }
}

/// What an action does, in words.
pub fn words(action: &KeyAction) -> String {
    match action {
        KeyAction::Default => "its factory binding".into(),
        KeyAction::Keys { .. } => format!("types {}", short(action)),
        KeyAction::Consumer { usage } => match MEDIA.iter().position(|m| m.0 == *usage) {
            Some(i) => format!(
                "the media key {}",
                [
                    "play/pause",
                    "next track",
                    "previous track",
                    "mute",
                    "volume up",
                    "volume down"
                ][i]
            ),
            None => format!("media usage {usage}"),
        },
        KeyAction::Mouse { button } => format!("mouse button {button}"),
        KeyAction::Cpi => "the CPI button".into(),
        KeyAction::WheelUp => "the mouse wheel up".into(),
        KeyAction::WheelDown => "the mouse wheel down".into(),
        KeyAction::PanLeft => "scroll left".into(),
        KeyAction::PanRight => "scroll right".into(),
        KeyAction::Firmware { .. } => "one of the keyboard's own shortcuts".into(),
        KeyAction::Macro { slot } => format!("macro {slot}, stored on the keyboard"),
        KeyAction::Unbound => "nothing".into(),
    }
}

/// The editor's values: which kind of action (`BindingsUi.kinds`), and its details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Editor {
    pub kind: i32,
    /// Ctrl, Shift, Alt, Super.
    pub mods: [bool; 4],
    /// The key sent with them: 0 for none, else 1 + its place in the list.
    pub usage: i32,
    pub media: i32,
    /// The mouse button, from 0.
    pub button: i32,
}

impl Editor {
    /// The editor showing `action`, as near as it can: one key besides the left
    /// modifiers.
    pub fn of(action: &KeyAction) -> Editor {
        let kind = |kind| Editor {
            kind,
            ..Editor::default()
        };
        match action {
            KeyAction::Default | KeyAction::Firmware { .. } | KeyAction::Macro { .. } => kind(0),
            KeyAction::Keys { codes } => Editor {
                kind: 1,
                mods: MODIFIERS.map(|m| codes.contains(&m)),
                usage: codes
                    .iter()
                    .find_map(|c| USAGES.iter().position(|u| u.1 == *c))
                    .map_or(0, |i| i as i32 + 1),
                ..Editor::default()
            },
            KeyAction::Consumer { usage } => Editor {
                kind: 2,
                media: MEDIA
                    .iter()
                    .position(|m| m.0 == *usage)
                    .map_or(0, |i| i as i32),
                ..Editor::default()
            },
            KeyAction::Mouse { button } => Editor {
                kind: 3,
                button: i32::from(*button) - 1,
                ..Editor::default()
            },
            KeyAction::Cpi => kind(4),
            KeyAction::WheelUp => kind(5),
            KeyAction::WheelDown => kind(6),
            KeyAction::PanLeft => kind(7),
            KeyAction::PanRight => kind(8),
            KeyAction::Unbound => kind(9),
        }
    }

    /// The action the editor shows.
    pub fn action(&self) -> Result<KeyAction, String> {
        let at = |i: i32| usize::try_from(i).ok();
        Ok(match self.kind {
            1 => {
                let mut codes: Vec<u8> = MODIFIERS
                    .iter()
                    .zip(self.mods)
                    .filter(|(_, on)| *on)
                    .map(|(m, _)| *m)
                    .collect();
                if let Some(u) = at(self.usage - 1).and_then(|i| USAGES.get(i)) {
                    codes.push(u.1);
                }
                if codes.is_empty() {
                    return Err("pick a key for it to send".into());
                }
                KeyAction::Keys { codes }
            }
            2 => KeyAction::Consumer {
                usage: MEDIA[at(self.media).unwrap_or(0).min(MEDIA.len() - 1)].0,
            },
            3 => KeyAction::Mouse {
                button: self.button.clamp(0, 7) as u8 + 1,
            },
            4 => KeyAction::Cpi,
            5 => KeyAction::WheelUp,
            6 => KeyAction::WheelDown,
            7 => KeyAction::PanLeft,
            8 => KeyAction::PanRight,
            9 => KeyAction::Unbound,
            _ => KeyAction::Default,
        })
    }
}

/// `bindings` with `code` doing `action` on `layer`; the default drops it from the list.
pub fn with_binding(bindings: &Bindings, layer: Layer, code: u8, action: KeyAction) -> Bindings {
    let mut out = bindings.clone();
    let list = out.layer_mut(layer);
    list.retain(|b| b.code != code);
    if action != KeyAction::Default {
        list.push(KeyBinding { code, action });
    }
    out
}

/// The keys that can hold the Fn layer, as the picker lists them after "the keyboard's
/// own": the bindable keys, in layout order.
fn fn_keys(kb: &KeyboardState) -> Vec<u8> {
    kb.layout
        .iter()
        .map(|k| k.code)
        .filter(|&c| bindable(kb, Layer::Normal, c))
        .collect()
}

/// What the last read showed, in a line.
fn summary(state: &BindingsState) -> String {
    let Some(read) = &state.read else {
        return "Not read from the keyboard since it connected.".into();
    };
    let differ = read.normal.len() + read.fn_layer.len() + read.second.len();
    let own = read
        .fn_layer
        .iter()
        .filter(|b| matches!(b.action, KeyAction::Firmware { .. }))
        .count();
    format!(
        "As last read, {differ} of the keyboard's bindings differ from the factory's, {own} of them its own Fn shortcuts."
    )
}

fn binding(b: &Bindings, layer: Layer, code: u8) -> Option<&KeyAction> {
    b.layer(layer)
        .iter()
        .find(|k| k.code == code)
        .map(|k| &k.action)
}

fn board(kb: &KeyboardState, state: &BindingsState, f: &BindingsForm, gated: bool) -> Vec<Key> {
    let layer = f.layer();
    kb.layout
        .iter()
        .map(|k| {
            let can = bindable(kb, layer, k.code);
            let ours = binding(&state.bindings, layer, k.code);
            let theirs = state
                .read
                .as_ref()
                .and_then(|r| binding(r, layer, k.code));
            Key {
                x: k.x,
                y: k.y,
                w: k.w,
                label: k.label.clone().into(),
                sub: ours.or(theirs).map(short).unwrap_or_default().into(),
                fill: SURFACE_2,
                ink: if can && gated { TEXT } else { TEXT_DIM },
                selected: f.picked == Some(k.code),
                fixed: !can,
                own: ours.is_some(),
            }
        })
        .collect()
}

/// The picked key's heading, what it does now, and whether it's one of the
/// keyboard's own shortcuts.
fn picked(kb: &KeyboardState, state: &BindingsState, f: &BindingsForm) -> (String, String, bool) {
    let Some(code) = f.picked else {
        return (String::new(), String::new(), false);
    };
    let name = kb
        .layout
        .iter()
        .find(|k| k.code == code)
        .map_or_else(|| format!("{code:#04x}"), label);
    let layer = f.layer();
    let heading = match layer {
        Layer::Normal => name,
        Layer::Fn => format!("Fn + {name}"),
        Layer::Second => format!("{name}, pressed past its deep-press point"),
    };
    let ours = binding(&state.bindings, layer, code);
    let theirs = state.read.as_ref().and_then(|r| binding(r, layer, code));
    let now = match (ours, theirs) {
        (Some(a), _) => format!("gglinux: {}.", words(a)),
        (None, Some(KeyAction::Firmware { .. })) => {
            "One of the keyboard's own shortcuts (brightness, media or the screen menu).".into()
        }
        (None, Some(a)) => format!("The keyboard's own: {}.", words(a)),
        (None, None) if layer == Layer::Normal => "Its factory binding: the key itself.".into(),
        (None, None) => "Its factory binding: nothing.".into(),
    };
    let own = ours.is_none() && matches!(theirs, Some(KeyAction::Firmware { .. }));
    (heading, now, own)
}

/// Show the daemon's key bindings.
pub fn show(ui: &AppWindow, s: &State) {
    let g = ui.global::<BindingsUi>();
    let Some(state) = s.bindings.as_ref() else {
        g.set_shown(false);
        mark(ui, false, SharedString::new());
        return;
    };
    let gated = s.experimental.is_some_and(|e| e.keyboard_bindings);
    g.set_shown(true);
    g.set_gated(gated);
    g.set_refusal(
        Experimental::default()
            .refusal(Feature::KeyboardBindings)
            .unwrap_or_default()
            .into(),
    );
    g.set_enabled(state.enabled);
    g.set_failed(state.error.clone().unwrap_or_default().into());
    g.set_read_summary(summary(state).into());
    if g.get_usage_names().row_count() == 0 {
        let names: Vec<SharedString> = std::iter::once("(none)")
            .chain(USAGES.iter().map(|u| u.0))
            .map(SharedString::from)
            .collect();
        g.set_usage_names(ModelRc::new(VecModel::from(names)));
    }
    crate::with_forms(|forms| {
        let f = forms.bindings.borrow();
        g.set_layer(f.layer as i32);
        let Some(kb) = s.keyboard.as_ref() else {
            return;
        };
        sync_rows(g.get_keys(), board(kb, state, &f, gated), |m| g.set_keys(m));
        let (heading, now, own) = picked(kb, state, &f);
        g.set_picked(heading.into());
        g.set_now(now.into());
        g.set_own_shortcut(own);
        let codes = fn_keys(kb);
        let names: Vec<SharedString> =
            std::iter::once("The keyboard's own (the SteelSeries key)".to_string())
                .chain(codes.iter().map(|&c| {
                    kb.layout
                        .iter()
                        .find(|k| k.code == c)
                        .map_or_else(String::new, label)
                }))
                .map(SharedString::from)
                .collect();
        if g.get_fn_keys().iter().ne(names.iter().cloned()) {
            g.set_fn_keys(ModelRc::new(VecModel::from(names)));
        }
        g.set_fn_key(
            state
                .bindings
                .fn_key
                .and_then(|k| codes.iter().position(|&c| c == k))
                .map_or(0, |i| i as i32 + 1),
        );
    });
}

/// Put the editor's values in the panel.
fn set_editor(ui: &AppWindow, e: Editor) {
    let g = ui.global::<BindingsUi>();
    g.set_kind(e.kind);
    g.set_ctrl(e.mods[0]);
    g.set_shift(e.mods[1]);
    g.set_alt(e.mods[2]);
    g.set_super_key(e.mods[3]);
    g.set_usage(e.usage);
    g.set_media(e.media);
    g.set_button(e.button);
}

/// The editor's values as the panel has them.
fn editor(ui: &AppWindow) -> Editor {
    let g = ui.global::<BindingsUi>();
    Editor {
        kind: g.get_kind().clamp(0, KINDS as i32 - 1),
        mods: [g.get_ctrl(), g.get_shift(), g.get_alt(), g.get_super_key()],
        usage: g.get_usage(),
        media: g.get_media(),
        button: g.get_button(),
    }
}

/// Fill the editor with what the picked key does now on the tab shown.
fn load_editor(ui: &AppWindow, s: &State, f: &BindingsForm) {
    let action = f.picked.and_then(|code| {
        let state = s.bindings.as_ref()?;
        let layer = f.layer();
        binding(&state.bindings, layer, code)
            .or_else(|| state.read.as_ref().and_then(|r| binding(r, layer, code)))
            .cloned()
    });
    set_editor(ui, Editor::of(&action.unwrap_or(KeyAction::Default)));
}

fn mark(ui: &AppWindow, busy: bool, error: SharedString) {
    let g = ui.global::<BindingsUi>();
    g.set_busy(busy);
    g.set_error(error);
}

fn redraw(ui: &AppWindow, s: &State, _forms: &Forms) {
    show(ui, s);
}

/// Send a request, marking the panel busy until the answer. The daemon announces what
/// changed; the fake daemon doesn't, so the bindings are read again after.
fn send(ui: &AppWindow, client: &Client, req: Request) {
    let mark = marker(client, mark, redraw);
    mark(ui, true, SharedString::new());
    let weak = ui.as_weak();
    let again = client.clone();
    client.request(req, TIMEOUT, move |answer| {
        let result = answer.map(|_| ());
        if result.is_ok() {
            again.request(Request::GetBindings, TIMEOUT, |_| {});
        }
        let error = crate::panel_error(result);
        let _ = weak.upgrade_in_event_loop(move |ui| mark(&ui, false, error));
    });
}

/// The bindings as the daemon has them, changed by `edit`, sent back.
fn change(
    ui: &AppWindow,
    client: &Client,
    edit: impl FnOnce(&BindingsState) -> Result<(Bindings, bool), String>,
) {
    let Some(state) = client.state().bindings else {
        return;
    };
    match edit(&state) {
        Ok((bindings, enabled)) => send(ui, client, Request::SetBindings { bindings, enabled }),
        Err(e) => mark(ui, false, e.into()),
    }
}

/// Answer the panel's callbacks.
pub fn bind(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    let g = ui.global::<BindingsUi>();

    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_enabled(move |on| {
        let Some(ui) = weak.upgrade() else { return };
        change(&ui, &c, |s| Ok((s.bindings.clone(), on)));
    });

    let weak = ui.as_weak();
    let c = client.clone();
    let f = forms.clone();
    g.on_pick_layer(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let s = c.state();
        f.bindings
            .borrow_mut()
            .show_layer(s.keyboard.as_ref(), usize::try_from(i).unwrap_or(0));
        load_editor(&ui, &s, &f.bindings.borrow());
        show(&ui, &s);
    });

    let weak = ui.as_weak();
    let c = client.clone();
    let f = forms.clone();
    g.on_key_clicked(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let s = c.state();
        let Some(kb) = s.keyboard.as_ref() else {
            return;
        };
        if let Some(k) = usize::try_from(i).ok().and_then(|i| kb.layout.get(i)) {
            f.bindings.borrow_mut().toggle(kb, k.code);
        }
        load_editor(&ui, &s, &f.bindings.borrow());
        show(&ui, &s);
    });

    let weak = ui.as_weak();
    let c = client.clone();
    let f = forms.clone();
    g.on_apply(move || {
        let Some(ui) = weak.upgrade() else { return };
        let (Some(code), layer) = ({
            let form = f.bindings.borrow();
            (form.picked, form.layer())
        }) else {
            return;
        };
        let action = editor(&ui).action();
        change(&ui, &c, |s| {
            Ok((with_binding(&s.bindings, layer, code, action?), s.enabled))
        });
    });

    let weak = ui.as_weak();
    let c = client.clone();
    g.on_read(move || {
        if let Some(ui) = weak.upgrade() {
            send(&ui, &c, Request::ReadBindings);
        }
    });

    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_fn_key(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let Some(kb) = c.state().keyboard else { return };
        let key = usize::try_from(i - 1)
            .ok()
            .and_then(|i| fn_keys(&kb).get(i).copied());
        change(&ui, &c, |s| {
            let mut b = s.bindings.clone();
            b.fn_key = key;
            Ok((b, s.enabled))
        });
    });

    let weak = ui.as_weak();
    let c = client.clone();
    g.on_shot_gate(move |on| {
        let mut experimental = c.state().experimental.unwrap_or_default();
        experimental.keyboard_bindings = on;
        let weak = weak.clone();
        let again = c.clone();
        c.request(
            Request::SetExperimental { experimental },
            TIMEOUT,
            move |_| {
                again.request(Request::GetExperimental, TIMEOUT, move |_| {
                    let _ = weak.upgrade_in_event_loop(|ui| crate::refresh(&ui));
                });
            },
        );
    });
}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.bindings.borrow_mut() = BindingsForm::default();
}

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]).
/// `bindings.gate:1` turns the experimental gate on in the (fake) daemon.
pub fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let g = ui.global::<BindingsUi>();
    match name {
        "bindings.gate" => g.invoke_shot_gate(n != 0),
        "bindings.set-enabled" => g.invoke_set_enabled(n != 0),
        "bindings.layer" => g.invoke_pick_layer(n),
        "bindings.key" => g.invoke_key_clicked(n),
        "bindings.kind" => g.set_kind(n),
        "bindings.ctrl" => g.set_ctrl(n != 0),
        "bindings.shift" => g.set_shift(n != 0),
        "bindings.usage" => g.set_usage(n),
        "bindings.media" => g.set_media(n),
        "bindings.button" => g.set_button(n),
        "bindings.apply" => g.invoke_apply(),
        "bindings.read" => g.invoke_read(),
        "bindings.fn-key" => g.invoke_set_fn_key(n),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::tests::board as test_board;
    use gg_ipc::Screen;

    fn keys(codes: &[u8]) -> KeyAction {
        KeyAction::Keys {
            codes: codes.to_vec(),
        }
    }

    #[test]
    fn the_editor_round_trips_every_kind_of_action_it_offers() {
        let actions = [
            KeyAction::Default,
            keys(&[0xE0, 0x04]),
            keys(&[0xE0, 0xE1, 0x29]),
            keys(&[0x68]),
            KeyAction::Consumer { usage: 226 },
            KeyAction::Mouse { button: 4 },
            KeyAction::Cpi,
            KeyAction::WheelUp,
            KeyAction::WheelDown,
            KeyAction::PanLeft,
            KeyAction::PanRight,
            KeyAction::Unbound,
        ];
        for a in actions {
            assert_eq!(Editor::of(&a).action().unwrap(), a, "{a:?}");
        }
        assert!(Editor {
            kind: 1,
            ..Editor::default()
        }
        .action()
        .is_err());
        assert_eq!(KINDS, 10);
    }

    #[test]
    fn a_binding_replaces_that_keys_on_its_layer_and_the_default_drops_it() {
        let b = Bindings {
            normal: vec![KeyBinding {
                code: 0x39,
                action: KeyAction::Unbound,
            }],
            ..Bindings::default()
        };
        let caps = with_binding(&b, Layer::Normal, 0x39, keys(&[0xE0, 0x04]));
        assert_eq!(caps.normal, [KeyBinding {
            code: 0x39,
            action: keys(&[0xE0, 0x04])
        }]);
        let fn_f1 = with_binding(&b, Layer::Fn, 0x3A, KeyAction::Consumer { usage: 226 });
        assert_eq!((fn_f1.normal.len(), fn_f1.fn_layer.len()), (1, 1));
        assert!(with_binding(&b, Layer::Normal, 0x39, KeyAction::Default)
            .normal
            .is_empty());
    }

    #[test]
    fn only_keys_with_a_binding_on_the_tab_can_be_picked() {
        let kb = test_board(&[], Screen::Keyboard);
        let mut f = BindingsForm::default();
        f.toggle(&kb, 0x29);
        assert_eq!(f.picked, Some(0x29), "Esc has a normal binding");
        f.show_layer(Some(&kb), 2);
        assert_eq!(f.picked, None, "but no deep press");
        f.toggle(&kb, 0x29);
        assert_eq!(f.picked, None);
        f.toggle(&kb, 0x04);
        assert_eq!(f.picked, Some(0x04));
        f.toggle(&kb, 0x04);
        assert_eq!(f.picked, None, "a second click lets it go");
        assert!(!bindable(&kb, Layer::Normal, 0x46), "not on the picture");
    }

    #[test]
    fn the_picture_shows_gglinuxs_bindings_outlined_and_the_keyboards_own() {
        let kb = test_board(&[], Screen::Keyboard);
        let state = BindingsState {
            enabled: true,
            bindings: Bindings {
                fn_layer: vec![KeyBinding {
                    code: 0x04,
                    action: KeyAction::Consumer { usage: 205 },
                }],
                ..Bindings::default()
            },
            read: Some(Bindings {
                fn_layer: vec![KeyBinding {
                    code: 0x07,
                    action: KeyAction::Firmware { raw: [6, 0, 0, 0] },
                }],
                ..Bindings::default()
            }),
            error: None,
        };
        let f = BindingsForm {
            layer: 1,
            picked: Some(0x07),
        };
        let keys = board(&kb, &state, &f, true);
        let a = keys.iter().find(|k| k.label == "A").unwrap();
        assert_eq!((a.sub.as_str(), a.own), ("Play", true));
        let d = keys.iter().find(|k| k.label == "D").unwrap();
        assert_eq!((d.sub.as_str(), d.own, d.selected), ("own", false, true));
        let (heading, now, own) = picked(&kb, &state, &f);
        assert_eq!(heading, "Fn + D");
        assert_eq!(
            now,
            "One of the keyboard's own shortcuts (brightness, media or the screen menu)."
        );
        assert!(own, "kept as it is");
        assert!(summary(&state).contains("1 of the keyboard's bindings"));
        assert!(summary(&BindingsState::default()).starts_with("Not read"));
    }

    #[test]
    fn a_lost_connection_forgets_the_picked_key() {
        let forms = Forms::default();
        forms.bindings.borrow_mut().picked = Some(0x04);
        forms.bindings.borrow_mut().layer = 2;
        reset(&forms);
        assert_eq!(*forms.bindings.borrow(), BindingsForm::default());
    }
}
