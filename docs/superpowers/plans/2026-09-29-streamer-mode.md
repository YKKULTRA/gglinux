# Streamer mode (M7c) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a second, independent **Stream** mix of Sonar's channels on a virtual device
(`gglinux_stream`) that OBS records, with a level per channel and a master level.

**Architecture:**
- **Drop-in:** with streamer mode on, Sonar's drop-in adds a null sink plus a loopback per
  channel, fed from each channel sink's monitor.
- **Levels:** the stream levels are the loopback playback nodes' volumes; the master is the null
  sink's.
- **Settings and API:** everything goes through the existing `Sonar` flow — settings,
  `expected_dropin`/`upgrade_dropin`, and levels kept across PipeWire restarts — tested against
  the existing PipeWire fake.

**Tech Stack:** Rust (gg-audio), PipeWire config, Svelte 5.

**Spec:** `docs/superpowers/specs/2026-09-29-streamer-mode-design.md`

**Depends on:** M7b done (stacked branch `m7c-streamer`).

## Global Constraints

- **Names:** the stream sink `gglinux_stream` ("gglinux Stream"); feeds `gglinux_<c>.stream`
  (playback) and `gglinux_<c>.stream_in` (capture of the `gglinux_<c>` monitor).
- **Null sink:** `adapter` + `support.null-audio-sink`, `Audio/Sink`, `[ FL FR ]`,
  `monitor.channel-volumes = true`, `object.linger = true`.
- **Feeds:** `libpipewire-module-loopback`. Capture side: `stream.capture.sink = true`,
  `target.object = "gglinux_<c>"`, passive, `node.dont-fallback`. Playback side:
  `target.object = "gglinux_stream"`, passive, `node.dont-fallback`.
- **Settings:** `streamer: bool` (false), `stream_volumes` (1.0 each when unset),
  `stream_master: f32` (1.0), all clamped to 0..=1.
- **Turning streamer on or off while Sonar is on:** rewrite the drop-in, restart PipeWire once,
  wait for the channels (and the stream sink when on), re-take the defaults (Game sink, gglinux
  Mic), and re-apply levels. **Never roll back** because the stream is missing: warn only.
- **In code, never restart the owner's PipeWire.** Tests use the fake, and the executor doesn't
  enable Sonar or streamer mode live.
- **Checks and style:** as before.

## Review Focus

1. **Stream nodes appearing a moment after the channels** following a restart. Levels must still
   be applied to them. Pinned by the Task 2 test `stream_levels_come_back_after_a_restart`, which
   makes `keep_levels` wait for the stream nodes too.
2. **Routing mistaking the feeds for app streams.** They're `Stream/Output/Audio` nodes; the
   `gglinux_` prefix must keep them out. Pinned by the Task 2 test `routing_ignores_the_stream_feeds`.
3. **Streamer toggled while Sonar is off.** It saves the setting only; no PipeWire restart. Pinned
   by the Task 2 test `streamer_while_sonar_is_off_only_saves`.
4. **A drop-in PipeWire can't parse.** That would silently kill all Sonar audio. Pinned by the
   Task 1 test `the_streamer_dropin_parses`, via `spa-json-dump` when installed.
5. **The default sink after the extra restart.** Game must still be the default. Pinned by the
   Task 2 test `streamer_on_with_sonar_on_rewrites_restarts_and_sets_levels`, which checks
   `set-default` again.

---

### Task 1: The drop-in (`gg-audio/src/config.rs`)

**Files:** `crates/gg-audio/src/config.rs`, and the callers of `dropin` in `sonar.rs`

**Interfaces:**
- Produces:
  - `config::STREAM_SINK: &str`;
  - `config::stream_feed_name(Channel) -> String`;
  - `config::dropin(output: &str, mic_input: Option<&str>, streamer: bool) -> Result<String>`.

- [ ] **Step 1: Failing tests** (in `config.rs` tests; update existing `dropin(a, b)` calls to
  `dropin(a, b, false)`)

```rust
    #[test]
    fn the_stream_mix_is_in_the_dropin_only_when_on() {
        let off = dropin("hs", None, false).unwrap();
        assert!(!off.contains(STREAM_SINK));
        let on = dropin("hs", None, true).unwrap();
        assert!(on.contains("support.null-audio-sink"));
        assert!(on.contains("monitor.channel-volumes = true"));
        assert_eq!(on.matches("libpipewire-module-loopback").count(), 4);
        for c in Channel::ALL {
            assert!(on.contains(&format!("\"{}\"", stream_feed_name(c))), "{c:?} feed");
            assert!(on.contains(&format!("\"{}.stream_in\"", sink_name(c))), "{c:?} capture");
        }
        assert!(on.contains("stream.capture.sink = true"));
        assert!(on.starts_with("# Written by gglinux"), "the header stays first");
    }

    #[test]
    fn the_streamer_dropin_parses() {
        if std::process::Command::new("spa-json-dump").arg("--help").output().is_err() {
            return; // not installed here: nothing to check against
        }
        let dir = crate::testing::scratch("streamer-parse");
        for (i, text) in [dropin("hs", None, true).unwrap(), dropin("hs", Some("mic"), true).unwrap()].iter().enumerate() {
            let path = dir.join(format!("{i}.conf"));
            std::fs::write(&path, text).unwrap();
            let out = std::process::Command::new("spa-json-dump").arg(&path).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
    }
```

- [ ] **Step 2: Run** `cargo test -p gg-audio config`. Expected: compile errors (`dropin` takes 2
  arguments; `STREAM_SINK` and `stream_feed_name` missing).

- [ ] **Step 3: Implement** in `config.rs`:

```rust
/// The virtual device the stream mix plays into; OBS records its monitor.
pub const STREAM_SINK: &str = "gglinux_stream";

/// The loopback carrying one channel into the stream mix (its playback node).
pub fn stream_feed_name(channel: Channel) -> String {
    format!("{PREFIX}{}.stream", channel.key())
}

/// The stream mix's device: a null sink whose own volume (the master level)
/// applies to its monitor, which is what OBS records.
fn stream_sink() -> String {
    format!(
        r#"context.objects = [
  {{ factory = adapter
    args = {{
      factory.name            = support.null-audio-sink
      node.name               = "{STREAM_SINK}"
      node.description        = "gglinux Stream"
      media.class             = "Audio/Sink"
      audio.position          = [ FL FR ]
      monitor.channel-volumes = true
      object.linger           = true
    }}
  }}
]
"#
    )
}

/// One channel into the stream mix. A sink's monitor carries the signal before
/// the sink's volume and filter chain, so the stream gets its own levels and no
/// headphone correction.
fn stream_feed(channel: Channel) -> String {
    format!(
        r#"    {{ name = libpipewire-module-loopback
      args = {{
        node.description = "gglinux {label} to Stream"
        audio.position   = [ FL FR ]
        capture.props = {{
          node.name           = "{sink}.stream_in"
          stream.capture.sink = true
          target.object       = "{sink}"
          node.passive        = true
          node.dont-fallback  = true
        }}
        playback.props = {{
          node.name          = "{feed}"
          target.object      = "{STREAM_SINK}"
          node.passive       = true
          node.dont-fallback = true
        }}
      }}
    }}
"#,
        label = label(channel),
        sink = sink_name(channel),
        feed = stream_feed_name(channel),
    )
}
```

In `dropin`, add the parameter `streamer: bool`, then:
- after the header comment lines and **before** `context.modules = [`, insert `stream_sink()` when
  `streamer` is set. The header is built as one string today: split it so the two comment lines
  come first, then the optional objects block, then `context.modules = [\n`.
- after the channel loop and before the mic module, append `stream_feed(channel)` for every
  channel when `streamer` is set.

In `sonar.rs`, `enable` calls `config::dropin(&output, mic_input.as_deref(), self.settings.streamer)`
and `expected_dropin` calls `config::dropin(output, mic, self.settings.streamer)`. Task 2 adds the
field; until then, pass `false` and ledger the one-task gap.

- [ ] **Step 4: Run** `cargo test -p gg-audio` and clippy. Expected: all pass;
  `the_streamer_dropin_parses` passes (or returns early if `spa-json-dump` is missing — check with
  `which spa-json-dump`, and ledger which).
- [ ] **Step 5: Commit** — `gg-audio: a stream mix in Sonar's drop-in`

---

### Task 2: Settings, the Sonar API and state

**Files:**
- `crates/gg-audio/src/settings.rs`, `crates/gg-audio/src/sonar.rs`;
- `crates/gg-ipc/src/lib.rs`;
- `crates/gglinuxd/examples/fakemouse.rs` (its fake `SonarState` gains `stream`).

**Interfaces:**
- Produces:
  - `gg_ipc::StreamMix { enabled: bool, present: bool, volumes: Vec<f32>, master: f32 }`;
  - `SonarState.stream: StreamMix`;
  - `Settings::{streamer, stream_volumes, stream_master}` and
    `Settings::stream_volume(Channel) -> f32`;
  - `Sonar::{set_streamer(bool), set_stream_volume(Channel, f32), set_stream_master(f32)}`,
    each `-> Result<()>`.

- [ ] **Step 1: Failing tests**

In the `sonar.rs` tests, extend `Fake::dump`: inside `if *self.up.borrow() { … }`, after the mic
block, add:

```rust
                let wrote_stream = std::fs::read_to_string(&self.dropin).is_ok_and(|t| t.contains("gglinux_stream"));
                if wrote_stream {
                    nodes.extend([
                        (25, "gglinux_stream", "Audio/Sink", None),
                        (35, "gglinux_game.stream", "Stream/Output/Audio", None),
                        (36, "gglinux_chat.stream", "Stream/Output/Audio", None),
                        (37, "gglinux_media.stream", "Stream/Output/Audio", None),
                        (38, "gglinux_aux.stream", "Stream/Output/Audio", None),
                        (45, "gglinux_game.stream_in", "Stream/Input/Audio", None),
                        (46, "gglinux_chat.stream_in", "Stream/Input/Audio", None),
                        (47, "gglinux_media.stream_in", "Stream/Input/Audio", None),
                        (48, "gglinux_aux.stream_in", "Stream/Input/Audio", None),
                    ]);
                }
```

Add the tests:

```rust
    #[test]
    fn streamer_while_sonar_is_off_only_saves() {
        let (settings_path, dropin) = paths("streamer-off");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        sonar.set_streamer(true).unwrap();
        assert_eq!(fake.count("systemctl"), 0, "no PipeWire restart");
        assert!(settings::load(&settings_path).streamer);
        sonar.enable().unwrap();
        assert!(std::fs::read_to_string(&dropin).unwrap().contains("gglinux_stream"), "applied at the next enable");
    }

    #[test]
    fn streamer_on_with_sonar_on_rewrites_restarts_and_sets_levels() {
        let (settings_path, dropin) = paths("streamer-on");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        sonar.set_stream_volume(Channel::Chat, 0.2).unwrap();
        let restarts = fake.count("systemctl");
        sonar.set_streamer(true).unwrap();
        assert_eq!(fake.count("systemctl"), restarts + 1, "one restart");
        assert!(std::fs::read_to_string(&dropin).unwrap().contains("gglinux_stream"));
        assert!(fake.ran(&["wpctl", "set-volume", "36", "0.200"]), "Chat's stream level");
        assert!(fake.ran(&["wpctl", "set-volume", "25", "1.000"]), "the master level");
        let defaults = fake.runs.borrow().iter().filter(|r| r[..] == ["wpctl", "set-default", "20"]).count();
        assert!(defaults >= 2, "Game is made the default again after the extra restart");
        let s = sonar.snapshot();
        assert!(s.stream.enabled && s.stream.present);
        assert_eq!(s.stream.volumes, vec![1.0, 0.2, 1.0, 1.0]);
    }

    #[test]
    fn stream_volumes_go_to_the_feed_nodes() {
        let (settings_path, dropin) = paths("stream-volumes");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_streamer(true).unwrap();
        sonar.set_stream_volume(Channel::Game, 0.5).unwrap();
        sonar.set_stream_master(0.8).unwrap();
        assert!(fake.ran(&["wpctl", "set-volume", "35", "0.500"]));
        assert!(fake.ran(&["wpctl", "set-volume", "25", "0.800"]));
        assert!(sonar.set_stream_volume(Channel::Game, f32::NAN).is_err());
    }

    #[test]
    fn routing_ignores_the_stream_feeds() {
        let (settings_path, dropin) = paths("stream-routing");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_streamer(true).unwrap();
        sonar.tick();
        for id in [35, 36, 37, 38, 45, 46, 47, 48] {
            assert!(!fake.runs.borrow().iter().any(|r| r[0] == "pw-metadata" && r[1] == id.to_string()), "node {id} was routed");
        }
    }

    #[test]
    fn stream_levels_come_back_after_a_restart() {
        let (settings_path, dropin) = paths("stream-restart");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_streamer(true).unwrap();
        sonar.set_stream_volume(Channel::Media, 0.3).unwrap();
        let before = fake.runs.borrow().iter().filter(|r| r[..] == ["wpctl", "set-volume", "37", "0.300"]).count();
        *fake.up.borrow_mut() = false; // PipeWire restarted elsewhere…
        sonar.tick();
        *fake.up.borrow_mut() = true; // …and the nodes are back
        sonar.tick();
        let after = fake.runs.borrow().iter().filter(|r| r[..] == ["wpctl", "set-volume", "37", "0.300"]).count();
        assert_eq!(after, before + 1);
    }
```

If `tick()` does more than refresh/route/keep levels in a way that interferes, read it first
(`sonar.rs:75`) and adjust only the test's setup, not the assertions. Ledger any such change.

- [ ] **Step 2: Run** `cargo test -p gg-audio`. Expected: compile errors (`set_streamer` etc. and
  `stream` missing).

- [ ] **Step 3: Implement**

`gg-ipc`, after `MicState`:

```rust
/// Streamer mode: a second mix of the channels, on the "gglinux Stream" device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamMix {
    pub enabled: bool,
    /// Whether PipeWire has the stream device now.
    pub present: bool,
    /// Per-channel stream level, 0..=1, in `Channel::ALL` order.
    pub volumes: Vec<f32>,
    /// The stream's master level, 0..=1.
    pub master: f32,
}
```

- `SonarState` gains `pub stream: StreamMix,`.
- `Request` gains:
  - `/// Turn streamer mode on or off (restarts PipeWire while Sonar is on).` `SetStreamer { enabled: bool },`
  - `SetStreamVolume { channel: Channel, volume: f32 },`
  - `SetStreamMaster { volume: f32 },`
- `main.rs` gets temporary error arms for these, replaced in Task 3.

`settings.rs` `Settings` gains:

```rust
    /// Streamer mode: a second mix of the channels for OBS.
    pub streamer: bool,
    /// Per-channel stream level, 0..=1; 1.0 when unset.
    pub stream_volumes: BTreeMap<Channel, f32>,
    /// The stream mix's master level, 0..=1.
    pub stream_master: f32,
```

- `Default`: `streamer: false, stream_volumes: BTreeMap::new(), stream_master: 1.0`.
- Add the method:

```rust
    pub fn stream_volume(&self, channel: Channel) -> f32 {
        self.stream_volumes.get(&channel).copied().unwrap_or(1.0)
    }
```

`sonar.rs`:
1. In `enable`, replace the two `if let Some(node) … set_default` blocks with
   `self.take_defaults()?;`, and add:

```rust
    /// Make the Game channel the default output and gglinux Mic the default input.
    fn take_defaults(&self) -> Result<()> {
        if let Some(node) = self.graph.node(&config::sink_name(Channel::Game)) {
            self.tools.run(&tools::set_default(node.id))?;
        }
        if let Some(node) = self.graph.node(mic::SOURCE) {
            self.tools.run(&tools::set_default(node.id))?;
        }
        Ok(())
    }
```

2. The new public methods:

```rust
    /// Turn streamer mode on or off. While Sonar is on this rewrites the
    /// drop-in and restarts PipeWire (1–2 s of silence), like turning Sonar on.
    pub fn set_streamer(&mut self, enabled: bool) -> Result<()> {
        if self.settings.streamer == enabled {
            return Ok(());
        }
        self.settings.streamer = enabled;
        self.persist();
        if !self.settings.enabled {
            return Ok(());
        }
        let output = self.settings.output.clone().context("Sonar has no output device")?;
        let text = self.expected_dropin(&output)?;
        std::fs::write(&self.dropin_path, text).with_context(|| format!("writing {}", self.dropin_path.display()))?;
        info!(enabled, "streamer mode changed; restarting PipeWire");
        self.tools.run(&tools::restart_pipewire())?;
        let ready = |g: &Graph| {
            Channel::ALL.iter().all(|&c| g.node(&config::sink_name(c)).is_some())
                && (!enabled || g.node(config::STREAM_SINK).is_some())
        };
        if !self.wait_until(ready) {
            warn!("the stream mix did not appear after the restart");
        }
        self.take_defaults()?;
        self.levels_applied = false;
        self.keep_levels();
        self.routed.clear();
        self.route();
        Ok(())
    }

    pub fn set_stream_volume(&mut self, channel: Channel, volume: f32) -> Result<()> {
        if !volume.is_finite() {
            bail!("volume must be a number");
        }
        let volume = volume.clamp(0.0, 1.0);
        self.settings.stream_volumes.insert(channel, volume);
        self.persist();
        if let Some(node) = self.graph.node(&config::stream_feed_name(channel)) {
            self.tools.run(&tools::set_volume(node.id, volume))?;
        }
        Ok(())
    }

    pub fn set_stream_master(&mut self, volume: f32) -> Result<()> {
        if !volume.is_finite() {
            bail!("volume must be a number");
        }
        self.settings.stream_master = volume.clamp(0.0, 1.0);
        self.persist();
        if let Some(node) = self.graph.node(config::STREAM_SINK) {
            self.tools.run(&tools::set_volume(node.id, self.settings.stream_master))?;
        }
        Ok(())
    }
```

3. `apply_levels`, before `self.apply_mic()?;`:

```rust
        for channel in Channel::ALL {
            if let Some(node) = self.graph.node(&config::stream_feed_name(channel)) {
                self.tools.run(&tools::set_volume(node.id, self.settings.stream_volume(channel)))?;
            }
        }
        if let Some(node) = self.graph.node(config::STREAM_SINK) {
            self.tools.run(&tools::set_volume(node.id, self.settings.stream_master))?;
        }
```

4. In `keep_levels`, `present` also waits for the stream nodes when streamer is on:

```rust
        let channels = Channel::ALL.iter().all(|&c| self.graph.node(&config::sink_name(c)).is_some());
        let stream = !self.settings.streamer
            || (self.graph.node(config::STREAM_SINK).is_some()
                && Channel::ALL.iter().all(|&c| self.graph.node(&config::stream_feed_name(c)).is_some()));
        let present = channels && stream;
```

5. `expected_dropin` and `enable` pass `self.settings.streamer`, replacing Task 1's `false`.
6. `snapshot` gains:

```rust
            stream: StreamMix {
                enabled: self.settings.streamer,
                present: self.graph.node(config::STREAM_SINK).is_some(),
                volumes: Channel::ALL.iter().map(|&c| self.settings.stream_volume(c)).collect(),
                master: self.settings.stream_master,
            },
```

   Import `StreamMix`.
7. `fakemouse.rs`: its fake `SonarState` gains
   `stream: StreamMix { enabled: false, present: false, volumes: vec![1.0; 4], master: 1.0 }`.

- [ ] **Step 4: Run** the workspace tests and clippy. Expected: `failed=0` (the 5 new tests pass);
  clippy `0`.
- [ ] **Step 5: Commit** — `gg-audio: streamer mode — a second mix of the channels for OBS`

---

### Task 3: Daemon, UI and docs

**Files:** `crates/gglinuxd/src/sonar.rs`, `main.rs`, `examples/fakemouse.rs`, `ui/src/lib/types.ts`,
`ui/src/lib/daemon.svelte.ts`, `ui/src/lib/SonarPanel.svelte`, `PLAN.md`

- [ ] **Step 1: Daemon**
  - `sonar.rs` `Command` gains `SetStreamer { enabled: bool, reply: Reply<()> }`,
    `SetStreamVolume { channel: Channel, volume: f32, reply: Reply<()> }` and
    `SetStreamMaster { volume: f32, reply: Reply<()> }`.
  - Their `handle` arms follow the existing pattern
    (`let _ = reply.send(sonar.set_streamer(enabled).map_err(text));`, and so on).
  - `main.rs` replaces the Task-2 placeholders with
    `outcome(workers.sonar.send::<()>(|reply| SonarCommand::SetStreamer { enabled, reply }).await)`,
    and likewise for the other two.
  - `fakemouse.rs`: `SetStreamer` sets `s.sonar.stream.enabled`, and `present` equal to it;
    `SetStreamVolume` writes `volumes[index of channel]`; `SetStreamMaster` writes `master`. Add
    the names to its naming function.

Run workspace tests and clippy. Expected: green, `0`.

- [ ] **Step 2: UI**
  - `types.ts`: `interface StreamMix { enabled: boolean; present: boolean; volumes: number[]; master: number }`,
    `stream: StreamMix` on `SonarState`, and the three requests.
  - `daemon.svelte.ts`: `setStreamer(enabled)`, `setStreamVolume(channel, volume)` and
    `setStreamMaster(volume)` through the existing `#sonarCommand`.
  - `SonarPanel.svelte`, a section after the channel list:

```svelte
    <section>
      <div class="head"><h4>Streamer mode</h4></div>
      <label class="toggle">
        <input
          type="checkbox"
          checked={sonar.stream.enabled}
          disabled={busy !== null}
          onchange={(e) => run("streamer", () => daemon.setStreamer(e.currentTarget.checked))}
        />
        A separate mix for your stream (turning this on or off restarts PipeWire: 1–2 s of silence)
      </label>
      {#if sonar.stream.enabled}
        {#if !sonar.stream.present}<p class="hint warn">PipeWire hasn't created the stream device yet.</p>{/if}
        {#each sonar.channels as c, i (c.channel)}
          <div class="row">
            <span class="label">{c.channel} → stream</span>
            <input
              type="range"
              min="0"
              max="100"
              value={Math.round(sonar.stream.volumes[i] * 100)}
              onchange={(e) => quietly(() => daemon.setStreamVolume(c.channel, e.currentTarget.valueAsNumber / 100))}
            />
            <span class="unit">{Math.round(sonar.stream.volumes[i] * 100)}%</span>
          </div>
        {/each}
        <div class="row">
          <span class="label">Stream master</span>
          <input
            type="range"
            min="0"
            max="100"
            value={Math.round(sonar.stream.master * 100)}
            onchange={(e) => quietly(() => daemon.setStreamMaster(e.currentTarget.valueAsNumber / 100))}
          />
          <span class="unit">{Math.round(sonar.stream.master * 100)}%</span>
        </div>
        <p class="hint">In OBS, add an Audio Output Capture of <em>Monitor of gglinux Stream</em>.</p>
      {/if}
    </section>
```

  Match the panel's existing names: its `run`/`quietly` helpers, the variable holding the Sonar
  state (`sonar` in the channel loop), the busy flag's shape, and the class names (`head`, `row`,
  `label`, `unit`, `hint`, `toggle`). Read the file around its channel section first, adjust the
  snippet to them, and add a `.warn` style if the panel lacks one. Channel names display as the
  panel already shows them elsewhere; reuse its label helper if it has one.

  Run `cd ui && npm run check`. Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: PLAN.md**, M7 section:

```markdown
**M7c streamer mode done in software** (spec `docs/superpowers/specs/2026-09-29-streamer-mode-design.md`):
a second, independent Stream mix of Sonar's channels on a "gglinux Stream" device (per-channel and
master levels) for OBS to record. Builds on Sonar, which is still unverified live.
```

- [ ] **Step 4: Final checks and commit** — `Sonar: streamer mode in the daemon and UI`

## Hardware acceptance (the owner, with Sonar's first live test)

1. Turn Sonar on, then streamer mode.
2. In OBS, record *Monitor of gglinux Stream*; play a game and Discord; set Chat → stream to 0%.
3. The recording has the game but not Discord; the owner still hears both.
