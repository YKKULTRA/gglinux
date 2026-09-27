# Sonar EQ (M3b) — design

**Status:** approved in conversation 2026-09-27 ("yess").
**Builds on:** `2026-09-27-sonar-channels-design.md` (M3a).

## Goal

A parametric EQ per Sonar channel and a headphone correction shared by all channels, edited on a
live frequency-response curve with draggable bands, with AutoEQ presets importable by pasting a
`ParametricEQ.txt`. Success: dragging a band changes the sound within a fraction of a second; the
drawn curve is the one PipeWire applies; an AutoEQ file for the owner's headphones imports in one
step; everything survives a reboot.

## Facts (checked 2026-09-27)

- PipeWire 1.6.8's filter-graph builtin plugin provides `bq_lowshelf`, `bq_peaking`,
  `bq_highshelf` (controls `Freq`, `Q`, `Gain`) and `linear` (control `Mult`).
- All three biquads are RBJ cookbook filters with `alpha = sin(w0) / (2Q)` — shelves included
  (`spa/plugins/audioconvert/biquad.c`), the same Q convention AutoEQ uses. At 0 dB each is an
  exact passthrough.
- `libpipewire-module-filter-chain` takes the same `capture.props` / `playback.props` as the
  loopback M3a uses, so channel sinks and outlets keep their names.
- A filter's type is fixed by the graph; only its controls change at runtime
  (`pw-cli set-param <node> Props '{ params = [ "<node>:<control>" <value> … ] }'`).

## Design

**Graph (per channel, in the drop-in):** `pre` (`linear`, headphone preamp) → `hp_1` low shelf,
`hp_2`…`hp_11` peaking, `hp_12` high shelf (headphone correction) → `eq_1` low shelf, `eq_2`…`eq_9`
peaking, `eq_10` high shelf (channel EQ). 23 stages, stereo (`audio.channels = 2` duplicates the
mono graph). All start flat.

**Live control:** one `pw-cli set-param <channel sink> Props { params = [ … ] }` sets a channel's
whole chain (preamp as a linear multiplier, 22 bands × Freq/Q/Gain). Applied with the channel
volumes: on enable, on startup, whenever the channels reappear, and on every edit.

**Layouts and limits:** channel EQ = [low shelf, 8 × peaking, high shelf], gains ±12 dB; headphone
correction = [low shelf, 10 × peaking, high shelf], gains and preamp ±20 dB. Frequency 20 Hz–20 kHz,
Q 0.1–10. Band kinds must match the layout (the UI never changes them). Defaults: channel bands at
31, 62, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz (shelves Q 0.707, peaks Q 1); headphone shelves at
105 Hz / 10 kHz with peaks at the same ten centres.

**AutoEQ import:** `Preamp: X dB` and `Filter N: ON|OFF <PK|LSC|HSC> Fc F Hz Gain G dB Q Q` lines.
LSC → the low-shelf slot, HSC → the high-shelf slot, PK → the next of the ten peaking slots;
unused slots stay flat; OFF filters are skipped. Refused (with the line number): a second shelf of
the same kind, an eleventh peaking filter, any other filter type, values out of range, a file with
no filters. An optional name labels the correction.

**Settings:** `sonar.json` gains `eq` (per channel; missing = flat) and `headphone`.

**Upgrade from M3a:** on startup with Sonar on, if the drop-in differs from what this version
writes for the saved output, rewrite it and restart PipeWire once (logged).

**IPC:** `BandKind { low_shelf, peaking, high_shelf }`, `EqBand { kind, freq, q, gain }`,
`ChannelEq { channel, bands }`, `HeadphoneEq { preamp, bands, name }`; `SonarState` gains
`eq: [ChannelEq]` and `headphone`. Requests `set_channel_eq {channel, bands}`,
`set_headphone_eq {eq}`, `import_auto_eq {text, name}`.

**UI:** an EQ section in the Sonar tab with tabs Game / Chat / Media / Aux / Headphone. An SVG
curve (20 Hz–20 kHz log, ±limit dB) computed in TypeScript from the same RBJ formulas; each band is
a handle — drag for frequency and gain (0.5 dB steps), scroll for Q; updates are sent at most every
150 ms while dragging and once on release. "Reset gains" per tab. Headphone tab adds a preamp
slider, the correction's name, and an AutoEQ paste box with an optional name.

## Error handling

Invalid bands/preamp/AutoEQ text → error message, nothing stored or sent. A failing `pw-cli` →
error surfaced; the setting stays stored and is re-applied with the next level push.

## Testing

Unit: layouts and flat defaults validate; AutoEQ parsing (slots, OFF, every refusal); `props`
string (every stage, preamp multiplier); drop-in graph (23 stages, 22 links per channel, same chain
on all four); orchestration with the fake `PwTools` (enable sets every chain; channel EQ validated,
saved and set live; headphone EQ reaches all four; import names the correction; outdated drop-in
rewritten with one restart; current drop-in left alone).
Hardware (owner, with Sonar on): a band drag is audible; an AutoEQ import audibly changes the
headphones; the drawn curve matches a sweep by ear.
