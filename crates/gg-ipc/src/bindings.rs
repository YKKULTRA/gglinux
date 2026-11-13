// SPDX-License-Identifier: GPL-3.0-or-later
//! Key bindings on the Apex Pro TKL Gen 3: what each key does on its own, while the Fn key is
//! held, and past its second (deep-press) actuation point.
//!
//! The keyboard keeps bindings in RAM; gglinux sends them again whenever the keyboard
//! connects, like the switches. Only keys whose action differs from the default are listed.
//! Built from Apex Control's protocol reference (CC BY 4.0); see `docs/PROTOCOLS.md`.

use serde::{Deserialize, Serialize};

/// Which of the keyboard's three binding layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// What a key does on its own.
    Normal,
    /// What it does while the Fn (SteelSeries) key is held.
    Fn,
    /// What an adjustable key does past its second actuation point.
    Second,
}

/// What a key does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeyAction {
    /// The factory action: the key itself on the normal layer, nothing on the others.
    Default,
    /// One to four keyboard usages sent together; modifiers are usages `0xE0..=0xE7`.
    Keys {
        codes: Vec<u8>,
    },
    /// A consumer-page usage (media keys): 205 play/pause, 181 next, 182 previous, 226 mute,
    /// 233 volume up, 234 volume down.
    Consumer {
        usage: u16,
    },
    /// Mouse button 1 to 8.
    Mouse {
        button: u8,
    },
    Cpi,
    WheelUp,
    WheelDown,
    PanLeft,
    PanRight,
    /// One of the keyboard's own built-in functions (brightness, media, screen menu). Its four
    /// bytes are opaque: kept exactly as the keyboard reported them, never made up.
    Firmware {
        raw: [u8; 4],
    },
    /// A macro stored on the keyboard, by slot. Its format isn't known yet.
    Macro {
        slot: u8,
    },
    /// Does nothing.
    Unbound,
}

/// One key's binding on one layer, by HID usage code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    pub code: u8,
    pub action: KeyAction,
}

/// The bindings that differ from the factory ones.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Bindings {
    pub normal: Vec<KeyBinding>,
    pub fn_layer: Vec<KeyBinding>,
    pub second: Vec<KeyBinding>,
    /// The key that holds the Fn layer, by HID code; `None` is the keyboard's own (the
    /// SteelSeries key, `0xF0`), sent once if gglinux had moved it.
    pub fn_key: Option<u8>,
}

impl Bindings {
    /// The most keyboard usages one key can send at once.
    pub const MAX_KEYS: usize = 4;
    /// How many keys can have a deep-press binding.
    pub const MAX_SECOND: usize = 8;

    pub fn layer(&self, layer: Layer) -> &[KeyBinding] {
        match layer {
            Layer::Normal => &self.normal,
            Layer::Fn => &self.fn_layer,
            Layer::Second => &self.second,
        }
    }

    pub fn layer_mut(&mut self, layer: Layer) -> &mut Vec<KeyBinding> {
        match layer {
            Layer::Normal => &mut self.normal,
            Layer::Fn => &mut self.fn_layer,
            Layer::Second => &mut self.second,
        }
    }

    /// Check what can be checked without the keyboard, with a message meant for the user.
    /// Which keys exist and which have a second actuation point is the daemon's to check.
    pub fn validate(&self) -> Result<(), String> {
        // The keyboard's own limit, as GG states it ("Maximum of 8 dual actuation keybinds
        // permitted", GG 123, 2026-11-13).
        let deep = self
            .layer(Layer::Second)
            .iter()
            .filter(|b| !matches!(b.action, KeyAction::Default))
            .count();
        if deep > Self::MAX_SECOND {
            return Err(format!(
                "the keyboard takes at most {} deep-press bindings, not {deep}",
                Self::MAX_SECOND
            ));
        }
        for layer in [Layer::Normal, Layer::Fn, Layer::Second] {
            let mut seen = Vec::new();
            for b in self.layer(layer) {
                if seen.contains(&b.code) {
                    return Err(format!("key {:#04x} is bound twice on one layer", b.code));
                }
                seen.push(b.code);
                match &b.action {
                    KeyAction::Keys { codes } => {
                        if codes.is_empty() || codes.len() > Self::MAX_KEYS {
                            return Err(format!(
                                "a key sends 1 to {} keys at once, not {}",
                                Self::MAX_KEYS,
                                codes.len()
                            ));
                        }
                        if let Some(c) = codes.iter().find(|&&c| !(0x04..=0xE7).contains(&c)) {
                            return Err(format!("{c:#04x} is not a keyboard key"));
                        }
                    }
                    KeyAction::Mouse { button } if !(1..=8).contains(button) => {
                        return Err(format!("mouse button {button} out of range (1..=8)"));
                    }
                    KeyAction::Consumer { usage: 0 } => {
                        return Err("a media key needs a usage".into());
                    }
                    _ => {}
                }
            }
        }
        if let Some(k) = self.fn_key {
            if k < 0x04 {
                return Err(format!("{k:#04x} can't be the Fn key"));
            }
        }
        Ok(())
    }
}

/// The keyboard's bindings as the daemon sees them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BindingsState {
    /// gglinux sends `bindings` whenever the keyboard connects. Off sends nothing more: what was
    /// sent stays on the keyboard until it's unplugged.
    pub enabled: bool,
    pub bindings: Bindings,
    /// What the keyboard reported at the last read on this connection, if any.
    pub read: Option<Bindings>,
    /// Why the last read or write didn't go through.
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(code: u8, action: KeyAction) -> KeyBinding {
        KeyBinding { code, action }
    }

    #[test]
    fn caps_lock_as_ctrl_a_is_valid_and_round_trips() {
        let b = Bindings {
            normal: vec![bind(
                0x39,
                KeyAction::Keys {
                    codes: vec![0xE0, 0x04],
                },
            )],
            ..Bindings::default()
        };
        b.validate().unwrap();
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains(r#""kind":"keys""#), "{json}");
        assert_eq!(serde_json::from_str::<Bindings>(&json).unwrap(), b);
    }

    #[test]
    fn bad_bindings_are_refused_with_a_reason() {
        let cases = [
            vec![
                bind(0x04, KeyAction::Unbound),
                bind(0x04, KeyAction::Default),
            ],
            vec![bind(0x04, KeyAction::Keys { codes: vec![] })],
            vec![bind(
                0x04,
                KeyAction::Keys {
                    codes: vec![4, 5, 6, 7, 8],
                },
            )],
            vec![bind(0x04, KeyAction::Keys { codes: vec![0xF0] })],
            vec![bind(0x04, KeyAction::Mouse { button: 9 })],
            vec![bind(0x04, KeyAction::Consumer { usage: 0 })],
        ];
        for normal in cases {
            let b = Bindings {
                normal: normal.clone(),
                ..Bindings::default()
            };
            assert!(b.validate().is_err(), "{normal:?}");
        }
        // The same key on two different layers is fine.
        let b = Bindings {
            normal: vec![bind(0x04, KeyAction::Unbound)],
            fn_layer: vec![bind(0x04, KeyAction::Cpi)],
            ..Bindings::default()
        };
        b.validate().unwrap();
    }

    #[test]
    fn firmware_functions_keep_their_bytes() {
        let a = KeyAction::Firmware {
            raw: [0x0A, 0, 0, 0],
        };
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<KeyAction>(&json).unwrap(), a);
    }
}
