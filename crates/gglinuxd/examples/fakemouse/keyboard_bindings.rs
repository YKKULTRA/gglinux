// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake keyboard's key bindings, kept like the daemon keeps them. Its keyboard starts
//! stock: every key itself on the normal layer, the nine shortcuts of its own on the Fn
//! layer, nothing past the second point. A change is merged onto that as the daemon merges
//! it (so a change to one of those shortcuts is refused), and a read lags like the
//! keyboard: the first read after a change still shows what was there before it.
//!
//! Like the daemon, it refuses `read_bindings` and `set_bindings` while the fake's
//! `keyboard_bindings` gate is off. It announces nothing; a client reads the state again.
//! `Fake` has no field for this, so the state lives here.

use std::sync::Mutex;

use gg_device_keyboard::bindings::{self as kb, Action, Block, Change, FIRMWARE, STOCK_FN_LAYER};
use gg_device_keyboard::layout::US;
use gg_ipc::{
    Bindings, BindingsState, Feature, KeyAction, KeyBinding, Layer, Request, ResponseData,
};

use crate::{error, Fake};

const LAYERS: [(Layer, kb::Layer); 3] = [
    (Layer::Normal, kb::Layer::Normal),
    (Layer::Fn, kb::Layer::Fn),
    (Layer::Second, kb::Layer::Second),
];

type Layers = [Vec<Block>; 3];

/// The fake keyboard and what the fake daemon keeps about it.
struct Board {
    state: BindingsState,
    /// What the keyboard holds now.
    layers: Layers,
    /// What it held before the last change, shown once by the next read.
    before: Option<Layers>,
}

static BOARD: Mutex<Option<Board>> = Mutex::new(None);

/// A stock keyboard's layer.
fn stock(layer: kb::Layer) -> Vec<Block> {
    layer
        .keys()
        .iter()
        .map(|&code| match STOCK_FN_LAYER.iter().find(|s| s.0 == code) {
            Some(&(_, codes)) if layer == kb::Layer::Fn => Block {
                code,
                function: FIRMWARE,
                codes,
            },
            _ => Block::default_for(layer, code),
        })
        .collect()
}

fn stock_layers() -> Layers {
    LAYERS.map(|(_, l)| stock(l))
}

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    let mut guard = BOARD.lock().unwrap_or_else(|e| e.into_inner());
    let board = guard.get_or_insert_with(|| Board {
        state: BindingsState::default(),
        layers: stock_layers(),
        before: None,
    });
    let refusal = s.experimental.refusal(Feature::KeyboardBindings);
    match req {
        Request::GetBindings => ResponseData::Bindings {
            state: board.state.clone(),
        },
        Request::ReadBindings | Request::SetBindings { .. } if refusal.is_some() => {
            error(refusal.unwrap_or_default())
        }
        Request::ReadBindings => {
            let shown = board.before.take().unwrap_or_else(|| board.layers.clone());
            board.state.read = Some(decode(&shown));
            ResponseData::Bindings {
                state: board.state.clone(),
            }
        }
        Request::SetBindings { bindings, enabled } => match set(board, bindings, enabled) {
            Ok(()) => ResponseData::Success,
            Err(message) => error(message),
        },
        other => error(format!(
            "{} is not a key-bindings request",
            crate::log::method_name(&other)
        )),
    }
}

/// Check, merge onto the stock keyboard as the daemon does, and keep.
fn set(board: &mut Board, bindings: Bindings, enabled: bool) -> Result<(), String> {
    check(&bindings)?;
    let base = stock_layers();
    let mut merged: Layers = Default::default();
    for (i, (layer, kl)) in LAYERS.into_iter().enumerate() {
        merged[i] = kb::merge(
            kl,
            &base[i],
            &changes(&bindings, &board.state.bindings, layer),
        )
        .map_err(|e| e.to_string())?;
    }
    if enabled && merged != board.layers {
        board.before = Some(std::mem::replace(&mut board.layers, merged));
    }
    board.state.bindings = bindings;
    board.state.enabled = enabled;
    board.state.error = None;
    Ok(())
}

fn label(code: u8) -> String {
    match US.iter().find(|k| k.0 == code) {
        Some((_, "", ..)) => "Space".into(),
        Some((_, label, ..)) => (*label).to_owned(),
        None => format!("key {code:#04x}"),
    }
}

/// The daemon's checks, with its messages.
fn check(bindings: &Bindings) -> Result<(), String> {
    bindings.validate()?;
    for (layer, kl) in LAYERS {
        if let Some(k) = bindings
            .layer(layer)
            .iter()
            .find(|k| !kl.keys().contains(&k.code))
        {
            return Err(match layer {
                Layer::Second => format!(
                    "{} has a fixed switch, so it has no deep press",
                    label(k.code)
                ),
                Layer::Normal | Layer::Fn => {
                    format!("{} can't be bound on this keyboard", label(k.code))
                }
            });
        }
    }
    match bindings.fn_key {
        Some(k) if !kb::KEYS.contains(&k) => Err(format!("{} can't hold the Fn layer", label(k))),
        _ => Ok(()),
    }
}

fn changes(new: &Bindings, old: &Bindings, layer: Layer) -> Vec<(u8, Change)> {
    let now = new.layer(layer);
    let mut out: Vec<(u8, Change)> = now.iter().map(|b| (b.code, change(&b.action))).collect();
    out.extend(
        old.layer(layer)
            .iter()
            .filter(|b| !now.iter().any(|n| n.code == b.code))
            .map(|b| (b.code, Change::Default)),
    );
    out
}

fn change(action: &KeyAction) -> Change {
    Change::To(match action {
        KeyAction::Default => return Change::Default,
        KeyAction::Keys { codes } => Action::Keys(codes.clone()),
        KeyAction::Consumer { usage } => Action::Consumer(*usage),
        KeyAction::Mouse { button } => Action::Mouse(*button),
        KeyAction::Cpi => Action::Cpi,
        KeyAction::WheelUp => Action::WheelUp,
        KeyAction::WheelDown => Action::WheelDown,
        KeyAction::PanLeft => Action::PanLeft,
        KeyAction::PanRight => Action::PanRight,
        KeyAction::Firmware { raw } => Action::Firmware(*raw),
        KeyAction::Macro { slot } => Action::Macro(*slot),
        KeyAction::Unbound => Action::Unbound,
    })
}

/// What a read shows: every key that isn't its factory default.
fn decode(layers: &Layers) -> Bindings {
    let mut out = Bindings::default();
    for ((layer, kl), blocks) in LAYERS.into_iter().zip(layers) {
        for b in blocks
            .iter()
            .filter(|b| **b != Block::default_for(kl, b.code))
        {
            let action = match b.action() {
                Action::Unbound => KeyAction::Unbound,
                Action::Mouse(button) => KeyAction::Mouse { button },
                Action::Cpi => KeyAction::Cpi,
                Action::WheelUp => KeyAction::WheelUp,
                Action::WheelDown => KeyAction::WheelDown,
                Action::PanLeft => KeyAction::PanLeft,
                Action::PanRight => KeyAction::PanRight,
                Action::Keys(codes) => KeyAction::Keys { codes },
                Action::Consumer(usage) => KeyAction::Consumer { usage },
                Action::Firmware(raw) => KeyAction::Firmware { raw },
                Action::Macro(slot) => KeyAction::Macro { slot },
                Action::External(_) | Action::Other { .. } => continue,
            };
            out.layer_mut(layer).push(KeyBinding {
                code: b.code,
                action,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Board {
        Board {
            state: BindingsState::default(),
            layers: stock_layers(),
            before: None,
        }
    }

    #[test]
    fn a_stock_read_shows_the_nine_shortcuts_and_a_change_shows_one_read_late() {
        let mut b = fresh();
        assert_eq!(decode(&b.layers).fn_layer.len(), 9);
        let caps = Bindings {
            normal: vec![KeyBinding {
                code: 0x39,
                action: KeyAction::Keys {
                    codes: vec![0xE0, 0x04],
                },
            }],
            ..Bindings::default()
        };
        set(&mut b, caps.clone(), true).unwrap();
        let first = b.before.take().unwrap();
        assert!(decode(&first).normal.is_empty(), "the first read lags");
        assert_eq!(decode(&b.layers).normal, caps.normal);
        let f9 = Bindings {
            fn_layer: vec![KeyBinding {
                code: 0x42,
                action: KeyAction::Cpi,
            }],
            ..Bindings::default()
        };
        assert!(set(&mut b, f9, true).unwrap_err().contains("own shortcuts"));
        assert_eq!(b.state.bindings, caps, "nothing kept");
    }
}
