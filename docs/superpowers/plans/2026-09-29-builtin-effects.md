# Built-in effects (M6b) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** an audio-visualizer base effect (the keys as a spectrum analyser of what's playing), and
a mic-mute light (one key lit while the default microphone is muted).

**Architecture:**
- **Analysis:** a pure analyser (`keyboard/spectrum.rs`, `rustfft`) turns 1024-sample hops into 18
  smoothed band levels.
- **Capture:** a capture thread (`keyboard/audio.rs`) feeds it from `pw-record --raw` of the
  default output's monitor.
- **Mute watching:** a watcher thread (`keyboard/mic.rs`) polls `wpctl`.
- **Worker:** the keyboard worker starts and stops both as settings require. The visualizer is
  drawn by the pure `effects` module, and the mute light composes on top of everything.

**Tech Stack:** Rust (rustfft 6), PipeWire CLI tools, Svelte 5.

**Spec:** `docs/superpowers/specs/2026-09-29-builtin-effects-design.md`

**Depends on:** the M6a plan being done first; this builds on its `Overlay` / `layers` code in
`keyboard.rs`. Work continues on the M6a branch, stacked: `git checkout -b m6b-effects` from the
M6a tip.

## Global Constraints

- **Capture command:**
  `pw-record --raw -P "{ stream.capture.sink = true }" --format f32 --rate 48000 --channels 1 --latency 20ms -`,
  producing little-endian f32 samples at 48 kHz, mono.
- **Analysis:**
  - hop 1024, window 2048 (Hann), 18 log-spaced bands from 40 Hz to 16 kHz;
  - band value = peak magnitude, normalised by `4/N` (a full-scale sine gives 0 dB);
  - level = `clamp((dB + 60)/60, 0, 1)`;
  - smoothing `new ≥ old ? new : max(new, old · 0.85)`.
- **Drawing:**
  - sensitivity 0.25–4.0 multiplies the level at draw time (clamped to 1);
  - band = `floor(x_centre / 18.25 · 18)`;
  - `row = round(max(y_centre − 1, 0))` and `from_bottom = 5 − row`;
  - a key is lit iff `from_bottom + 1 ≤ level·6 (+1e-4)`, with colour
    `hsv((1 − from_bottom/5)/3)`.
- **Mic:**
  - `wpctl get-volume @DEFAULT_AUDIO_SOURCE@` every 500 ms; muted iff the output contains
    `[MUTED]`; send only on change;
  - the default light is off, key Pause (`0x48`), red `{255,0,0}`;
  - the key must be on the layout;
  - the light draws on top of everything, only while `lighting.enabled`.
- **Lifetimes:** capture runs only while drawing with the Visualizer effect; the mic watcher only
  while `lighting.enabled && mic_mute.enabled` and the keyboard is connected. Both stop on drop.
  Failed starts retry every 2 s.
- **Privacy:** audio is never written anywhere. Don't log sample data.
- **Checks and style:** as in M6a: workspace tests, clippy clean, `npm run check`, hand formatting.

## Review Focus

1. **`pw-record` not installed, or dying mid-run.** The bars go dark, a retry follows every 2 s,
   and there's no busy loop. Pinned by the Task 4 tests `a_missing_program_is_an_error_not_a_panic`
   and `the_capture_reports_when_its_program_ends`.
2. **Switching away from Visualizer.** The `pw-record` child is killed and reaped, not left running
   in the background. Pinned by the Task 4 test `dropping_the_capture_kills_its_program_promptly`.
3. **`wpctl` failing** (no PipeWire session). This counts as not muted and never floods the channel.
   Pinned by the Task 5 test `a_failing_command_counts_as_unmuted_and_is_sent_once`.
4. **A mute key saved that the layout lacks** (a hand edit). It's rejected by `set_lighting`
   validation, and draws nothing on load. Pinned by the Task 6 test
   `a_mute_key_must_be_on_the_keyboard`.
5. **Mute and a GameSense overlay on the same key.** Mute wins. Pinned by the Task 6 test
   `the_mute_light_wins_over_overlays`.

---

### Task 1: Settings for the visualizer and the mute light (IPC)

**Files:**
- Modify: `crates/gg-ipc/src/lib.rs`
- Modify: `crates/gglinuxd/src/keyboard/effects.rs` (a temporary `Visualizer` arm)

**Interfaces:**
- Produces:
  - `Effect::Visualizer { sensitivity: f32 }` (JSON `{"kind":"visualizer","sensitivity":1.0}`);
  - `Lighting::SENSITIVITY: RangeInclusive<f32> = 0.25..=4.0`;
  - `MicMuteLight { enabled: bool, key: u8, color: Rgb }` (Default: off, `0x48`, red);
  - `Lighting.mic_mute: MicMuteLight` (`#[serde(default)]`).

- [ ] **Step 1: Write the failing tests** (append to `gg-ipc` tests)

```rust
    #[test]
    fn the_visualizer_effect_and_its_sensitivity_bounds() {
        let v: Effect = serde_json::from_str(r#"{"kind":"visualizer","sensitivity":1.5}"#).unwrap();
        assert_eq!(v, Effect::Visualizer { sensitivity: 1.5 });
        let with = |sensitivity| Lighting { effect: Effect::Visualizer { sensitivity }, ..Lighting::default() };
        assert!(with(0.25).validate().is_ok() && with(4.0).validate().is_ok());
        assert!(with(0.2).validate().is_err() && with(4.5).validate().is_err() && with(f32::NAN).validate().is_err());
    }

    #[test]
    fn the_mic_mute_light_defaults_off_on_pause_in_red_and_old_files_still_load() {
        let d = MicMuteLight::default();
        assert_eq!((d.enabled, d.key, d.color), (false, 0x48, Rgb { r: 255, g: 0, b: 0 }));
        let old = r#"{"enabled":true,"effect":{"kind":"static"},"keys":[],"brightness":70}"#;
        assert_eq!(serde_json::from_str::<Lighting>(old).unwrap().mic_mute, d);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p gg-ipc visualizer mic_mute 2>&1 | grep -E "^error" | head -3`
Expected: `no variant … Visualizer` and `cannot find type MicMuteLight`.

- [ ] **Step 3: Implement**

`Effect` gains, after `Spectrum`:

```rust
    /// The keys as a spectrum analyser of whatever is playing.
    Visualizer { sensitivity: f32 },
```

After the `Reactive` impls:

```rust
/// A key lit while the default microphone is muted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicMuteLight {
    pub enabled: bool,
    /// HID code of the key; must be on the keyboard.
    pub key: u8,
    pub color: Rgb,
}

impl Default for MicMuteLight {
    fn default() -> Self {
        MicMuteLight { enabled: false, key: 0x48, color: Rgb { r: 255, g: 0, b: 0 } }
    }
}
```

`Lighting` gains, after `reactive`:

```rust
    /// Absent in files saved before it existed.
    #[serde(default)]
    pub mic_mute: MicMuteLight,
```

In `impl Lighting`, add `pub const SENSITIVITY: std::ops::RangeInclusive<f32> = 0.25..=4.0;`, and in
`validate`:
- the `let period = match self.effect { … }` gets the arm `Effect::Visualizer { .. } => None,`;
- before the reactive check, add:

```rust
        if let Effect::Visualizer { sensitivity } = self.effect {
            if !(sensitivity.is_finite() && Self::SENSITIVITY.contains(&sensitivity)) {
                return Err(format!("visualizer sensitivity {sensitivity} out of range (0.25..=4)"));
            }
        }
```

`impl Default for Lighting` gains `mic_mute: MicMuteLight::default(),`.

In `effects.rs`, add a temporary arm to `color`'s match, replaced in Task 3:
`Effect::Visualizer { .. } => Rgb::default(),`. Any `Lighting { … }` struct literals in tests that
list every field (the `lit` helper in effects.rs) gain `mic_mute: gg_ipc::MicMuteLight::default()`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --workspace --all-targets 2>&1 | grep -E "^test result" | awk '{p+=$4;f+=$6} END{print "passed="p" failed="f}'; cargo clippy --workspace --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: `failed=0`, `0`.

- [ ] **Step 5: Commit** — `gg-ipc: settings for the audio visualizer and the mic-mute light`

---

### Task 2: The spectrum analyser

**Files:**
- Create: `crates/gglinuxd/src/keyboard/spectrum.rs`
- Modify: `crates/gglinuxd/src/keyboard.rs` (`mod spectrum;`)
- Modify: `Cargo.toml` (workspace dep `rustfft = "6"`), `crates/gglinuxd/Cargo.toml` (`rustfft.workspace = true`)

**Interfaces:**
- Produces:
  - `spectrum::{BANDS = 18, HOP = 1024, RATE = 48_000.0}`;
  - `Analyzer::new()` and `Analyzer::push(&mut self, hop: &[f32]) -> [f32; BANDS]`;
  - `smooth(old: f32, new: f32) -> f32`.

- [ ] **Step 1: Write the failing tests** (the module with `todo!()` bodies, then):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn sine(hz: f32, amplitude: f32, from: usize) -> Vec<f32> {
        (from..from + HOP).map(|i| amplitude * (TAU * hz * i as f32 / RATE).sin()).collect()
    }

    fn loudest(levels: &[f32; BANDS]) -> usize {
        (0..BANDS).max_by(|&a, &b| levels[a].total_cmp(&levels[b])).unwrap()
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
        assert!(levels[9] > 0.8, "a −6 dB tone is near the top: {}", levels[9]);
        assert!(levels[2] < 0.3 && levels[16] < 0.3, "far bands stay low: {levels:?}");
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
        assert!(quiet > 0.2 && quiet < loud - 0.4, "−40 dB sits well below −6 dB: {quiet} vs {loud}");
    }

    #[test]
    fn bars_rise_at_once_and_fall_by_fifteen_percent_a_hop() {
        assert_eq!(smooth(0.5, 0.8), 0.8);
        assert!((smooth(0.8, 0.0) - 0.68).abs() < 1e-6);
        assert_eq!(smooth(0.8, 0.75), 0.75, "a small drop is taken as is");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p gglinuxd spectrum`
Expected: FAIL; the tests panic at `not yet implemented`.

- [ ] **Step 3: Implement**

```rust
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
        Analyzer { fft, hann, samples: vec![0.0; WINDOW], levels: [0.0; BANDS], edges }
    }

    /// Take one hop of new samples (at most a window's worth); return the smoothed levels.
    pub fn push(&mut self, hop: &[f32]) -> [f32; BANDS] {
        let hop = &hop[hop.len().saturating_sub(WINDOW)..];
        self.samples.drain(..hop.len());
        self.samples.extend_from_slice(hop);
        let mut buf: Vec<Complex<f32>> =
            self.samples.iter().zip(&self.hann).map(|(s, w)| Complex::new(s * w, 0.0)).collect();
        self.fft.process(&mut buf);
        let norm = 4.0 / WINDOW as f32; // Hann's coherent gain is ½: a full-scale sine → 1
        for band in 0..BANDS {
            let peak = buf[self.edges[band]..self.edges[band + 1]].iter().map(|c| c.norm() * norm).fold(0.0, f32::max);
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
```

`rustfft` re-exports `num_complex`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p gglinuxd spectrum && cargo clippy -p gglinuxd --all-targets 2>&1 | grep -E "^(warning|error)" -A3 | head`
Expected: 4 passed. `Analyzer` isn't used outside the tests until Task 4. If rustc reports
`dead_code`, add `#[allow(dead_code)] // used from Task 4` on `mod spectrum;`, and ledger it.

If `a_1khz_tone_lands_in_its_band` gets a different band index, print the band edges in Hz, fix the
test's comment and index to the band that truly contains 1 kHz, and ledger a ruling. The spec
fixes the band layout, not the index.

- [ ] **Step 5: Commit** — `gglinuxd: a spectrum analyser for the audio visualizer`

---

### Task 3: Drawing the visualizer

**Files:**
- Modify: `crates/gglinuxd/src/keyboard/effects.rs`
- Modify: `crates/gglinuxd/src/keyboard.rs` (the `color` call gains `&[]`)

**Interfaces:**
- Produces: `effects::color(lighting, positions, code, t, hits, bands: &[f32]) -> Rgb`.

- [ ] **Step 1: Write the failing tests**

In `effects.rs` tests, append `, &[]` as the new last argument of every existing `color(` call.
A Python one-off can do it: in the tests module, replace `, &hits)` with `, &hits, &[])`,
`, &[])` with `, &[], &[])`, and `&unknown)` with `&unknown, &[])`, then check that each `color(`
call has six arguments. Then add:

```rust
    fn viz(sensitivity: f32) -> Lighting {
        lit(Effect::Visualizer { sensitivity })
    }

    #[test]
    fn the_visualizer_lights_bars_from_the_bottom_green_to_red() {
        let p = positions();
        let low = [0.2; 18];
        let space = color(&viz(1.0), &p, 0x2C, 0.0, &[], &low);
        assert_eq!(space, hsv(1.0 / 3.0), "the bottom row lights green at a low level");
        assert_eq!(color(&viz(1.0), &p, 0x3A, 0.0, &[], &low), Rgb::default(), "the F-row stays dark");
        let full = [1.0; 18];
        assert_eq!(color(&viz(1.0), &p, 0x3A, 0.0, &[], &full), hsv(0.0), "a full bar reaches the F-row in red");
        assert_eq!(color(&viz(1.0), &p, 0x3A, 0.0, &[], &[]), Rgb::default(), "no audio yet: dark");
    }

    #[test]
    fn columns_are_bands_and_sensitivity_scales_the_bars() {
        let p = positions();
        let mut bands = [0.0; 18];
        bands[0] = 1.0; // only the leftmost band
        assert_ne!(color(&viz(1.0), &p, 0x29, 0.0, &[], &bands), Rgb::default(), "Esc is in band 0");
        assert_eq!(color(&viz(1.0), &p, 0x48, 0.0, &[], &bands), Rgb::default(), "Pause is in band 17");
        let quiet = [0.1; 18];
        assert_eq!(color(&viz(1.0), &p, 0x2C, 0.0, &[], &quiet), Rgb::default(), "0.1·6 < 1 row");
        assert_ne!(color(&viz(2.0), &p, 0x2C, 0.0, &[], &quiet), Rgb::default(), "doubled: 0.2·6 ≥ 1 row");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p gglinuxd effects 2>&1 | grep -E "^error" | head -3`
Expected: `this function takes 5 arguments but 6 arguments were supplied`.

- [ ] **Step 3: Implement**

- `color` gains the parameter `bands: &[f32]` after `hits`, and the `Visualizer` arm becomes
  `Effect::Visualizer { sensitivity } => visualizer(positions.get(&code), bands, sensitivity),`.
- Add:

```rust
/// Rows on the board: the F-row is 0, the bottom row 5.
const ROWS: f32 = 6.0;

/// A key's colour in the visualizer: lit if its column's bar reaches its row.
fn visualizer(pos: Option<&Pos>, bands: &[f32], sensitivity: f32) -> Rgb {
    let (Some(p), false) = (pos, bands.is_empty()) else { return Rgb::default() };
    let band = ((p.x / WIDTH * bands.len() as f32) as usize).min(bands.len() - 1);
    let level = (bands[band] * sensitivity).min(1.0);
    let from_bottom = (ROWS - 1.0) - (p.y - 1.0).max(0.0).round();
    if from_bottom + 1.0 <= level * ROWS + 1e-4 {
        hsv((1.0 - from_bottom / (ROWS - 1.0)) / 3.0)
    } else {
        Rgb::default()
    }
}
```

- In `keyboard.rs` `render`, `effects::color(…, t, hits)` becomes `effects::color(…, t, hits, &[])`.
  Task 6 passes the real levels.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p gglinuxd effects && cargo clippy -p gglinuxd --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: all `effects` tests pass; clippy `0`.

- [ ] **Step 5: Commit** — `gglinuxd: draw the audio visualizer`

---

### Task 4: Audio capture

**Files:**
- Create: `crates/gglinuxd/src/keyboard/audio.rs`
- Modify: `crates/gglinuxd/src/keyboard.rs` (`mod audio;`; `Command::AudioStopped`; its `Debug` arm)

**Interfaces:**
- Consumes: `spectrum::{Analyzer, BANDS, HOP}` (Task 2).
- Produces:
  - `audio::PW_RECORD: [&str; 13]`;
  - `audio::Levels = Arc<Mutex<[f32; BANDS]>>`;
  - `audio::Capture::start(argv: &[&str], levels: Levels, tx: Sender<Command>) -> io::Result<Capture>`
    (stops and reaps on drop);
  - `Command::AudioStopped`.

- [ ] **Step 1: Add the command variant**

In `enum Command`, before `Shutdown`:

```rust
    /// The audio capture's program ended by itself; retried later.
    AudioStopped,
```

Add the `Debug` arm `Command::AudioStopped => "AudioStopped",` and a no-op arm in `handle`
(`Command::AudioStopped => {}`), which Task 6 replaces.

- [ ] **Step 2: Write the failing tests** (`audio.rs` with `todo!()` bodies, then):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn levels() -> Levels {
        Arc::new(Mutex::new([0.0; BANDS]))
    }

    #[test]
    fn the_capture_reports_when_its_program_ends() {
        // Four hops of a 1 kHz tone as raw little-endian f32, then end of file.
        let path = std::env::temp_dir().join(format!("gglinux-audio-{}.raw", std::process::id()));
        let bytes: Vec<u8> = (0..4 * HOP)
            .flat_map(|i| (0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin()).to_le_bytes())
            .collect();
        std::fs::write(&path, bytes).unwrap();
        let (tx, rx) = mpsc::channel();
        let l = levels();
        let _capture = Capture::start(&["cat", path.to_str().unwrap()], Arc::clone(&l), tx).unwrap();
        assert!(matches!(rx.recv_timeout(Duration::from_secs(2)), Ok(Command::AudioStopped)));
        std::fs::remove_file(&path).unwrap();
        let got = *l.lock().unwrap();
        let loudest = (0..BANDS).max_by(|&a, &b| got[a].total_cmp(&got[b])).unwrap();
        assert_eq!(loudest, 9, "the tone was analysed: {got:?}");
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let (tx, _rx) = mpsc::channel();
        assert!(Capture::start(&["/nonexistent/pw-record"], levels(), tx).is_err());
    }

    #[test]
    fn dropping_the_capture_kills_its_program_promptly() {
        let (tx, rx) = mpsc::channel();
        let capture = Capture::start(&["sleep", "30"], levels(), tx).unwrap();
        let started = Instant::now();
        drop(capture);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "no AudioStopped for a stop we asked for");
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p gglinuxd keyboard::audio`
Expected: FAIL; the tests panic at `not yet implemented`.

- [ ] **Step 4: Implement**

```rust
//! Feeds the visualizer: records the default output's monitor with
//! `pw-record` and keeps 18 band levels current. Audio is analysed in memory
//! only; nothing is written anywhere.

use std::io::{self, Read};
use std::process::{Child, Command as Process, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::spectrum::{Analyzer, BANDS, HOP};
use super::Command;

/// Mono 48 kHz f32 of what's playing, raw on stdout.
pub const PW_RECORD: [&str; 13] = [
    "pw-record", "--raw", "-P", "{ stream.capture.sink = true }", "--format", "f32", "--rate", "48000",
    "--channels", "1", "--latency", "20ms", "-",
];

pub type Levels = Arc<Mutex<[f32; BANDS]>>;

pub struct Capture {
    stop: Arc<AtomicBool>,
    child: Child,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Run `argv` (raw little-endian f32, mono, 48 kHz on stdout) and keep `levels` current.
    pub fn start(argv: &[&str], levels: Levels, tx: Sender<Command>) -> io::Result<Capture> {
        let (program, args) = argv.split_first().ok_or_else(|| io::Error::other("empty command"))?;
        let mut child = Process::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
        let mut out = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new().name("gg-audio".into()).spawn(move || {
            let mut analyzer = Analyzer::new();
            let mut bytes = vec![0u8; HOP * 4];
            let mut hop = vec![0f32; HOP];
            while out.read_exact(&mut bytes).is_ok() {
                for (s, b) in hop.iter_mut().zip(bytes.chunks_exact(4)) {
                    *s = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                }
                let new = analyzer.push(&hop);
                *levels.lock().unwrap_or_else(|e| e.into_inner()) = new;
            }
            if !flag.load(Ordering::Relaxed) {
                let _ = tx.send(Command::AudioStopped);
            }
        })?;
        Ok(Capture { stop, child, thread: Some(thread) })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(t) = self.thread.take() {
            let _ = t.join(); // the pipe closed with the child, so the read has returned
        }
    }
}
```

Add `mod audio;` to `keyboard.rs`. If it's unused until Task 6, add
`#[allow(dead_code)] // used from Task 6` and ledger it.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test -p gglinuxd keyboard::audio && cargo clippy -p gglinuxd --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: 3 passed; clippy `0`.

- [ ] **Step 6: Commit** — `gglinuxd: capture what's playing for the visualizer`

---

### Task 5: The mic-mute watcher

**Files:**
- Create: `crates/gglinuxd/src/keyboard/mic.rs`
- Modify: `crates/gglinuxd/src/keyboard.rs` (`mod mic;`; `Command::MicMuted(bool)`; `Debug` arm; a
  no-op `handle` arm)

**Interfaces:**
- Produces:
  - `mic::WPCTL: [&str; 3]`;
  - `mic::muted(output: &str) -> bool`;
  - `mic::Watch::start(argv: Vec<String>, every: Duration, tx: Sender<Command>) -> Watch` (stops on
    drop);
  - `Command::MicMuted(bool)`.

- [ ] **Step 1: Write the failing tests** (`mic.rs` with `todo!()` bodies, then):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn next(rx: &mpsc::Receiver<Command>) -> Option<bool> {
        match rx.recv_timeout(Duration::from_millis(400)) {
            Ok(Command::MicMuted(m)) => Some(m),
            Ok(other) => panic!("unexpected {other:?}"),
            Err(_) => None,
        }
    }

    #[test]
    fn wpctl_output_says_muted_in_brackets() {
        assert!(muted("Volume: 1.00 [MUTED]\n"));
        assert!(!muted("Volume: 0.40\n"));
        assert!(!muted(""));
    }

    #[test]
    fn the_state_is_sent_once_then_only_on_change() {
        let (tx, rx) = mpsc::channel();
        let _w = Watch::start(argv(&["echo", "Volume: 0.40 [MUTED]"]), Duration::from_millis(50), tx);
        assert_eq!(next(&rx), Some(true));
        assert_eq!(next(&rx), None, "polled again, unchanged: nothing sent");
    }

    #[test]
    fn a_failing_command_counts_as_unmuted_and_is_sent_once() {
        let (tx, rx) = mpsc::channel();
        let _w = Watch::start(argv(&["/nonexistent/wpctl"]), Duration::from_millis(50), tx);
        assert_eq!(next(&rx), Some(false));
        assert_eq!(next(&rx), None);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p gglinuxd keyboard::mic`
Expected: FAIL; the tests panic at `not yet implemented`.

- [ ] **Step 3: Implement**

```rust
//! Watches whether the default microphone is muted, for the mute light.

use std::process::Command as Process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::debug;

use super::Command;

pub const WPCTL: [&str; 3] = ["wpctl", "get-volume", "@DEFAULT_AUDIO_SOURCE@"];
const STEP: Duration = Duration::from_millis(25);

/// `wpctl get-volume` appends `[MUTED]` when muted.
pub fn muted(output: &str) -> bool {
    output.contains("[MUTED]")
}

pub struct Watch {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    pub fn start(argv: Vec<String>, every: Duration, tx: Sender<Command>) -> Watch {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("gg-mic".into())
            .spawn(move || {
                let mut last: Option<bool> = None;
                let mut warned = false;
                while !flag.load(Ordering::Relaxed) {
                    let now = match Process::new(&argv[0]).args(&argv[1..]).output() {
                        Ok(out) => muted(&String::from_utf8_lossy(&out.stdout)),
                        Err(e) => {
                            if !warned {
                                debug!(error = %e, "could not read the microphone's mute state");
                                warned = true;
                            }
                            false
                        }
                    };
                    if last != Some(now) {
                        last = Some(now);
                        if tx.send(Command::MicMuted(now)).is_err() {
                            return;
                        }
                    }
                    let until = Instant::now() + every;
                    while Instant::now() < until && !flag.load(Ordering::Relaxed) {
                        std::thread::sleep(STEP);
                    }
                }
            })
            .expect("spawn gg-mic thread");
        Watch { stop, thread: Some(thread) }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
```

In `keyboard.rs`:
- `Command` gains `/// The default microphone was muted or unmuted.` followed by `MicMuted(bool),`;
- `Debug` gains `Command::MicMuted(_) => "MicMuted",`;
- `handle` gains the placeholder arm `Command::MicMuted(_) => {}`, which Task 6 replaces;
- add `mod mic;` (with a dead-code allow until Task 6 if needed; ledger it).

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p gglinuxd keyboard::mic && cargo clippy -p gglinuxd --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: 3 passed; clippy `0`.

- [ ] **Step 5: Commit** — `gglinuxd: watch the microphone's mute state`

---

### Task 6: Wire both into the keyboard worker

**Files:**
- Modify: `crates/gglinuxd/src/keyboard.rs`

**Interfaces:**
- Consumes: Tasks 1–5.
- Produces pure helpers:
  - `wants_audio(lighting: &Lighting, drawing: bool) -> bool`;
  - `wants_mic(lighting: &Lighting, connected: bool) -> bool`;
  - `mic_color(mic: Option<(u8, gg_ipc::Rgb)>, code: u8) -> Option<Rgb>`;
  - `check_keys(lighting: &Lighting) -> Result<(), String>`.

- [ ] **Step 1: Write the failing tests** (in `keyboard.rs` tests)

```rust
    #[test]
    fn the_capture_and_the_mic_watch_run_only_when_wanted() {
        let viz = Lighting { effect: Effect::Visualizer { sensitivity: 1.0 }, ..lighting(true, false, Effect::Static) };
        assert!(wants_audio(&viz, true));
        assert!(!wants_audio(&viz, false), "not drawing");
        assert!(!wants_audio(&lighting(true, false, Effect::Static), true), "another effect");
        let mut m = lighting(true, false, Effect::Static);
        m.mic_mute.enabled = true;
        assert!(wants_mic(&m, true));
        assert!(!wants_mic(&m, false), "keyboard away");
        m.enabled = false;
        assert!(!wants_mic(&m, true), "lighting off");
    }

    #[test]
    fn the_mute_light_wins_over_overlays() {
        let red = gg_ipc::Rgb { r: 255, g: 0, b: 0 };
        assert_eq!(mic_color(Some((0x48, red)), 0x48), Some(gg_device_keyboard::Rgb { r: 255, g: 0, b: 0 }));
        assert_eq!(mic_color(Some((0x48, red)), 0x47), None);
        assert_eq!(mic_color(None, 0x48), None);
    }

    #[test]
    fn a_mute_key_must_be_on_the_keyboard() {
        let mut l = lighting(true, false, Effect::Static);
        assert!(check_keys(&l).is_ok());
        l.mic_mute.key = 0x59; // keypad 1: not on a TKL
        assert!(check_keys(&l).is_err());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p gglinuxd keyboard::tests 2>&1 | grep -E "^error" | head -3`
Expected: `cannot find function wants_audio` (and the others).

- [ ] **Step 3: Implement**

1. Imports: `use gg_ipc::Effect;`, `use audio::{Capture, Levels};`, `use spectrum::BANDS;`.
2. Constant: `const MIC_POLL: Duration = Duration::from_millis(500);`
3. `State` gains:

```rust
    /// Visualizer band levels, written by the capture thread.
    levels: Levels,
    capture: Option<Capture>,
    last_capture_attempt: Option<Instant>,
    mic_watch: Option<mic::Watch>,
    mic_muted: bool,
```

   initialised to `Arc::new(Mutex::new([0.0; BANDS])), None, None, None, false`. Add
   `std::sync::Mutex` to the imports.
4. Helpers:

```rust
fn wants_audio(lighting: &Lighting, drawing: bool) -> bool {
    drawing && matches!(lighting.effect, Effect::Visualizer { .. })
}

fn wants_mic(lighting: &Lighting, connected: bool) -> bool {
    lighting.enabled && lighting.mic_mute.enabled && connected
}

/// The mute light's colour for this key, if it's the mute key.
fn mic_color(mic: Option<(u8, gg_ipc::Rgb)>, code: u8) -> Option<Rgb> {
    mic.filter(|&(key, _)| key == code).map(|(_, c)| Rgb { r: c.r, g: c.g, b: c.b })
}

/// Settings that name keys must name keys this keyboard has.
fn check_keys(lighting: &Lighting) -> Result<(), String> {
    let key = lighting.mic_mute.key;
    if !ANSI_TKL.iter().any(|k| k.0 == key) {
        return Err(format!("the mic-mute key {key:#04x} is not on this keyboard"));
    }
    Ok(())
}

fn sync_audio(state: &mut State) {
    let on_screen = state.device.is_some() && drawing(state.lighting.enabled, !state.layers.is_empty());
    if !wants_audio(&state.lighting, on_screen) {
        if state.capture.take().is_some() {
            *state.levels.lock().unwrap_or_else(|e| e.into_inner()) = [0.0; BANDS];
        }
        return;
    }
    if state.capture.is_some() || !is_older_than(state.last_capture_attempt, REOPEN_INTERVAL) {
        return;
    }
    state.last_capture_attempt = Some(Instant::now());
    match Capture::start(&audio::PW_RECORD, Arc::clone(&state.levels), state.tx.clone()) {
        Ok(c) => {
            debug!("capturing audio for the visualizer");
            state.capture = Some(c);
        }
        Err(e) => debug!(error = %e, "could not start pw-record for the visualizer"),
    }
}

fn sync_mic(state: &mut State) {
    if !wants_mic(&state.lighting, state.device.is_some()) {
        state.mic_watch = None;
        state.mic_muted = false;
        return;
    }
    if state.mic_watch.is_none() {
        let argv = mic::WPCTL.iter().map(|s| s.to_string()).collect();
        state.mic_watch = Some(mic::Watch::start(argv, MIC_POLL, state.tx.clone()));
    }
}
```

5. Loop: after `sync_reader(&mut state);` add `sync_audio(&mut state); sync_mic(&mut state);`.
6. `handle`:
   - `SetLighting` validates `lighting.validate().and_then(|()| check_keys(&lighting))`;
   - replace the placeholder arms:

```rust
        Command::AudioStopped => {
            state.capture = None;
            *state.levels.lock().unwrap_or_else(|e| e.into_inner()) = [0.0; BANDS];
            state.last_capture_attempt = Some(Instant::now());
        }
        Command::MicMuted(m) => {
            state.mic_muted = m;
            state.last_frame = None;
        }
```

7. `render`: before `let Some(device) = …`, add:

```rust
    let bands = *state.levels.lock().unwrap_or_else(|e| e.into_inner());
    let m = &state.lighting.mic_mute;
    let mic = (state.lighting.enabled && m.enabled && state.mic_muted).then_some((m.key, m.color));
```

   and the frame closure becomes:

```rust
        .and_then(|()| device.show(|code| match mic_color(mic, code).or_else(|| overlay_color(layers, code)) {
            Some(c) => effects::dim(c, state.lighting.brightness),
            None => effects::color(&state.lighting, &state.positions, code, t, hits, &bands),
        }));
```

8. Remove any temporary `#[allow(dead_code)]` put on `mod spectrum;`, `mod audio;` or `mod mic;`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --workspace --all-targets 2>&1 | grep -E "^test result" | awk '{p+=$4;f+=$6} END{print "passed="p" failed="f}'; cargo clippy --workspace --all-targets 2>&1 | grep -E "^(warning|error)" -A4 | head`
Expected: `failed=0`; no clippy output.

- [ ] **Step 5: Commit** — `gglinuxd: the visualizer and the mute light in the keyboard worker`

---

### Task 7: UI and docs

**Files:**
- Modify: `ui/src/lib/types.ts`, `ui/src/lib/KeyboardPanel.svelte`, `PLAN.md`, `README.md`

- [ ] **Step 1: Types**

`types.ts`:
- add `| { kind: "visualizer"; sensitivity: number }` to `Effect`;
- add

```ts
export interface MicMuteLight {
  enabled: boolean;
  /** HID code of the key */
  key: number;
  color: Rgb;
}
```

- add `mic_mute: MicMuteLight;` to `Lighting`.

- [ ] **Step 2: The panel**

In `KeyboardPanel.svelte`:
- `EFFECTS` gains `{ kind: "visualizer", label: "Audio" }`.
- `period` becomes:
  `let period = $derived(lighting && "period_s" in lighting.effect ? lighting.effect.period_s : 4);`
- `setEffect` builds `{ kind: "visualizer", sensitivity: 1 }` for `"visualizer"`. Write it as an
  explicit branch before the `breathing` check, so the `period_s` shape is never built for it.
- `setPeriod` returns early unless `"period_s" in lighting.effect`.
- The Speed row shows only `{#if "period_s" in lighting.effect}`.
- After the Speed row, add:

```svelte
        {#if lighting.effect.kind === "visualizer"}
          {@const sens = lighting.effect.sensitivity}
          <div class="row">
            <span class="label">Sensitivity</span>
            <input
              type="range"
              min="0.25"
              max="4"
              step="0.25"
              value={sens}
              onchange={(e) => apply({ effect: { kind: "visualizer", sensitivity: e.currentTarget.valueAsNumber } })}
            />
            <span class="unit">{sens.toFixed(2)}×</span>
          </div>
          <p class="hint">What's playing is analysed live and never recorded; KDE may show that audio is being captured.</p>
        {/if}
```

- After the Reactive section, add:

```svelte
      <section>
        <div class="head"><h4>Mic mute light</h4></div>
        <label class="toggle">
          <input
            type="checkbox"
            checked={lighting.mic_mute.enabled}
            disabled={busy}
            onchange={(e) => apply({ mic_mute: { ...lighting.mic_mute, enabled: e.currentTarget.checked } })}
          />
          Light a key while the microphone is muted
        </label>
        {#if lighting.mic_mute.enabled}
          <div class="row">
            <span class="label">Key</span>
            <select
              value={lighting.mic_mute.key}
              onchange={(e) => apply({ mic_mute: { ...lighting.mic_mute, key: Number(e.currentTarget.value) } })}
            >
              {#each kb.layout.filter((k) => k.label) as k (k.code)}
                <option value={k.code}>{k.label}</option>
              {/each}
            </select>
            <input
              type="color"
              value={hex(lighting.mic_mute.color)}
              onchange={(e) => apply({ mic_mute: { ...lighting.mic_mute, color: rgbOf(e.currentTarget.value) } })}
            />
          </div>
        {/if}
      </section>
```

- Style: `.hint { margin: 0.5rem 0 0; font-size: 0.78rem; color: var(--text-dim); }`.

Run: `cd ui && npm run check 2>&1 | tail -1`
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: Docs**

- PLAN.md, M6 section, after the M6a progress paragraph:

```markdown
**M6b built-in effects done in software** (2026-09-29; spec
`docs/superpowers/specs/2026-09-29-builtin-effects-design.md`): an **Audio** visualizer effect (18
bands from `pw-record` of the default output, bars rising from the bottom row) and a **mic-mute
light** (a chosen key, red by default, while PipeWire's default microphone is muted). Not yet
verified on hardware.
```

- README.md, after the GameSense paragraph:

```markdown
**Audio visualizer and mic-mute light** use PipeWire's own `pw-record` and `wpctl`; nothing extra to
install. The visualizer analyses what's playing live and never records it.
```

- [ ] **Step 4: Final checks** — workspace tests, clippy `0`, `npm run check` clean.

- [ ] **Step 5: Commit** — `UI: audio visualizer and mic-mute light; docs`

---

## Hardware acceptance (the owner)

1. Keyboard → Effect **Audio**, then play music: bars rise from the bottom row, bass on the left.
2. Mic mute light on (Pause); mute the microphone in KDE: Pause lights red within half a second.
   Unmute: it goes back.
3. Switch to another effect: `pgrep pw-record` finds nothing.
