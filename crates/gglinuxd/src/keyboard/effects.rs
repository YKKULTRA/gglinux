//! Key colours for each lighting effect at a moment in time.
//!
//! Pure, so every effect is tested without the keyboard; the worker asks for
//! one frame at a time and sends it.

use std::collections::HashMap;
use std::f32::consts::TAU;

use gg_device_keyboard::layout::{ANSI_TKL, WIDTH};
use gg_device_keyboard::Rgb;
use gg_ipc::{Effect, Lighting};

/// Horizontal position of each key's centre, 0 (left edge) to 1 (right edge).
pub fn positions() -> HashMap<u8, f32> {
    ANSI_TKL.iter().map(|&(code, _, x, _, w)| (code, (x + w / 2.0) / WIDTH)).collect()
}

/// The colour of the LED with HID code `code`, `t` seconds into the effect.
pub fn color(lighting: &Lighting, positions: &HashMap<u8, f32>, code: u8, t: f32) -> Rgb {
    let raw = match lighting.effect {
        Effect::Static => lighting
            .keys
            .iter()
            .find(|k| k.code == code)
            .map(|k| rgb(k.color))
            .unwrap_or_default(),
        Effect::Breathing { color, period_s } => scale(rgb(color), (1.0 - (TAU * t / period_s).cos()) / 2.0),
        Effect::Wave { period_s } => {
            let x = positions.get(&code).copied().unwrap_or(0.0);
            hsv(x - t / period_s)
        }
        Effect::Spectrum { period_s } => hsv(t / period_s),
    };
    scale(raw, f32::from(lighting.brightness) / 100.0)
}

/// Whether the effect needs a stream of frames, or one holds.
pub fn animated(effect: &Effect) -> bool {
    !matches!(effect, Effect::Static)
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

fn scale(c: Rgb, k: f32) -> Rgb {
    let k = k.clamp(0.0, 1.0);
    let f = |v: u8| (f32::from(v) * k).round() as u8;
    Rgb { r: f(c.r), g: f(c.g), b: f(c.b) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::KeyColor;

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
        assert_eq!(color(&l, &p, 0x0A, 5.0), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(color(&l, &p, 0x04, 5.0), Rgb::default());
        assert_eq!(color(&l, &p, 0x59, 5.0), Rgb::default(), "numpad codes the TKL lacks stay dark");
    }

    #[test]
    fn breathing_goes_from_dark_to_full_each_period() {
        let l = lit(Effect::Breathing { color: RED, period_s: 2.0 });
        let p = positions();
        assert_eq!(color(&l, &p, 0x04, 0.0), Rgb::default());
        assert_eq!(color(&l, &p, 0x04, 1.0), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(color(&l, &p, 0x04, 2.0), Rgb::default());
    }

    #[test]
    fn the_wave_moves_across_the_keyboard() {
        let l = lit(Effect::Wave { period_s: 4.0 });
        let p = positions();
        let esc = color(&l, &p, 0x29, 0.0);
        assert_ne!(esc, color(&l, &p, 0x48, 0.0), "Esc and Pause differ");
        assert_ne!(color(&l, &p, 0x29, 1.0), esc, "and it moves");
    }

    #[test]
    fn spectrum_is_one_colour_for_every_key() {
        let l = lit(Effect::Spectrum { period_s: 6.0 });
        let p = positions();
        assert_eq!(color(&l, &p, 0x29, 1.5), color(&l, &p, 0x48, 1.5));
    }

    #[test]
    fn brightness_scales_every_effect() {
        let p = positions();
        let off = Lighting { brightness: 0, ..lit(Effect::Spectrum { period_s: 6.0 }) };
        assert_eq!(color(&off, &p, 0x29, 1.0), Rgb::default());
        let half = Lighting { brightness: 50, keys: vec![KeyColor { code: 0x0A, color: RED }], ..lit(Effect::Static) };
        assert_eq!(color(&half, &p, 0x0A, 0.0).r, 128);
    }
}
