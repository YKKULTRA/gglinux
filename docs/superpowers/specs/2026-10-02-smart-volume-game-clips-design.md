# Smart Volume and games-only clips (M7e) — design

**Status:** decided 2026-10-02. The owner set the scope (Smart Volume per channel with three
levels, clips only while a game runs, and an honesty pass over PLAN.md); the design is the
author's, since the owner asked not to review specs. **Milestone:** M7, part e: the gaps PLAN.md
claimed but nothing built. **Builds on** Sonar (M3, not yet run live), profiles (M7a) and clips
(M7b).

**Where this departs from the owner's instructions.**
- **What the owner said:** use a builtin compressor or limiter if PipeWire has one. If not, use a
  LADSPA plugin, written only when its file exists, taken out by the startup drop-in upgrade if it
  disappears, with the UI asking for `swh-plugins` when it's missing.
- **What this design does instead:** PipeWire has no such builtin (Facts), and this design doesn't
  take the LADSPA path. It builds a loudness leveller from PipeWire's EBU R128 plugin.
- **Why: the hard constraint.**
  - That plugin ships in `pipewire-audio` with the builtins the channels already need, so it can't
    be missing while they work.
  - A LADSPA plugin can be uninstalled, and PipeWire reads the drop-in at login, before gglinuxd
    can take the stage out.
  - A file check has that race whatever the plugin. It's moot here only because the plugin comes
    from the same package as the builtins.
- **What it gives up against `sc4m_1916`** (a mono SC4 per side):
  - real attack and release times;
  - a gain that moves smoothly instead of stepping once per block;
  - a knee;
  - a CPU cost close to nothing. This one costs about 1–2 ms of every cycle with all four channels
    playing (Facts, Trade-offs).
- **So it's on trial.** Hardware acceptance is a go/no-go. A no-go builds the owner's LADSPA path
  after all (Design, "The owner's LADSPA fallback").

## Goal

1. **Smart Volume**, like GG Sonar's: per output channel (Game, Chat, Media, Aux), off or one of
   **Quiet / Balanced / Loud**. It evens out a channel's loudness: quiet stretches come up, sudden
   loud sounds go down. Off by default. It switches on, off and between levels **instantly**,
   through the stage's own controls, never with a PipeWire restart. Its sound and its CPU cost
   are a go/no-go on the owner's hardware (Status).
2. **Games-only clips**: a "games only" setting for the replay buffer. While it's on, the recorder
   runs only while a program from the profiles' game rules is running. It starts when such a game
   appears and stops (with the usual `SIGINT`) after it exits. The Moments page shows when it's
   waiting for a game.
3. **PLAN.md tells the truth**: every row of the parity checklist says whether it's verified on
   hardware, built in software only, blocked, or skipped by the owner's decision.

**Hard constraint:** a channel's audio must never depend on an optional package. Today the
channel chains use only PipeWire's builtin filters. If Smart Volume needed a plugin that is
missing (or uninstalled later), the channel's filter chain would fail to load. The channel
modules aren't `nofail` today, so PipeWire would not start at all (Facts). The ones that carry
Smart Volume's stage will be.

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
  block. With `use-histogram = true` it doesn't. That only fixes the allocating, not the cost
  below.
- **Only the linked input is measured.** `ebur128` accepts null data, so its unlinked inputs stay
  NULL and `run()` skips them. Each instance measures one side (`In FL`): eight meters with four
  channels × two sides.
- **Memory:**
  - libebur128 keeps a 3 s buffer of doubles per meter state, ≈1.15 MB at 48 kHz.
  - The plugin makes seven states per instance, one per input, used or not.
  - It makes them when the graph is activated, which is when a channel starts playing
    (`playback_state_changed` in `module-filter-chain.c`). It frees them when the channel's
    playback stream loses its format (`param_changed`), as it does when the channel is suspended.
  - That's about **16 MB per playing channel**, and **65 MB with all four playing**.
  - It scales with the rate: ×2 at 96 kHz, and ×4 at 192 kHz (≈260 MB). This PipeWire runs at
    48 kHz only (`clock.allowed-rates [ 48000 ]`).
- **The meter's cost per block** (measured 2026-10-02 on this machine, an i9-14900KS, against the
  installed libebur128 1.2.6, with a C program replaying the plugin's `activate()` and `run()`
  for one side, 10 s of blocks after the 3 s buffer filled):
  - The `Momentary LUFS` and `Shortterm LUFS` ports are connected, so every block `run()` calls
    `ebur128_loudness_momentary` and `ebur128_loudness_shortterm`.
  - Each sums its whole window from scratch (`ebur128_calc_gating_block`): 19,200 + 144,000
    squared samples at 48 kHz, one serial chain of double adds.
  - `add_frames` repeats those sums every 100 ms for the integrated loudness and the loudness
    range, and the true peak oversamples ×4.
  - So the cost hardly depends on the quantum:

  | Per block | P-core (5.9 GHz) | E-core (4.5 GHz) |
  |---|---|---|
  | one side | 0.12–0.14 ms | 0.23–0.28 ms |
  | one channel (two sides) | 0.24–0.29 ms | 0.46–0.56 ms |
  | all four channels | 0.93–1.15 ms | 1.83–2.25 ms |

  As a share of the cycle, with all four channels playing:

  | Quantum (48 kHz) | Cycle | P-core | E-core |
  |---|---|---|---|
  | 1024 | 21.3 ms | 5% | 11% |
  | 512 | 10.7 ms | 10% | 19% |
  | 256 | 5.3 ms | 18% | 36% |
  | 128 | 2.7 ms | 35% | 70% |
  | 64 | 1.3 ms | 70% | 137% |

  - It's paid with Smart Volume **off**: the meter runs whatever `sv_range`'s bounds are.
  - It's paid **per playing channel**. The outlets are `node.passive`, so a channel nothing plays
    into is idle (`pipewire-props(7)`), and its graph isn't running. Four channels playing at once
    is the worst case.
  - It **doubles at 96 kHz**: 0.25 ms per side on a P-core at quantum 1024.
- **One realtime thread runs every channel.**
  - `context.num-data-loops` is at its default, 1, so the channels' graphs run one after another.
  - The data thread isn't pinned, so it can land on either kind of core.
  - Here (`pw-dump`, the `settings` metadata): `clock.rate 48000`, `clock.quantum 1024`,
    `clock.min-quantum 32`.
  - A client that asks for low latency pulls the whole graph's quantum down.
  - M3's acceptance in PLAN.md is "xruns clean at quantum 512→256", and its latency risk plans
    `clock.force-quantum 256`.
- **A module that fails to load:**
  - `pipewire.conf(5)` says `nofail` is "to not fail PipeWire startup if the module fails to
    load". Without it, a failed module in `context.modules` stops PipeWire from starting.
  - `module-filter-chain.c` 1.6.9 loads the graph in its module init
    (`pw_context_load_spa_handle(…, "filter.graph", …)`, failing on `NULL`).
  - filter-graph's `load_graph` fails on a plugin type it can't load, an unknown label, an unknown
    link port ("unknown output port"), or an unknown `capture.volumes` control ("unknown control
    port").
  - The channel modules aren't `nofail` today: a graph PipeWire refuses would take **all audio**
    down, not just the channels. `config.rs` already makes every stream piece `nofail` for this
    reason.
  - Plugin instances are made later, when a channel starts playing (`activate_graph`). A failure
    there ("can't start graph") errors only that channel's stream; the node stays.
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

**LADSPA, the owner's fallback** (kept for a no-go; see Status):
- `swh-plugins` 0.4.17-7.1 is installed (explicitly; nothing requires it). PipeWire's LADSPA
  loader, `libspa-filter-graph-plugin-ladspa.so`, is in `pipewire-audio`. Ports, read with
  ctypes from each `.so`'s `ladspa_descriptor`:
  - `sc4m_1916.so`, label `sc4m` (mono): `RMS/peak` 0…1, `Attack time (ms)` 1.5…400,
    `Release time (ms)` 2…800, `Threshold level (dB)` −30…0, `Ratio (1:n)` 1…20,
    `Knee radius (dB)` 1…10, `Makeup gain (dB)` 0…24; outputs `Amplitude (dB)` and
    `Gain reduction (dB)`; audio `Input`, `Output`. The mic chain already runs it (`mic.rs`,
    behind `mic::plugins_present`).
  - `sc4_1882.so`, label `sc4` (stereo): the same controls; audio `Left/Right input/output`.
  - `hard_limiter_1413.so`, label `hardLimiter` (mono): `dB limit` −50…0, `Wet level` 0…1,
    `Residue level` 0…1; audio `Input`, `Output`.
  - `fast_lookahead_limiter_1913.so`, label `fastLookaheadLimiter` (stereo): `Input gain (dB)`,
    `Limit (dB)`, `Release time (s)`; audio `Input 1/2`, `Output 1/2`.
- These are better dynamics processors than a leveller made from a meter, and much cheaper. But
  `swh-plugins` is optional.
- PipeWire reads the drop-in when it starts, which at login is before gglinuxd can take a missing
  plugin out. Any file check has that race. The EBU R128 plugin escapes it only because it ships
  with the builtins.

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
  `capture.volumes`, no `nofail`. The Sonar tab says Smart Volume is unavailable, and
  `set_smart_volume` is refused.
- `expected_dropin` follows the same check, so the startup `upgrade_dropin` adds the stage once
  the meter exists, and takes it out if it disappears (one PipeWire restart either way, as for
  every drop-in change).
- On a PipeWire whose plugins live elsewhere, the check fails safe: no stage.
- The file is only a proxy. It doesn't prove that this PipeWire takes the exact port names,
  `lufs2gain` or `capture.volumes`; an older or newer `ebur128` could differ. So:

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
    that already matches can still be refused by a PipeWire upgraded since. `set_output` and
    `set_mic_input` also write the drop-in without a restart, so the stage can first be loaded at
    login.
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
  `reason` says why it's unavailable: the meter is missing (naming the file and
  `pipewire-audio`), or PipeWire refused the stage (turn Sonar off and on to try again);
- the request `set_smart_volume { channel, settings }`.

**UI:**
- a **Smart Volume** section in the Sonar tab, under Channels: per channel an *On* box and a level
  menu, and the hint "evens out a channel's loudness; switching is instant";
- when it's unavailable, the daemon's `reason`.

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
- **Against a real compressor** (`sc4m_1916`, the owner's fallback):
  - no attack or release: cuts land within a block, and boosts follow the 3 s loudness;
  - the gain steps once per block, where a compressor's moves every sample;
  - no knee;
  - far more CPU (below).

  In exchange it needs no optional package, and it aims at a loudness target (LUFS), as a
  leveller should, rather than at peaks.
- **CPU cost** (Facts):
  - About **1 ms of every cycle on a P-core, 2 ms on an E-core**, with all four channels playing,
    whatever the quantum.
  - That's **18–36% of the cycle at quantum 256**, 35–70% at 128, and over budget at 64 on an
    E-core.
  - It's paid with Smart Volume off, because toggling must not restart PipeWire. It's paid only by
    channels that are playing.
  - `use-histogram` keeps the meter from allocating in the audio thread; it doesn't make it
    cheaper.
- **Memory:** up to 65 MB more in PipeWire at 48 kHz (Facts), on the same terms as the CPU.
- **The stream mix doesn't get Smart Volume:** it taps the sink before the chain, like the EQ.
- **The first start after this lands** rewrites the drop-in once (on a machine where Sonar is on).

**Go/no-go** (the owner, on Sonar's first live test; see the plan's hardware acceptance):
- **It decides on:**
  - how the levels sound (pumping, roughness);
  - PipeWire's memory against the expected ~65 MB;
  - xruns at quantum 256, M3's bar, with Smart Volume off and all four channels playing. Quantum
    128 is recorded, not judged.
- **Go:** it stays as it is.
- **No-go:** the owner's LADSPA fallback below, in a follow-up plan.
- **The owner's other choices:**
  - drop Smart Volume;
  - put the stage only in channels that have it on. That costs one PipeWire restart the first time
    it's turned on for a channel, which the owner's "never a restart to toggle" rules out today.

### The owner's LADSPA fallback (only on a no-go)

It isn't built in this plan.
- **The stage:** `sc4m_1916` (`label = sc4m`), one per side, which fits the duplicated mono chain.
  - Its ports are in Facts. The levels become threshold, ratio and makeup presets.
  - Off is ratio 1:1 with no makeup: no compression, though not a bit-for-bit passthrough.
  - `hard_limiter_1413` (mono) could follow it to catch peaks.
  - `vol` and `capture.volumes` stay after it, for the same reason as now.
- **Guards:**
  - the stage is written only when `sc4m_1916.so` is found (like `mic::plugins_present`);
  - the channel modules that carry it are `nofail`;
  - the startup upgrade takes it out when the file disappears;
  - the refusal check above catches the rest;
  - the Sonar tab asks for `swh-plugins` when it's missing.
- **What's left of the race:** if `swh-plugins` is uninstalled, the next login's PipeWire skips
  the channel modules. Apps play to WirePlumber's fallback output, not through Sonar, until
  gglinuxd starts and takes the stage out with one restart. The owner's instructions accept that
  window; this design avoids it.
- **The swap:** `smart.rs`'s nodes, links, props and plugin check change. The settings, IPC, UI,
  live switching and refusal handling carry over.

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
  `capture.volumes` on `vol:Mult`, the channel modules `nofail` with it, and today's chain
  byte-for-byte without it;
- the drop-in parses with `spa-json-dump`;
- enabling sets every channel's Smart Volume;
- a change is saved and sent live, with no restart;
- without the meter: the old chain, `available: false` with the reason, the request refused, no
  controls sent;
- the startup upgrade adds the stage when the meter appears, and removes it when it disappears;
- a refused stage (the fake starts PipeWire without the channels when the drop-in carries
  `sv_meter`) is taken out, recorded, and reported:
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
- the volume slider and keys with the volume moved;
- the go/no-go: how the levels sound (pumping, roughness), and xruns at quantum 256 (with 128
  recorded) with Smart Volume off and all four channels playing;
- PipeWire's memory;
- games-only clips with a real game.
