# Smart Volume and games-only clips (M7e) — design

**Status:** decided 2026-10-02. The owner set the scope (Smart Volume per channel with three
levels, clips only while a game runs, and an honesty pass over PLAN.md); the design is the
author's, since the owner asked not to review specs. **Milestone:** M7, part e: the gaps PLAN.md
claimed but nothing built. **Builds on** Sonar (M3, not yet run live), profiles (M7a) and clips
(M7b).

## Goal

1. **Smart Volume**, like GG Sonar's: per output channel (Game, Chat, Media, Aux), off or one of
   **Quiet / Balanced / Loud**. It evens out a channel's loudness: quiet stretches come up, sudden
   loud sounds go down. Off by default. It switches on, off and between levels **instantly**,
   through the stage's own controls, never with a PipeWire restart.
2. **Games-only clips**: a "games only" setting for the replay buffer. While it's on, the recorder
   runs only while a program from the profiles' game rules is running. It starts when such a game
   appears and stops (with the usual `SIGINT`) after it exits. The Moments page shows when it's
   waiting for a game.
3. **PLAN.md tells the truth**: every row of the parity checklist says whether it's verified on
   hardware, built in software only, blocked, or skipped by the owner's decision.

**Hard constraint:** a channel's audio must never depend on an optional package. Today the
channel chains use only PipeWire's builtin filters. If Smart Volume needed a plugin that is
missing (or uninstalled later), the channel's filter chain would fail to load and the channel
would go silent.

Out of scope:
- event-based highlight capture (saving a clip on a kill, a death, a goal): no game here reports
  such events, and GG does it per title with its own integrations;
- per-game clip settings, and saving a clip automatically when a game exits;
- Smart Volume on the virtual mic (it has its own compressor) or on the stream mix;
- a true look-ahead limiter, and user-tunable targets;
- the headset's kernel backend (deferred; see PLAN.md).

## Facts (checked 2026-10-02 on this machine, and in PipeWire 1.6.9's source)

**PipeWire:**
- PipeWire is **1.6.9** (`pipewire 1:1.6.9-1`). PLAN.md's environment line still says 1.6.8.
- Its builtin filters (`libpipewire-module-filter-chain(7)`, and the plugin's strings) are: mixer,
  copy, the biquads, param_eq, convolver, delay, invert, clamp, linear, recip, abs, sqrt, exp, log,
  mult, sine, max, dcblock, ramp, debug, zeroramp, noisegate, busy and null. **There is no
  compressor, limiter or other dynamics builtin.**
- There is an **EBU R128 plugin** (`type = ebur128`), with two filters:
  - `ebur128`, a loudness meter. It has seven audio inputs (`In FL`, `In FR`, … `In DUAL MONO`),
    each passed through to the matching `Out …`. Its control outputs include `Momentary LUFS`
    (a 400 ms window) and `Shortterm LUFS` (3 s). It takes a `config` object with
    `max-history`, `max-window` and `use-histogram`.
  - `lufs2gain`: control inputs `LUFS` and `Target LUFS` (default −23, range −70…0), and a
    control output `Gain` = 10^((target − LUFS)/20). Source: `plugin_ebur128.c` at tag 1.6.9.
    The gain is **unbounded**, and 1.0 when `LUFS` isn't finite. libebur128 1.2.6 reports
    −∞ for digital silence.
- **Packaging:**
  - `/usr/lib/spa-0.2/filter-graph/libspa-filter-graph-plugin-ebur128.so` belongs to
    **`pipewire-audio` 1:1.6.9-1**, the same package as `libspa-filter-graph-plugin-builtin.so`,
    which the channels already need.
  - It links `libebur128.so.1` (`libebur128` 1.2.6), a hard dependency of `pipewire-audio`
    (`Required By: pipewire-audio`; it is optional for nothing).
  - So a stage built from it adds **no optional package**.
- **Links and controls** (`filter-graph.c` 1.6.9):
  - A link may join a control output (a "notify" port) to a control input; `parse_link` falls
    back to control ports.
  - A linked control input reads the upstream value of the same instance.
  - Links count as ordering dependencies, so meter → gain → apply all run within one block.
  - Values set through `Props` are clamped to the port's range; linked values are not.
- **Every notify port of every node is connected.** So `ebur128` turns on all its modes,
  loudness range and true peak included. In libebur128 1.2.6, the loudness range without
  `use-histogram` mallocs and sorts on every call, which would mean in the audio thread, every
  block. With `use-histogram = true` it doesn't.
- **Memory:** libebur128 keeps a 3 s buffer of doubles per meter state, ≈1.15 MB at 48 kHz. The
  plugin makes seven states per instance, one per input, used or not. Four channels × two sides
  come to about **65 MB more in the PipeWire process**.
- **`clamp`:** audio `In`/`Out`, plus a control `Control` → `Notify`, both clamped to `Min`/`Max`
  (port range −100…100). **`linear`:** `Out = In × Mult + Add` (Mult −10…10), also for
  `Control` → `Notify`.
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
- `spa-json-dump` is at `/usr/bin`; a module using these nodes, `config` and `capture.volumes`
  parses.

**The LADSPA alternative, rejected:**
- `swh-plugins` 0.4.17-7.1 is installed, with `sc4_1882.so` and `fast_lookahead_limiter_1913.so`.
  Their ports, read with ctypes:
  - `sc4`: `RMS/peak`, `Attack time (ms)`, `Release time (ms)`, `Threshold level (dB)` −30…0,
    `Ratio (1:n)` 1…20, `Knee radius (dB)`, `Makeup gain (dB)` 0…24, outputs `Amplitude (dB)`,
    `Gain reduction (dB)`; audio `Left/Right input/output`.
  - `fastLookaheadLimiter`: `Input gain (dB)`, `Limit (dB)`, `Release time (s)`; audio
    `Input 1/2`, `Output 1/2`.
- Both are better dynamics processors than a leveller made from builtins. But `swh-plugins` is
  optional.
- PipeWire reads the drop-in when it starts, which at login is before gglinuxd can take a missing
  plugin out. A user who uninstalled it would get silent channels until the daemon ran. That
  breaks the hard constraint, so it isn't used.

**Clips and profiles:**
- The profiles watcher scans `/proc` every 2 s, and only while rules exist or one is active.
- Its `pick()` returns the first rule whose program runs (and whose profile exists).
- Its `active` is set only once the lighting was actually applied, so it can't say "a game is
  running" when there's no keyboard.

**This machine's state:**
- `gpu-screen-recorder` isn't installed.
- There's no `sonar.json`, `clips.json` or Sonar drop-in, and `profiles.json` has no rules.
- So nothing restarts PipeWire here when this lands.

**Kernel:**
- It runs 7.2.6-arch2-1.
- The `hid-steelseries` modules of every installed kernel (7.2.7-arch1, 7.2.7-zen1,
  7.2.8-cachyos) alias only `1038:12C2`, `12B6` and `1410`, **not `12E0`** (the Nova Pro).

## Design

### Smart Volume

**The stage** goes at the end of every channel's chain, after the channel EQ (`eq_10`). It uses
only `ebur128` and builtins. Its node names start with `sv_`.

```text
eq_10 → sv_meter ───────────────────────────────────────→ sv_gain → vol → outlet
          ├ Momentary LUFS → sv_fast (lufs2gain) ┐           ↑ Mult    ↑ Mult = the sink's volume
          └ Shortterm LUFS → sv_slow (lufs2gain) ┴→ sv_pick → sv_range
                                                   (clamp)    (clamp)
```

- **`sv_meter`** (`ebur128`, `use-histogram = true`) measures the side it's on (`In FL`) and
  passes the audio through.
- **`sv_fast` and `sv_slow`** turn the 400 ms and the 3 s loudness into the gain that would meet
  the level's target.
- **`sv_pick`** is a `clamp` with `Min` 0, whose `Max` is linked to `sv_slow:Gain`. That gives
  min(fast gain, slow gain):
  - a sudden loud sound pulls the fast gain down at once (the window includes the current
    block, and loud energy dominates it);
  - a quiet stretch is raised only as fast as the 3 s loudness lets the slow gain rise.

  So it's fast down, slow up: what a leveller needs, from parts without attack and release.
- **`sv_range`** (`clamp`) bounds the result. Its bounds are the on/off switch and the strength:
  - **off:** `Min = Max = 1`, an exact passthrough (× 1.0);
  - **on:** `Min` = −24 dB, `Max` = the level's boost.
- **`sv_gain`** (`linear`) applies the gain to the audio (`Mult` is linked).
- **`vol`** (`linear`) is the **channel's volume**. `capture.volumes = [ { control = "vol:Mult" } ]`
  moves the sink's volume from before the chain to after the leveller. Otherwise the leveller
  would undo the volume slider and the keyboard's volume keys (Game is the default output). `vol`
  is the last node, so it is the graph's output.

**Levels:**

| Level | Target | Most boost | Most cut |
|---|---|---|---|
| Quiet | −26 LUFS | +6 dB | −24 dB |
| Balanced | −20 LUFS | +9 dB | −24 dB |
| Loud | −15 LUFS | +12 dB | −24 dB |

**Live controls:** one `pw-cli set-param <channel sink> Props` per channel, the same path and node
as the EQ, but a separate command:
`{ params = [ "sv_fast:Target LUFS" t "sv_slow:Target LUFS" t "sv_range:Min" lo "sv_range:Max" hi ] }`.
They're sent when Sonar comes up (with the other levels, after any PipeWire restart) and on every
change.

**Only when PipeWire has the meter:**
- `PwTools::smart_volume_supported()` looks for `filter-graph/libspa-filter-graph-plugin-ebur128.so`
  under `SPA_PLUGIN_DIR`, then `/usr/lib64/spa-0.2`, `/usr/lib/spa-0.2` and `/usr/local/lib/spa-0.2`.
- If it's missing, the channels are written exactly as today: no stage, no `vol`, no
  `capture.volumes`. The Sonar tab says Smart Volume is unavailable, and `set_smart_volume` is
  refused.
- `expected_dropin` follows the same check, so the startup `upgrade_dropin` adds the stage once
  the meter exists, and takes it out if it disappears (one PipeWire restart either way, as for
  every drop-in change).
- On a PipeWire whose plugins live elsewhere, the check fails safe: no stage.

**Settings** (`sonar.json`):
- `smart_volume: BTreeMap<Channel, SmartVolume>`, where a missing entry means off/Balanced;
- the struct's `#[serde(default)]` lets old files load.

**IPC:**
- `SmartLevel { quiet, balanced, loud }` (default `balanced`);
- `SmartVolume { enabled, level }` (`#[serde(default)]`);
- `SonarState.smart_volume: SmartVolumeState { available, settings: Vec<SmartVolume> /* Channel::ALL order */ }`;
- the request `set_smart_volume { channel, settings }`.

**UI:**
- a **Smart Volume** section in the Sonar tab, under Channels: per channel an *On* box and a level
  menu, and the hint "evens out a channel's loudness; switching is instant";
- without the meter, it names the missing file and `pipewire-audio`.

**Trade-offs:**
- **Each side levels on its own.** The graph runs once per side, so a loud sound panned hard left
  turns the left side down more than the right: the stereo image shifts for a moment. A linked
  stereo meter would need the whole chain rewritten as a stereo graph (every node and every EQ
  `Props` name doubled). Not worth it for a leveller.
- **The gain steps once per block** (a quantum, ~21 ms at 1024/48 kHz) with no smoothing, so fast
  changes may be audible as a slight roughness. The slow side keeps boosts gentle; cuts are
  quick on purpose.
- **No peak limiter.** Transients shorter than a block pass at the gain of that block. *Loud*
  can clip peaks that were already near 0 dBFS.
- **Noise floor.** A quiet but not silent stretch is raised by at most the level's boost (6–12 dB),
  never more. Digital silence isn't raised at all (gain 1).
- **Cost:** about 65 MB more in PipeWire (Facts), and some CPU per side. It's paid whether Smart
  Volume is on or off, because toggling must not restart PipeWire.
- **The stream mix doesn't get Smart Volume:** it taps the sink before the chain, like the EQ.
- **The first start after this lands** rewrites the drop-in once (on a machine where Sonar is on).

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
- **An unexpected exit while not wanted** (the game's gone) isn't retried.
- Changing the settings works as before (stop, then start if wanted). Turning `games_only` on
  with no game stops the recorder.

**IPC** (`ClipsState`):
- `game: Option<String>`, the game from the rules that's running now;
- `waiting: bool`: on and games-only, with nothing recording and no game.

**UI (Moments):**
- under the main switch, the checkbox **"Only while a game from your game rules runs"**;
- while waiting: "Waiting for a game from your game rules (Settings → Profiles)";
- while recording games-only: "Recording while *game* runs."

### PLAN.md

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

## Testing

**Unit (gg-audio, with the PipeWire fake):**
- the props for every level, and off as an exact passthrough;
- the meter found by file;
- the stage's links only name its own nodes, and `vol` is the last node;
- the drop-in with and without the stage: placement after `eq_10`, nine links per channel,
  `capture.volumes` on `vol:Mult`, and today's chain unchanged without it;
- the drop-in parses with `spa-json-dump`;
- enabling sets every channel's Smart Volume;
- a change is saved and sent live, with no restart;
- without the meter: the old chain, `available: false`, the request refused, no controls sent;
- the startup upgrade adds the stage when the meter appears, and removes it when it disappears;
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

**Hardware (the owner, when Sonar is first tried live):** see the plan's acceptance list. It covers
whether PipeWire loads the stage, the volume slider and keys with the volume moved, how the levels
sound (pumping, roughness), PipeWire's memory, and games-only clips with a real game.
