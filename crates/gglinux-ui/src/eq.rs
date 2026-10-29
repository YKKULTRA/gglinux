// SPDX-License-Identifier: GPL-3.0-or-later
//! The parametric EQ picture: the curve and its handles.
//!
//! The response is computed exactly as PipeWire's filter-chain filters do it
//! (spa/plugins/audioconvert/biquad.c): RBJ cookbook biquads with
//! alpha = sin(w0) / 2Q for peaking bands and both shelves. What the editor
//! draws is what PipeWire applies.

use gg_ipc::{BandKind, EqBand};

/// PipeWire's graph rate here; at 44.1 kHz the curve differs only near 20 kHz.
const RATE: f64 = 48_000.0;
/// The picture, in its own units; the window scales it.
pub const W: f32 = 640.0;
pub const H: f32 = 220.0;
const PAD: f32 = 28.0;
pub const F_MIN: f32 = 20.0;
pub const F_MAX: f32 = 20_000.0;

struct Coeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a0: f64,
    a1: f64,
    a2: f64,
}

fn coeffs(b: &EqBand) -> Coeffs {
    let a = 10f64.powf(b.gain as f64 / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * b.freq as f64 / RATE;
    let k = w0.cos();
    let alpha = w0.sin() / (2.0 * b.q as f64);
    match b.kind {
        BandKind::Peaking => Coeffs {
            b0: 1.0 + alpha * a,
            b1: -2.0 * k,
            b2: 1.0 - alpha * a,
            a0: 1.0 + alpha / a,
            a1: -2.0 * k,
            a2: 1.0 - alpha / a,
        },
        BandKind::LowShelf => {
            let k2 = 2.0 * a.sqrt() * alpha;
            Coeffs {
                b0: a * (a + 1.0 - (a - 1.0) * k + k2),
                b1: 2.0 * a * (a - 1.0 - (a + 1.0) * k),
                b2: a * (a + 1.0 - (a - 1.0) * k - k2),
                a0: a + 1.0 + (a - 1.0) * k + k2,
                a1: -2.0 * (a - 1.0 + (a + 1.0) * k),
                a2: a + 1.0 + (a - 1.0) * k - k2,
            }
        }
        BandKind::HighShelf => {
            let k2 = 2.0 * a.sqrt() * alpha;
            Coeffs {
                b0: a * (a + 1.0 + (a - 1.0) * k + k2),
                b1: -2.0 * a * (a - 1.0 + (a + 1.0) * k),
                b2: a * (a + 1.0 + (a - 1.0) * k - k2),
                a0: a + 1.0 - (a - 1.0) * k + k2,
                a1: 2.0 * (a - 1.0 - (a + 1.0) * k),
                a2: a + 1.0 - (a - 1.0) * k - k2,
            }
        }
    }
}

/// |H(e^jw)| in dB at frequency f.
fn magnitude_db(c: &Coeffs, f: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * f / RATE;
    let (cw, c2w) = (w.cos(), (2.0 * w).cos());
    let num = c.b0 * c.b0
        + c.b1 * c.b1
        + c.b2 * c.b2
        + 2.0 * (c.b0 * c.b1 + c.b1 * c.b2) * cw
        + 2.0 * c.b0 * c.b2 * c2w;
    let den = c.a0 * c.a0
        + c.a1 * c.a1
        + c.a2 * c.a2
        + 2.0 * (c.a0 * c.a1 + c.a1 * c.a2) * cw
        + 2.0 * c.a0 * c.a2 * c2w;
    10.0 * (num / den).log10()
}

/// The chain's combined response at frequency f, in dB. A 0 dB band is flat.
pub fn response_db(bands: &[EqBand], f: f32) -> f32 {
    bands
        .iter()
        .filter(|b| b.gain != 0.0)
        .map(|b| magnitude_db(&coeffs(b), f as f64))
        .sum::<f64>() as f32
}

fn log_min() -> f32 {
    F_MIN.log10()
}

fn log_span() -> f32 {
    F_MAX.log10() - F_MIN.log10()
}

pub fn x(f: f32) -> f32 {
    PAD + (f.log10() - log_min()) / log_span() * (W - 2.0 * PAD)
}

pub fn freq_at(px: f32) -> f32 {
    10f32.powf(log_min() + (px - PAD) / (W - 2.0 * PAD) * log_span())
}

pub fn y(db: f32, limit: f32) -> f32 {
    H / 2.0 - db / limit * (H / 2.0 - PAD / 2.0)
}

pub fn db_at(py: f32, limit: f32) -> f32 {
    (H / 2.0 - py) / (H / 2.0 - PAD / 2.0) * limit
}

/// The curve as SVG path commands in the picture's units.
pub fn path(bands: &[EqBand], preamp: f32, limit: f32) -> String {
    (0..160)
        .map(|i| {
            let f = 10f32.powf(log_min() + i as f32 / 159.0 * log_span());
            let db = (response_db(bands, f) + preamp).clamp(-limit, limit);
            format!(
                "{}{:.1} {:.1}",
                if i == 0 { "M " } else { " L " },
                x(f),
                y(db, limit)
            )
        })
        .collect()
}

/// A band moved to where it was dragged: frequency to the nearest Hz, gain to
/// the half dB.
pub fn dragged(b: &mut EqBand, px: f32, py: f32, limit: f32) {
    b.freq = freq_at(px).clamp(F_MIN, F_MAX).round();
    b.gain = (db_at(py, limit).clamp(-limit, limit) * 2.0).round() / 2.0;
}

/// A band made wider or narrower by one wheel step.
pub fn wheeled(b: &mut EqBand, up: bool) {
    let q = b.q * if up { 1.1 } else { 1.0 / 1.1 };
    b.q = (q.clamp(Q_MIN, Q_MAX) * 100.0).round() / 100.0;
}

/// The range PipeWire's filters take, as the daemon checks it.
pub const Q_MIN: f32 = 0.1;
pub const Q_MAX: f32 = 10.0;

/// Which of a band's numbers a typed value is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Freq,
    Gain,
    Q,
}

/// A typed value set on a band, brought into range: frequency to the Hz, gain
/// to the tenth of a dB (AutoEQ's step), Q to the hundredth. `None` when the
/// text isn't a number.
pub fn typed(b: &mut EqBand, field: Field, text: &str, limit: f32) -> Option<()> {
    let v = number(text, field == Field::Freq)?;
    match field {
        Field::Freq => b.freq = v.clamp(F_MIN, F_MAX).round(),
        Field::Gain => b.gain = (v.clamp(-limit, limit) * 10.0).round() / 10.0,
        Field::Q => b.q = (v.clamp(Q_MIN, Q_MAX) * 100.0).round() / 100.0,
    }
    Some(())
}

/// A number as people type it: "250", "2.5k" (with `kilo`), "-3,5", "+2 dB",
/// "1000 Hz".
pub fn number(text: &str, kilo: bool) -> Option<f32> {
    let t = text.trim().to_ascii_lowercase().replace(',', ".");
    let t = t
        .strip_suffix("hz")
        .or_else(|| t.strip_suffix("db"))
        .unwrap_or(&t)
        .trim_end();
    let (t, scale) = match t.strip_suffix('k') {
        Some(rest) if kilo => (rest.trim_end(), 1000.0),
        _ => (t, 1.0),
    };
    let v: f32 = t.parse().ok()?;
    v.is_finite().then_some(v * scale)
}

fn signed(v: f32) -> String {
    if v > 0.0 {
        format!("+{v}")
    } else {
        format!("{v}")
    }
}

pub fn tip(b: &EqBand) -> String {
    format!("{} Hz · {} dB · Q {}", b.freq.round(), signed(b.gain), b.q)
}

/// What a band's number boxes show.
pub fn boxes(b: &EqBand) -> [String; 3] {
    [
        format!("{}", b.freq.round()),
        signed(b.gain),
        format!("{}", b.q),
    ]
}

/// "Band 1 · low shelf".
pub fn band_name(i: usize, kind: BandKind) -> String {
    let kind = match kind {
        BandKind::LowShelf => "low shelf",
        BandKind::Peaking => "peak",
        BandKind::HighShelf => "high shelf",
    };
    format!("Band {} · {kind}", i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(kind: BandKind, freq: f32, gain: f32) -> EqBand {
        EqBand {
            kind,
            freq,
            q: 1.0,
            gain,
        }
    }

    fn close(a: f32, b: f32, within: f32) -> bool {
        (a - b).abs() < within
    }

    #[test]
    fn a_peak_reaches_its_gain_at_its_frequency_and_fades_away() {
        let b = [band(BandKind::Peaking, 1000.0, 6.0)];
        assert!(close(response_db(&b, 1000.0), 6.0, 0.001));
        assert!(response_db(&b, 50.0).abs() < 0.2);
        assert!(response_db(&b, 18_000.0).abs() < 0.3);
    }

    #[test]
    fn a_peak_is_half_its_gain_a_q_wide() {
        // Q 1 puts the half-gain points at f0 · (√1.25 ∓ 0.5); bilinear warping
        // moves them a little at 48 kHz.
        let b = [band(BandKind::Peaking, 1000.0, 6.0)];
        assert!(close(response_db(&b, 618.03), 2.9943, 0.001));
        assert!(close(response_db(&b, 1618.03), 2.9851, 0.001));
    }

    #[test]
    fn a_shelf_is_half_its_gain_at_its_corner_and_whole_far_beyond() {
        let mut low = band(BandKind::LowShelf, 105.0, 7.0);
        low.q = 0.707;
        assert!(close(response_db(&[low], 105.0), 3.5, 0.001));
        assert!(close(response_db(&[low], 20.0), 6.9897, 0.001));
        assert!(response_db(&[low], 1000.0).abs() < 0.002);
        let mut high = band(BandKind::HighShelf, 10_000.0, -2.0);
        high.q = 0.707;
        assert!(close(response_db(&[high], 10_000.0), -1.0, 0.001));
        assert!(close(response_db(&[high], 20_000.0), -1.9964, 0.001));
        assert!(response_db(&[high], 100.0).abs() < 0.001);
    }

    #[test]
    fn a_chain_matches_the_biquads_evaluated_on_the_unit_circle() {
        // AutoEQ's own sample, against H(e^jw) worked out with complex numbers
        // (the same RBJ coefficients, evaluated independently of the closed form).
        let chain = [
            EqBand {
                kind: BandKind::LowShelf,
                freq: 105.0,
                q: 0.70,
                gain: 7.0,
            },
            EqBand {
                kind: BandKind::Peaking,
                freq: 180.0,
                q: 0.53,
                gain: -3.4,
            },
            EqBand {
                kind: BandKind::Peaking,
                freq: 1200.0,
                q: 1.40,
                gain: 2.1,
            },
            EqBand {
                kind: BandKind::HighShelf,
                freq: 10_000.0,
                q: 0.70,
                gain: -2.0,
            },
        ];
        for (f, db) in [
            (20.0, 6.8349),
            (100.0, 1.4628),
            (180.0, -2.5601),
            (1000.0, 1.2812),
            (1200.0, 1.8337),
            (5000.0, -0.0333),
            (10_000.0, -0.9914),
            (19_000.0, -1.9875),
        ] {
            let got = response_db(&chain, f);
            assert!(close(got, db, 0.001), "{f} Hz: {got} dB, expected {db}");
        }
    }

    #[test]
    fn flat_bands_draw_a_straight_line_at_zero() {
        let flat = [band(BandKind::Peaking, 1000.0, 0.0)];
        let p = path(&flat, 0.0, 12.0);
        assert!(p.starts_with("M 28.0 110.0"), "{p}");
        assert_eq!(p.matches(" L ").count(), 159);
        assert!(p.split(" L ").all(|pt| pt.ends_with(" 110.0")));
    }

    #[test]
    fn the_preamp_lifts_the_curve_and_the_plot_clips_it() {
        let flat = [band(BandKind::Peaking, 1000.0, 0.0)];
        // +10 of a ±20 plot is half way up the drawable height.
        assert!(path(&flat, 10.0, 20.0).starts_with(&format!("M 28.0 {:.1}", y(10.0, 20.0))));
        let p = path(&[band(BandKind::Peaking, 1000.0, 12.0)], 6.0, 12.0);
        assert!(p
            .split(" L ")
            .all(|pt| pt.rsplit(' ').next().unwrap().parse::<f32>().unwrap() >= y(12.0, 12.0)));
    }

    #[test]
    fn the_axes_are_logarithmic_in_frequency_and_linear_in_gain() {
        assert!(close(x(F_MIN), 28.0, 0.001));
        assert!(close(x(F_MAX), W - 28.0, 0.01));
        // 20 Hz to 20 kHz is three decades: each takes a third.
        assert!(close(x(200.0) - x(20.0), (W - 56.0) / 3.0, 0.01));
        assert!(close(freq_at(x(1234.0)), 1234.0, 0.1));
        assert_eq!(y(0.0, 12.0), H / 2.0);
        assert!(close(db_at(y(-7.5, 20.0), 20.0), -7.5, 0.001));
    }

    #[test]
    fn a_drag_lands_where_the_pointer_is() {
        let mut b = band(BandKind::Peaking, 1000.0, 0.0);
        dragged(&mut b, x(250.0), y(3.0, 12.0), 12.0);
        assert_eq!((b.freq, b.gain), (250.0, 3.0));
        dragged(&mut b, x(250.0), y(3.2, 12.0), 12.0);
        assert_eq!(b.gain, 3.0, "to the half dB");
        dragged(&mut b, -100.0, -100.0, 12.0);
        assert_eq!((b.freq, b.gain), (F_MIN, 12.0), "clamped to the picture");
    }

    #[test]
    fn the_wheel_steps_q_within_its_range() {
        let mut b = band(BandKind::Peaking, 1000.0, 0.0);
        wheeled(&mut b, true);
        assert_eq!(b.q, 1.1);
        b.q = 9.95;
        wheeled(&mut b, true);
        assert_eq!(b.q, 10.0);
        b.q = 0.1;
        wheeled(&mut b, false);
        assert_eq!(b.q, 0.1);
    }

    #[test]
    fn numbers_read_as_people_type_them() {
        assert_eq!(number("250", true), Some(250.0));
        assert_eq!(number(" 2.5k ", true), Some(2500.0));
        assert_eq!(number("2,5 kHz", true), Some(2500.0));
        assert_eq!(number("1000 Hz", true), Some(1000.0));
        assert_eq!(number("-3,5", false), Some(-3.5));
        assert_eq!(number("+2 dB", false), Some(2.0));
        assert_eq!(number("2k", false), None, "only frequencies take k");
        for bad in ["", "loud", "inf", "NaN", "-"] {
            assert_eq!(number(bad, true), None, "{bad:?}");
        }
    }

    #[test]
    fn typed_values_are_brought_into_range_and_rounded() {
        let mut b = band(BandKind::Peaking, 1000.0, 0.0);
        typed(&mut b, Field::Freq, "40k", 12.0).unwrap();
        assert_eq!(b.freq, F_MAX);
        typed(&mut b, Field::Freq, "99.6", 12.0).unwrap();
        assert_eq!(b.freq, 100.0);
        typed(&mut b, Field::Gain, "-30", 12.0).unwrap();
        assert_eq!(b.gain, -12.0);
        typed(&mut b, Field::Gain, "3.14", 20.0).unwrap();
        assert_eq!(b.gain, 3.1);
        typed(&mut b, Field::Q, "0", 12.0).unwrap();
        assert_eq!(b.q, Q_MIN);
        typed(&mut b, Field::Q, "1.234", 12.0).unwrap();
        assert_eq!(b.q, 1.23);
        let before = b;
        assert_eq!(typed(&mut b, Field::Q, "wide", 12.0), None);
        assert_eq!(b, before, "untouched by a bad entry");
    }

    #[test]
    fn a_band_reads_back_as_typed() {
        let b = EqBand {
            kind: BandKind::Peaking,
            freq: 1351.0,
            q: 0.7,
            gain: -4.5,
        };
        assert_eq!(boxes(&b), ["1351", "-4.5", "0.7"]);
        assert_eq!(tip(&EqBand { gain: 4.5, ..b }), "1351 Hz · +4.5 dB · Q 0.7");
        let mut back = b;
        for (field, text) in [Field::Freq, Field::Gain, Field::Q]
            .into_iter()
            .zip(boxes(&b))
        {
            typed(&mut back, field, &text, 12.0).unwrap();
        }
        assert_eq!(back, b);
        assert_eq!(band_name(0, BandKind::LowShelf), "Band 1 · low shelf");
    }
}
