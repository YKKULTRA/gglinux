# Built-in effects (M6b) — design

**Status:** decided 2026-09-29. The owner asked for game-independent effects and chose not to
review specs; these decisions are the author's. **Milestone:** M6, part b.

## Goal

Two effects that don't depend on any game:

1. **Audio visualizer**, a new base effect. The keys become a spectrum analyser of whatever is
   playing: columns left to right are frequency bands from bass to treble, and bars rise from the
   bottom row.
2. **Mic-mute light.** A chosen key lights (red by default) while the default microphone is muted
   in PipeWire, drawn on top of everything gglinux shows.

Out of scope:
- Discord's own mute: that needs Discord RPC with an OAuth app. Discord's mute doesn't reach
  PipeWire, so it isn't shown.
- CPU/GPU meters, a clock (the OLED is blocked), and per-app audio sources.

## Facts (checked 2026-09-29)

- `pw-record --raw -P '{ stream.capture.sink = true }' --format f32 --rate 48000 --channels 1 -`
  writes raw 32-bit float samples of the default output's monitor to stdout. Without `--raw` it
  prepends a Sun `.snd` header.
- `wpctl get-volume @DEFAULT_AUDIO_SOURCE@` prints `Volume: 1.00`, with ` [MUTED]` appended when
  the default source is muted.
- The layout's key centres are rows at y = 0.5 (F-row), 2, 3, 4, 5 and 6, and x spans 0–18.25 key
  units.

## Design

### Visualizer

**Settings:** `Effect::Visualizer { sensitivity }` (`{"kind":"visualizer","sensitivity":1.0}`),
with sensitivity in 0.25–4.0, validated. `animated` is true.

**Capture** (`keyboard/audio.rs`):
- A thread runs the `pw-record` command above, reading 1024-sample hops (about 21 ms). It runs only
  while the Visualizer is the current effect and gglinux is drawing, and is killed and reaped
  otherwise, the same lifecycle as the key reader.
- It keeps a sliding 2048-sample window, applies a Hann window and runs an FFT (`rustfft`).
- **Bands:** 18 log-spaced bands from 40 Hz to 16 kHz. Each band is the peak magnitude in its bins.
- **Level:** `(dB + 60) / 60`, clamped to 0..1. Sensitivity multiplies it when drawing
  (clamped again), so moving the slider needs no capture restart.
- **Smoothing:** a new level at or above the old one is taken as is (fast attack); a lower one
  becomes `max(new, old · 0.85)` per hop (falling bars).
- The latest 18 levels are shared with the keyboard worker through a mutex.
- If `pw-record` is missing or exits, the bars stay at zero, and it's retried every 2 s.
- Audio is processed in memory only and never stored. KDE may show a "recording" indicator while
  it runs, and the UI says so.

**Drawing (pure):**
- A key's band is `floor(x_centre / 18.25 · 18)`, clamped.
- Its row from the bottom is `5 − row`, where `row = round(max(y_centre − 1, 0))`.
- A key is lit if `row_from_bottom + 1 ≤ level · 6`. Lit keys take the hue for their height: green
  at the bottom, through yellow, to red at the top (`hsv((1 − row_from_bottom/5) / 3)`). Unlit keys
  are dark.
- Brightness applies as usual.

### Mic-mute light

**Settings:** `Lighting.mic_mute: MicMuteLight { enabled, key, color }`, with `#[serde(default)]`.
The default is off, the Pause key (`0x48`), and red. `key` must be on the layout (validated).

**Watching** (`keyboard/mic.rs`):
- A thread runs `wpctl get-volume @DEFAULT_AUDIO_SOURCE@` every 500 ms, while the light is enabled
  and lighting is on.
- It sends `Command::MicMuted(bool)` only when the state changes.
- Pure parse: muted if the output contains `[MUTED]`. A failed run counts as not muted and is
  logged once.

**Drawing:** while muted, the key shows its colour on top of everything, including GameSense. It's
part of gglinux lighting, so it isn't drawn while an overlay has taken the keyboard over with
lighting off.

### IPC and UI

- The Keyboard panel's effect picker gains **Audio**, with a sensitivity slider and a line that
  says audio is analysed live and never recorded.
- A **Mic mute light** section: on/off, the key (a dropdown of the layout's labels), and a colour.
- The fake daemon passes the new fields through.

## Testing

**Unit:**
- a 1 kHz sine lands in the band that contains 1 kHz and no other band is louder;
- silence gives all zeros;
- sensitivity scales levels;
- smoothing: fast attack, and a 0.85 decay per hop;
- drawing: the bottom row lights at a low level and the top only at full, colours go from green at
  the bottom to red at the top, and brightness applies;
- `wpctl` output parsing;
- the mute light draws on top of overlays;
- IPC JSON and validation (sensitivity bounds; a mute key that isn't on the layout).

**Hardware (the owner):** play music with the Audio effect on; mute the mic in KDE and watch the
Pause key light.
