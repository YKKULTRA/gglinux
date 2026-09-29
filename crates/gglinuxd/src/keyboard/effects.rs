//! Key colours for each lighting effect at a moment in time.
//!
//! Pure, so every effect is tested without the keyboard; the worker asks for
//! one frame at a time and sends it.

use std::collections::HashMap;
use std::f32::consts::TAU;

use gg_device_keyboard::layout::{ANSI_TKL, WIDTH};
use gg_device_keyboard::Rgb;
use gg_ipc::{Effect, Lighting, Reactive, ReactiveMode};

/// How fast a ripple's ring spreads, in key units per second.
pub const RIPPLE_SPEED: f32 = 20.0;
/// Half the ring's width, in key units.
const RING: f32 = 1.0;

/// A key's centre, in key units from the top-left of the board.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
}

/// A key press, `t` seconds into the effect. No `Debug`: which keys someone
/// presses is never printed.
#[derive(Clone, Copy)]
pub struct Hit {
    pub code: u8,
    pub t: f64,
}

/// The centre of every key on the board.
pub fn positions() -> HashMap<u8, Pos> {
    ANSI_TKL.iter().map(|&(code, _, x, y, w)| (code, Pos { x: x + w / 2.0, y: y + 0.5 })).collect()
}

/// The colour of the LED with HID code `code`, `t` seconds into the effect,
/// with recent key presses drawn over it.
///
/// `t` is `f64` because the daemon runs for weeks: as `f32`, seconds since
/// start lose frame precision after about six days.
pub fn color(lighting: &Lighting, positions: &HashMap<u8, Pos>, code: u8, t: f64, hits: &[Hit]) -> Rgb {
    let base = match lighting.effect {
        Effect::Static => lighting
            .keys
            .iter()
            .find(|k| k.code == code)
            .map(|k| rgb(k.color))
            .unwrap_or_default(),
        Effect::Breathing { color, period_s } => {
            scale(rgb(color), (1.0 - (TAU * cycle(t, period_s)).cos()) / 2.0)
        }
        Effect::Wave { period_s } => {
            let x = positions.get(&code).map_or(0.0, |p| p.x / WIDTH);
            hsv(x - cycle(t, period_s))
        }
        Effect::Spectrum { period_s } => hsv(cycle(t, period_s)),
    };
    let r = &lighting.reactive;
    let w = if r.enabled { reactive_weight(r, positions, code, t, hits) } else { 0.0 };
    scale(mix(base, rgb(r.color), w), f32::from(lighting.brightness) / 100.0)
}

/// How strongly key `code` shows the reactive colour at `t`: the strongest
/// live press, 0 (none) to 1 (full).
pub fn reactive_weight(r: &Reactive, positions: &HashMap<u8, Pos>, code: u8, t: f64, hits: &[Hit]) -> f32 {
    let duration = r.duration_s;
    if !(duration.is_finite() && duration > 0.0) {
        return 0.0; // a hand-edited file; never divide by it
    }
    let here = positions.get(&code);
    hits.iter()
        .map(|h| {
            let age = (t - h.t) as f32; // small, so exact enough once subtracted in f64
            if !(0.0..duration).contains(&age) {
                return 0.0;
            }
            let fade = 1.0 - age / duration;
            match r.mode {
                ReactiveMode::Fade => if h.code == code { fade } else { 0.0 },
                ReactiveMode::Ripple => {
                    let (Some(a), Some(b)) = (here, positions.get(&h.code)) else { return 0.0 };
                    let d = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
                    fade * (1.0 - (d - RIPPLE_SPEED * age).abs() / RING).max(0.0)
                }
            }
        })
        .fold(0.0, f32::max)
}

/// Whether the effect needs a stream of frames, or one holds.
pub fn animated(effect: &Effect) -> bool {
    !matches!(effect, Effect::Static)
}

/// An overlay's colour at the lighting's brightness.
pub fn dim(c: Rgb, brightness: u8) -> Rgb {
    scale(c, f32::from(brightness) / 100.0)
}

/// A fully saturated, full-value colour; `hue` wraps at 1.
pub fn hsv(hue: f32) -> Rgb {
    let h = hue.rem_euclid(1.0) * 6.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    let byte = |v: f32| (v * 255.0).round() as u8;
    Rgb { r: byte(r), g: byte(g), b: byte(b) }
}

fn rgb(c: gg_ipc::Rgb) -> Rgb {
    Rgb { r: c.r, g: c.g, b: c.b }
}

/// How far through its current period an effect is at `t`, 0..1.
fn cycle(t: f64, period_s: f32) -> f32 {
    (t / f64::from(period_s)).fract() as f32
}

/// `a` moved toward `b` by `w` (0..=1).
fn mix(a: Rgb, b: Rgb, w: f32) -> Rgb {
    let w = w.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (f32::from(x) * (1.0 - w) + f32::from(y) * w).round() as u8;
    Rgb { r: f(a.r, b.r), g: f(a.g, b.g), b: f(a.b, b.b) }
}

fn scale(c: Rgb, k: f32) -> Rgb {
    let k = k.clamp(0.0, 1.0);
    let f = |v: u8| (f32::from(v) * k).round() as u8;
    Rgb { r: f(c.r), g: f(c.g), b: f(c.b) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::{KeyColor, Reactive, ReactiveMode};

    const RED: gg_ipc::Rgb = gg_ipc::Rgb { r: 255, g: 0, b: 0 };

    fn lit(effect: Effect) -> Lighting {
        Lighting { enabled: true, effect, keys: Vec::new(), brightness: 100, reactive: gg_ipc::Reactive::default() }
    }

    #[test]
    fn hsv_hits_the_primaries() {
        assert_eq!(hsv(0.0), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(hsv(1.0 / 3.0), Rgb { r: 0, g: 255, b: 0 });
        assert_eq!(hsv(2.0 / 3.0), Rgb { r: 0, g: 0, b: 255 });
        assert_eq!(hsv(1.0), hsv(0.0), "the hue wraps");
    }

    #[test]
    fn static_colours_come_from_the_key_list_and_unlisted_keys_are_dark() {
        let l = Lighting { keys: vec![KeyColor { code: 0x0A, color: RED }], ..lit(Effect::Static) };
        let p = positions();
        assert_eq!(color(&l, &p, 0x0A, 5.0, &[]), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(color(&l, &p, 0x04, 5.0, &[]), Rgb::default());
        assert_eq!(color(&l, &p, 0x59, 5.0, &[]), Rgb::default(), "numpad codes the TKL lacks stay dark");
    }

    #[test]
    fn breathing_goes_from_dark_to_full_each_period() {
        let l = lit(Effect::Breathing { color: RED, period_s: 2.0 });
        let p = positions();
        assert_eq!(color(&l, &p, 0x04, 0.0, &[]), Rgb::default());
        assert_eq!(color(&l, &p, 0x04, 1.0, &[]), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(color(&l, &p, 0x04, 2.0, &[]), Rgb::default());
    }

    #[test]
    fn the_wave_moves_across_the_keyboard() {
        let l = lit(Effect::Wave { period_s: 4.0 });
        let p = positions();
        let esc = color(&l, &p, 0x29, 0.0, &[]);
        assert_ne!(esc, color(&l, &p, 0x48, 0.0, &[]), "Esc and Pause differ");
        assert_ne!(color(&l, &p, 0x29, 1.0, &[]), esc, "and it moves");
    }

    #[test]
    fn spectrum_is_one_colour_for_every_key() {
        let l = lit(Effect::Spectrum { period_s: 6.0 });
        let p = positions();
        assert_eq!(color(&l, &p, 0x29, 1.5, &[]), color(&l, &p, 0x48, 1.5, &[]));
    }

    #[test]
    fn brightness_scales_every_effect() {
        let p = positions();
        let off = Lighting { brightness: 0, ..lit(Effect::Spectrum { period_s: 6.0 }) };
        assert_eq!(color(&off, &p, 0x29, 1.0, &[]), Rgb::default());
        let half = Lighting { brightness: 50, keys: vec![KeyColor { code: 0x0A, color: RED }], ..lit(Effect::Static) };
        assert_eq!(color(&half, &p, 0x0A, 0.0, &[]).r, 128);
    }

    const BLUE: gg_ipc::Rgb = gg_ipc::Rgb { r: 0, g: 0, b: 255 };

    fn reactive(mode: ReactiveMode, duration_s: f32) -> Lighting {
        let r = Reactive { enabled: true, mode, color: gg_ipc::Rgb { r: 255, g: 255, b: 255 }, duration_s };
        Lighting { reactive: r, ..lit(Effect::Static) }
    }

    fn dist(p: &HashMap<u8, Pos>, a: u8, b: u8) -> f32 {
        ((p[&a].x - p[&b].x).powi(2) + (p[&a].y - p[&b].y).powi(2)).sqrt()
    }

    #[test]
    fn fade_lights_the_pressed_key_and_fades_back() {
        let l = reactive(ReactiveMode::Fade, 0.6);
        let p = positions();
        let hits = [Hit { code: 0x04, t: 1.0 }];
        assert_eq!(color(&l, &p, 0x04, 1.0, &hits), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(color(&l, &p, 0x04, 1.3, &hits), Rgb { r: 128, g: 128, b: 128 });
        assert_eq!(color(&l, &p, 0x04, 1.6, &hits), Rgb::default(), "gone after the duration");
        assert_eq!(color(&l, &p, 0x05, 1.0, &hits), Rgb::default(), "other keys untouched");
    }

    #[test]
    fn ripple_starts_on_the_pressed_key_and_reaches_far_keys_later() {
        let l = reactive(ReactiveMode::Ripple, 1.0);
        let p = positions();
        let hits = [Hit { code: 0x29, t: 0.0 }]; // Esc
        let w = |code, t| reactive_weight(&l.reactive, &p, code, t, &hits);
        assert_eq!(w(0x29, 0.0), 1.0);
        assert_eq!(w(0x3A, 0.0), 0.0, "F1 is dark at the moment of the press");
        let far = dist(&p, 0x29, 0x48) / RIPPLE_SPEED; // Pause
        assert!(far > 0.5, "Pause is far from Esc");
        assert_eq!(w(0x48, 0.1), 0.0, "not reached yet");
        assert!(w(0x48, f64::from(far)) > 0.1, "reached later");
        assert_eq!(w(0x48, 1.0), 0.0, "gone after the duration");
    }

    #[test]
    fn the_strongest_press_wins_and_nothing_overflows() {
        let l = reactive(ReactiveMode::Fade, 0.6);
        let p = positions();
        let hits = [Hit { code: 0x04, t: 0.0 }, Hit { code: 0x04, t: 0.3 }];
        assert_eq!(reactive_weight(&l.reactive, &p, 0x04, 0.3, &hits), 1.0);
        assert_eq!(color(&l, &p, 0x04, 0.3, &hits), Rgb { r: 255, g: 255, b: 255 });
    }

    #[test]
    fn presses_blend_over_the_base_then_brightness_applies() {
        let mut l = reactive(ReactiveMode::Fade, 1.0);
        l.keys = vec![KeyColor { code: 0x04, color: RED }];
        l.reactive.color = BLUE;
        let p = positions();
        let hits = [Hit { code: 0x04, t: 0.0 }];
        assert_eq!(color(&l, &p, 0x04, 0.5, &hits), Rgb { r: 128, g: 0, b: 128 });
        l.brightness = 50;
        assert_eq!(color(&l, &p, 0x04, 0.5, &hits), Rgb { r: 64, g: 0, b: 64 });
    }

    #[test]
    fn reactive_off_ignores_presses_and_unknown_keys_are_harmless() {
        let mut l = reactive(ReactiveMode::Ripple, 1.0);
        let p = positions();
        let unknown = [Hit { code: 0x7F, t: 0.0 }]; // not on the TKL
        assert_eq!(color(&l, &p, 0x29, 0.0, &unknown), Rgb::default());
        l.reactive.enabled = false;
        assert_eq!(color(&l, &p, 0x29, 0.0, &[Hit { code: 0x29, t: 0.0 }]), Rgb::default());
    }

    #[test]
    fn a_bad_duration_lights_nothing() {
        let p = positions();
        let hits = [Hit { code: 0x04, t: 0.0 }];
        for d in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            for mode in [ReactiveMode::Fade, ReactiveMode::Ripple] {
                let l = reactive(mode, d);
                assert_eq!(reactive_weight(&l.reactive, &p, 0x04, 0.0, &hits), 0.0, "{mode:?} {d}");
                assert_eq!(color(&l, &p, 0x04, 0.0, &hits), Rgb::default());
            }
        }
    }

    #[test]
    fn effects_stay_smooth_after_weeks_of_uptime() {
        let month = 30.0 * 24.0 * 3600.0; // seconds since the daemon started
        let p = positions();
        let l = reactive(ReactiveMode::Fade, 0.6);
        let hits = [Hit { code: 0x04, t: month }];
        let w = reactive_weight(&l.reactive, &p, 0x04, month + 0.3, &hits);
        assert!((w - 0.5).abs() < 0.01, "a month in, a fade is still smooth: {w}");
        let wave = lit(Effect::Wave { period_s: 4.0 });
        assert_ne!(color(&wave, &p, 0x29, month, &[]), color(&wave, &p, 0x29, month + 1.0 / 30.0, &[]),
            "and the wave still moves from one frame to the next");
    }

    #[test]
    fn dim_applies_the_brightness_to_overlay_colours() {
        assert_eq!(dim(Rgb { r: 200, g: 100, b: 0 }, 50), Rgb { r: 100, g: 50, b: 0 });
        assert_eq!(dim(Rgb { r: 200, g: 100, b: 0 }, 100), Rgb { r: 200, g: 100, b: 0 });
    }
}
