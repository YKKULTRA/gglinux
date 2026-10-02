# Smart Volume and games-only clips (M7e) — design

**Status:** decided 2026-10-02, revised after review the same day. The owner set the scope (Smart
Volume per channel with three levels, clips only while a game runs, and an honesty pass over
PLAN.md) and how Smart Volume is built: from a PipeWire builtin compressor or limiter if there is
one, otherwise from a LADSPA plugin that is written only when its file exists, taken out by the
startup drop-in upgrade if it disappears, with the UI asking for `swh-plugins` when it's missing.
The rest of the design is the author's, since the owner asked not to review specs. **Milestone:**
M7, part e: the gaps PLAN.md claimed but nothing built. **Builds on** Sonar (M3, not yet run
live), profiles (M7a) and clips (M7b).

**Revised:** the first draft built Smart Volume from PipeWire's EBU R128 meter instead, so that no
optional package was involved. That re-decided a trade-off the owner had already made, and review
found the leveller turns a sudden loud sound up, not down, for its first ~130 ms (see "Considered,
not built"). This design follows the owner's instructions: PipeWire has no dynamics
builtin (Facts), so it's swh's SC4 compressor behind the owner's guards.

## Goal

1. **Smart Volume**, like GG Sonar's: per output channel (Game, Chat, Media, Aux), off or one of
   **Quiet / Balanced / Loud**. It evens out a channel's loudness: quiet sounds come up by the
   level's makeup gain, and sounds above its threshold are compressed 4:1. Off by default. It
   switches on, off and between levels **instantly**, through the stage's own controls, never
   with a PipeWire restart.
2. **Games-only clips**: a "games only" setting for the replay buffer. While it's on, the recorder
   runs only while a program from the profiles' game rules is running. It starts when such a game
   appears and stops (with the usual `SIGINT`) after it exits. The Moments page shows when it's
   waiting for a game.
3. **PLAN.md tells the truth**: every row of the parity checklist says whether it's verified on
   hardware, built in software only, blocked, or skipped by the owner's decision.

**Hard constraint:** a channel's audio must never depend on an optional package. Today the
channel chains use only PipeWire's builtin filters. Smart Volume's compressor comes from
`swh-plugins`, which is optional, and a channel whose graph names a plugin PipeWire can't load
fails to load. The channel modules aren't `nofail` today, so PipeWire would not start at all
(Facts). So the stage is guarded (Design, "The guards"):
- it is written only when the plugin is where PipeWire looks for it;
- it is taken out when the plugin disappears: at startup, and within a second while gglinux runs;
- the channel modules that carry it are `nofail`, and a stage PipeWire refuses is taken out.

What's left, which the owner's instructions accept: if `swh-plugins` is removed while gglinux
isn't running, the next PipeWire start skips the channels until gglinux starts (Design, "The
guards").

Out of scope:
- event-based highlight capture (saving a clip on a kill, a death, a goal): no game here reports
  such events, and GG does it per title with its own integrations;
- per-game clip settings, and saving a clip automatically when a game exits;
- Smart Volume on the virtual mic (it has its own compressor) or on the stream mix;
- a peak limiter after the compressor, and user-tunable compressor settings;
- the headset's kernel backend (deferred; see PLAN.md).

## Facts (checked 2026-10-02 on this machine, and in PipeWire 1.6.9's source)

**PipeWire:**
- PipeWire is **1.6.9** (`pipewire 1:1.6.9-1`). PLAN.md's environment line still says 1.6.8.
- Its builtin filters (`libpipewire-module-filter-chain(7)`, and the plugin's strings) are: mixer,
  copy, the biquads, param_eq, convolver, delay, invert, clamp, linear, recip, abs, sqrt, exp, log,
  mult, sine, max, dcblock, ramp, debug, zeroramp, noisegate, busy and null. **There is no
  compressor, limiter or other dynamics builtin.** (Its EBU R128 plugin is a loudness meter; see
  "Considered, not built".)
- **The LADSPA loader** (`libspa-filter-graph-plugin-ladspa.so`) is in `pipewire-audio`, like the
  builtins. `plugin_ladspa.c` 1.6.9:
  - It looks for `<plugin>.so` in `LADSPA_PATH` if that is set, **instead of** its defaults, and
    otherwise in `/usr/lib64/ladspa`, `/usr/lib/ladspa` and `/usr/lib` (its `LIBDIR` here; the
    `.so`'s strings say `/usr/lib64/ladspa:/usr/lib/ladspa:/usr/lib`). Empty entries are skipped.
  - `mic::plugins_present` searches `LADSPA_PATH` and then `/usr/lib64/ladspa`, `/usr/lib/ladspa`
    and `/usr/local/lib/ladspa`. That doesn't match: it can find a plugin PipeWire won't load.
  - The plugin file is opened when the graph is loaded, in the module's init. A missing file fails
    `load_graph` ("failed to load plugin"), and with it the module.
  - The plugin's `activate` and `deactivate` are passed through as they are.
- **A module that fails to load:**
  - `pipewire.conf(5)` says `nofail` is "to not fail PipeWire startup if the module fails to
    load". Without it, a failed module in `context.modules` stops PipeWire from starting.
  - `module-filter-chain.c` 1.6.9 loads the graph in its module init
    (`pw_context_load_spa_handle(…, "filter.graph", …)`, failing on `NULL`).
  - filter-graph's `load_graph` fails on a plugin it can't load, an unknown label, an unknown link
    port ("unknown output port"), or an unknown `capture.volumes` control ("unknown control
    port").
  - The channel modules aren't `nofail` today: a graph PipeWire refuses would take **all audio**
    down, not just the channels. `config.rs` already makes the mic and every stream piece `nofail`
    for this reason.
  - Plugin instances are made later, when a channel starts playing (`activate_graph`). A failure
    there ("can't start graph") errors only that channel's stream; the node stays.
- **Links** (`filter-graph.c` 1.6.9): an output may feed several inputs, which all read its one
  buffer (`parse_link`, `port_ensure_data`). An input takes one link ("use a mixer").
- **`mixer`:** audio `In 1` … `In 8` → `Out`, controls `Gain 1` … `Gain 8` (default 1.0, range
  −10…10). `mixer_run` skips an input whose gain is 0. With one input left at gain 1.0, the mix
  is a copy, bit-for-bit, in the C, SSE and AVX2 versions of `mix_gain` (1.6.9; this CPU has
  AVX2).
  **`linear`:** `Out = In × Mult + Add`.
- **Graph inputs and outputs:**
  - With no explicit inputs and outputs, a graph uses the first node's inputs and the last
    node's outputs.
  - It is duplicated to match the stream's channels. Today's mono chain runs twice, once per side
    (`audio.channels = 2`).
- **Volume:**
  - Normally a filter-chain sink's volume is the stream's software volume, applied before the
    graph.
  - `capture.volumes = [ { control = "<node>:<port>" } ]` (inside `filter.graph`) moves it onto a
    control port instead. With one entry and a duplicated graph, FL's volume goes to instance 0
    and FR's to instance 1 (`sync_volume`, `i % n_hndl`).
  - The stream's own volume then stays at 1.0, and mute still mutes.
- **On a pause** (`playback_state_changed`, `PAUSED`), the filter chain calls `reset_graph`, which
  deactivates and re-activates every plugin instance.
- One realtime thread runs every channel (`context.num-data-loops` is 1). Here: `clock.rate
  48000`, `clock.quantum 1024`, `clock.min-quantum 32`. M3's acceptance is "xruns clean at
  quantum 512→256".
- `spa-json-dump` is at `/usr/bin`; a module using `ladspa`, `mixer` and `capture.volumes` parses.

**swh-plugins:**
- `swh-plugins` 0.4.17-7.1 is installed (explicitly; nothing requires it). Ports, read with ctypes
  from each `.so`'s `ladspa_descriptor`:
  - `sc4m_1916.so`, label `sc4m` (mono): `RMS/peak` 0…1, `Attack time (ms)` 1.5…400,
    `Release time (ms)` 2…800, `Threshold level (dB)` −30…0, `Ratio (1:n)` 1…20,
    `Knee radius (dB)` 1…10, `Makeup gain (dB)` 0…24; outputs `Amplitude (dB)` and
    `Gain reduction (dB)`; audio `Input`, `Output`. Properties: hard-RT capable. No `activate` or
    `deactivate`. The mic chain already runs it (`mic.rs`, behind `mic::plugins_present`).
  - `sc4_1882.so`, label `sc4` (stereo): the same controls; audio `Left/Right input/output`.
  - `hard_limiter_1413.so`, label `hardLimiter` (mono): `dB limit` −50…0, `Wet level` 0…1,
    `Residue level` 0…1; audio `Input`, `Output`. It clips at the limit.
  - `fast_lookahead_limiter_1913.so`, label `fastLookaheadLimiter` (stereo): `Input gain (dB)`,
    `Limit (dB)`, `Release time (s)`; output `Attenuation (dB)` and `latency`; audio
    `Input 1/2`, `Output 1/2`.
- **How `sc4m` works** (`sc4m_1916.xml` in swh/ladspa):
  - RMS over 64 × 4 samples (256 samples, 5.3 ms at 48 kHz); the envelope is updated every 4
    samples.
  - An attack under 2 ms means an instant envelope and no gain smoothing. Attack and release are
    rounded to steps of 1/256 s; 800 ms, the longest release, is a time constant of about 0.8 s.
  - dB conversions go through lookup tables, so a 1:1 ratio with no makeup isn't exact.
  - The gain starts at 0 when an instance is made.
  - With no `activate`, PipeWire's reset on a pause leaves its state alone.
- **What `sc4m` does**, measured 2026-10-02 by running it in-process with ctypes (no PipeWire),
  on noise at 48 kHz in blocks of 1024. Settings: RMS detection, attack 1.5 ms, release 800 ms,
  ratio 4:1, knee 6 dB.
  - **Off through the compressor alone** (ratio 1, makeup 0) isn't a passthrough. The gain is
    1.000011–1.000022 (+0.0001–0.0002 dB), and the first 3 samples after it's made are silent.
  - **Steady gain**, by the level's threshold / makeup:

  | Input (dBFS RMS) | −40 | −30 | −20 | −15 | −10 | −6 |
  |---|---|---|---|---|---|---|
  | Quiet (−30 / +3) | +3.0 | +1.5 | −5.2 | −9.0 | −12.7 | −15.7 |
  | Balanced (−24 / +6) | +6.0 | +6.0 | +2.3 | −1.5 | −5.2 | −8.2 |
  | Loud (−22 / +9) | +9.0 | +9.0 | +6.5 | +3.0 | −0.7 | −3.7 |

  - **A loud sound after a quiet stretch** (3 s at −35, then −10 dBFS RMS): the gain in its first
    milliseconds, against no Smart Volume:

  | After the onset | 0–1 ms | 1–3 ms | 3–5 ms | 5–10 ms | steady |
  |---|---|---|---|---|---|
  | Quiet | −1.0 | −8.9 | −11.2 | −12.1 | −12.7 |
  | Balanced | +3.9 | −1.4 | −3.7 | −4.6 | −5.2 |
  | Loud | +7.5 | +3.1 | +0.8 | −0.1 | −0.7 |

    The burst's peak in its first 10 ms is +0.5 dBFS; it comes out at +1.1 (Quiet), +5.1
    (Balanced) and +8.8 dBFS (Loud).
  - **After digital silence** (3 s): a quiet sound (−30) gets the level's makeup from its first
    sample (+3/+6/+9 dB), as it would at any time. A loud one (−10) starts at +4.3 dB on Balanced
    and +7.9 dB on Loud for its first millisecond, and is within 1 dB of its steady gain by
    5–10 ms.
  - **Distortion:** a 50 Hz sine at −12 dBFS RMS comes out with harmonics at −64 dB on every
    level. With the mic's `RMS/peak` 0.5 it would be about −44 dB, and −38 dB with peak
    detection.
  - **CPU:** 8.6 µs per 1024-sample block for one side on a P-core and 12.8 µs on an E-core
    (2.4 and 4.1 µs at 256; the ctypes call is included). With all four channels playing, that's
    eight instances: about 0.07–0.10 ms of a 21.3 ms cycle at quantum 1024, and 0.02–0.03 ms of
    5.3 ms at 256, under 1% either way. Memory is a few kilobytes per instance.

**Clips and profiles:**
- The profiles watcher scans `/proc` every 2 s, and only while rules exist or one is active.
- Its `pick()` returns the first rule whose program runs (and whose profile exists).
- Its `active` is set only once the lighting was actually applied, so it can't say "a game is
  running" when there's no keyboard.

**This machine's state:**
- `gpu-screen-recorder` isn't installed.
- There's no `sonar.json`, `clips.json` or Sonar drop-in, and `profiles.json` has no rules.
- So nothing restarts PipeWire here when this lands.
- Sonar's daemon ticks every second while Sonar is on (`gglinuxd/src/sonar.rs`, `TICK`).

**Kernel:**
- It runs 7.2.6-arch2-1.
- The `hid-steelseries` modules of every installed kernel (7.2.7-arch1, 7.2.7-zen1,
  7.2.8-cachyos) alias only `1038:12C2`, `12B6` and `1410`, **not `12E0`** (the Nova Pro).

## Design

### Smart Volume

**The stage** goes at the end of every channel's chain, after the channel EQ (`eq_10`). Its node
names start with `sv_`.

```text
        ┌→ sv_comp (sc4m) ──→ In 2 ┐
eq_10 ──┤                          sv_mix (mixer) → vol → outlet
        └──────────────────→ In 1 ─┘                  ↑ Mult = the sink's volume
```

- **`sv_comp`** is `sc4m_1916`, one per side: the graph is mono and runs once per side, so a mono
  compressor fits it as it is. Its fixed settings:
  - RMS detection, so it follows loudness rather than peaks, with little distortion (Facts);
  - attack 1.5 ms, which the plugin treats as instant; its 5 ms RMS window does the smoothing;
  - release 800 ms, the plugin's slowest, so quiet sounds come back up gently;
  - ratio 4:1 and a 6 dB knee.

  The level sets its threshold and makeup gain.
- **`sv_mix`** (`mixer`) takes the channel EQ's output on `In 1` and the compressor's on `In 2`.
  - **Off:** `Gain 1` = 1.0, `Gain 2` = 0.0. PipeWire's mixer skips the compressor and copies the
    EQ's output: an exact passthrough. (The compressor alone at 1:1 isn't one; see Facts.)
  - **On:** `Gain 1` = 0.0, `Gain 2` = 1.0.
  - The compressor runs either way, at the level's settings. So switching on lands on a
    compressor that has already settled, and switching costs no restart.
- **`vol`** (`linear`) is the **channel's volume**. `capture.volumes = [ { control = "vol:Mult" } ]`
  moves the sink's volume from before the chain to after the compressor. Otherwise the compressor
  would squash the volume slider and the keyboard's volume keys (Game is the default output):
  above the threshold, turning the volume down 12 dB would make it only 3 dB quieter. `vol` is the
  last node, so it is the graph's output.

**Levels:**

| Level | Threshold | Makeup | Quiet sounds | Unchanged at about | A −10 dBFS sound |
|---|---|---|---|---|---|
| Quiet | −30 dB | +3 dB | +3 dB | −26 dBFS | −12.7 dB |
| Balanced | −24 dB | +6 dB | +6 dB | −16 dBFS | −5.2 dB |
| Loud | −22 dB | +9 dB | +9 dB | −10 dBFS | −0.7 dB |

"Unchanged at" is where the gain crosses 0 dB (threshold + makeup × 4/3, ignoring the knee). The
last column is measured (Facts). Loud doesn't push sounds that are already loud past where they
were; it brings everything quieter up.

**Live controls:** one `pw-cli set-param <channel sink> Props` per channel, the same path and node
as the EQ, but a separate command:
`{ params = [ "sv_comp:Threshold level (dB)" t "sv_comp:Makeup gain (dB)" m "sv_mix:Gain 1" dry "sv_mix:Gain 2" wet ] }`.
Off keeps the level's threshold and makeup on the compressor. They're sent when Sonar comes up
(with the other levels, after any PipeWire restart) and on every change.

**The guards** (the hard constraint):
- **Only where PipeWire can load it:** `PwTools::smart_volume_supported()` looks for
  `sc4m_1916.so` exactly where PipeWire 1.6.9 does: `LADSPA_PATH` if it is set, else
  `/usr/lib64/ladspa`, `/usr/lib/ladspa` and `/usr/lib`. `mic::plugins_present` moves to the same
  lookup, so the mic and Smart Volume agree with PipeWire and with each other.
- **Without it,** the channels are written exactly as today: no stage, no `vol`, no
  `capture.volumes`, no `nofail`. The Sonar tab asks for `swh-plugins`, and `set_smart_volume` is
  refused.
- **At startup,** `expected_dropin` follows the same check, so `upgrade_dropin` adds the stage once
  the plugin exists, and takes it out if it disappeared (one PipeWire restart either way, as for
  every drop-in change).
- **While gglinux runs,** Sonar's one-second tick notices the plugin going. It rewrites the drop-in
  without the stage at once, so no later PipeWire start, gglinux's or anyone's, loads a plugin
  that isn't there. PipeWire keeps the copy it already loaded until then, so there's no restart
  while the channels are up. If they're already gone (PipeWire restarted in between), it restarts
  PipeWire once more to bring them back. A plugin installed while gglinux runs isn't picked up
  until Sonar is turned off and on, or gglinux restarts: adding the stage takes a restart. Until
  then the channels PipeWire runs have no stage, so Smart Volume shows as unavailable and
  `set_smart_volume` is refused. Whether they have it is read from the drop-in each time they come
  up, so `set_output` and `set_mic_input`, which write it without a restart, keep the stage out of
  it unless the channels already carry it.
- **What's left:** `swh-plugins` removed while gglinux isn't running. The next PipeWire start then
  skips the channel modules (they're `nofail`), and apps play to WirePlumber's fallback output,
  not through Sonar, until gglinuxd starts and takes the stage out with one restart. The owner's
  instructions accept this window.
- **The file is only a proxy.** A broken `.so`, another `swh-plugins` version's port names, or a
  `LADSPA_PATH` that PipeWire's environment doesn't share would still fail the graph. So:

**When PipeWire refuses the stage:**
- **The channel modules that carry the stage are `nofail`.** A refused graph then costs the
  channels, not PipeWire. With the Game sink gone, WirePlumber should fall back to another
  default output (not yet seen here), so apps still play, just not through Sonar. A
  drop-in without the stage stays byte-for-byte today's, so existing installs don't restart.
- **Missing channels mean a refusal** when the drop-in on disk carries the stage and PipeWire
  answers `pw-dump`, but the channels don't appear within the usual 5 s. It's checked:
  - after `enable()`'s restart;
  - after `set_streamer`'s restart;
  - on every startup with Sonar on, whether or not `upgrade_dropin` rewrote the file. A drop-in
    that already matches can still be refused by a PipeWire upgraded since.
- **Then:**
  - `sonar.json` records `smart_volume_refused: true`, first, so a crash can't bring the stage
    back;
  - the drop-in is rewritten without the stage;
  - PipeWire restarts once more.
  - In `enable()`, if the channels still don't come, the stage wasn't to blame: the change is
    rolled back as today, and the refusal is forgotten.
- **While refused:**
  - `expected_dropin` leaves the stage out, so no later rewrite brings it back;
  - `set_smart_volume` is refused;
  - the Sonar tab says why.
- **Turning Sonar off forgets a refusal**, so turning it on again tries the stage again. So does
  `enable()` itself, at its start, since a refusal on record while Sonar is off can only be stale
  (a crash mid-`enable()`). A refusal is only ever on record while Sonar is on.
- **A failure when a channel first plays** (instantiating the plugins) errors only that channel's
  stream. The node is there, so this check can't see it. Hardware acceptance step 1 plays into
  every channel to catch it.

**Settings** (`sonar.json`):
- `smart_volume: BTreeMap<Channel, SmartVolume>`, where a missing entry means off/Balanced;
- `smart_volume_refused: bool`, false by default;
- the struct's `#[serde(default)]` lets old files load.

**IPC:**
- `SmartLevel { quiet, balanced, loud }` (default `balanced`);
- `SmartVolume { enabled, level }` (`#[serde(default)]`);
- `SonarState.smart_volume: SmartVolumeState { available, reason: Option<String>, settings: Vec<SmartVolume> /* Channel::ALL order */ }`;
  `reason` says why it's unavailable: the compressor is missing (install `swh-plugins`, then turn
  Sonar off and on), PipeWire refused the stage (turn Sonar off and on to try again), or the
  channels were made before `swh-plugins` was installed (turn Sonar off and on to add it);
- the request `set_smart_volume { channel, settings }`.

**UI:**
- a **Smart Volume** section in the Sonar tab, under Channels: per channel an *On* box and a level
  menu, and the hint "quiet sounds come up, loud ones are held down; switching is instant";
- when it's unavailable, the daemon's `reason`, which asks for `swh-plugins` when that's missing.

**Trade-offs:**
- **Each side is compressed on its own.** The graph runs once per side, so a loud sound panned hard
  left turns the left side down more than the right: the stereo image shifts for a moment. The
  stereo `sc4` links the sides, but it needs a stereo graph: the whole chain rewritten, every node
  and every EQ `Props` name doubled. Not worth it for a leveller.
- **Onsets overshoot briefly.** A sound that starts after a quiet stretch, or after silence, plays
  at up to the level's makeup for its first 1–3 ms, and is within about 1 dB of its steady gain by
  5–10 ms (Facts). So a bang on Loud is up to 9 dB louder than with Smart Volume off for a couple
  of milliseconds, then settles: about as loud as with it off for a −10 dBFS bang, quieter for
  anything louder. Quiet sounds stay up by the makeup throughout: that's the point.
- **No peak limiter.** On Balanced and Loud, the first millisecond of a full-scale bang can go over
  0 dBFS (+5 and +9 dBFS in the measurement). The output's conversion clips it when the channel
  volume is near 100%; `vol` comes after the compressor, so a lower channel volume leaves room.
  `fast_lookahead_limiter_1913` (stereo, adds latency) or `hard_limiter_1413` (a clipper) could
  follow the compressor; not built.
- **Release is at most 800 ms.** A broadcast leveller recovers over seconds, so music with steady
  loud beats may pump a little.
- **Its state survives a pause** (no `activate`): a channel resuming after a pause starts with the
  gain it had, and settles within about a second.
- **CPU and memory** are negligible (Facts). The compressor runs while Smart Volume is off, to keep
  switching seamless.
- **The stream mix doesn't get Smart Volume:** it taps the sink before the chain, like the EQ.
- **The first start after this lands** rewrites the drop-in once (on a machine with Sonar on and
  `swh-plugins` installed).
- **The mic needs `swh-plugins` too**, so removing it takes out both; the drop-in rewrite drops
  both.

### Considered, not built: a leveller from PipeWire's EBU R128 meter

The first draft. It needs no optional package, since the meter ships in `pipewire-audio`.
- **What it was:** `ebur128` (a loudness meter) → `lufs2gain` on its `Momentary LUFS` (400 ms) and
  `Shortterm LUFS` (3 s) → the smaller of the two gains (`clamp`) → bounded per level (`clamp`)
  → applied (`linear`) → `vol`. Targets −26/−20/−15 LUFS, boosts of at most +6/+9/+12 dB.
- **Why not:**
  - **It isn't what the owner asked for.** It's a meter, not a dynamics builtin, and it re-decided
    a trade-off the owner had made.
  - **It turns onsets up, not down.** The momentary window holds 19,200 samples at 48 kHz, so in
    the first block of a louder sound the new level weighs only quantum/19,200. The step is
    underestimated by up to 10·log10(19,200/quantum): **12.7 dB at quantum 1024, 18.7 dB at
    256.**
    - Example, Balanced (target −20, at most +9): a quiet stretch at −35 LUFS sits at +9 dB. An
      explosion at −15 LUFS then plays at about **+7 dB in its first block**, where it would be
      0 dB with Smart Volume off. It's still above 0 dB about 130 ms later, and reaches its −5 dB
      steady state only after about 400 ms.
    - After digital silence it's worse: the loudness is −∞, so the gain is 1, and the next onset
      reads as 12.7 dB quieter than it is. Every sound starting from silence (a notification
      ping, each Chat talk-spurt) swells by up to the boost (+6/+9/+12 dB), then decays.
    - libebur128 zero-fills its buffer when a meter is made, and the filter chain resets the graph
      on every pause, which re-makes the meters. So every channel resume starts from silence too.
  - **It is expensive.** Every notify port is connected, so the meter runs all its
    modes, summing its 400 ms and 3 s windows from scratch every block: 0.12–0.14 ms per side per
    block on a P-core, 0.23–0.28 ms on an E-core, whatever the quantum (measured here against
    libebur128 1.2.6). With four channels, that's 18% / 36% of the cycle at quantum 256, paid with
    Smart Volume off. And about 16 MB of meter buffers per playing channel.
  - **It's cruder:** no attack, release or knee, and a gain that steps once per block.
- **A mitigation existed:** the meter's `Window LUFS` reports the loudness over `max-window`
  (libebur128 keeps its 3 s buffer for the short-term mode). With `max-window = 0.1`, a third
  `lufs2gain` and one more `clamp`, cuts land within about 100 ms, and the onset error falls to
  6.7 dB at quantum 1024, for about 4,800 more squared samples per block on top of 163,000.
- **If the owner ever wants Smart Volume without `swh-plugins`,** this is where to start. The
  settings, IPC, UI, live switching and refusal handling carry over; `smart.rs`'s nodes, links,
  props and check change.

### Games-only clips

**Settings:**
- `ClipsSettings.games_only: bool`, default false; `#[serde(default)]` already covers old
  `clips.json` files.

**Where the game comes from:** the profiles watcher, rather than a second scanner.
- Each scan publishes the process name of `pick()`'s rule (or `None`) on a
  `tokio::sync::watch::Sender<Option<String>>`.
- It publishes this before, and regardless of, the lighting change, so a missing keyboard doesn't
  hide a running game.
- The scan also runs while a game is still published, so removing the last rule clears it.
- `main.rs` creates the channel and hands the receiver to clips.

**The recorder** (`clips.rs`):
- It is *wanted* when `enabled && (!games_only || game.is_some())`. `start()` does nothing
  otherwise.
- **A game appears:** start (cancelling any pending stop).
- **The game is gone, while recording games-only:** stop after a **15 s grace**, with the usual
  `SIGINT`. The grace gives the owner time to save the game's last moments after quitting. It
  also bridges a launcher handing over to the game. If the game comes back within it, the same
  recorder carries on.
- **An unexpected exit while not wanted** (the game's gone) isn't retried, and a restart still
  pending when the game goes is dropped, with its "retrying" message.
- Changing the settings works as before (stop, then start if wanted). Turning `games_only` on
  with no game stops the recorder.

**IPC** (`ClipsState`):
- `game: Option<String>`, the game from the rules that's running now;
- `waiting: bool`: on and games-only, with nothing recording and no game.

**UI (Moments):**
- under the main switch, the checkbox **"Only while a game from your game rules runs"**;
- while waiting: "Waiting for a game from your game rules (Settings → Profiles)";
- while recording games-only: "Recording while *game* runs."

### PLAN.md, the README and the package

- Re-join M6's "**Accept:**" sentence, which the M6a/M6b notes split. M7's is whole and stays as
  it is.
- Rewrite the parity checklist with one status per row: **verified** (on the owner's hardware),
  **built** (software, tested against fakes), **parked**, **blocked** (M4b OLED, M5 actuation:
  they need a Windows GG USB capture), **deferred**, or **skipped** by the owner (PrismSync,
  virtual surround, CS2 GSI, Discord mute, firmware update, the 3D aim trainer, event-based
  highlights).
  - Built-in apps are the audio visualizer and the mic-mute light; the clock needs the blocked
    OLED.
  - Profiles are lighting profiles.
- An M7e progress note.
- The headset kernel-backend item is **deferred**:
  - no `hid-steelseries` supports `0x12e0` on this kernel (7.2);
  - hidraw keeps working after one lands;
  - code against an unseen sysfs layout would be untested guesswork.
- The README and the Arch package's `optdepends` say `swh-plugins` is also for Smart Volume.

## Testing

**Unit (gg-audio, with the PipeWire fake):**
- the props for every level, and off as an exact passthrough (the mixer at 1 and 0);
- the levels inside the plugin's ranges;
- the compressor found by file, and plugins looked for where PipeWire looks (`LADSPA_PATH`
  replacing the defaults);
- the stage's links only name its own nodes, and `vol` is the last node;
- the drop-in with and without the stage: placement after `eq_10` (two links out of it), four
  more links per channel, `sc4m_1916` the only LADSPA plugin in the channels, `capture.volumes` on
  `vol:Mult`, the channel modules `nofail` with it, and today's chain byte-for-byte without it;
- the drop-in parses with `spa-json-dump`;
- enabling sets every channel's Smart Volume;
- a change is saved and sent live, with no restart;
- without the compressor: the old chain, `available: false` with a reason naming `swh-plugins`,
  the request refused, no controls sent;
- the startup upgrade adds the stage when the compressor appears, and removes it when it
  disappears;
- the compressor removed while Sonar runs: the drop-in loses the stage at the next tick, with no
  restart while the channels are up, and one restart when they're already gone;
- a refused stage (the fake starts PipeWire without the channels when the drop-in carries
  `sv_comp`) is taken out, recorded, and reported:
  - when Sonar turns on;
  - after the startup upgrade adds it;
  - on a startup with it already on disk;
  - after a streamer-mode restart.
- turning Sonar off forgets a refusal; an `enable()` whose channels never come at all still rolls
  back, and forgets it too;
- an old `sonar.json` loads.

**Unit (gglinuxd):**
- the running game is published even when the lighting can't be changed;
- games-only waits, starts with the game, keeps recording through the grace, then stops with
  `SIGINT`;
- a game back within the grace keeps the same recorder;
- turning games-only on with no game stops the recorder;
- a crash after the game isn't retried;
- old `clips.json` files load.

**Checks:**
- `cargo test --workspace --all-targets`;
- `cargo clippy --workspace --all-targets` with no warnings;
- `npm run check` in `ui/`.

**Hardware (the owner, when Sonar is first tried live):** see the plan's acceptance list. It covers:
- whether PipeWire loads the stage, and plays through every channel;
- the volume slider and keys with Smart Volume on;
- how the levels sound, and onsets against Smart Volume off: a bang after a quiet passage on
  Loud, and a ping or a voice after silence on Chat;
- xruns at quantum 256;
- optionally, removing `swh-plugins` with Sonar on;
- games-only clips with a real game.
