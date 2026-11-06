// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apex Pro TKL Gen 3's switches, in the keyboard card: the actuation
//! point, own points per key, the deep-press point, Rapid Trigger and Rapid Tap.
//! The deep-press point is greyed, with the reason, while `keyboard_bindings` is off
//! in Settings → Experimental.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use gg_ipc::{
    Experimental, Feature, KeyActuation, KeyInfo, KeyboardState, Request, Switches, TapMode,
    TapPair,
};
use slint::{Color, ComponentHandle, Model, SharedString};

use crate::client::{Client, State, TIMEOUT};
use crate::keyboard::{keyboard, marker, names, SURFACE_2, TEXT, TEXT_DIM};
use crate::{act, sync_rows, AppWindow, Forms, Key, SwitchesUi, TapRow};

const MAX_PAIRS: usize = 10;
const TAP_MODES: [TapMode; 4] = [
    TapMode::LastWins,
    TapMode::Neither,
    TapMode::FirstWins,
    TapMode::SecondWins,
];

/// A change to the switches, from a control's values.
type SwitchPatch = Rc<dyn Fn(&mut Switches, &KeyboardState, &SwitchesForm, [i32; 4], i32)>;

/// What the switches panel keeps between the daemon's answers.
#[derive(Debug, Default)]
pub struct SwitchesForm {
    /// The keys picked on the switches picture.
    pub selected: BTreeSet<u8>,
}

impl SwitchesForm {
    /// Select a key on the switches picture, or let it go. Only keys with
    /// adjustable switches can be picked.
    pub fn toggle(&mut self, kb: &KeyboardState, code: u8) {
        if kb.analog_keys.contains(&code) && !self.selected.remove(&code) {
            self.selected.insert(code);
        }
    }
}

fn mm(tenths: u8) -> String {
    format!("{:.1}", tenths as f32 / 10.0)
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

pub fn show(ui: &AppWindow, s: &State, forms: &Forms) {
    let Some(kb) = s.keyboard.as_ref() else {
        // Gone with the daemon, like the lighting's paint, and errors from before too.
        reset(forms);
        mark(ui, false, SharedString::new());
        return;
    };
    show_switches(ui, kb, &forms.switches.borrow());
    show_deep(ui, kb, s.experimental.is_some_and(|e| e.keyboard_bindings));
}

/// The deep-press point: whether it may be set, and which keys have one.
fn show_deep(ui: &AppWindow, kb: &KeyboardState, gated: bool) {
    let g = ui.global::<SwitchesUi>();
    g.set_deep_gated(gated);
    g.set_deep_refusal(
        Experimental::default()
            .refusal(Feature::KeyboardBindings)
            .unwrap_or_default()
            .into(),
    );
    g.set_deep_keys(deep_keys(kb).into());
}

/// The keys with a deep-press point, in words.
fn deep_keys(kb: &KeyboardState) -> String {
    let keys: Vec<String> = kb
        .switches
        .second
        .iter()
        .map(|k| {
            let name = kb
                .layout
                .iter()
                .find(|l| l.code == k.code)
                .map_or_else(|| format!("{:#04x}", k.code), crate::keyboard::label);
            format!("{name} at {} mm", mm(k.actuation))
        })
        .collect();
    if keys.is_empty() {
        "No key has a deep-press point.".into()
    } else {
        format!("Deep press: {}.", keys.join(", "))
    }
}

fn show_switches(ui: &AppWindow, kb: &KeyboardState, f: &SwitchesForm) {
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
                selected: f.selected.contains(&k.code),
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
    g.set_selected(f.selected.len() as i32);
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

/// The selected keys get a deep-press point (`Some`), or none.
pub fn with_second(sw: &Switches, selected: &BTreeSet<u8>, level: Option<u8>) -> Vec<KeyActuation> {
    let mut keys: Vec<KeyActuation> = sw
        .second
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

fn mark(ui: &AppWindow, busy: bool, error: SharedString) {
    let g = ui.global::<SwitchesUi>();
    g.set_busy(busy);
    g.set_error(error);
}

fn redraw(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    show(ui, &client.state(), forms);
}

pub fn bind(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
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
            patch(&mut sw, &kb, &forms.switches.borrow(), args, level);
            act(
                &ui,
                &client,
                Request::SetSwitches { switches: sw },
                TIMEOUT,
                marker(&client, mark, show),
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
            &form.selected,
            (a[0] != 0).then_some(level.clamp(1, 40) as u8),
        );
    }));
    g.on_set_own(move |own| {
        let level = weak
            .upgrade()
            .map_or(15, |ui| ui.global::<SwitchesUi>().get_own_level());
        f([own as i32, 0, 0, 0], level)
    });
    let weak = ui.as_weak();
    let f = switches(Rc::new(|sw, _, form, a, level| {
        sw.second = with_second(
            sw,
            &form.selected,
            (a[0] != 0).then_some(level.clamp(1, 40) as u8),
        );
    }));
    g.on_set_deep(move |on| {
        let level = weak
            .upgrade()
            .map_or(30, |ui| ui.global::<SwitchesUi>().get_deep_level());
        f([on as i32, 0, 0, 0], level)
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
        sw.rapid_trigger.keys = form.selected.iter().copied().collect()
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
            forms2.switches.borrow_mut().toggle(&kb, k.code);
        }
        redraw(&ui, &client2, &forms2);
    });
    let weak = ui.as_weak();
    let client2 = client.clone();
    let forms2 = forms.clone();
    g.on_clear_selection(move || {
        forms2.switches.borrow_mut().selected.clear();
        if let Some(ui) = weak.upgrade() {
            redraw(&ui, &client2, &forms2);
        }
    });
}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.switches.borrow_mut() = SwitchesForm::default();
}

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]).
pub(crate) fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let sw = ui.global::<SwitchesUi>();
    match name {
        "switches.set-enabled" => sw.invoke_set_enabled(n != 0),
        "switches.set-actuation" => sw.invoke_set_actuation(n),
        "switches.key" => sw.invoke_key_clicked(n),
        "switches.own-level" => sw.set_own_level(n),
        "switches.set-own" => sw.invoke_set_own(n != 0),
        "switches.clear-selection" => sw.invoke_clear_selection(),
        "switches.deep-level" => sw.set_deep_level(n),
        "switches.set-deep" => sw.invoke_set_deep(n != 0),
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
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::tests::board;
    use gg_ipc::Screen;

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

    #[test]
    fn deep_press_points_replace_the_selected_keys_only_and_are_listed() {
        let mut sw = Switches {
            second: vec![KeyActuation {
                code: 0x07,
                actuation: 25,
            }],
            ..Switches::default()
        };
        let sel: BTreeSet<u8> = [0x04].into();
        sw.second = with_second(&sw, &sel, Some(30));
        assert_eq!(
            sw.second,
            [
                KeyActuation {
                    code: 0x07,
                    actuation: 25
                },
                KeyActuation {
                    code: 0x04,
                    actuation: 30
                }
            ]
        );
        let mut kb = board(&[], Screen::Keyboard);
        kb.switches = sw.clone();
        assert_eq!(deep_keys(&kb), "Deep press: D at 2.5 mm, A at 3.0 mm.");
        let none: BTreeSet<u8> = [0x04, 0x07].into();
        assert!(with_second(&sw, &none, None).is_empty());
        kb.switches.second.clear();
        assert_eq!(deep_keys(&kb), "No key has a deep-press point.");
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
    fn a_second_click_lets_a_key_go_and_fixed_switches_cant_be_picked() {
        let kb = board(&[], Screen::Keyboard);
        let mut f = SwitchesForm::default();
        f.toggle(&kb, 0x29);
        f.toggle(&kb, 0x07);
        assert_eq!(f.selected, [0x07].into());
        f.toggle(&kb, 0x07);
        assert!(f.selected.is_empty());
    }
}
