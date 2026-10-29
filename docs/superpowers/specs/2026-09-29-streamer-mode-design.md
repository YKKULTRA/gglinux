# Streamer mode (M7c) — design

**Status:** decided 2026-09-29 by the author (the owner asked not to review specs). **Milestone:**
M7, part c. It builds on Sonar (M3), which **hasn't been verified live yet**. Turning streamer mode
on while Sonar is on restarts PipeWire, exactly like turning Sonar on. The UI warns about the
1–2 s gap, and the owner decides when.

## Goal

GG's Streamer mode: two mixes of the same channels.
- **Monitor** is what the owner hears: the existing path, with its channel volumes, ChatMix and
  headphone EQ.
- **Stream** is what the audience hears: a virtual device, **gglinux Stream**, that OBS captures.
  It has its own level per channel and a master level.

For example, the stream can carry the game loud and the Discord chat quiet, while the owner hears
the opposite.

Out of scope:
- a stream-only microphone path (OBS captures the mic, or gglinux Mic, directly);
- per-app stream routing;
- a separate stream EQ.

## Design

**PipeWire objects** (in the Sonar drop-in, only while streamer mode is on):
- **The stream sink:** `gglinux_stream`, a null audio sink (`adapter` / `support.null-audio-sink`,
  `Audio/Sink`, FL FR, `monitor.channel-volumes = true`), so its own volume, the master level,
  applies to what OBS records from **"Monitor of gglinux Stream"**.
- **One feed per channel:** a `libpipewire-module-loopback`.
  - Its capture side reads the channel sink's monitor (`stream.capture.sink = true`,
    `target.object = "gglinux_<c>"`, named `gglinux_<c>.stream_in`, passive, no fallback).
  - Its playback side plays into `gglinux_stream` (named `gglinux_<c>.stream`, passive, no
    fallback).
  - A sink's monitor carries the signal **before** its volume and before the filter chain. So the
    stream mix is independent of the owner's channel volumes, ChatMix and headphone correction.
    That's what a stream should get.
- **Stream levels:** each feed's playback-node volume (`wpctl set-volume`). The master level is the
  `gglinux_stream` volume.

**Settings** (`sonar.json`):
- `streamer: bool` (default false);
- `stream_volumes: BTreeMap<Channel, f32>` (default 1.0 each);
- `stream_master: f32` (default 1.0).

**Sonar API:**
- `set_streamer(enabled)`: saves it. If Sonar is on, it rewrites the drop-in and restarts
  PipeWire, then waits up to the usual appear timeout for `gglinux_stream` and applies the levels.
  If the stream nodes never appear, the channels keep working, and the stream shows as not
  present with a warning; it isn't rolled back.
- `set_stream_volume(channel, v)` and `set_stream_master(v)`: 0..=1, saved, and applied if present.
- The drop-in generator takes the flag, so `upgrade_dropin` keeps the file in step.
- `apply_levels` also applies the stream levels.

**IPC:**
- `SonarState.stream: StreamMix { enabled, present, volumes: Vec<f32> /* Channel::ALL order */, master: f32 }`;
- requests: `set_streamer { enabled }`, `set_stream_volume { channel, volume }`,
  `set_stream_master { volume }`.

**UI (Sonar tab):**
- a Streamer mode switch, with "turning this on or off restarts PipeWire (1–2 s of silence)";
- while on, a Stream slider beside each channel's volume, and a Stream master slider;
- the hint "In OBS, add an Audio Output Capture of *Monitor of gglinux Stream*";
- "not present" shown when PipeWire didn't create it.

## Testing

**Unit (gg-audio, with the existing PipeWire fake):**
- the drop-in has the null sink and four feeds exactly when streamer is on, with the right
  `target.object` values;
- the drop-in doesn't validate differently: a parse check with `spa-json-dump`, if installed, in a
  test that skips when it's missing;
- enabling streamer while Sonar is on writes the drop-in and restarts once;
- stream volumes and master are applied to the right node ids;
- routing never touches `.stream` / `.stream_in` nodes;
- stream levels are re-applied after a PipeWire restart;
- streamer while Sonar is off just saves.

**Hardware (the owner, when Sonar is first tried live):**
1. Turn Sonar on, then streamer mode.
2. In OBS, capture "Monitor of gglinux Stream" and set Chat to 0 in the stream: the recording lacks
   Discord, but the owner still hears it.
