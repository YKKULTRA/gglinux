# Sonar mic chain (M3c) — design

**Status:** approved in conversation 2026-09-27 ("i approve"); the owner installed
`noise-suppression-for-voice` 1.21 and `swh-plugins` 0.4.17 for it.
**Builds on:** M3a channels, M3b EQ.

## Goal

A virtual mic, **gglinux Mic**, that every app records from while Sonar is on: the physical mic
through RNNoise noise suppression, a noise gate, and a compressor, each switchable and adjustable
live from the Sonar tab. Success: background noise drops out of a call with suppression on and
comes back with it off, instantly; the gate silences the mic between words; nothing about the
output channels depends on the mic chain loading.

## Facts (checked 2026-09-27 on this machine / PipeWire source)

- `librnnoise_ladspa.so` label `noise_suppressor_mono`: controls `VAD Threshold (%)` 0–99,
  `VAD Grace Period (ms)`, `Retroactive VAD Grace (ms)`, **`Dry Mix` 0–1** (a built-in bypass).
- `sc4m_1916.so` label `sc4m`: `RMS/peak`, `Attack time (ms)`, `Release time (ms)`,
  `Threshold level (dB)` −30–0, `Ratio (1:n)` 1–20, `Knee radius (dB)`, `Makeup gain (dB)` 0–24.
- PipeWire's builtin `noisegate`: `Open Threshold` / `Close Threshold` are linear amplitude 0–1
  (defaults 0.04 / 0.03); `Attack (s)`, `Hold (s)`, `Release (s)`; `Level` is a control port whose
  default (NaN) makes the gate measure its own input. A threshold of 0 keeps it open.
- `flags = [ nofail ]` on a module makes PipeWire skip it, instead of failing, if it can't load.

## Design

**Graph** (a fifth module in the Sonar drop-in, only written when both plugins are installed):
mono filter chain `rnnoise → gate → comp`; capture side `gglinux_mic.in` reads the chosen physical
mic (`target.object`, `node.passive`, **`node.dont-fallback`** — it must never fall back to the
default source, which is itself); playback side `gglinux_mic`, `Audio/Source`. `nofail`.

**Live controls** (`pw-cli set-param` on the filter's capture node, like the channels):
noise suppression on = `Dry Mix` 0, off = 1; strength = `VAD Threshold (%)` 0–95; gate on =
open threshold 10^(dB/20) (−80…−10 dB), close = 0.7 × open; off = both 0; compressor fixed at
−18 dB, knee 3 dB, 5 ms / 100 ms, on = ratio 4:1 + makeup 0–18 dB, off = 1:1 and 0 dB. Mic volume
is the virtual source's volume. Defaults: suppression on at 50 %, gate off at −45 dB, compressor
off with 6 dB makeup.

**Lifecycle:** enabling Sonar picks the mic (saved choice if present, else the current default
input if physical, else the first physical input), remembers the default input, writes the mic
module, and after PipeWire restarts makes gglinux Mic the default input. If the channels come up
but the mic doesn't, Sonar stays on without it (logged). Disabling restores the previous default
input. Plugins installed after Sonar was enabled are picked up by the startup drop-in upgrade.
Changing the input re-targets the capture stream live and rewrites the drop-in (no restart).

**Settings:** `mic_input`, `previous_default_source`, `mic` (the switches and levels),
`mic_volume`. **IPC:** `AudioOutput` becomes `AudioDevice` (used for inputs too);
`MicSettings`, `MicState { available, present, input, inputs, volume, settings }` in
`SonarState.mic`; requests `set_mic_input {name}`, `set_mic_settings {settings}`,
`set_mic_volume {volume}`. **UI:** a Microphone section in the Sonar tab; without the plugins it
says which packages to install.

**Docs:** README lists the two packages as optional dependencies (only the mic chain needs them).

## Testing

Unit: module text (nofail, mono, dont-fallback, labels, links), live props for every on/off
state, ranges, plugin detection by file; graph reads physical inputs and the default input;
drop-in includes the mic only when asked; orchestration with the fake (mic installed and made
default; no plugins → channels still fine and `available: false`; settings validated/saved/live;
volume; disable restores the default input; upgrade adds the mic once plugins exist).
Hardware (owner): record with suppression on/off; gate between words; compressor evens levels.
