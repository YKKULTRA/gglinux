// SPDX-License-Identifier: GPL-3.0-or-later
//! The zone stack as key colours: each zone paints its keys over the zones before it.
//!
//! A [`Scene`] is built once a frame from the stack and the board, so each zone's keys
//! and its width are worked out once, not once per key. Pure, like [`super::effects`].

use std::collections::HashMap;

use gg_device_keyboard::layout::WIDTH;
use gg_device_keyboard::Rgb;
use gg_ipc::{KeyColor, Reactive, Zone, ZoneEffect, ZoneKeys};

use super::effects::{self, Hit, Pos};

/// What a frame draws from besides the stack: the moment, the presses, the audio levels
/// and the screen's colours under each key (empty while Reflect has no frame).
pub struct Inputs<'a> {
    pub t: f64,
    pub hits: &'a [Hit],
    pub bands: &'a [f32],
    pub screen: &'a HashMap<u8, Rgb>,
}

/// One zone, with the span its wave or gradient runs across.
struct Placed<'a> {
    zone: &'a Zone,
    /// The centres of its leftmost and rightmost keys, in key units.
    left: f32,
    right: f32,
}

/// A stack ready to draw on one board.
pub struct Scene<'a> {
    zones: Vec<Placed<'a>>,
    /// The per-key colours, for zones that show them.
    keys: &'a [KeyColor],
    positions: &'a HashMap<u8, Pos>,
}

impl<'a> Scene<'a> {
    pub fn new(zones: &'a [Zone], keys: &'a [KeyColor], positions: &'a HashMap<u8, Pos>) -> Self {
        let zones = zones
            .iter()
            .map(|zone| {
                let (left, right) = match &zone.keys {
                    // The whole board, as the one effect before zones ran across it.
                    ZoneKeys::All => (0.0, WIDTH),
                    ZoneKeys::Keys { codes } => codes
                        .iter()
                        .filter_map(|c| positions.get(c))
                        .fold((f32::MAX, f32::MIN), |(l, r), p| (l.min(p.x), r.max(p.x))),
                };
                Placed { zone, left, right }
            })
            .collect();
        Scene {
            zones,
            keys,
            positions,
        }
    }

    /// The colour the stack gives key `code`: black under every zone, then each zone
    /// that has the key paints it in turn.
    pub fn color(&self, code: u8, inputs: &Inputs) -> Rgb {
        let pos = self.positions.get(&code);
        let t = inputs.t;
        let mut c = Rgb::default();
        for p in &self.zones {
            if !p.zone.keys.contains(code) {
                continue;
            }
            c = match p.zone.effect {
                ZoneEffect::Static { color } => effects::rgb(color),
                ZoneEffect::PerKey => self
                    .keys
                    .iter()
                    .find(|k| k.code == code)
                    .map(|k| effects::rgb(k.color))
                    .unwrap_or_default(),
                ZoneEffect::Breathing { color, period_s } => effects::breathe(color, t, period_s),
                ZoneEffect::Wave { period_s } => {
                    effects::hsv(p.across(pos) - effects::cycle(t, period_s))
                }
                ZoneEffect::Gradient { from, to } => {
                    effects::mix(effects::rgb(from), effects::rgb(to), p.across(pos))
                }
                ZoneEffect::Spectrum { period_s } => effects::hsv(effects::cycle(t, period_s)),
                ZoneEffect::Visualizer { sensitivity } => {
                    effects::visualizer(pos, inputs.bands, sensitivity)
                }
                ZoneEffect::Reactive {
                    mode,
                    color,
                    duration_s,
                } => {
                    let r = Reactive {
                        enabled: true,
                        mode,
                        color,
                        duration_s,
                    };
                    let w = effects::reactive_weight(&r, self.positions, code, t, inputs.hits);
                    effects::mix(c, effects::rgb(color), w)
                }
                // No frame yet, or a key the screen has no colour for: what's below shows.
                ZoneEffect::Reflect => inputs.screen.get(&code).copied().unwrap_or(c),
            };
        }
        c
    }
}

impl Placed<'_> {
    /// How far across the zone a key sits: 0 on its leftmost key, 1 on its rightmost.
    fn across(&self, pos: Option<&Pos>) -> f32 {
        let Some(p) = pos else { return 0.0 };
        let span = self.right - self.left;
        if span > 0.0 {
            ((p.x - self.left) / span).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// Whether frames must keep coming for this stack.
pub fn animated(zones: &[Zone]) -> bool {
    zones.iter().any(|z| z.effect.animated())
}

/// Whether a zone of the stack shows key presses.
pub fn reactive(zones: &[Zone]) -> bool {
    zones
        .iter()
        .any(|z| matches!(z.effect, ZoneEffect::Reactive { .. }))
}

/// Whether a zone of the stack draws what's playing.
pub fn listens(zones: &[Zone]) -> bool {
    zones
        .iter()
        .any(|z| matches!(z.effect, ZoneEffect::Visualizer { .. }))
}

/// Whether a zone of the stack takes the screen's colours.
pub fn reflects(zones: &[Zone]) -> bool {
    zones.iter().any(|z| z.effect == ZoneEffect::Reflect)
}

/// The longest a press shows in any zone of the stack, in seconds.
pub fn press_duration(zones: &[Zone]) -> f32 {
    zones
        .iter()
        .filter_map(|z| match z.effect {
            ZoneEffect::Reactive { duration_s, .. } if duration_s.is_finite() => Some(duration_s),
            _ => None,
        })
        .fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_device_keyboard::layout::US;
    use gg_ipc::{ReactiveMode, ZonePreset};

    const RED: gg_ipc::Rgb = gg_ipc::Rgb { r: 255, g: 0, b: 0 };
    const GREEN: gg_ipc::Rgb = gg_ipc::Rgb { r: 0, g: 255, b: 0 };
    const BLUE: gg_ipc::Rgb = gg_ipc::Rgb { r: 0, g: 0, b: 255 };

    fn dev(c: gg_ipc::Rgb) -> Rgb {
        effects::rgb(c)
    }

    fn at(t: f64) -> Inputs<'static> {
        static NONE: std::sync::OnceLock<HashMap<u8, Rgb>> = std::sync::OnceLock::new();
        Inputs {
            t,
            hits: &[],
            bands: &[],
            screen: NONE.get_or_init(HashMap::new),
        }
    }

    fn keys(codes: &[u8]) -> ZoneKeys {
        ZoneKeys::Keys {
            codes: codes.to_vec(),
        }
    }

    #[test]
    fn later_zones_paint_over_earlier_ones_and_keys_in_no_zone_are_dark() {
        let p = effects::positions(&US);
        let zones = [
            Zone::new("base", ZoneKeys::All, ZoneEffect::Static { color: BLUE }),
            Zone::new("wasd", ZonePreset::Wasd.keys(), ZoneEffect::Static { color: RED }),
        ];
        let scene = Scene::new(&zones, &[], &p);
        assert_eq!(scene.color(0x1A, &at(0.0)), dev(RED), "W: the WASD zone");
        assert_eq!(scene.color(0x29, &at(0.0)), dev(BLUE), "Esc: the base");
        let only_wasd = [zones[1].clone()];
        let scene = Scene::new(&only_wasd, &[], &p);
        assert_eq!(scene.color(0x29, &at(0.0)), Rgb::default(), "in no zone");
        let swapped = [zones[1].clone(), zones[0].clone()];
        let scene = Scene::new(&swapped, &[], &p);
        assert_eq!(scene.color(0x1A, &at(0.0)), dev(BLUE), "the order is the stack");
    }

    #[test]
    fn a_gradient_runs_across_its_own_zone_from_edge_to_edge() {
        let p = effects::positions(&US);
        let row = [0x1E, 0x1F, 0x20, 0x21, 0x22]; // 1 to 5
        let zones = [Zone::new(
            "g",
            keys(&row),
            ZoneEffect::Gradient {
                from: RED,
                to: BLUE,
            },
        )];
        let scene = Scene::new(&zones, &[], &p);
        assert_eq!(scene.color(0x1E, &at(0.0)), dev(RED), "the left edge");
        assert_eq!(scene.color(0x22, &at(0.0)), dev(BLUE), "the right edge");
        let mid = scene.color(0x20, &at(0.0));
        assert_eq!((mid.r, mid.g, mid.b), (128, 0, 128), "halfway: halfway");
    }

    #[test]
    fn a_wave_in_a_small_zone_runs_the_whole_rainbow_across_it() {
        let p = effects::positions(&US);
        let wasd = Zone::new("w", ZonePreset::Wasd.keys(), ZoneEffect::Wave { period_s: 4.0 });
        let zones = [wasd];
        let scene = Scene::new(&zones, &[], &p);
        // A is the leftmost key of WASD, D the rightmost: the two ends of one rainbow.
        assert_eq!(scene.color(0x04, &at(0.0)), effects::hsv(0.0));
        assert_eq!(scene.color(0x07, &at(0.0)), effects::hsv(1.0));
        assert_ne!(scene.color(0x16, &at(0.0)), scene.color(0x04, &at(0.0)));
        assert_ne!(scene.color(0x04, &at(1.0)), scene.color(0x04, &at(0.0)), "and it moves");
    }

    #[test]
    fn the_one_zone_of_old_lighting_draws_exactly_what_the_effect_drew() {
        let p = effects::positions(&US);
        let zones = [Zone::new("all", ZoneKeys::All, ZoneEffect::Wave { period_s: 4.0 })];
        let scene = Scene::new(&zones, &[], &p);
        for &(code, ..) in US.iter() {
            let x = p[&code].x / WIDTH;
            assert_eq!(
                scene.color(code, &at(1.3)),
                effects::hsv(x - effects::cycle(1.3, 4.0)),
                "{code:#04x}"
            );
        }
    }

    #[test]
    fn per_key_zones_show_the_painted_colours_and_unpainted_keys_dark() {
        let p = effects::positions(&US);
        let painted = [KeyColor {
            code: 0x04,
            color: GREEN,
        }];
        let zones = [
            Zone::new("base", ZoneKeys::All, ZoneEffect::Static { color: BLUE }),
            Zone::new("paint", keys(&[0x04, 0x05]), ZoneEffect::PerKey),
        ];
        let scene = Scene::new(&zones, &painted, &p);
        assert_eq!(scene.color(0x04, &at(0.0)), dev(GREEN));
        assert_eq!(scene.color(0x05, &at(0.0)), Rgb::default(), "in the zone, unpainted");
        assert_eq!(scene.color(0x06, &at(0.0)), dev(BLUE));
    }

    #[test]
    fn a_reactive_zone_lights_its_own_keys_over_what_is_below() {
        let p = effects::positions(&US);
        let zones = [
            Zone::new("base", ZoneKeys::All, ZoneEffect::Static { color: BLUE }),
            Zone::new(
                "wasd",
                ZonePreset::Wasd.keys(),
                ZoneEffect::Reactive {
                    mode: ReactiveMode::Fade,
                    color: RED,
                    duration_s: 1.0,
                },
            ),
        ];
        let scene = Scene::new(&zones, &[], &p);
        let w = [Hit { code: 0x1A, t: 0.0 }];
        let q = [Hit { code: 0x14, t: 0.0 }];
        let none = HashMap::new();
        let with = |hits| Inputs {
            t: 0.5,
            hits,
            bands: &[],
            screen: &none,
        };
        let half = scene.color(0x1A, &with(&w));
        assert_eq!((half.r, half.g, half.b), (128, 0, 128), "half faded over blue");
        assert_eq!(scene.color(0x14, &with(&q)), dev(BLUE), "Q is in no reactive zone");
        assert_eq!(scene.color(0x1A, &with(&[])), dev(BLUE), "no press: the base");
    }

    #[test]
    fn a_reflect_zone_shows_the_screen_and_what_is_below_until_a_frame_comes() {
        let p = effects::positions(&US);
        let zones = [
            Zone::new("base", ZoneKeys::All, ZoneEffect::Static { color: BLUE }),
            Zone::new("screen", ZoneKeys::All, ZoneEffect::Reflect),
        ];
        let scene = Scene::new(&zones, &[], &p);
        assert_eq!(scene.color(0x04, &at(0.0)), dev(BLUE), "no frame yet");
        let screen = HashMap::from([(0x04, dev(RED))]);
        let inputs = Inputs {
            t: 0.0,
            hits: &[],
            bands: &[],
            screen: &screen,
        };
        assert_eq!(scene.color(0x04, &inputs), dev(RED));
        assert_eq!(scene.color(0x05, &inputs), dev(BLUE), "a key without a colour");
    }

    #[test]
    fn the_stack_says_what_it_needs() {
        let still = [Zone::new("a", ZoneKeys::All, ZoneEffect::Static { color: RED })];
        assert!(!animated(&still) && !reactive(&still) && !listens(&still) && !reflects(&still));
        let press = ZoneEffect::Reactive {
            mode: ReactiveMode::Ripple,
            color: RED,
            duration_s: 2.0,
        };
        let busy = [
            Zone::new("a", ZoneKeys::All, ZoneEffect::Visualizer { sensitivity: 1.0 }),
            Zone::new("b", ZoneKeys::All, press),
            Zone::new("c", ZoneKeys::All, ZoneEffect::Reflect),
        ];
        assert!(animated(&busy) && reactive(&busy) && listens(&busy) && reflects(&busy));
        assert_eq!(press_duration(&busy), 2.0);
        assert_eq!(press_duration(&still), 0.0);
        let gradient = [Zone::new(
            "g",
            ZoneKeys::All,
            ZoneEffect::Gradient {
                from: RED,
                to: BLUE,
            },
        )];
        assert!(!animated(&gradient), "a gradient holds still");
    }
}
