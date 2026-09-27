# Headset controls (M2 remainder) — design

**Status:** approved in conversation 2026-09-27.
**Milestone:** completes M2's control surface on top of the headset worker from
`2026-09-27-headset-screen-protection-design.md`.

## Goal

Every setting the Arctis Nova Pro Wireless base station has is changeable from gglinux, so its
damaged knob and on-device menu never need to be used. Success: each setting below can be changed
from the UI; the device-reported ones show the device's value within a second; the rest show what
gglinux last wrote, labelled as such.

Out of scope: Bluetooth settings, line-out routing, ChatMix (M3 drives it), analog gain, anything
about the base-station knob.

## Settings and their wire encoding

| Setting | Values | Command | Read back? |
|---|---|---|---|
| Noise mode | off / transparency / ANC | `06 bd <0\|1\|2>` | yes — status byte 10, event `07 bd` |
| Transparency level | 1–10 | `06 b9 <lvl>` | yes — status byte 8, event `07 b9` |
| Sidetone | off / low / medium / high | `06 39 <0-3>` | **no** |
| Mic volume | 1–10 (1 = muted) | `06 37 <lvl>` | **no** |
| Mic-mute LED | 1–10 | `06 bf <lvl>` | yes — status byte 11 |
| Auto-off | never / 1 / 5 / 10 / 15 / 30 / 60 min | `06 c1 <0-6>` | yes — status byte 12 |
| 2.4 GHz mode | speed / range | `06 c3 <0\|1>` | yes — status byte 13 |
| EQ preset | Flat / Bass Boost / Focus / Smiley / Custom | `06 2e <0-4>` | **no** |
| Custom EQ | 10 bands, −10…+10 dB, 0.5 dB steps | `06 2e 04` then `06 33 <b0..b9>` | **no** |
| Mic mute (display only) | muted / unmuted | — (headset button) | yes — status byte 9 |

Every change is followed by `06 09` (save), as HeadsetControl does, so settings survive a power
cycle. UI controls apply on release, so this is one flash write per user action.

### Corrections to the crate (from the references, 2026-09-27)

- **Auto-off `0` = never** exists (HeadsetControl "Disabled", loteran `never`); add `AutoOff::Never`.
- **Mic LED is 1–10**; both references treat 1 as the minimum ("off" = 1 in HeadsetControl). The
  crate currently accepts 0.
- **Mic volume is 1–10.** loteran's shipped slider uses `0x01..0x0a`; the `0xa0` maximum in our
  crate and `PROTOCOLS.md` came from a comment in the same file that its own UI contradicts. Not
  readable back, so this is confirmed by ear during acceptance.
- **Noise mode** is a typed enum (`0` off, `1` transparency, `2` ANC), not a raw byte.
- **Transparency** is validated to 1–10.

## Design

### `gg-device-headset`

- `NoiseMode { Off, Transparency, Anc }` with `code()`/`from_code()`; `AutoOff::Never`.
- `pub enum Setting { NoiseMode(NoiseMode), Transparency(u8), Sidetone(u8), MicVolume(u8),
  MicLed(u8), AutoOff(AutoOff), WirelessMode(WirelessMode), EqPreset(u8), EqBands(Vec<f32>) }`
  with `fn packets(&self) -> Result<Vec<Vec<u8>>>` — pure, validates, and returns every command
  the setting needs (EQ bands = select Custom + bands). Unit-tested byte-for-byte.
- `Headset::apply(&self, setting: &Setting) -> Result<()>` sends `packets()` then save.
- `Status` gains accessors `noise_mode() -> Option<NoiseMode>` and `mic_muted() -> Option<bool>`;
  the raw fields stay (the wake rules compare them).

### IPC (`gg-ipc`)

```rust
#[serde(tag = "setting", content = "value", rename_all = "snake_case")]
pub enum HeadsetChange {
    NoiseMode(NoiseMode), Transparency(u8), Sidetone(u8), MicVolume(u8), MicLed(u8),
    AutoOffMinutes(u16), RadioMode(RadioMode), EqPreset(u8), EqBands(Vec<f32>),
}
Request::SetHeadset { change: HeadsetChange } -> Success | Error
```

`gg-ipc` keeps its own `NoiseMode { Off, Transparency, Anc }` and `RadioMode { Speed, Range }`
(it is a leaf crate with no device dependency, as with `DpiStage`).

`HeadsetState` gains `controls: HeadsetControls`:

```rust
pub struct HeadsetControls {
    // Read from the base station's status every second; None until known.
    pub noise_mode: Option<NoiseMode>,
    pub transparency: Option<u8>,
    pub mic_led: Option<u8>,
    pub mic_muted: Option<bool>,
    pub auto_off_minutes: Option<u16>,
    pub radio_mode: Option<RadioMode>,
    // Written by gglinux this session. The device cannot report these.
    pub sidetone: Option<u8>,
    pub mic_volume: Option<u8>,
    pub eq_preset: Option<u8>,
    pub eq_bands: Option<Vec<f32>>,
}
```

`HeadsetState` drops `Eq` (keeps `PartialEq`) because of the `f32` bands.

### Worker

A pure `headset/controls.rs::setting_for(&HeadsetChange) -> Result<Setting, String>` maps IPC to
the device type (tested). `Command::SetControl { change, reply }`: no device → error
"base station not connected"; else `apply`, record the write-only values, feed the screen policy an
`OurEvent` (the change is shown for the lit period), force the next poll now so read-back values
refresh, and publish.

### UI

`HeadsetPanel` gains four sections: **Noise control** (segmented Off / Transparency / ANC;
transparency slider, shown in transparency mode), **Microphone** (sidetone segmented; mic volume
and LED sliders; a read-only muted/unmuted line), **Power & radio** (auto-off select; speed/range
segmented), **Equalizer** (preset segmented; with Custom, ten vertical sliders and an Apply
button). Write-only values carry a "last set here" note; before any write they read "unknown".

## Testing

- Unit: `Setting::packets` for every variant including range rejections; `AutoOff::Never` round
  trip and status decode of code 0; `setting_for` mapping and its rejections; IPC JSON for each
  `HeadsetChange` variant.
- Hardware (owner): noise mode and LED/auto-off/radio visible in the panel within 1 s of a change;
  sidetone, mic volume (1 vs 10), and EQ presets/custom audible.
