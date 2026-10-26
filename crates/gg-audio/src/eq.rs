// SPDX-License-Identifier: GPL-3.0-or-later
//! Parametric EQ: band layouts, defaults, validation, AutoEQ import, and the
//! filter-chain parameters that set bands live.
//!
//! A filter's type is part of the graph PipeWire builds and cannot change at
//! runtime, so every channel has the same fixed chain — a preamp, the headphone
//! correction (low shelf, ten peaking, high shelf), then the channel EQ (low
//! shelf, eight peaking, high shelf) — and only frequencies, Qs and gains move.

use anyhow::{bail, ensure, Context, Result};
use gg_ipc::{BandKind, EqBand, HeadphoneEq};

/// Channel EQ reaches ±12 dB, like Sonar; headphone correction needs more room.
pub const CHANNEL_GAIN_LIMIT: f32 = 12.0;
pub const HEADPHONE_GAIN_LIMIT: f32 = 20.0;

const CENTRES: [f32; 10] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// A 0.707 (Butterworth) shelf slope.
const SHELF_Q: f32 = 0.707;

/// Band kinds of a channel EQ, in chain order.
pub fn channel_layout() -> Vec<BandKind> {
    layout(10)
}

/// Band kinds of the headphone correction, in chain order.
pub fn headphone_layout() -> Vec<BandKind> {
    layout(12)
}

fn layout(n: usize) -> Vec<BandKind> {
    let mut kinds = vec![BandKind::Peaking; n];
    kinds[0] = BandKind::LowShelf;
    kinds[n - 1] = BandKind::HighShelf;
    kinds
}

fn flat_band(kind: BandKind, freq: f32) -> EqBand {
    let q = if kind == BandKind::Peaking {
        1.0
    } else {
        SHELF_Q
    };
    EqBand {
        kind,
        freq,
        q,
        gain: 0.0,
    }
}

/// A flat channel EQ. Every band at 0 dB is an exact passthrough.
pub fn flat_channel() -> Vec<EqBand> {
    channel_layout()
        .into_iter()
        .zip(CENTRES)
        .map(|(kind, freq)| flat_band(kind, freq))
        .collect()
}

/// A flat headphone correction.
pub fn flat_headphone() -> HeadphoneEq {
    let mut bands = vec![flat_band(BandKind::LowShelf, 105.0)];
    bands.extend(
        CENTRES
            .iter()
            .map(|&freq| flat_band(BandKind::Peaking, freq)),
    );
    bands.push(flat_band(BandKind::HighShelf, 10_000.0));
    HeadphoneEq {
        preamp: 0.0,
        bands,
        name: None,
    }
}

/// Check bands against a layout and the ranges the filters accept.
pub fn validate(bands: &[EqBand], layout: &[BandKind], gain_limit: f32) -> Result<()> {
    ensure!(
        bands.len() == layout.len(),
        "expected {} EQ bands, got {}",
        layout.len(),
        bands.len()
    );
    for (i, (band, kind)) in bands.iter().zip(layout).enumerate() {
        let n = i + 1;
        ensure!(
            band.kind == *kind,
            "band {n} must be {kind:?}, not {:?}",
            band.kind
        );
        ensure!(
            band.freq.is_finite() && (20.0..=20_000.0).contains(&band.freq),
            "band {n} frequency {} Hz out of range (20..=20000)",
            band.freq
        );
        ensure!(
            band.q.is_finite() && (0.1..=10.0).contains(&band.q),
            "band {n} Q {} out of range (0.1..=10)",
            band.q
        );
        ensure!(
            band.gain.is_finite() && band.gain.abs() <= gain_limit,
            "band {n} gain {} dB out of range (±{gain_limit})",
            band.gain
        );
    }
    Ok(())
}

pub fn validate_headphone(eq: &HeadphoneEq) -> Result<()> {
    ensure!(
        eq.preamp.is_finite() && eq.preamp.abs() <= HEADPHONE_GAIN_LIMIT,
        "preamp {} dB out of range (±{HEADPHONE_GAIN_LIMIT})",
        eq.preamp
    );
    validate(&eq.bands, &headphone_layout(), HEADPHONE_GAIN_LIMIT)
}

/// Parse AutoEQ's `ParametricEQ.txt` into a headphone correction.
///
/// ```text
/// Preamp: -6.2 dB
/// Filter 1: ON LSC Fc 105 Hz Gain 7.0 dB Q 0.70
/// Filter 2: ON PK Fc 180 Hz Gain -3.4 dB Q 0.53
/// ```
///
/// Shelves go to the shelf slots and peaking filters fill the ten peaking slots
/// in order; unused slots stay flat. A second shelf of the same kind, an
/// eleventh peak, or another filter type is refused rather than approximated.
pub fn parse_autoeq(text: &str) -> Result<HeadphoneEq> {
    let mut eq = flat_headphone();
    let (mut low, mut high, mut peaks, mut any) = (false, false, 0usize, false);
    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("Preamp:") {
            eq.preamp = number(rest.trim().trim_end_matches("dB"))
                .with_context(|| format!("line {line_no}: preamp"))?;
            continue;
        }
        ensure!(
            line.starts_with("Filter"),
            "line {line_no}: not an AutoEQ line: {line}"
        );
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.get(2) == Some(&"OFF") {
            continue;
        }
        let value = |key: &str| -> Result<f32> {
            let at = words
                .iter()
                .position(|w| *w == key)
                .with_context(|| format!("line {line_no}: no {key}"))?;
            number(words.get(at + 1).copied().unwrap_or_default())
                .with_context(|| format!("line {line_no}: {key}"))
        };
        let (freq, gain, q) = (value("Fc")?, value("Gain")?, value("Q")?);
        let slot = match words.get(3).copied() {
            Some("LSC") | Some("LS") => {
                ensure!(!low, "line {line_no}: only one low shelf fits");
                low = true;
                0
            }
            Some("HSC") | Some("HS") => {
                ensure!(!high, "line {line_no}: only one high shelf fits");
                high = true;
                11
            }
            Some("PK") | Some("PEQ") => {
                ensure!(peaks < 10, "line {line_no}: more than ten peaking filters");
                peaks += 1;
                peaks
            }
            other => bail!(
                "line {line_no}: filter type {} is not supported (PK, LSC, HSC)",
                other.unwrap_or("?")
            ),
        };
        eq.bands[slot] = EqBand {
            kind: eq.bands[slot].kind,
            freq,
            q,
            gain,
        };
        any = true;
    }
    ensure!(
        any,
        "no filters found — paste the contents of an AutoEQ ParametricEQ.txt"
    );
    validate_headphone(&eq)?;
    Ok(eq)
}

fn number(text: &str) -> Result<f32> {
    let value: f32 = text
        .trim()
        .parse()
        .with_context(|| format!("{text:?} is not a number"))?;
    ensure!(value.is_finite(), "{text:?} is not a finite number");
    Ok(value)
}

/// The `Props` value that sets a channel's whole chain in one `pw-cli set-param`.
pub fn props(channel: &[EqBand], headphone: &HeadphoneEq) -> String {
    let mut params = vec![format!(
        "\"pre:Mult\" {:.6}",
        10f32.powf(headphone.preamp / 20.0)
    )];
    for (i, band) in headphone.bands.iter().enumerate() {
        params.push(band_params(&format!("hp_{}", i + 1), band));
    }
    for (i, band) in channel.iter().enumerate() {
        params.push(band_params(&format!("eq_{}", i + 1), band));
    }
    format!("{{ params = [ {} ] }}", params.join(" "))
}

fn band_params(name: &str, band: &EqBand) -> String {
    format!(
        "\"{name}:Freq\" {:.1} \"{name}:Q\" {:.3} \"{name}:Gain\" {:.2}",
        band.freq, band.q, band.gain
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Preamp: -6.2 dB
Filter 1: ON LSC Fc 105 Hz Gain 7.0 dB Q 0.70
Filter 2: ON PK Fc 180 Hz Gain -3.4 dB Q 0.53
Filter 3: ON PK Fc 1200 Hz Gain 2.1 dB Q 1.40
Filter 4: ON HSC Fc 10000 Hz Gain -2.0 dB Q 0.70
";

    #[test]
    fn flat_defaults_fit_their_layouts() {
        let channel = flat_channel();
        assert_eq!(
            channel.iter().map(|b| b.kind).collect::<Vec<_>>(),
            channel_layout()
        );
        assert!(channel.iter().all(|b| b.gain == 0.0));
        validate(&channel, &channel_layout(), CHANNEL_GAIN_LIMIT).unwrap();
        let hp = flat_headphone();
        assert_eq!(
            hp.bands.iter().map(|b| b.kind).collect::<Vec<_>>(),
            headphone_layout()
        );
        validate_headphone(&hp).unwrap();
    }

    #[test]
    fn autoeq_fills_shelves_and_peaks_in_order() {
        let eq = parse_autoeq(SAMPLE).unwrap();
        assert_eq!(eq.preamp, -6.2);
        assert_eq!(
            eq.bands[0],
            EqBand {
                kind: BandKind::LowShelf,
                freq: 105.0,
                q: 0.70,
                gain: 7.0
            }
        );
        assert_eq!(
            eq.bands[1],
            EqBand {
                kind: BandKind::Peaking,
                freq: 180.0,
                q: 0.53,
                gain: -3.4
            }
        );
        assert_eq!(eq.bands[2].freq, 1200.0);
        assert!(
            eq.bands[3..=10].iter().all(|b| b.gain == 0.0),
            "unused slots stay flat"
        );
        assert_eq!(
            eq.bands[11],
            EqBand {
                kind: BandKind::HighShelf,
                freq: 10000.0,
                q: 0.70,
                gain: -2.0
            }
        );
    }

    #[test]
    fn autoeq_skips_off_filters() {
        let eq = parse_autoeq(
            "Filter 1: OFF PK Fc 100 Hz Gain 5 dB Q 1\nFilter 2: ON PK Fc 200 Hz Gain 1 dB Q 1",
        )
        .unwrap();
        assert_eq!(eq.bands[1].freq, 200.0);
        assert_eq!(eq.bands[2].gain, 0.0);
    }

    #[test]
    fn autoeq_refuses_what_it_cannot_represent() {
        let two_low = "Filter 1: ON LSC Fc 100 Hz Gain 1 dB Q 0.7\nFilter 2: ON LSC Fc 200 Hz Gain 1 dB Q 0.7";
        assert!(parse_autoeq(two_low)
            .unwrap_err()
            .to_string()
            .contains("line 2"));
        let eleven: String = (1..=11)
            .map(|i| format!("Filter {i}: ON PK Fc {} Hz Gain 1 dB Q 1\n", 100 * i))
            .collect();
        assert!(parse_autoeq(&eleven).is_err());
        assert!(
            parse_autoeq("Filter 1: ON LP Fc 100 Hz Gain 0 dB Q 1").is_err(),
            "low-pass"
        );
        assert!(parse_autoeq("").is_err(), "no filters");
        assert!(parse_autoeq("hello").is_err());
        assert!(
            parse_autoeq("Filter 1: ON PK Fc 100 Hz Gain 30 dB Q 1").is_err(),
            "too loud"
        );
    }

    #[test]
    fn validate_rejects_wrong_kinds_and_ranges() {
        let layout = channel_layout();
        let mut bands = flat_channel();
        bands[0].kind = BandKind::Peaking;
        assert!(
            validate(&bands, &layout, CHANNEL_GAIN_LIMIT).is_err(),
            "kind fixed by the graph"
        );
        for broken in [
            EqBand {
                freq: 10.0,
                ..flat_channel()[3]
            },
            EqBand {
                q: 0.0,
                ..flat_channel()[3]
            },
            EqBand {
                gain: 13.0,
                ..flat_channel()[3]
            },
            EqBand {
                gain: f32::NAN,
                ..flat_channel()[3]
            },
        ] {
            let mut bands = flat_channel();
            bands[3] = broken;
            assert!(
                validate(&bands, &layout, CHANNEL_GAIN_LIMIT).is_err(),
                "{broken:?}"
            );
        }
        assert!(
            validate(&flat_channel()[..9], &layout, CHANNEL_GAIN_LIMIT).is_err(),
            "count"
        );
    }

    #[test]
    fn props_set_every_stage_in_one_command() {
        let p = props(&flat_channel(), &flat_headphone());
        assert!(p.starts_with("{ params = [ ") && p.ends_with(" ] }"), "{p}");
        assert!(p.contains("\"pre:Mult\" 1.000000"));
        assert!(p.contains("\"hp_12:Gain\" 0.00"));
        assert!(p.contains("\"eq_10:Freq\" 16000.0"));
        assert_eq!(p.matches(":Freq\"").count(), 22);
    }

    #[test]
    fn preamp_is_a_linear_multiplier() {
        let hp = HeadphoneEq {
            preamp: -6.0,
            ..flat_headphone()
        };
        assert!(props(&flat_channel(), &hp).contains("\"pre:Mult\" 0.501187"));
    }
}
