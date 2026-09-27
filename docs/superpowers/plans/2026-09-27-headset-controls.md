# Headset controls (M2 remainder) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every Arctis Nova Pro Wireless base-station setting is changeable from the gglinux UI, with device-reported values shown live and write-only values labelled as last-set-here.

**Architecture:** `gg-device-headset` gets a typed `Setting` whose pure `packets()` owns every encoding and range, and `Headset::apply` (packets + save). One IPC method `set_headset { change }` carries a tagged `HeadsetChange`; the worker maps it to a `Setting` with a pure, tested function, applies it, remembers write-only values, and lights the screen. `HeadsetState.controls` exposes both kinds of value to a larger `HeadsetPanel`.

**Tech Stack:** Rust 2021 (hidapi 2, serde), Svelte 5 + TypeScript.

**Spec:** `docs/superpowers/specs/2026-09-27-headset-controls-design.md`

## Global Constraints

- Ranges (verbatim from spec): transparency 1–10; sidetone 0–3; mic volume 1–10; mic LED 1–10; auto-off minutes {0 (never), 1, 5, 10, 15, 30, 60} → codes 0–6; EQ preset 0–4; EQ bands: 10 values, −10.0…+10.0 dB, 0.5 dB steps.
- Every applied change is followed by `06 09` (save). Custom bands are sent as `06 2e 04` then `06 33 …`.
- Noise mode codes: 0 off, 1 transparency, 2 ANC. Mic status byte: 0 unmuted, 1 muted.
- `gg-ipc` stays free of device-crate dependencies.
- Out of scope: Bluetooth, line-out, ChatMix, analog gain, the base-station knob.

## Review Focus

1. **A client sends an out-of-range value** (mic volume 160 from an old client, LED 0) → an error naming the range, and **nothing is sent to the device** — test in Task 3 (`invalid_changes_are_rejected_before_the_device`), checked by validating `packets()` before `apply`.
2. **Auto-off minutes that are not a device level** (45) → error, not rounding — test in Task 3 (`setting_for` rejects 45).
3. **Custom EQ applied without selecting the Custom slot would be inaudible** → `EqBands` always emits the preset select first — test in Task 1 (`every_setting_encodes_to_its_documented_commands`).
4. **Status reports an ANC byte outside 0–2** → shown as unknown, not mislabelled — test in Task 1 (`status_reports_noise_mode_and_mute`).
5. **Applying custom bands must record the preset as Custom** so the UI shows the right preset — test in Task 3 (`recording_custom_bands_marks_the_preset_custom`).

---

### Task 1: Device crate — typed settings, corrected ranges, apply

**Files:** Modify `crates/gg-device-headset/src/proto.rs`, `crates/gg-device-headset/src/lib.rs`, `crates/gg-device-headset/examples/headsetprobe.rs`

**Interfaces — Produces:** `proto::NoiseMode { Off, Transparency, Anc }` (`code`, `from_code`), `AutoOff::Never`, `proto::Setting { NoiseMode(NoiseMode), Transparency(u8), Sidetone(u8), MicVolume(u8), MicLed(u8), AutoOff(AutoOff), WirelessMode(WirelessMode), EqPreset(u8), EqBands(Vec<f32>) }` with `packets(&self) -> Result<Vec<Vec<u8>>>`, `Status::noise_mode() -> Option<NoiseMode>`, `Status::mic_muted() -> Option<bool>`, `Headset::apply(&self, &Setting) -> Result<()>`, `Headset::set_noise_mode(NoiseMode)`; re-exports `NoiseMode, Setting` from the crate root.

- [ ] **Step 1: Failing tests** — in `proto.rs` tests: change `mic_volume_packet(0xa0).unwrap(), vec![0x37, 0xa0]` to `mic_volume_packet(10).unwrap(), vec![0x37, 0x0a]`, and append:

```rust
    #[test]
    fn every_setting_encodes_to_its_documented_commands() {
        let cases: Vec<(Setting, Vec<Vec<u8>>)> = vec![
            (Setting::NoiseMode(NoiseMode::Off), vec![vec![0xbd, 0]]),
            (Setting::NoiseMode(NoiseMode::Transparency), vec![vec![0xbd, 1]]),
            (Setting::NoiseMode(NoiseMode::Anc), vec![vec![0xbd, 2]]),
            (Setting::Transparency(10), vec![vec![0xb9, 10]]),
            (Setting::Sidetone(3), vec![vec![0x39, 3]]),
            (Setting::MicVolume(1), vec![vec![0x37, 1]]),
            (Setting::MicLed(1), vec![vec![0xbf, 1]]),
            (Setting::AutoOff(AutoOff::Never), vec![vec![0xc1, 0]]),
            (Setting::WirelessMode(WirelessMode::Speed), vec![vec![0xc3, 0]]),
            (Setting::EqPreset(1), vec![vec![0x2e, 1]]),
        ];
        for (setting, expected) in cases {
            assert_eq!(setting.packets().unwrap(), expected, "{setting:?}");
        }
        let bands = Setting::EqBands(vec![0.0; EQ_BANDS]).packets().unwrap();
        assert_eq!(bands[0], vec![0x2e, 0x04], "custom slot first, or the bands stay inaudible");
        assert_eq!(bands[1][0], 0x33);
        assert_eq!(bands.len(), 2);
    }

    #[test]
    fn settings_reject_values_the_references_do_not_use() {
        assert!(Setting::Transparency(0).packets().is_err());
        assert!(Setting::Transparency(11).packets().is_err());
        assert!(Setting::MicVolume(0).packets().is_err());
        assert!(Setting::MicVolume(11).packets().is_err(), "the old 0xa0 scale is gone");
        assert!(Setting::MicLed(0).packets().is_err(), "1 is the dimmest; 0 is not a level");
        assert!(Setting::MicLed(11).packets().is_err());
        assert!(Setting::Sidetone(4).packets().is_err());
        assert!(Setting::EqPreset(5).packets().is_err());
        assert!(Setting::EqBands(vec![0.0; 9]).packets().is_err());
    }

    #[test]
    fn auto_off_never_is_code_zero() {
        assert_eq!(AutoOff::Never.code(), 0);
        assert_eq!(AutoOff::Never.minutes(), 0);
        assert_eq!(AutoOff::from_code(0), Some(AutoOff::Never));
        assert_eq!(AutoOff::from_minutes(0).unwrap(), AutoOff::Never);
        let mut r = status_reply(0x08, 8, 8);
        r[offset::AUTO_OFF] = 0;
        assert_eq!(Status::decode(&r).unwrap().auto_off, Some(AutoOff::Never));
    }

    #[test]
    fn status_reports_noise_mode_and_mute() {
        let mut r = status_reply(0x08, 8, 8);
        r[offset::ANC] = 1;
        r[offset::MIC_STATUS] = 1;
        let s = Status::decode(&r).unwrap();
        assert_eq!(s.noise_mode(), Some(NoiseMode::Transparency));
        assert_eq!(s.mic_muted(), Some(true));
        r[offset::ANC] = 9;
        r[offset::MIC_STATUS] = 7;
        let odd = Status::decode(&r).unwrap();
        assert_eq!(odd.noise_mode(), None, "unknown byte is unknown, not a mode");
        assert_eq!(odd.mic_muted(), None);
    }
```

- [ ] **Step 2: Run** `cargo test -p gg-device-headset` — Expected: compile errors (`Setting`, `NoiseMode`, `AutoOff::Never` missing).

- [ ] **Step 3: Implement in `proto.rs`**

Replace the `MIC_VOLUME_MAX` doc + const with:

```rust
/// Mic volume steps: 1 (muted) to 10 (100%). loteran's shipped slider uses this
/// scale; an older comment claiming `0xa0` for 100% is contradicted by it.
pub const MIC_VOLUME_MAX: u8 = 10;

/// Mic-mute LED and transparency levels share a 1–10 scale; 1 is the minimum
/// (HeadsetControl's "off" is 1), there is no 0.
const LEVEL_MIN: u8 = 1;
const LEVEL_MAX: u8 = 10;
```

Replace `mic_volume_packet`, `mic_led_packet`, `transparency_packet`, and `anc_packet` with:

```rust
/// Microphone volume, 1 (muted) to [`MIC_VOLUME_MAX`].
pub fn mic_volume_packet(volume: u8) -> Result<Vec<u8>> {
    ensure!(
        (1..=MIC_VOLUME_MAX).contains(&volume),
        "mic volume {volume} out of range (1..={MIC_VOLUME_MAX})"
    );
    Ok(vec![CMD_MIC_VOLUME, volume])
}

/// Mic-mute LED brightness, 1..=10.
pub fn mic_led_packet(brightness: u8) -> Result<Vec<u8>> {
    ensure!(
        (LEVEL_MIN..=LEVEL_MAX).contains(&brightness),
        "mic LED brightness {brightness} out of range ({LEVEL_MIN}..={LEVEL_MAX})"
    );
    Ok(vec![CMD_MIC_LED, brightness])
}

/// Transparency (passthrough) level, 1..=10.
pub fn transparency_packet(level: u8) -> Result<Vec<u8>> {
    ensure!(
        (LEVEL_MIN..=LEVEL_MAX).contains(&level),
        "transparency level {level} out of range ({LEVEL_MIN}..={LEVEL_MAX})"
    );
    Ok(vec![CMD_TRANSPARENCY, level])
}

/// Noise control mode — what the headset's ANC button cycles through.
pub fn noise_mode_packet(mode: NoiseMode) -> Vec<u8> {
    vec![CMD_ANC, mode.code()]
}
```

Add after the `WirelessMode` impl:

```rust
/// Noise control. Status byte 10 and the `07 bd` event carry the same codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseMode {
    Off,
    Transparency,
    Anc,
}

impl NoiseMode {
    pub fn code(self) -> u8 {
        match self {
            NoiseMode::Off => 0,
            NoiseMode::Transparency => 1,
            NoiseMode::Anc => 2,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => NoiseMode::Off,
            1 => NoiseMode::Transparency,
            2 => NoiseMode::Anc,
            _ => return None,
        })
    }
}

/// One user-facing setting, as a client changes it.
#[derive(Debug, Clone, PartialEq)]
pub enum Setting {
    NoiseMode(NoiseMode),
    Transparency(u8),
    Sidetone(u8),
    MicVolume(u8),
    MicLed(u8),
    AutoOff(AutoOff),
    WirelessMode(WirelessMode),
    EqPreset(u8),
    EqBands(Vec<f32>),
}

impl Setting {
    /// Every command this setting needs, validated, in send order. Custom bands
    /// stay inaudible until the custom slot is selected, so that goes first.
    pub fn packets(&self) -> Result<Vec<Vec<u8>>> {
        Ok(match self {
            Setting::NoiseMode(mode) => vec![noise_mode_packet(*mode)],
            Setting::Transparency(level) => vec![transparency_packet(*level)?],
            Setting::Sidetone(level) => vec![sidetone_packet(*level)?],
            Setting::MicVolume(volume) => vec![mic_volume_packet(*volume)?],
            Setting::MicLed(brightness) => vec![mic_led_packet(*brightness)?],
            Setting::AutoOff(timer) => vec![auto_off_packet(*timer)],
            Setting::WirelessMode(mode) => vec![wireless_mode_packet(*mode)],
            Setting::EqPreset(preset) => vec![eq_preset_packet(*preset)?],
            Setting::EqBands(gains) => {
                vec![eq_preset_packet(EQ_CUSTOM_PRESET)?, eq_bands_packet(gains)?]
            }
        })
    }
}
```

`AutoOff`: add a first variant `Never` (doc `/// Never switch off.`), and in `code` add `AutoOff::Never => 0`, in `minutes` add `AutoOff::Never => 0`, in `from_code` add `0 => AutoOff::Never`, in `from_minutes` add `0 => AutoOff::Never` (keep the existing arms and error).

In `Status`, change the three raw field docs to `/// Raw status byte; see [`Status::noise_mode`].` (ANC), `/// Raw status byte; see [`Status::mic_muted`].` (mic), `/// Raw status byte, 1..=10 when meaningful.` (transparency), and add after `decode`:

```rust
    /// Decoded noise mode; `None` for a byte outside the known codes.
    pub fn noise_mode(&self) -> Option<NoiseMode> {
        NoiseMode::from_code(self.anc_raw)
    }

    /// Whether the mic is muted (status byte 9: 0 live, 1 muted).
    pub fn mic_muted(&self) -> Option<bool> {
        match self.mic_status_raw {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
```

- [ ] **Step 4: `lib.rs`** — re-export: `pub use proto::{AutoOff, NoiseMode, PowerStatus, Setting, Status, WirelessMode, EQ_BANDS, EQ_CUSTOM_PRESET};`. Replace `set_transparency` and `set_anc` with:

```rust
    pub fn set_transparency(&self, level: u8) -> Result<()> {
        self.send(&proto::transparency_packet(level)?)
    }

    pub fn set_noise_mode(&self, mode: NoiseMode) -> Result<()> {
        self.send(&proto::noise_mode_packet(mode))
    }

    /// Apply one setting and commit it to flash, as HeadsetControl does after
    /// every change, so it survives a power cycle.
    pub fn apply(&self, setting: &Setting) -> Result<()> {
        for packet in setting.packets()? {
            self.send(&packet)?;
        }
        self.save()
    }
```

In `examples/headsetprobe.rs`, replace the three raw lines with:

```rust
            println!("  mic muted        {:?}", s.mic_muted());
            println!("  noise mode       {:?}", s.noise_mode());
            println!("  transparency     {}", s.transparency_raw);
```

- [ ] **Step 5: Run** `cargo test -p gg-device-headset && cargo build -p gg-device-headset --examples` — Expected: 18 unit tests + doctest pass; examples build.

- [ ] **Step 6: Commit** `git commit -m "Headset: typed settings with corrected ranges, and apply-with-save"`

---

### Task 2: IPC — `set_headset` and `HeadsetControls`

**Files:** Modify `crates/gg-ipc/src/lib.rs`, `crates/gglinuxd/examples/fakemouse.rs`, `crates/gglinuxd/src/main.rs` (stub arm)

**Interfaces — Produces:** `gg_ipc::{NoiseMode, RadioMode, HeadsetChange, HeadsetControls}`, `HeadsetState.controls`, `Request::SetHeadset { change }`.

- [ ] **Step 1: Failing tests** — append to `gg-ipc` tests; also add `controls: HeadsetControls::default(),` to the `HeadsetState` literal in `headset_state_serializes_for_the_ui`:

```rust
    #[test]
    fn headset_changes_parse_from_their_documented_json() {
        let cases = [
            (r#"{"setting":"noise_mode","value":"anc"}"#, HeadsetChange::NoiseMode(NoiseMode::Anc)),
            (r#"{"setting":"sidetone","value":2}"#, HeadsetChange::Sidetone(2)),
            (r#"{"setting":"auto_off_minutes","value":0}"#, HeadsetChange::AutoOffMinutes(0)),
            (r#"{"setting":"radio_mode","value":"range"}"#, HeadsetChange::RadioMode(RadioMode::Range)),
            (
                r#"{"setting":"eq_bands","value":[0,0,0,0,0,0,0,0,0,1.5]}"#,
                HeadsetChange::EqBands(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.5]),
            ),
        ];
        for (json, expected) in cases {
            let line = format!(r#"{{"id":1,"req":{{"method":"set_headset","params":{{"change":{json}}}}}}}"#);
            match serde_json::from_str::<RequestFrame>(&line).unwrap().req {
                Request::SetHeadset { change } => assert_eq!(change, expected, "{json}"),
                other => panic!("{json} parsed as {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_controls_serialize_as_null() {
        let json = serde_json::to_string(&HeadsetControls::default()).unwrap();
        assert!(json.contains(r#""sidetone":null"#), "{json}");
        assert!(json.contains(r#""noise_mode":null"#), "{json}");
        assert!(json.contains(r#""eq_bands":null"#), "{json}");
    }
```

- [ ] **Step 2: Run** `cargo test -p gg-ipc` — Expected: compile errors.

- [ ] **Step 3: Implement** — before `HeadsetState`:

```rust
/// Headset noise control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseMode {
    Off,
    Transparency,
    Anc,
}

/// 2.4 GHz link profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioMode {
    Speed,
    Range,
}

/// One headset setting to change: `{"setting":"sidetone","value":2}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "setting", content = "value", rename_all = "snake_case")]
pub enum HeadsetChange {
    NoiseMode(NoiseMode),
    /// 1..=10.
    Transparency(u8),
    /// 0 off, 1 low, 2 medium, 3 high.
    Sidetone(u8),
    /// 1 (muted)..=10.
    MicVolume(u8),
    /// 1..=10.
    MicLed(u8),
    /// 0 (never), 1, 5, 10, 15, 30 or 60.
    AutoOffMinutes(u16),
    RadioMode(RadioMode),
    /// 0 Flat, 1 Bass Boost, 2 Focus, 3 Smiley, 4 Custom.
    EqPreset(u8),
    /// Ten gains in dB, -10.0..=10.0 in 0.5 steps. Selects the Custom preset.
    EqBands(Vec<f32>),
}

/// Headset settings. The first group is read from the base station every
/// second; the second is what gglinux wrote this session, because the device
/// cannot report it. `None` means not known.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HeadsetControls {
    pub noise_mode: Option<NoiseMode>,
    pub transparency: Option<u8>,
    pub mic_led: Option<u8>,
    pub mic_muted: Option<bool>,
    pub auto_off_minutes: Option<u16>,
    pub radio_mode: Option<RadioMode>,
    pub sidetone: Option<u8>,
    pub mic_volume: Option<u8>,
    pub eq_preset: Option<u8>,
    pub eq_bands: Option<Vec<f32>>,
}
```

`HeadsetState`: change derive to `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]` (drop `Eq`) and add a last field `pub controls: HeadsetControls,`. `Request`: add after `SetHeadsetScreen`: `/// Change one headset setting and save it on the base station.` `SetHeadset { change: HeadsetChange },`.

- [ ] **Step 4: Keep the workspace compiling.** `main.rs` `dispatch`: add `Request::SetHeadset { .. } => ResponseData::Error { message: "not wired up yet".into() },` after the `SetHeadsetScreen` arm. `fakemouse.rs`: import `HeadsetChange, HeadsetControls`; add field `controls: HeadsetControls,` to `Fake` (init `HeadsetControls::default()`); in `headset_state` add `controls: s.controls.clone(),`; `handle` arm:

```rust
        Request::SetHeadset { change } => {
            fake_apply(&mut s.controls, change);
            ResponseData::Success
        }
```

with, after `headset_state`:

```rust
/// Echo a change into the fake's controls, as the real device would report it.
fn fake_apply(c: &mut HeadsetControls, change: HeadsetChange) {
    match change {
        HeadsetChange::NoiseMode(m) => c.noise_mode = Some(m),
        HeadsetChange::Transparency(v) => c.transparency = Some(v),
        HeadsetChange::Sidetone(v) => c.sidetone = Some(v),
        HeadsetChange::MicVolume(v) => c.mic_volume = Some(v),
        HeadsetChange::MicLed(v) => c.mic_led = Some(v),
        HeadsetChange::AutoOffMinutes(v) => c.auto_off_minutes = Some(v),
        HeadsetChange::RadioMode(m) => c.radio_mode = Some(m),
        HeadsetChange::EqPreset(p) => c.eq_preset = Some(p),
        HeadsetChange::EqBands(b) => {
            c.eq_bands = Some(b);
            c.eq_preset = Some(4);
        }
    }
}
```

`method_name`: `Request::SetHeadset { .. } => "set_headset",`; `describe`: `Request::SetHeadset { change } => format!("set_headset {change:?}"),`.

- [ ] **Step 5: Run** `cargo test -p gg-ipc && cargo build --workspace --examples` — Expected: 9 gg-ipc tests pass; workspace builds.

- [ ] **Step 6: Commit** `git commit -m "IPC: set_headset and the headset controls it reports"`

---

### Task 3: Worker — map, apply, remember, report

**Files:** Create `crates/gglinuxd/src/headset/controls.rs`; modify `crates/gglinuxd/src/headset.rs`, `crates/gglinuxd/src/main.rs`

**Interfaces — Consumes:** Task 1 `Setting`, `Headset::apply`, `Status::{noise_mode, mic_muted}`; Task 2 IPC types. **Produces:** `controls::{setting_for, WriteOnly, record, noise_mode, radio_mode}`, `headset::Command::SetControl { change, reply }`.

- [ ] **Step 1: Failing tests** — create `controls.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gg_device_headset::NoiseMode as DevNoise;

    #[test]
    fn changes_map_onto_device_settings() {
        assert_eq!(
            setting_for(&HeadsetChange::NoiseMode(NoiseMode::Anc)).unwrap(),
            Setting::NoiseMode(DevNoise::Anc)
        );
        assert_eq!(
            setting_for(&HeadsetChange::RadioMode(RadioMode::Range)).unwrap(),
            Setting::WirelessMode(WirelessMode::Range)
        );
        assert_eq!(
            setting_for(&HeadsetChange::AutoOffMinutes(0)).unwrap(),
            Setting::AutoOff(AutoOff::Never)
        );
        assert_eq!(setting_for(&HeadsetChange::Sidetone(2)).unwrap(), Setting::Sidetone(2));
    }

    #[test]
    fn auto_off_minutes_must_be_a_device_level() {
        assert!(setting_for(&HeadsetChange::AutoOffMinutes(45)).is_err(), "no rounding");
    }

    #[test]
    fn invalid_changes_are_rejected_before_the_device() {
        for change in [
            HeadsetChange::MicVolume(160),
            HeadsetChange::MicLed(0),
            HeadsetChange::Transparency(11),
            HeadsetChange::EqBands(vec![0.0; 3]),
        ] {
            assert!(validated(&change).is_err(), "{change:?}");
        }
        assert!(validated(&HeadsetChange::MicVolume(10)).is_ok());
    }

    #[test]
    fn recording_custom_bands_marks_the_preset_custom() {
        let mut w = WriteOnly::default();
        record(&mut w, &HeadsetChange::EqBands(vec![1.0; 10]));
        assert_eq!(w.eq_preset, Some(4));
        assert_eq!(w.eq_bands, Some(vec![1.0; 10]));
        record(&mut w, &HeadsetChange::Sidetone(3));
        assert_eq!(w.sidetone, Some(3));
        record(&mut w, &HeadsetChange::NoiseMode(NoiseMode::Off));
        assert_eq!(w.mic_volume, None, "read-back settings are not recorded");
    }
}
```

and in `headset.rs` add `mod controls;` after `mod screen;`.

- [ ] **Step 2: Run** `cargo test -p gglinuxd controls` — Expected: compile errors.

- [ ] **Step 3: Implement** — prepend to `controls.rs`:

```rust
//! Headset settings between the IPC and the device: mapping a client's change
//! onto the device's typed setting, and remembering what the device cannot
//! report back.

use gg_device_headset::{AutoOff, NoiseMode as DevNoise, Setting, WirelessMode};
use gg_ipc::{HeadsetChange, NoiseMode, RadioMode};

/// Map a client's change onto the device setting. Ranges are the device
/// crate's to judge (see [`validated`]); only the auto-off conversion, which
/// has no device meaning outside its levels, is checked here.
pub fn setting_for(change: &HeadsetChange) -> Result<Setting, String> {
    Ok(match change {
        HeadsetChange::NoiseMode(mode) => Setting::NoiseMode(match mode {
            NoiseMode::Off => DevNoise::Off,
            NoiseMode::Transparency => DevNoise::Transparency,
            NoiseMode::Anc => DevNoise::Anc,
        }),
        HeadsetChange::Transparency(level) => Setting::Transparency(*level),
        HeadsetChange::Sidetone(level) => Setting::Sidetone(*level),
        HeadsetChange::MicVolume(volume) => Setting::MicVolume(*volume),
        HeadsetChange::MicLed(level) => Setting::MicLed(*level),
        HeadsetChange::AutoOffMinutes(minutes) => {
            Setting::AutoOff(AutoOff::from_minutes(*minutes).map_err(|e| e.to_string())?)
        }
        HeadsetChange::RadioMode(mode) => Setting::WirelessMode(match mode {
            RadioMode::Speed => WirelessMode::Speed,
            RadioMode::Range => WirelessMode::Range,
        }),
        HeadsetChange::EqPreset(preset) => Setting::EqPreset(*preset),
        HeadsetChange::EqBands(gains) => Setting::EqBands(gains.clone()),
    })
}

/// A setting that is safe to send: mapped, and every packet it needs encodes.
/// Checked before the device is touched, so a bad value never half-applies.
pub fn validated(change: &HeadsetChange) -> Result<Setting, String> {
    let setting = setting_for(change)?;
    setting.packets().map_err(|e| format!("{e:#}"))?;
    Ok(setting)
}

/// Settings the device cannot report, as gglinux last wrote them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WriteOnly {
    pub sidetone: Option<u8>,
    pub mic_volume: Option<u8>,
    pub eq_preset: Option<u8>,
    pub eq_bands: Option<Vec<f32>>,
}

/// Remember an applied change if the device cannot report it back.
pub fn record(written: &mut WriteOnly, change: &HeadsetChange) {
    match change {
        HeadsetChange::Sidetone(level) => written.sidetone = Some(*level),
        HeadsetChange::MicVolume(volume) => written.mic_volume = Some(*volume),
        HeadsetChange::EqPreset(preset) => written.eq_preset = Some(*preset),
        HeadsetChange::EqBands(gains) => {
            written.eq_bands = Some(gains.clone());
            written.eq_preset = Some(gg_device_headset::EQ_CUSTOM_PRESET);
        }
        _ => {}
    }
}

pub fn noise_mode(mode: DevNoise) -> NoiseMode {
    match mode {
        DevNoise::Off => NoiseMode::Off,
        DevNoise::Transparency => NoiseMode::Transparency,
        DevNoise::Anc => NoiseMode::Anc,
    }
}

pub fn radio_mode(mode: WirelessMode) -> RadioMode {
    match mode {
        WirelessMode::Speed => RadioMode::Speed,
        WirelessMode::Range => RadioMode::Range,
    }
}
```

- [ ] **Step 4: Run** `cargo test -p gglinuxd controls` — Expected: 4 pass.

- [ ] **Step 5: Wire the worker** — in `headset.rs`: import `gg_ipc::{..., HeadsetChange, HeadsetControls}`; add `written: controls::WriteOnly,` to `State` (init `controls::WriteOnly::default()`); add the command variant

```rust
    /// Apply one setting and save it on the base station.
    SetControl { change: HeadsetChange, reply: Reply<()> },
```

and its `handle` arm:

```rust
        Command::SetControl { change, reply } => {
            let _ = reply.send(set_control(state, &change));
            publish(state, event_tx);
        }
```

with, after `handle`:

```rust
/// Validate, apply with save, remember write-only values, and show the change.
fn set_control(state: &mut State, change: &HeadsetChange) -> Result<(), String> {
    let setting = controls::validated(change)?;
    let Some(device) = state.device.as_ref() else {
        return Err("base station not connected".into());
    };
    if let Err(e) = device.apply(&setting) {
        warn!(?change, error = %format!("{e:#}"), "setting write failed; reopening");
        drop_device(state);
        return Err(format!("{e:#}"));
    }
    controls::record(&mut state.written, change);
    // The change is shown for one lit period, and read-back values refresh now.
    let action = state.screen.on(Input::OurEvent, Instant::now());
    perform(state, action);
    state.last_poll = None;
    Ok(())
}
```

In `snapshot`, add the field:

```rust
        controls: HeadsetControls {
            noise_mode: status.and_then(|s| s.noise_mode()).map(controls::noise_mode),
            transparency: status.map(|s| s.transparency_raw).filter(|v| (1..=10).contains(v)),
            mic_led: status.map(|s| s.mic_led_brightness),
            mic_muted: status.and_then(|s| s.mic_muted()),
            auto_off_minutes: status.and_then(|s| s.auto_off).map(|a| a.minutes()),
            radio_mode: status.and_then(|s| s.wireless_mode).map(controls::radio_mode),
            sidetone: state.written.sidetone,
            mic_volume: state.written.mic_volume,
            eq_preset: state.written.eq_preset,
            eq_bands: state.written.eq_bands.clone(),
        },
```

In `main.rs` `dispatch`, replace the stub `SetHeadset` arm with:

```rust
        Request::SetHeadset { change } => outcome(
            workers
                .headset
                .send::<()>(|reply| HeadsetCommand::SetControl { change, reply })
                .await,
        ),
```

- [ ] **Step 6: Run** `cargo test --workspace && cargo build --workspace --examples && cargo clippy --workspace --all-targets` — Expected: all pass, no warnings.

- [ ] **Step 7: Commit** `git commit -m "gglinuxd: apply headset settings and report them"`

---

### Task 4: UI — headset controls

**Files:** Modify `ui/src/lib/types.ts`, `ui/src/lib/daemon.svelte.ts`, `ui/src/lib/HeadsetPanel.svelte`

- [ ] **Step 1: Types** — add to `types.ts` (before `HeadsetState`):

```ts
export type NoiseMode = "off" | "transparency" | "anc";
export type RadioMode = "speed" | "range";

/** One setting change; mirrors gg-ipc HeadsetChange. */
export type HeadsetChange =
  | { setting: "noise_mode"; value: NoiseMode }
  | { setting: "transparency"; value: number }
  | { setting: "sidetone"; value: number }
  | { setting: "mic_volume"; value: number }
  | { setting: "mic_led"; value: number }
  | { setting: "auto_off_minutes"; value: number }
  | { setting: "radio_mode"; value: RadioMode }
  | { setting: "eq_preset"; value: number }
  | { setting: "eq_bands"; value: number[] };

/** Read-back values first, then what gglinux wrote (the device can't report those). */
export interface HeadsetControls {
  noise_mode: NoiseMode | null;
  transparency: number | null;
  mic_led: number | null;
  mic_muted: boolean | null;
  auto_off_minutes: number | null;
  radio_mode: RadioMode | null;
  sidetone: number | null;
  mic_volume: number | null;
  eq_preset: number | null;
  eq_bands: number[] | null;
}
```

Add `controls: HeadsetControls;` as the last `HeadsetState` field, and `| { method: "set_headset"; params: { change: HeadsetChange } }` to `Request`.

- [ ] **Step 2: Client** — `daemon.svelte.ts`: import `HeadsetChange`; after `setHeadsetScreen`:

```ts
  async setHeadset(change: HeadsetChange): Promise<void> {
    const res = await this.request({ method: "set_headset", params: { change } });
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getHeadsetState();
  }
```

- [ ] **Step 3: Panel** — in `HeadsetPanel.svelte`:

Script additions (import `HeadsetChange, NoiseMode, RadioMode` types):

```ts
  const NOISE_MODES: { value: NoiseMode; label: string }[] = [
    { value: "off", label: "Off" },
    { value: "transparency", label: "Transparency" },
    { value: "anc", label: "ANC" },
  ];
  const SIDETONE = ["Off", "Low", "Medium", "High"];
  const AUTO_OFF = [
    { value: 0, label: "Never" },
    { value: 1, label: "1 min" },
    { value: 5, label: "5 min" },
    { value: 10, label: "10 min" },
    { value: 15, label: "15 min" },
    { value: 30, label: "30 min" },
    { value: 60, label: "60 min" },
  ];
  const RADIO: { value: RadioMode; label: string }[] = [
    { value: "speed", label: "Speed" },
    { value: "range", label: "Range" },
  ];
  const EQ_PRESETS = ["Flat", "Bass Boost", "Focus", "Smiley", "Custom"];
  const EQ_CUSTOM = 4;
  /** Standard octave centres; the device's exact band frequencies are undocumented. */
  const BAND_LABELS = ["31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];

  let controls = $derived(daemon.headset?.controls ?? null);

  /** Working copy of the custom curve, seeded once from what was last applied. */
  let bands = $state<number[]>(Array(10).fill(0));
  let editingCustom = $state(false);
  let bandsSeeded = false;
  $effect(() => {
    const applied = controls?.eq_bands;
    if (bandsSeeded || !applied) return;
    bandsSeeded = true;
    bands = [...applied];
  });

  async function change(c: HeadsetChange) {
    busy = true;
    error = null;
    try {
      await daemon.setHeadset(c);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  const signed = (db: number) => (db > 0 ? `+${db}` : `${db}`);
```

Markup: after the screen `{/if}` and before the error block:

```svelte
  {#if headset?.connected && controls}
    <section>
      <div class="head"><h4>Noise control</h4></div>
      <div class="segmented">
        {#each NOISE_MODES as m (m.value)}
          <button class:on={controls.noise_mode === m.value} disabled={busy}
            onclick={() => change({ setting: "noise_mode", value: m.value })}>{m.label}</button>
        {/each}
      </div>
      {#if controls.noise_mode === "transparency"}
        <div class="row">
          <span class="label">Transparency</span>
          <input type="range" min="1" max="10" step="1" value={controls.transparency ?? 5}
            disabled={busy}
            onchange={(e) => change({ setting: "transparency", value: e.currentTarget.valueAsNumber })} />
          <span class="unit">{controls.transparency ?? "–"}</span>
        </div>
      {/if}
    </section>

    <section>
      <div class="head">
        <h4>Microphone</h4>
        {#if controls.mic_muted !== null}
          <span class="state">{controls.mic_muted ? "muted" : "live"}</span>
        {/if}
      </div>
      <div class="row">
        <span class="label">Sidetone</span>
        <div class="segmented">
          {#each SIDETONE as label, i (i)}
            <button class:on={controls.sidetone === i} disabled={busy}
              onclick={() => change({ setting: "sidetone", value: i })}>{label}</button>
          {/each}
        </div>
      </div>
      <div class="row">
        <span class="label">Mic volume</span>
        <input type="range" min="1" max="10" step="1" value={controls.mic_volume ?? 10}
          disabled={busy}
          onchange={(e) => change({ setting: "mic_volume", value: e.currentTarget.valueAsNumber })} />
        <span class="unit">{controls.mic_volume ?? "unknown"}</span>
      </div>
      <div class="row">
        <span class="label">Mute light</span>
        <input type="range" min="1" max="10" step="1" value={controls.mic_led ?? 10}
          disabled={busy}
          onchange={(e) => change({ setting: "mic_led", value: e.currentTarget.valueAsNumber })} />
        <span class="unit">{controls.mic_led ?? "–"}</span>
      </div>
      <p class="hint">
        The headset can't report sidetone or mic volume, so those show what was last set here
        ("unknown" until then).
      </p>
    </section>

    <section>
      <div class="head"><h4>Power &amp; radio</h4></div>
      <div class="row">
        <span class="label">Auto-off</span>
        <select value={controls.auto_off_minutes ?? ""} disabled={busy}
          onchange={(e) => change({ setting: "auto_off_minutes", value: Number(e.currentTarget.value) })}>
          {#if controls.auto_off_minutes === null}<option value="">unknown</option>{/if}
          {#each AUTO_OFF as a (a.value)}<option value={a.value}>{a.label}</option>{/each}
        </select>
      </div>
      <div class="row">
        <span class="label">2.4 GHz</span>
        <div class="segmented">
          {#each RADIO as r (r.value)}
            <button class:on={controls.radio_mode === r.value} disabled={busy}
              onclick={() => change({ setting: "radio_mode", value: r.value })}>{r.label}</button>
          {/each}
        </div>
      </div>
    </section>

    <section>
      <div class="head"><h4>Equalizer</h4></div>
      <div class="segmented">
        {#each EQ_PRESETS as label, i (i)}
          <button class:on={controls.eq_preset === i || (i === EQ_CUSTOM && editingCustom)}
            disabled={busy}
            onclick={() => {
              if (i === EQ_CUSTOM) editingCustom = true;
              else { editingCustom = false; void change({ setting: "eq_preset", value: i }); }
            }}>{label}</button>
        {/each}
      </div>
      {#if editingCustom || controls.eq_preset === EQ_CUSTOM}
        <ul class="bands">
          {#each bands as db, i (i)}
            <li>
              <span class="freq">{BAND_LABELS[i]}</span>
              <input type="range" min="-10" max="10" step="0.5" bind:value={bands[i]} disabled={busy} />
              <span class="db">{signed(db)} dB</span>
            </li>
          {/each}
        </ul>
        <button class="apply" disabled={busy}
          onclick={() => change({ setting: "eq_bands", value: bands.map(Number) })}>
          Apply custom EQ
        </button>
      {/if}
      <p class="hint">The base station can't report its EQ, so the preset shows what was last set here.</p>
    </section>
  {/if}
```

Styles to add:

```css
  .segmented {
    display: inline-flex;
    flex-wrap: wrap;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 8px;
    overflow: hidden;
  }
  .segmented button {
    padding: 0.35rem 0.75rem;
    font-size: 0.82rem;
    color: var(--text-dim);
    background: none;
    border: none;
    cursor: pointer;
  }
  .segmented button.on {
    color: var(--bg);
    background: var(--accent);
    font-weight: 600;
  }
  .segmented button:disabled {
    cursor: default;
  }
  select {
    padding: 0.3rem 0.45rem;
    font-size: 0.85rem;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 7px;
  }
  .bands {
    margin: 0.7rem 0 0;
    padding: 0;
    list-style: none;
  }
  .bands li {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.1rem 0;
  }
  .freq {
    width: 2.4rem;
    font-family: var(--mono);
    font-size: 0.76rem;
    color: var(--text-dim);
    text-align: right;
  }
  .bands input {
    flex: 1;
    max-width: 16rem;
  }
  .db {
    width: 4rem;
    font-family: var(--mono);
    font-size: 0.78rem;
    color: var(--text-dim);
  }
  .apply {
    margin-top: 0.75rem;
    padding: 0.4rem 0.85rem;
    font-size: 0.83rem;
    font-weight: 500;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 8px;
    cursor: pointer;
  }
  .apply:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent);
  }
  .apply:disabled {
    opacity: 0.5;
    cursor: default;
  }
```

- [ ] **Step 4: Run** `cd ui && npm run check && npx vite build` — Expected: 0 errors, 0 warnings; build succeeds.

- [ ] **Step 5: Try against the fake** — `cargo run -p gglinuxd --example fakemouse` + `npm run tauri dev` (owner, optional): every control round-trips.

- [ ] **Step 6: Commit** `git commit -m "UI: headset noise, mic, power, radio and EQ controls"`

---

### Task 5: Docs

**Files:** Modify `docs/PROTOCOLS.md`, `PLAN.md`

- [ ] **Step 1:** `PROTOCOLS.md` setter table — ANC row → `| Noise mode | \`06 bd <0\|1\|2>\` | 0 off, 1 transparency, 2 ANC (loteran; status byte 10, event \`07 bd\`) |`; transparency → `1–10`; mic volume → `\`06 37 <1-10>\` | 1 = muted, 10 = 100% (loteran's shipped slider; the \`0xa0\` figure elsewhere is contradicted by it — confirm by ear)`; mic LED → `\`06 bf <1-10>\` | 1 is the minimum (HeadsetControl "off" = 1)`; auto-off → `\`06 c1 <0-6>\` | \`0\`=never, \`1\`=1m … \`6\`=60m`.
- [ ] **Step 2:** `PLAN.md` M2 progress paragraph — replace "Still to do for M2: the setters in the UI (sidetone, ANC, EQ, mic, auto-off), the ChatMix dial, …" with "The UI now also sets noise mode, transparency, sidetone, mic volume and LED, auto-off, 2.4 GHz mode and the on-device EQ (spec: `…/2026-09-27-headset-controls-design.md`). Still to do for M2: owner acceptance on hardware; the ChatMix dial moves to M3, which drives it; the base station's damaged knob is out of scope by the owner's choice."
- [ ] **Step 3:** Commit `git commit -m "docs: headset setting ranges from the references; M2 progress"`

---

### Task 6: Hardware acceptance (owner)

- [ ] Noise mode Off/Transparency/ANC from the UI → panel shows the new mode within 1 s; audible difference.
- [ ] Mute light 1 vs 10 → visibly different LED when muted. Auto-off and 2.4 GHz → panel shows them within 1 s.
- [ ] Sidetone Off vs High → own voice audible. Mic volume 1 vs 10 → recorded level differs (e.g. `pw-record` or a voice chat).
- [ ] EQ Flat vs Bass Boost vs a custom curve → audible.
