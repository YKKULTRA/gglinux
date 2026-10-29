// SPDX-License-Identifier: GPL-3.0-or-later
//! What one handler paints for an event's current value, at a moment in time.
//! Pure: the registry supplies the value, its range and its age.

use serde_json::{Map, Value};

use crate::proto::{parse_rgb, ColorSpec, Handler, Mode, Ranged, Rate, Rgb, Target};

const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

/// An event's value as it stands now.
pub struct Reading<'a> {
    pub value: i64,
    pub min: i64,
    pub max: i64,
    pub frame: Option<&'a Map<String, Value>>,
    /// Seconds since the value arrived, for flashing.
    pub age: f64,
}

impl Reading<'_> {
    /// The value as 0..=100 of its range.
    pub fn percent(&self) -> f32 {
        if self.max <= self.min {
            return if self.value >= self.max { 100.0 } else { 0.0 };
        }
        let p = (self.value - self.min) as f64 / (self.max - self.min) as f64 * 100.0;
        p.clamp(0.0, 100.0) as f32
    }
}

/// The colour a spec gives this reading; `None` when a range list has no match.
pub fn color_of(spec: &ColorSpec, r: &Reading) -> Option<Rgb> {
    match spec {
        ColorSpec::Static(c) => Some(*c),
        ColorSpec::Gradient { zero, hundred } => Some(lerp(*zero, *hundred, r.percent() / 100.0)),
        ColorSpec::Ranges(ranges) => ranges
            .iter()
            .find(|x| (x.low..=x.high).contains(&r.value))
            .and_then(|x| color_of(&x.color, r)),
    }
}

/// Whether a handler shows its colour now, or is in the dark half of a flash.
pub fn lit(rate: Option<&Rate>, r: &Reading) -> bool {
    let Some(rate) = rate else { return true };
    let f = pick(&rate.frequency, r.value).unwrap_or(0.0);
    if f <= 0.0 {
        return true;
    }
    let period = 1.0 / f64::from(f);
    let flashes = (r.age / period).floor();
    let limit = rate
        .repeat_limit
        .as_ref()
        .and_then(|l| pick(l, r.value))
        .unwrap_or(0);
    if limit > 0 && flashes >= f64::from(limit) {
        return true; // done flashing; it stays on
    }
    r.age - flashes * period < period / 2.0
}

/// The keys one handler paints for this reading, with their colours.
pub fn paint(h: &Handler, r: &Reading) -> Vec<(u8, Rgb)> {
    let Target::Keys(keys) = &h.target else {
        return Vec::new();
    };
    let color = match h.mode {
        Mode::ContextColor => h
            .context_frame_key
            .as_ref()
            .and_then(|k| r.frame?.get(k))
            .and_then(|v| parse_rgb(v).ok()),
        _ => h.color.as_ref().and_then(|c| color_of(c, r)),
    };
    let Some(color) = color else {
        return Vec::new();
    };
    let color = if lit(h.rate.as_ref(), r) {
        color
    } else {
        BLACK
    };
    match h.mode {
        Mode::Color | Mode::ContextColor => keys.iter().map(|&k| (k, color)).collect(),
        Mode::Percent => {
            let filled = r.percent() / 100.0 * keys.len() as f32;
            keys.iter()
                .enumerate()
                .map(|(i, &k)| (k, scale(color, (filled - i as f32).clamp(0.0, 1.0))))
                .collect()
        }
        Mode::Count => {
            let n = r.value.clamp(0, keys.len() as i64) as usize;
            keys.iter()
                .enumerate()
                .map(|(i, &k)| (k, if i < n { color } else { BLACK }))
                .collect()
        }
    }
}

fn pick<T: Copy>(r: &Ranged<T>, value: i64) -> Option<T> {
    match r {
        Ranged::Fixed(x) => Some(*x),
        Ranged::Ranges(list) => list
            .iter()
            .find(|(lo, hi, _)| (*lo..=*hi).contains(&value))
            .map(|&(_, _, x)| x),
    }
}

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let f = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Rgb {
        r: f(a.r, b.r),
        g: f(a.g, b.g),
        b: f(a.b, b.b),
    }
}

fn scale(c: Rgb, k: f32) -> Rgb {
    let f = |v: u8| (f32::from(v) * k).round() as u8;
    Rgb {
        r: f(c.r),
        g: f(c.g),
        b: f(c.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::ColorRange;
    use serde_json::json;

    const RED: Rgb = Rgb { r: 255, g: 0, b: 0 };
    const GREEN: Rgb = Rgb { r: 0, g: 255, b: 0 };
    const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

    fn at(value: i64) -> Reading<'static> {
        Reading {
            value,
            min: 0,
            max: 100,
            frame: None,
            age: 0.0,
        }
    }

    fn handler(mode: Mode, keys: &[u8], color: ColorSpec) -> Handler {
        Handler {
            target: Target::Keys(keys.to_vec()),
            mode,
            color: Some(color),
            rate: None,
            context_frame_key: None,
        }
    }

    #[test]
    fn percent_scales_between_min_and_max_and_clamps() {
        assert_eq!(at(50).percent(), 50.0);
        assert_eq!(
            Reading {
                value: 15,
                min: 10,
                max: 20,
                ..at(0)
            }
            .percent(),
            50.0
        );
        assert_eq!(at(-5).percent(), 0.0);
        assert_eq!(at(250).percent(), 100.0);
        assert_eq!(
            Reading {
                value: 7,
                min: 7,
                max: 7,
                ..at(0)
            }
            .percent(),
            100.0,
            "an empty range is full at its value"
        );
    }

    #[test]
    fn gradients_blend_by_percent_and_ranges_pick_by_value() {
        let g = ColorSpec::Gradient {
            zero: RED,
            hundred: GREEN,
        };
        assert_eq!(color_of(&g, &at(0)), Some(RED));
        assert_eq!(color_of(&g, &at(100)), Some(GREEN));
        assert_eq!(
            color_of(&g, &at(50)),
            Some(Rgb {
                r: 128,
                g: 128,
                b: 0
            })
        );
        let ranges = ColorSpec::Ranges(vec![
            ColorRange {
                low: 0,
                high: 10,
                color: ColorSpec::Static(RED),
            },
            ColorRange {
                low: 11,
                high: 100,
                color: ColorSpec::Static(GREEN),
            },
        ]);
        assert_eq!(
            color_of(&ranges, &at(10)),
            Some(RED),
            "bounds are inclusive"
        );
        assert_eq!(color_of(&ranges, &at(11)), Some(GREEN));
        assert_eq!(
            color_of(&ranges, &at(101)),
            None,
            "outside every range paints nothing"
        );
    }

    #[test]
    fn color_mode_paints_the_whole_zone() {
        let h = handler(Mode::Color, &[1, 2, 3], ColorSpec::Static(RED));
        assert_eq!(paint(&h, &at(5)), vec![(1, RED), (2, RED), (3, RED)]);
    }

    #[test]
    fn percent_mode_fills_in_order_dims_the_last_key_and_darkens_the_rest() {
        let h = handler(Mode::Percent, &[1, 2, 3, 4, 5], ColorSpec::Static(RED));
        assert_eq!(
            paint(&h, &at(50)),
            vec![
                (1, RED),
                (2, RED),
                (3, Rgb { r: 128, g: 0, b: 0 }),
                (4, BLACK),
                (5, BLACK),
            ]
        );
        assert!(paint(&h, &at(0)).iter().all(|&(_, c)| c == BLACK));
    }

    #[test]
    fn count_mode_lights_value_keys() {
        let h = handler(Mode::Count, &[1, 2, 3, 4], ColorSpec::Static(GREEN));
        assert_eq!(
            paint(&h, &at(2)),
            vec![(1, GREEN), (2, GREEN), (3, BLACK), (4, BLACK)]
        );
        assert_eq!(
            paint(&h, &at(9))
                .iter()
                .filter(|&&(_, c)| c == GREEN)
                .count(),
            4,
            "clamped to the zone"
        );
    }

    #[test]
    fn context_color_comes_from_the_frame() {
        let h = Handler {
            target: Target::Keys(vec![7]),
            mode: Mode::ContextColor,
            color: None,
            rate: None,
            context_frame_key: Some("tint".into()),
        };
        let frame = json!({ "tint": { "red": 1, "green": 2, "blue": 3 } });
        let frame = frame.as_object().unwrap();
        assert_eq!(
            paint(
                &h,
                &Reading {
                    frame: Some(frame),
                    ..at(0)
                }
            ),
            vec![(7, Rgb { r: 1, g: 2, b: 3 })]
        );
        assert!(paint(&h, &at(0)).is_empty(), "no frame, nothing painted");
    }

    #[test]
    fn flashing_is_on_for_the_first_half_of_each_period() {
        let rate = Rate {
            frequency: Ranged::Fixed(2.0),
            repeat_limit: None,
        };
        let r = |age| Reading { age, ..at(50) };
        assert!(lit(Some(&rate), &r(0.1)));
        assert!(!lit(Some(&rate), &r(0.3)));
        assert!(lit(Some(&rate), &r(0.55)));
        assert!(lit(None, &r(0.3)), "no rate: steady");
        let off = Rate {
            frequency: Ranged::Fixed(0.0),
            repeat_limit: None,
        };
        assert!(lit(Some(&off), &r(0.3)), "frequency 0: steady");
    }

    #[test]
    fn repeat_limits_stop_the_flashing_on_and_ranges_pick_the_frequency() {
        let limited = Rate {
            frequency: Ranged::Fixed(2.0),
            repeat_limit: Some(Ranged::Fixed(2)),
        };
        assert!(
            !lit(Some(&limited), &Reading { age: 0.8, ..at(50) }),
            "second flash, off half"
        );
        assert!(
            lit(Some(&limited), &Reading { age: 1.3, ..at(50) }),
            "after two flashes it stays on"
        );
        let unlimited = Rate {
            frequency: Ranged::Fixed(2.0),
            repeat_limit: Some(Ranged::Fixed(0)),
        };
        assert!(
            !lit(Some(&unlimited), &Reading { age: 5.3, ..at(50) }),
            "0 means no limit"
        );
        let low_only = Rate {
            frequency: Ranged::Ranges(vec![(0, 20, 4.0)]),
            repeat_limit: None,
        };
        assert!(
            !lit(Some(&low_only), &Reading { age: 0.2, ..at(10) }),
            "10 is in 0..=20: flashing"
        );
        assert!(
            lit(Some(&low_only), &Reading { age: 0.2, ..at(50) }),
            "50 matches no range: steady"
        );
    }

    #[test]
    fn a_flashing_handler_paints_black_in_its_off_half_and_unsupported_paints_nothing() {
        let mut h = handler(Mode::Color, &[1], ColorSpec::Static(RED));
        h.rate = Some(Rate {
            frequency: Ranged::Fixed(2.0),
            repeat_limit: None,
        });
        assert_eq!(paint(&h, &Reading { age: 0.3, ..at(1) }), vec![(1, BLACK)]);
        h.target = Target::Unsupported;
        assert!(paint(&h, &at(1)).is_empty());
    }
}
