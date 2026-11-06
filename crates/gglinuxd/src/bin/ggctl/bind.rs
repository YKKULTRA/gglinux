// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl bind`: key bindings: what each key does on its own, with Fn held and past its
//! second (deep-press) actuation point.
//!
//! ```text
//! ggctl bind                          # the bindings gglinux sends, and the last read
//! ggctl bind read                     # read the keyboard's own back
//! ggctl bind on|off                   # send them each time the keyboard connects, or stop
//!                                     #   (what was sent stays until it's unplugged)
//! ggctl bind capslock ctrl+a          # a key or combination, up to 4 keys
//! ggctl bind fn+f1 media:mute         # with Fn held (or --layer fn)
//! ggctl bind deep+w mouse:4           # past W's deep-press point (or --layer deep)
//! ggctl bind q default                # back to the factory binding
//! ```
//!
//! Actions: key names joined with `+`, `media:playpause|next|previous|mute|volup|voldown`,
//! `mouse:1`–`mouse:8` (or `left`, `right`, `middle`, `back`, `forward`), `cpi`,
//! `wheel:up|down`, `pan:left|right`, `none` and `default`. A change reads the bindings,
//! edits them and sends them back, and turns them on.

use anyhow::{bail, Context, Result};
use gg_ipc::{BindingsState, KeyAction, KeyBinding, Layer, Request, ResponseData};

use crate::Action;

/// HID keyboard usages by name, for keys and the keys an action sends.
const USAGES: [(&str, u8); 101] = [
    ("a", 0x04),
    ("b", 0x05),
    ("c", 0x06),
    ("d", 0x07),
    ("e", 0x08),
    ("f", 0x09),
    ("g", 0x0A),
    ("h", 0x0B),
    ("i", 0x0C),
    ("j", 0x0D),
    ("k", 0x0E),
    ("l", 0x0F),
    ("m", 0x10),
    ("n", 0x11),
    ("o", 0x12),
    ("p", 0x13),
    ("q", 0x14),
    ("r", 0x15),
    ("s", 0x16),
    ("t", 0x17),
    ("u", 0x18),
    ("v", 0x19),
    ("w", 0x1A),
    ("x", 0x1B),
    ("y", 0x1C),
    ("z", 0x1D),
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
    ("enter", 0x28),
    ("esc", 0x29),
    ("backspace", 0x2A),
    ("tab", 0x2B),
    ("space", 0x2C),
    ("minus", 0x2D),
    ("equal", 0x2E),
    ("lbracket", 0x2F),
    ("rbracket", 0x30),
    ("backslash", 0x31),
    ("hash", 0x32),
    ("semicolon", 0x33),
    ("quote", 0x34),
    ("grave", 0x35),
    ("comma", 0x36),
    ("period", 0x37),
    ("slash", 0x38),
    ("capslock", 0x39),
    ("f1", 0x3A),
    ("f2", 0x3B),
    ("f3", 0x3C),
    ("f4", 0x3D),
    ("f5", 0x3E),
    ("f6", 0x3F),
    ("f7", 0x40),
    ("f8", 0x41),
    ("f9", 0x42),
    ("f10", 0x43),
    ("f11", 0x44),
    ("f12", 0x45),
    ("printscreen", 0x46),
    ("scrolllock", 0x47),
    ("pause", 0x48),
    ("insert", 0x49),
    ("home", 0x4A),
    ("pageup", 0x4B),
    ("delete", 0x4C),
    ("end", 0x4D),
    ("pagedown", 0x4E),
    ("right", 0x4F),
    ("left", 0x50),
    ("down", 0x51),
    ("up", 0x52),
    ("iso", 0x64),
    ("application", 0x65),
    ("f13", 0x68),
    ("f14", 0x69),
    ("f15", 0x6A),
    ("f16", 0x6B),
    ("f17", 0x6C),
    ("f18", 0x6D),
    ("f19", 0x6E),
    ("f20", 0x6F),
    ("f21", 0x70),
    ("f22", 0x71),
    ("f23", 0x72),
    ("f24", 0x73),
    ("ctrl", 0xE0),
    ("shift", 0xE1),
    ("alt", 0xE2),
    ("super", 0xE3),
    ("rctrl", 0xE4),
    ("rshift", 0xE5),
    ("ralt", 0xE6),
    ("rsuper", 0xE7),
];

/// Other names people type.
const ALIASES: [(&str, &str); 16] = [
    ("caps", "capslock"),
    ("escape", "esc"),
    ("return", "enter"),
    ("del", "delete"),
    ("ins", "insert"),
    ("pgup", "pageup"),
    ("pgdn", "pagedown"),
    ("prtsc", "printscreen"),
    ("control", "ctrl"),
    ("lctrl", "ctrl"),
    ("lshift", "shift"),
    ("lalt", "alt"),
    ("win", "super"),
    ("lsuper", "super"),
    ("altgr", "ralt"),
    ("backtick", "grave"),
];

/// Media keys, as consumer-page usages.
const MEDIA: [(&str, u16); 6] = [
    ("playpause", 205),
    ("next", 181),
    ("previous", 182),
    ("mute", 226),
    ("volup", 233),
    ("voldown", 234),
];

const MOUSE: [(&str, u8); 5] = [
    ("left", 1),
    ("right", 2),
    ("middle", 3),
    ("back", 4),
    ("forward", 5),
];

/// What the command line asks of the bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindEdit {
    /// Send them each time the keyboard connects, or stop sending them. Off undoes
    /// nothing: what was sent stays on the keyboard until it's unplugged.
    Enabled(bool),
    /// One key on one layer; turns the bindings on.
    Key {
        layer: Layer,
        code: u8,
        action: KeyAction,
    },
}

impl BindEdit {
    /// The `set_bindings` this makes of `state`.
    pub fn apply(&self, state: &BindingsState) -> Request {
        let mut bindings = state.bindings.clone();
        let enabled = match self {
            BindEdit::Enabled(on) => *on,
            BindEdit::Key {
                layer,
                code,
                action,
            } => {
                let list = bindings.layer_mut(*layer);
                list.retain(|b| b.code != *code);
                if *action != KeyAction::Default {
                    list.push(KeyBinding {
                        code: *code,
                        action: action.clone(),
                    });
                }
                true
            }
        };
        Request::SetBindings { bindings, enabled }
    }
}

/// What `ggctl bind ...` asks for, before anything is read from the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bind {
    Show,
    Read,
    Edit(BindEdit),
}

fn usage(name: &str) -> Option<u8> {
    let name = name.to_ascii_lowercase();
    let name = ALIASES
        .iter()
        .find(|a| a.0 == name)
        .map_or(name.as_str(), |a| a.1);
    if let Some(hex) = name.strip_prefix("0x") {
        return u8::from_str_radix(hex, 16).ok();
    }
    USAGES.iter().find(|u| u.0 == name).map(|u| u.1)
}

/// A key on the keyboard, by name: as [`usage`], but Menu and Fn are the keys with those
/// labels (`0xE7` and the SteelSeries key `0xF0`).
fn key(name: &str) -> Result<u8> {
    match name.to_ascii_lowercase().as_str() {
        "menu" => Ok(0xE7),
        "fn" | "steelseries" => Ok(0xF0),
        _ => usage(name).with_context(|| {
            format!("'{name}' is not a key name (try: a, f1, capslock, space, 0x39)")
        }),
    }
}

fn action_of(word: &str) -> Result<KeyAction> {
    let lower = word.to_ascii_lowercase();
    Ok(match lower.as_str() {
        "default" => KeyAction::Default,
        "none" | "nothing" | "unbound" => KeyAction::Unbound,
        "cpi" => KeyAction::Cpi,
        "wheel:up" => KeyAction::WheelUp,
        "wheel:down" => KeyAction::WheelDown,
        "pan:left" => KeyAction::PanLeft,
        "pan:right" => KeyAction::PanRight,
        w if w.starts_with("media:") => {
            let m = &w["media:".len()..];
            let usage = MEDIA
                .iter()
                .find(|x| x.0 == m)
                .map(|x| x.1)
                .with_context(|| {
                    format!("'{m}' is not a media key (try: playpause, next, previous, mute, volup, voldown)")
                })?;
            KeyAction::Consumer { usage }
        }
        w if w.starts_with("mouse:") => {
            let b = &w["mouse:".len()..];
            let button = match MOUSE.iter().find(|x| x.0 == b) {
                Some(x) => x.1,
                None => b.parse().ok().filter(|n| (1..=8).contains(n)).with_context(|| {
                    format!("'{b}' is not a mouse button (1 to 8, or left, right, middle, back, forward)")
                })?,
            };
            KeyAction::Mouse { button }
        }
        w => {
            let codes = w
                .split('+')
                .map(|k| {
                    usage(k).with_context(|| {
                        format!(
                            "'{k}' is not a key name (try: ctrl+a, f13, media:playpause, mouse:4)"
                        )
                    })
                })
                .collect::<Result<Vec<u8>>>()?;
            if codes.len() > 4 {
                bail!("a key sends up to 4 keys at once, not {}", codes.len());
            }
            KeyAction::Keys { codes }
        }
    })
}

fn layer_of(word: &str) -> Result<Layer> {
    match word {
        "normal" => Ok(Layer::Normal),
        "fn" => Ok(Layer::Fn),
        "deep" => Ok(Layer::Second),
        other => bail!("'{other}' is not a layer (normal, fn or deep)"),
    }
}

/// `ggctl bind`'s words, read as a command.
pub fn parse(rest: &[&str]) -> Result<Bind> {
    let mut layer = None;
    let mut words = Vec::new();
    let mut it = rest.iter();
    while let Some(&w) = it.next() {
        if w == "--layer" {
            layer = Some(layer_of(it.next().context("--layer expects fn or deep")?)?);
        } else if let Some(l) = w.strip_prefix("--layer=") {
            layer = Some(layer_of(l)?);
        } else {
            words.push(w);
        }
    }
    Ok(match words.as_slice() {
        [] if layer.is_none() => Bind::Show,
        ["read"] if layer.is_none() => Bind::Read,
        ["on"] if layer.is_none() => Bind::Edit(BindEdit::Enabled(true)),
        ["off"] if layer.is_none() => Bind::Edit(BindEdit::Enabled(false)),
        [k, a] => {
            let (prefixed, name) = match k.to_ascii_lowercase().split_once('+') {
                Some((p, name)) if p == "fn" || p == "deep" => (Some(layer_of(p)?), name.to_owned()),
                _ => (None, (*k).to_owned()),
            };
            Bind::Edit(BindEdit::Key {
                layer: prefixed.or(layer).unwrap_or(Layer::Normal),
                code: key(&name)?,
                action: action_of(a)?,
            })
        }
        [_] => bail!("'ggctl bind {}' needs an action too (e.g. ctrl+a, media:mute, mouse:4, default)", rest.join(" ")),
        _ => bail!("try: ggctl bind, ggctl bind read, ggctl bind on|off, ggctl bind <key> <action> [--layer fn|deep]"),
    })
}

/// What `main` does for `ggctl bind ...`. A change reads the bindings from the daemon
/// first, after the words have been checked, so a typo fails the same either way.
pub fn action(rest: &[&str]) -> Result<Action> {
    Ok(Action::Send(match parse(rest)? {
        Bind::Show => Request::GetBindings,
        Bind::Read => Request::ReadBindings,
        Bind::Edit(edit) => {
            let ResponseData::Bindings { state } =
                crate::send(crate::connect()?, Request::GetBindings)?
            else {
                bail!("the daemon did not send its key bindings (is it older than ggctl?)");
            };
            edit.apply(&state)
        }
    }))
}

/// A usage's name, for printing.
fn usage_name(code: u8) -> String {
    USAGES
        .iter()
        .find(|u| u.1 == code)
        .map_or_else(|| format!("{code:#04x}"), |u| u.0.to_owned())
}

fn key_name(code: u8) -> String {
    match code {
        0xE7 => "menu".into(),
        0xF0 => "fn".into(),
        c => usage_name(c),
    }
}

/// What an action does, in words.
pub fn describe(action: &KeyAction) -> String {
    match action {
        KeyAction::Default => "default".into(),
        KeyAction::Keys { codes } => codes
            .iter()
            .map(|&c| usage_name(c))
            .collect::<Vec<_>>()
            .join("+"),
        KeyAction::Consumer { usage } => match MEDIA.iter().find(|m| m.1 == *usage) {
            Some(m) => format!("media:{}", m.0),
            None => format!("media usage {usage}"),
        },
        KeyAction::Mouse { button } => format!("mouse:{button}"),
        KeyAction::Cpi => "cpi".into(),
        KeyAction::WheelUp => "wheel:up".into(),
        KeyAction::WheelDown => "wheel:down".into(),
        KeyAction::PanLeft => "pan:left".into(),
        KeyAction::PanRight => "pan:right".into(),
        KeyAction::Firmware { raw } => format!(
            "the keyboard's own shortcut ({:02x} {:02x} {:02x} {:02x})",
            raw[0], raw[1], raw[2], raw[3]
        ),
        KeyAction::Macro { slot } => format!("macro {slot}"),
        KeyAction::Unbound => "none".into(),
    }
}

fn print_layers(b: &gg_ipc::Bindings, indent: &str) {
    let mut any = false;
    for (layer, name) in [
        (Layer::Normal, ""),
        (Layer::Fn, "fn+"),
        (Layer::Second, "deep+"),
    ] {
        for k in b.layer(layer) {
            println!(
                "{indent}{name}{} → {}",
                key_name(k.code),
                describe(&k.action)
            );
            any = true;
        }
    }
    if !any {
        println!("{indent}(all keys as they come from the factory)");
    }
}

/// `ggctl bind`'s answer.
pub fn print(state: &BindingsState) {
    println!(
        "bindings: {}",
        if state.enabled {
            "on (gglinux sends them when the keyboard connects)"
        } else {
            "off (gglinux sends nothing; what it sent stays until the keyboard is unplugged)"
        }
    );
    if let Some(k) = state.bindings.fn_key {
        println!("Fn key: {}", key_name(k));
    }
    print_layers(&state.bindings, "  ");
    match &state.read {
        Some(read) => {
            println!("read from the keyboard on this connection:");
            print_layers(read, "  ");
        }
        None => println!("not read from the keyboard on this connection (ggctl bind read)"),
    }
    if let Some(e) = &state.error {
        println!("error: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::Bindings;

    fn edit(line: &str) -> BindEdit {
        let words: Vec<&str> = line.split_whitespace().collect();
        match parse(&words).unwrap() {
            Bind::Edit(e) => e,
            other => panic!("{line}: {other:?}"),
        }
    }

    fn keys(codes: &[u8]) -> KeyAction {
        KeyAction::Keys {
            codes: codes.to_vec(),
        }
    }

    #[test]
    fn the_examples_parse_to_one_key_on_one_layer() {
        let cases = [
            ("capslock ctrl+a", Layer::Normal, 0x39, keys(&[0xE0, 0x04])),
            (
                "f12 media:playpause",
                Layer::Normal,
                0x45,
                KeyAction::Consumer { usage: 205 },
            ),
            (
                "j mouse:4",
                Layer::Normal,
                0x0D,
                KeyAction::Mouse { button: 4 },
            ),
            ("Q default", Layer::Normal, 0x14, KeyAction::Default),
            ("caps f13", Layer::Normal, 0x39, keys(&[0x68])),
            (
                "fn+f1 media:mute",
                Layer::Fn,
                0x3A,
                KeyAction::Consumer { usage: 226 },
            ),
            (
                "--layer fn f1 media:mute",
                Layer::Fn,
                0x3A,
                KeyAction::Consumer { usage: 226 },
            ),
            (
                "deep+w mouse:back",
                Layer::Second,
                0x1A,
                KeyAction::Mouse { button: 4 },
            ),
            ("w cpi --layer=deep", Layer::Second, 0x1A, KeyAction::Cpi),
            ("menu none", Layer::Normal, 0xE7, KeyAction::Unbound),
            ("0x2c wheel:down", Layer::Normal, 0x2C, KeyAction::WheelDown),
            (
                "esc ctrl+shift+esc",
                Layer::Normal,
                0x29,
                keys(&[0xE0, 0xE1, 0x29]),
            ),
        ];
        for (line, layer, code, action) in cases {
            assert_eq!(
                edit(line),
                BindEdit::Key {
                    layer,
                    code,
                    action
                },
                "{line}"
            );
        }
        assert_eq!(parse(&[]).unwrap(), Bind::Show);
        assert_eq!(parse(&["read"]).unwrap(), Bind::Read);
        assert_eq!(edit("on"), BindEdit::Enabled(true));
        assert_eq!(edit("off"), BindEdit::Enabled(false));
    }

    #[test]
    fn mistakes_are_errors_before_anything_is_asked() {
        for line in [
            "capslock",
            "nokey ctrl+a",
            "a ctrl+nokey",
            "a mouse:9",
            "a mouse:0",
            "a media:louder",
            "a ctrl+shift+alt+super+b",
            "--layer up a b",
            "--layer",
            "a b c",
            "read --layer fn",
        ] {
            let words: Vec<&str> = line.split_whitespace().collect();
            assert!(parse(&words).is_err(), "{line}");
        }
        // Main's `action` parses before it connects: a typo never reaches the daemon.
        assert!(action(&["nokey", "a"]).is_err());
    }

    #[test]
    fn a_key_edit_replaces_that_keys_binding_and_turns_the_bindings_on() {
        let state = BindingsState {
            enabled: false,
            bindings: Bindings {
                normal: vec![
                    KeyBinding {
                        code: 0x39,
                        action: KeyAction::Unbound,
                    },
                    KeyBinding {
                        code: 0x04,
                        action: KeyAction::Cpi,
                    },
                ],
                fn_key: Some(0xE7),
                ..Bindings::default()
            },
            ..BindingsState::default()
        };
        let Request::SetBindings { bindings, enabled } = edit("capslock ctrl+a").apply(&state)
        else {
            panic!()
        };
        assert!(enabled);
        assert_eq!(bindings.normal.len(), 2);
        assert_eq!(bindings.normal[1].action, keys(&[0xE0, 0x04]));
        assert_eq!(bindings.fn_key, Some(0xE7), "the rest kept");
        let Request::SetBindings { bindings, .. } = edit("a default").apply(&state) else {
            panic!()
        };
        assert_eq!(bindings.normal.len(), 1, "default drops it from the list");
        let Request::SetBindings { bindings, enabled } = edit("off").apply(&state) else {
            panic!()
        };
        assert!(!enabled);
        assert_eq!(bindings, state.bindings);
    }

    #[test]
    fn a_state_prints_without_trouble() {
        print(&BindingsState::default());
        print(&BindingsState {
            enabled: true,
            bindings: Bindings {
                fn_layer: vec![KeyBinding {
                    code: 0x3A,
                    action: KeyAction::Consumer { usage: 226 },
                }],
                fn_key: Some(0xF0),
                ..Bindings::default()
            },
            read: Some(Bindings::default()),
            error: Some("the keyboard isn't connected".into()),
        });
    }

    #[test]
    fn actions_print_as_they_are_typed() {
        for word in [
            "ctrl+a",
            "media:playpause",
            "mouse:4",
            "cpi",
            "wheel:up",
            "pan:right",
            "none",
            "f13",
        ] {
            assert_eq!(describe(&action_of(word).unwrap()), word);
        }
        assert_eq!(
            describe(&KeyAction::Firmware { raw: [1, 0, 0, 0] }),
            "the keyboard's own shortcut (01 00 00 00)"
        );
    }
}
