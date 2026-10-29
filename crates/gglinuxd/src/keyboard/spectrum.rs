// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio → 18 band levels for the visualizer. Pure: samples in, levels out.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

pub const BANDS: usize = 18;
pub const HOP: usize = 1024;
pub const RATE: f32 = 48_000.0;
const WINDOW: usize = 2048;
const LOW_HZ: f32 = 40.0;
const HIGH_HZ: f32 = 16_000.0;
const FLOOR_DB: f32 = -60.0;
const DECAY: f32 = 0.85;

pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    hann: Vec<f32>,
    /// The last `WINDOW` samples.
    samples: Vec<f32>,
    levels: [f32; BANDS],
    /// FFT bin where each band starts; band `i` is `edges[i]..edges[i + 1]`.
    edges: [usize; BANDS + 1],
}

impl Analyzer {
    pub fn new() -> Analyzer {
        let fft = FftPlanner::new().plan_fft_forward(WINDOW);
        let hann = (0..WINDOW)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / WINDOW as f32).cos())
            .collect();
        let mut edges = [0; BANDS + 1];
        for (i, edge) in edges.iter_mut().enumerate() {
            let hz = LOW_HZ * (HIGH_HZ / LOW_HZ).powf(i as f32 / BANDS as f32);
            *edge = (hz * WINDOW as f32 / RATE).round() as usize;
        }
        for i in 1..=BANDS {
            edges[i] = edges[i].max(edges[i - 1] + 1); // every band gets at least one bin
        }
        Analyzer {
            fft,
            hann,
            samples: vec![0.0; WINDOW],
            levels: [0.0; BANDS],
            edges,
        }
    }

    /// Take one hop of new samples (at most a window's worth); return the smoothed levels.
    pub fn push(&mut self, hop: &[f32]) -> [f32; BANDS] {
        let hop = &hop[hop.len().saturating_sub(WINDOW)..];
        self.samples.drain(..hop.len());
        self.samples.extend_from_slice(hop);
        let mut buf: Vec<Complex<f32>> = self
            .samples
            .iter()
            .zip(&self.hann)
            .map(|(s, w)| Complex::new(s * w, 0.0))
            .collect();
        self.fft.process(&mut buf);
        let norm = 4.0 / WINDOW as f32; // Hann's coherent gain is ½: a full-scale sine → 1
        for band in 0..BANDS {
            let peak = buf[self.edges[band]..self.edges[band + 1]]
                .iter()
                .map(|c| c.norm() * norm)
                .fold(0.0, f32::max);
            let db = 20.0 * peak.max(1e-9).log10();
            let raw = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
            self.levels[band] = smooth(self.levels[band], raw);
        }
        self.levels
    }
}

/// Bars rise at once and fall back gradually.
pub fn smooth(old: f32, new: f32) -> f32 {
    if new >= old {
        new
    } else {
        new.max(old * DECAY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn sine(hz: f32, amplitude: f32, from: usize) -> Vec<f32> {
        (from..from + HOP)
            .map(|i| amplitude * (TAU * hz * i as f32 / RATE).sin())
            .collect()
    }

    fn loudest(levels: &[f32; BANDS]) -> usize {
        (0..BANDS)
            .max_by(|&a, &b| levels[a].total_cmp(&levels[b]))
            .unwrap()
    }

    #[test]
    fn a_1khz_tone_lands_in_its_band() {
        let mut a = Analyzer::new();
        let mut levels = [0.0; BANDS];
        for hop in 0..4 {
            levels = a.push(&sine(1000.0, 0.5, hop * HOP));
        }
        // 40 Hz · 400^(i/18) ≤ 1000 Hz < 40 Hz · 400^((i+1)/18)  →  i = 9
        assert_eq!(loudest(&levels), 9, "{levels:?}");
        assert!(
            levels[9] > 0.8,
            "a −6 dB tone is near the top: {}",
            levels[9]
        );
        assert!(
            levels[2] < 0.3 && levels[16] < 0.3,
            "far bands stay low: {levels:?}"
        );
    }

    #[test]
    fn silence_is_zero() {
        let mut a = Analyzer::new();
        assert_eq!(a.push(&[0.0; HOP]), [0.0; BANDS]);
    }

    #[test]
    fn quieter_tones_give_lower_levels() {
        let level_at = |amplitude| {
            let mut a = Analyzer::new();
            let mut l = [0.0; BANDS];
            for hop in 0..4 {
                l = a.push(&sine(1000.0, amplitude, hop * HOP));
            }
            l[9]
        };
        let (loud, quiet) = (level_at(0.5), level_at(0.01));
        assert!(
            quiet > 0.2 && quiet < loud - 0.4,
            "−40 dB sits well below −6 dB: {quiet} vs {loud}"
        );
    }

    #[test]
    fn bars_rise_at_once_and_fall_by_fifteen_percent_a_hop() {
        assert_eq!(smooth(0.5, 0.8), 0.8);
        assert!((smooth(0.8, 0.0) - 0.68).abs() < 1e-6);
        assert_eq!(smooth(0.8, 0.75), 0.75, "a small drop is taken as is");
    }
}
