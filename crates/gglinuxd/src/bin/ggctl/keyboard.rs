// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl keyboard`, `ggctl switches` and `ggctl lighting`: the keyboard as the daemon has
//! it, its switch settings and its lighting zones, idle scene and Reflect.

use anyhow::{bail, Context, Result};
use gg_ipc::{
    IdleLighting, KeyboardState, Lighting, MacroBinding, ReactiveMode, ReflectStatus, Request,
    Rgb, Switches, Zone, ZoneEffect, ZoneKeys, ZonePreset,
};

use crate::Action;

/// What `ggctl keyboard` prints: whether it's there, its lighting, switches and screen.
pub fn print(state: &KeyboardState) {
    let mm = |t: u8| format!("{:.1} mm", f32::from(t) / 10.0);
    println!("connected: {}", if state.connected { "yes" } else { "no" });
    print_lighting(state);
    let s = &state.switches;
    if s.enabled {
        let own = if s.keys.is_empty() {
            String::new()
        } else {
            format!(" ({} keys with their own)", s.keys.len())
        };
        let rt = &s.rapid_trigger;
        let rapid = if rt.enabled {
            let keys = if rt.keys.is_empty() {
                "every key".to_owned()
            } else {
                format!("{} keys", rt.keys.len())
            };
            format!("on, {}, {keys}", mm(rt.sensitivity))
        } else {
            "off".to_owned()
        };
        let tap = if s.rapid_tap.enabled {
            format!("on, {} pairs", s.rapid_tap.pairs.len())
        } else {
            "off".to_owned()
        };
        let protection = match (s.protection.enabled, s.protection.keys.len()) {
            (false, _) => "off".to_owned(),
            (true, 0) => "on, every key".to_owned(),
            (true, n) => format!("on, {n} keys"),
        };
        println!(
            "switches:  actuation {}{own}; Rapid Trigger {rapid}; Rapid Tap {tap}; Protection Mode {protection}",
            mm(s.actuation)
        );
    } else {
        println!("switches:  left to the keyboard");
    }
    if let Some(e) = &state.switches_error {
        println!("           the keyboard refused them: {e}");
    }
    crate::screen::print(state);
}

/// One change to the switch settings.
#[derive(Debug, PartialEq)]
pub enum SwitchEdit {
    Enable(bool),
    /// Every key's actuation point, in tenths of a millimetre; per-key points are dropped.
    Actuation(u8),
    RapidTrigger {
        on: bool,
        sensitivity: Option<u8>,
    },
    /// Protection Mode, on every adjustable key.
    Protection(bool),
}

impl SwitchEdit {
    pub fn apply(self, s: &mut Switches) {
        match self {
            SwitchEdit::Enable(on) => s.enabled = on,
            SwitchEdit::Actuation(tenths) => {
                s.enabled = true;
                s.actuation = tenths;
                s.keys.clear();
            }
            SwitchEdit::Protection(on) => {
                s.enabled |= on;
                s.protection.enabled = on;
                s.protection.keys.clear();
            }
            SwitchEdit::RapidTrigger { on, sensitivity } => {
                s.enabled |= on;
                s.rapid_trigger.enabled = on;
                if let Some(d) = sensitivity {
                    s.rapid_trigger.sensitivity = d;
                }
            }
        }
    }
}

/// `1.5` (millimetres) as tenths of a millimetre.
fn tenths(mm: &str) -> Result<u8> {
    let v: f32 = mm
        .parse()
        .with_context(|| format!("'{mm}' is not a distance in mm"))?;
    let t = (v * 10.0).round();
    if !(1.0..=40.0).contains(&t) {
        bail!("{mm} mm is out of range (0.1 to 4.0)");
    }
    Ok(t as u8)
}

pub fn switches_action(rest: &[&str]) -> Result<Action> {
    Ok(match rest {
        [] => Action::Send(Request::GetKeyboardState),
        ["on"] => Action::EditSwitches(SwitchEdit::Enable(true)),
        ["off"] => Action::EditSwitches(SwitchEdit::Enable(false)),
        ["actuation", mm] => Action::EditSwitches(SwitchEdit::Actuation(tenths(mm)?)),
        ["protection", "on"] => Action::EditSwitches(SwitchEdit::Protection(true)),
        ["protection", "off"] => Action::EditSwitches(SwitchEdit::Protection(false)),
        ["rapid-trigger", "on"] => Action::EditSwitches(SwitchEdit::RapidTrigger {
            on: true,
            sensitivity: None,
        }),
        ["rapid-trigger", "on", mm] => {
            let d = tenths(mm)?;
            if d > 20 {
                bail!("the Rapid Trigger distance goes up to 2.0 mm");
            }
            Action::EditSwitches(SwitchEdit::RapidTrigger {
                on: true,
                sensitivity: Some(d),
            })
        }
        ["rapid-trigger", "off"] => Action::EditSwitches(SwitchEdit::RapidTrigger {
            on: false,
            sensitivity: None,
        }),
        _ => bail!(
            "usage: ggctl switches on|off | actuation 1.5 | rapid-trigger on [0.2]|off | protection on|off"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{args, parse, sends};

    fn edits(line: &str) -> SwitchEdit {
        match parse(&args(line)).unwrap() {
            Action::EditSwitches(e) => e,
            _ => panic!("{line} is not a switch edit"),
        }
    }

    #[test]
    fn switch_commands_edit_what_the_daemon_has() {
        assert_eq!(edits("switches on"), SwitchEdit::Enable(true));
        assert_eq!(
            edits("switches protection on"),
            SwitchEdit::Protection(true)
        );
        let mut s = Switches::default();
        s.protection.keys.push(0x04);
        SwitchEdit::Protection(true).apply(&mut s);
        assert!(s.enabled && s.protection.enabled && s.protection.keys.is_empty());
        SwitchEdit::Protection(false).apply(&mut s);
        assert!(s.enabled && !s.protection.enabled);
        assert!(parse(&args("switches protection maybe")).is_err());
        assert_eq!(edits("switches actuation 1.5"), SwitchEdit::Actuation(15));
        assert_eq!(
            edits("switches rapid-trigger on 0.2"),
            SwitchEdit::RapidTrigger {
                on: true,
                sensitivity: Some(2)
            }
        );
        assert!(parse(&args("switches actuation 4.5")).is_err());
        assert!(parse(&args("switches rapid-trigger on 2.5")).is_err());
        assert!(matches!(sends("switches"), Request::GetKeyboardState));

        let mut s = Switches {
            keys: vec![gg_ipc::KeyActuation {
                code: 0x04,
                actuation: 3,
            }],
            ..Switches::default()
        };
        SwitchEdit::Actuation(20).apply(&mut s);
        assert!(s.enabled && s.actuation == 20 && s.keys.is_empty());
        SwitchEdit::RapidTrigger {
            on: true,
            sensitivity: Some(5),
        }
        .apply(&mut s);
        assert!(s.rapid_trigger.enabled && s.rapid_trigger.sensitivity == 5);
    }
}

/// `#rrggbb`.
fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// A zone effect as `ggctl lighting zones set` takes it.
pub fn describe_effect(e: &ZoneEffect) -> String {
    match *e {
        ZoneEffect::Static { color } => format!("static {}", hex(color)),
        ZoneEffect::PerKey => "per-key (each key its painted colour)".into(),
        ZoneEffect::Breathing { color, period_s } => {
            format!("breathing {} {period_s:.1} s", hex(color))
        }
        ZoneEffect::Wave { period_s } => format!("wave {period_s:.1} s"),
        ZoneEffect::Gradient { from, to } => format!("gradient {} {}", hex(from), hex(to)),
        ZoneEffect::Spectrum { period_s } => format!("spectrum {period_s:.1} s"),
        ZoneEffect::Visualizer { sensitivity } => format!("audio {sensitivity:.2}x"),
        ZoneEffect::Reactive {
            mode,
            color,
            duration_s,
        } => {
            let mode = match mode {
                ReactiveMode::Fade => "fade",
                ReactiveMode::Ripple => "ripple",
            };
            format!("reactive {mode} {} {duration_s:.1} s", hex(color))
        }
        ZoneEffect::Reflect => "reflect (the screen's colours)".into(),
    }
}

fn describe_keys(keys: &ZoneKeys) -> String {
    match keys {
        ZoneKeys::All => "every key".into(),
        ZoneKeys::Keys { codes } => {
            let names: Vec<String> = codes.iter().map(|&c| MacroBinding::key_name(c)).collect();
            format!("{} keys ({})", codes.len(), names.join(","))
        }
    }
}

fn print_zones(zones: &[Zone], indent: &str) {
    if zones.is_empty() {
        println!("{indent}every key dark");
    }
    for (i, z) in zones.iter().enumerate() {
        println!(
            "{indent}{}. {}: {}; {}",
            i + 1,
            z.name,
            describe_keys(&z.keys),
            describe_effect(&z.effect)
        );
    }
}

/// The lighting part of `ggctl keyboard` (and all of `ggctl lighting`).
fn print_lighting(state: &KeyboardState) {
    let l = &state.lighting;
    if !l.enabled {
        println!("lighting:  left to the keyboard");
        return;
    }
    println!(
        "lighting:  on, brightness {}%; zones, later ones over earlier ones:",
        l.brightness
    );
    print_zones(&l.stack(), "           ");
    let i = &l.idle;
    if i.enabled {
        let now = if state.lighting_idle { " (drawn now)" } else { "" };
        println!(
            "idle:      {} min after you're away{now}:",
            i.after_minutes
        );
        print_zones(&i.zones, "           ");
    } else {
        println!("idle:      off");
    }
    match &state.reflect {
        ReflectStatus::Off => {}
        ReflectStatus::Running => println!("Reflect:   looking at the screen"),
        ReflectStatus::Stopped { why } => println!("Reflect:   stopped: {why}"),
    }
}

/// One change to the lighting, made to what the daemon has.
#[derive(Debug, PartialEq)]
pub enum LightingEdit {
    Enable(bool),
    /// A new zone on top of the stack.
    Add(Zone),
    Effect {
        name: String,
        effect: ZoneEffect,
    },
    Keys {
        name: String,
        keys: ZoneKeys,
    },
    Remove(String),
    /// One place up (painted later) or down.
    Move {
        name: String,
        up: bool,
    },
    Idle {
        on: bool,
        minutes: Option<u16>,
    },
    /// The idle scene: one effect on every key, or every key dark.
    IdleScene(Option<ZoneEffect>),
    Reflect(bool),
}

impl LightingEdit {
    pub fn apply(self, l: &mut Lighting) -> Result<()> {
        // Find a zone by its name, or say which there are.
        fn at(zones: &[Zone], name: &str) -> Result<usize> {
            zones.iter().position(|z| z.name == name).with_context(|| {
                let names: Vec<&str> = zones.iter().map(|z| z.name.as_str()).collect();
                format!("no zone called '{name}' (zones: {})", names.join(", "))
            })
        }
        match self {
            LightingEdit::Enable(on) => l.enabled = on,
            LightingEdit::Add(zone) => {
                let zones = l.zones_mut();
                if zones.iter().any(|z| z.name == zone.name) {
                    bail!("there is a zone called '{}' already", zone.name);
                }
                zones.push(zone);
                l.enabled = true;
            }
            LightingEdit::Effect { name, effect } => {
                let zones = l.zones_mut();
                let i = at(zones, &name)?;
                zones[i].effect = effect;
                l.enabled = true;
            }
            LightingEdit::Keys { name, keys } => {
                let zones = l.zones_mut();
                let i = at(zones, &name)?;
                zones[i].keys = keys;
            }
            LightingEdit::Remove(name) => {
                let zones = l.zones_mut();
                let i = at(zones, &name)?;
                zones.remove(i);
            }
            LightingEdit::Move { name, up } => {
                let zones = l.zones_mut();
                let i = at(zones, &name)?;
                let j = if up { i + 1 } else { i.wrapping_sub(1) };
                if j >= zones.len() {
                    bail!(
                        "'{name}' is already the {} zone",
                        if up { "top" } else { "bottom" }
                    );
                }
                zones.swap(i, j);
            }
            LightingEdit::Idle { on, minutes } => {
                l.idle.enabled = on;
                if let Some(m) = minutes {
                    l.idle.after_minutes = m;
                }
            }
            LightingEdit::IdleScene(effect) => {
                l.idle.zones = effect
                    .map(|e| Zone::new("idle", ZoneKeys::All, e))
                    .into_iter()
                    .collect();
            }
            LightingEdit::Reflect(true) => {
                l.reflect_on();
                l.enabled = true;
            }
            LightingEdit::Reflect(false) => l.reflect_off(),
        }
        Ok(())
    }
}

/// `ff8800` or `#ff8800`.
fn color(word: &str) -> Result<Rgb> {
    let hex = word.strip_prefix('#').unwrap_or(word);
    let bad = || format!("'{word}' is not a colour (try: ff8800)");
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!(bad());
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).with_context(bad);
    Ok(Rgb {
        r: byte(0)?,
        g: byte(2)?,
        b: byte(4)?,
    })
}

/// A number of seconds (or a sensitivity), or `default` when the word isn't there.
fn number(word: Option<&&str>, default: f32) -> Result<f32> {
    match word {
        None => Ok(default),
        Some(w) => w
            .trim_end_matches('s')
            .parse()
            .with_context(|| format!("'{w}' is not a number")),
    }
}

/// An effect from its words, as `ggctl --help` lists them.
fn effect(words: &[&str]) -> Result<ZoneEffect> {
    let e = match words {
        ["static", c] => ZoneEffect::Static { color: color(c)? },
        ["per-key"] => ZoneEffect::PerKey,
        ["breathing", c, rest @ ..] if rest.len() <= 1 => ZoneEffect::Breathing {
            color: color(c)?,
            period_s: number(rest.first(), 4.0)?,
        },
        ["wave", rest @ ..] if rest.len() <= 1 => ZoneEffect::Wave {
            period_s: number(rest.first(), 4.0)?,
        },
        ["gradient", from, to] => ZoneEffect::Gradient {
            from: color(from)?,
            to: color(to)?,
        },
        ["spectrum", rest @ ..] if rest.len() <= 1 => ZoneEffect::Spectrum {
            period_s: number(rest.first(), 6.0)?,
        },
        ["audio", rest @ ..] if rest.len() <= 1 => ZoneEffect::Visualizer {
            sensitivity: number(rest.first().map(|w| w.trim_end_matches('x')).as_ref(), 1.0)?,
        },
        ["reactive", mode, c, rest @ ..] if rest.len() <= 1 => ZoneEffect::Reactive {
            mode: match *mode {
                "fade" => ReactiveMode::Fade,
                "ripple" => ReactiveMode::Ripple,
                other => bail!("'{other}' is not fade or ripple"),
            },
            color: color(c)?,
            duration_s: number(rest.first(), 0.6)?,
        },
        ["reflect"] => ZoneEffect::Reflect,
        _ => bail!(
            "an effect is one of: static ff0000, per-key, breathing ff0000 [4], wave [4], \
             gradient ff0000 0000ff, spectrum [6], audio [1], reactive fade|ripple ffffff [0.6], reflect"
        ),
    };
    e.validate().map_err(anyhow::Error::msg)?;
    Ok(e)
}

/// A zone's keys: a preset's name, or key names with commas between them.
fn zone_keys(word: &str) -> Result<ZoneKeys> {
    if let Some(p) = ZonePreset::from_key(word) {
        return Ok(p.keys());
    }
    let codes = word
        .split(',')
        .filter(|w| !w.is_empty())
        .map(|w| {
            MacroBinding::key_by_name(w).with_context(|| {
                format!("'{w}' is not a key name or one of wasd, arrows, f-row, numbers, all")
            })
        })
        .collect::<Result<Vec<u8>>>()?;
    if codes.is_empty() {
        bail!("no keys given");
    }
    Ok(ZoneKeys::Keys { codes })
}

const LIGHTING_USAGE: &str = "usage: ggctl lighting [on|off | zones add NAME KEYS EFFECT… | zones set NAME EFFECT… | \
zones keys NAME KEYS | zones rm|up|down NAME | idle on [MINUTES]|off | idle scene off|EFFECT… | reflect on|off]";

pub fn lighting_action(rest: &[&str]) -> Result<Action> {
    let edit = match rest {
        [] | ["zones"] => return Ok(Action::Send(Request::GetKeyboardState)),
        ["on"] => LightingEdit::Enable(true),
        ["off"] => LightingEdit::Enable(false),
        ["zones", "add", name, keys, effect_words @ ..] if !effect_words.is_empty() => {
            LightingEdit::Add(Zone::new(*name, zone_keys(keys)?, effect(effect_words)?))
        }
        ["zones", "set", name, effect_words @ ..] if !effect_words.is_empty() => {
            LightingEdit::Effect {
                name: name.to_string(),
                effect: effect(effect_words)?,
            }
        }
        ["zones", "keys", name, keys] => LightingEdit::Keys {
            name: name.to_string(),
            keys: zone_keys(keys)?,
        },
        ["zones", "rm", name] => LightingEdit::Remove(name.to_string()),
        ["zones", dir @ ("up" | "down"), name] => LightingEdit::Move {
            name: name.to_string(),
            up: *dir == "up",
        },
        ["idle", "on"] => LightingEdit::Idle {
            on: true,
            minutes: None,
        },
        ["idle", "on", minutes] => {
            let m: u16 = minutes
                .trim_end_matches("min")
                .parse()
                .with_context(|| format!("'{minutes}' is not a number of minutes"))?;
            if !Lighting::IDLE_MINUTES.contains(&m) {
                bail!("the idle time goes from 0 to 120 minutes");
            }
            LightingEdit::Idle {
                on: true,
                minutes: Some(m),
            }
        }
        ["idle", "off"] => LightingEdit::Idle {
            on: false,
            minutes: None,
        },
        ["idle", "scene", "off"] => LightingEdit::IdleScene(None),
        ["idle", "scene", effect_words @ ..] if !effect_words.is_empty() => {
            LightingEdit::IdleScene(Some(effect(effect_words)?))
        }
        ["reflect", "on"] => LightingEdit::Reflect(true),
        ["reflect", "off"] => LightingEdit::Reflect(false),
        _ => bail!(LIGHTING_USAGE),
    };
    Ok(Action::EditLighting(edit))
}

#[cfg(test)]
mod lighting_tests {
    use super::*;
    use crate::{args, parse, sends};

    fn edits(line: &str) -> LightingEdit {
        match parse(&args(line)).unwrap() {
            Action::EditLighting(e) => e,
            _ => panic!("{line} is not a lighting edit"),
        }
    }

    const GREEN: Rgb = Rgb { r: 0, g: 255, b: 0 };

    #[test]
    fn lighting_commands_parse_into_edits() {
        assert!(matches!(sends("lighting"), Request::GetKeyboardState));
        assert!(matches!(sends("lighting zones"), Request::GetKeyboardState));
        assert_eq!(edits("lighting off"), LightingEdit::Enable(false));
        assert_eq!(
            edits("lighting zones add WASD wasd static 00ff00"),
            LightingEdit::Add(Zone::new(
                "WASD",
                ZonePreset::Wasd.keys(),
                ZoneEffect::Static { color: GREEN }
            ))
        );
        assert_eq!(
            edits("lighting zones add keys a,s,0x07 wave 2"),
            LightingEdit::Add(Zone::new(
                "keys",
                ZoneKeys::Keys {
                    codes: vec![0x04, 0x16, 0x07]
                },
                ZoneEffect::Wave { period_s: 2.0 }
            ))
        );
        assert_eq!(
            edits("lighting zones set WASD gradient #ff0000 0000ff"),
            LightingEdit::Effect {
                name: "WASD".into(),
                effect: ZoneEffect::Gradient {
                    from: Rgb { r: 255, g: 0, b: 0 },
                    to: Rgb { r: 0, g: 0, b: 255 },
                }
            }
        );
        assert_eq!(
            edits("lighting zones set all reactive ripple ffffff 1.5"),
            LightingEdit::Effect {
                name: "all".into(),
                effect: ZoneEffect::Reactive {
                    mode: ReactiveMode::Ripple,
                    color: Rgb {
                        r: 255,
                        g: 255,
                        b: 255
                    },
                    duration_s: 1.5,
                }
            }
        );
        assert_eq!(
            edits("lighting zones set all audio 2x"),
            LightingEdit::Effect {
                name: "all".into(),
                effect: ZoneEffect::Visualizer { sensitivity: 2.0 }
            }
        );
        assert_eq!(
            edits("lighting zones up WASD"),
            LightingEdit::Move {
                name: "WASD".into(),
                up: true
            }
        );
        assert_eq!(
            edits("lighting idle on 10"),
            LightingEdit::Idle {
                on: true,
                minutes: Some(10)
            }
        );
        assert_eq!(edits("lighting idle scene off"), LightingEdit::IdleScene(None));
        assert_eq!(
            edits("lighting idle scene breathing 0000ff 6"),
            LightingEdit::IdleScene(Some(ZoneEffect::Breathing {
                color: Rgb { r: 0, g: 0, b: 255 },
                period_s: 6.0
            }))
        );
        assert_eq!(edits("lighting reflect on"), LightingEdit::Reflect(true));
    }

    #[test]
    fn bad_lighting_commands_say_what_is_wrong() {
        let err = |line: &str| match parse(&args(line)) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("{line} parsed"),
        };
        assert!(err("lighting zones add z wasd static red").contains("not a colour"));
        assert!(err("lighting zones add z qwerty static ff0000").contains("not a key name"));
        assert!(err("lighting zones set z wave 0.1").contains("out of range"));
        assert!(err("lighting zones set z sparkle").contains("an effect is one of"));
        assert!(err("lighting idle on 500").contains("0 to 120"));
        assert!(err("lighting reflect maybe").starts_with("usage: ggctl lighting"));
    }

    #[test]
    fn edits_change_the_stack_by_zone_name() {
        let mut l = Lighting::default(); // from before zones: a wave over every key
        edits("lighting zones add WASD wasd static 00ff00")
            .apply(&mut l)
            .unwrap();
        assert!(l.enabled, "adding a zone turns gglinux's lighting on");
        let names = |l: &Lighting| -> Vec<String> { l.stack().iter().map(|z| z.name.clone()).collect() };
        assert_eq!(names(&l), ["all", "WASD"], "the old effect stays at the bottom");
        let again = edits("lighting zones add WASD arrows wave").apply(&mut l);
        assert!(again.unwrap_err().to_string().contains("already"));
        edits("lighting zones down WASD").apply(&mut l).unwrap();
        assert_eq!(names(&l), ["WASD", "all"]);
        let low = edits("lighting zones down WASD").apply(&mut l);
        assert!(low.unwrap_err().to_string().contains("bottom"));
        edits("lighting zones keys WASD arrows").apply(&mut l).unwrap();
        assert_eq!(l.stack()[0].keys, ZonePreset::Arrows.keys());
        let missing = edits("lighting zones rm nope").apply(&mut l).unwrap_err();
        assert_eq!(
            missing.to_string(),
            "no zone called 'nope' (zones: WASD, all)"
        );
        edits("lighting zones rm all").apply(&mut l).unwrap();
        assert_eq!(names(&l), ["WASD"]);

        edits("lighting idle on 3").apply(&mut l).unwrap();
        edits("lighting idle scene spectrum").apply(&mut l).unwrap();
        assert_eq!(
            l.idle,
            IdleLighting {
                enabled: true,
                after_minutes: 3,
                zones: vec![Zone::new(
                    "idle",
                    ZoneKeys::All,
                    ZoneEffect::Spectrum { period_s: 6.0 }
                )],
            }
        );
        edits("lighting idle off").apply(&mut l).unwrap();
        assert!(!l.idle.enabled && l.idle.after_minutes == 3);

        edits("lighting reflect on").apply(&mut l).unwrap();
        assert!(l.uses_reflect());
        edits("lighting reflect off").apply(&mut l).unwrap();
        assert!(!l.uses_reflect());
        assert!(l.validate().is_ok());
    }
}
