# Smart Volume and games-only clips (M7e) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:**
- **Smart Volume** per Sonar channel: off, or Quiet / Balanced / Loud, switched live.
- **Games-only clips:** the replay buffer runs only while a game from the profiles' rules runs.
- **PLAN.md tells the truth** about what is built, verified, blocked and skipped.

**Architecture:**
- **Smart Volume:**
  - swh's SC4 compressor (`sc4m_1916`, from the optional `swh-plugins`) at the end of each
    channel's filter chain, one per side. PipeWire has no dynamics builtin, so this is the
    owner's guarded LADSPA path (spec, Status).
  - A builtin `mixer` after it hears either the compressor (on) or the channel EQ (off: an exact
    passthrough). It is switched through its own controls with `pw-cli set-param`, like the EQ.
  - The sink's volume moves after it (`capture.volumes`), so the compressor can't squash the
    volume slider.
  - The guards: the stage is written only when `sc4m_1916.so` is where PipeWire looks for it. The
    startup drop-in upgrade, and Sonar's one-second tick while gglinux runs, take it out when the
    file goes.
  - The channel modules that carry the stage are `nofail`. If PipeWire refuses the stage, it
    still starts (without the channels). gglinux then sees the channels missing, records the
    refusal, and rewrites the drop-in without the stage.
  - The first draft's EBU R128 leveller is kept in the spec under "Considered, not built".
- **Games-only clips:** the profiles watcher publishes "the game from the rules running now" on a
  `tokio::sync::watch` channel, and the clips controller follows it. There is no second `/proc`
  scanner.

**Tech Stack:** Rust (gg-ipc, gg-audio, gglinuxd, tokio), PipeWire filter-chain config, Svelte 5.

**Spec:** `docs/superpowers/specs/2026-10-02-smart-volume-game-clips-design.md`

**Depends on:** M7d done (stacked branch `m7e-gaps` on `m7d-desktop`).

## Global Constraints

- **Hard constraint:** a channel's audio must never depend on an optional package.
  - Smart Volume's compressor is the one plugin in a channel chain from outside `pipewire-audio`,
    on the owner's instructions, and only behind the owner's guards. Every other channel node is
    `type = builtin`. **No other LADSPA plugin goes into a channel chain** (no limiter).
  - The stage is written only when `sc4m_1916.so` is where PipeWire 1.6.9 looks: the entries of
    `LADSPA_PATH` if it is set (they replace the defaults), else `/usr/lib64/ladspa`,
    `/usr/lib/ladspa`, `/usr/lib`. `mic::plugins_present` moves to the same lookup.
  - Without it, the drop-in is byte-for-byte today's.
  - The startup upgrade takes the stage out when the file is gone. So does `tick`, within a
    second, while Sonar is on: it rewrites the drop-in at once, and restarts PipeWire only if the
    channels are already gone.
- **A graph PipeWire refuses must never stop PipeWire.**
  - With the stage, each channel module carries `flags = [ nofail ]`; without it, nothing changes.
  - The stage is refused when the drop-in on disk carries it and PipeWire answers, but the channels
    don't come within `APPEAR_TIMEOUT`. This is checked after `enable()`'s and `set_streamer`'s
    restarts, and on every startup with Sonar on.
  - A refusal is persisted (`smart_volume_refused`) before the stage-less rewrite and its restart.
  - Turning Sonar off forgets it, and so does `enable()`, at its start and when it rolls back.
- **The stage's nodes, in order, after `eq_10`:** `sv_comp` (`type = ladspa`, `sc4m_1916`,
  `label = sc4m`: RMS detection, attack 1.5 ms, release 800 ms, ratio 4, knee 6 dB), `sv_mix`
  (builtin `mixer`: `In 1` = `eq_10:Out`, `In 2` = `sv_comp:Output`) and `vol` (linear). `vol` is
  always the last node. `capture.volumes = [ { control = "vol:Mult" } ]` sits inside
  `filter.graph`.
- **Levels** (threshold / makeup): Quiet −30 / +3 dB, Balanced −24 / +6 dB, Loud −22 / +9 dB.
  - On: `sv_mix:Gain 1` = 0.0, `Gain 2` = 1.0.
  - Off: `Gain 1` = 1.0, `Gain 2` = 0.0, an exact passthrough. The compressor keeps the level's
    settings, so switching on is seamless.
- **Live controls:** `pw-cli set-param <channel sink id> Props { params = [ … ] }`, a separate
  command from the EQ's. **Toggling or changing a level never restarts PipeWire.**
- **Settings:**
  - `sonar.json` `smart_volume: { <channel>: { enabled, level } }` (unset = off, Balanced) and
    `smart_volume_refused` (default false);
  - `clips.json` `games_only` (default false).
  - Both structs are `#[serde(default)]`, so old files load.
- **Games-only clips:**
  - The recorder is wanted when `enabled && (!games_only || game.is_some())`.
  - When the game goes, it stops after a 15 s grace with `SIGINT`.
  - An exit while not wanted isn't retried.
- **In code, never restart the owner's PipeWire, run `gpu-screen-recorder`, or open a portal.**
  Tests use the existing PipeWire fake and the stand-in recorder script.
- **Checks:**
  - `bash -c 'cargo test --workspace --all-targets 2>&1 | grep -E "^test result" | awk "{p+=\$4;f+=\$6} END{print \"passed=\"p\" failed=\"f; exit f>0}"'`
    (`passed=279 failed=0` at the base, `58df557`);
  - expected totals: Task 1 281, Task 2 286, Task 3 289, Task 4 295, Task 5 301, Task 6 301,
    Task 7 302, Task 8 306, Tasks 9 and 10 306;
  - `cargo clippy --workspace --all-targets 2>&1 | grep -cE "^(warning|error)"` → `0`;
  - `cd ui && npm run check` → 0 errors, 0 warnings.
- **Style:** as before: plain-English doc comments, the surrounding density, no new dependencies.

## Review Focus

1. **The hard constraint: a channel's audio must never depend on an optional package.** Smart
   Volume's compressor comes from `swh-plugins`, which is optional, and a channel whose graph names
   a plugin PipeWire can't load doesn't load. The owner's guards, each pinned:
   - The stage is written only when `sc4m_1916.so` is where PipeWire looks: `LADSPA_PATH` instead
     of the defaults, else `/usr/lib64/ladspa`, `/usr/lib/ladspa`, `/usr/lib` (`plugin_ladspa.c`
     1.6.9). The mic's old lookup (`LADSPA_PATH` then `/usr/local/lib/ladspa`) could find a plugin
     PipeWire won't load; both now share the right one. Task 2:
     `plugins_are_looked_for_where_pipewire_looks`, `the_compressor_is_found_by_file`.
   - Without it, the chain is exactly today's. Task 3:
     `without_smart_volume_the_chain_is_as_before`; Task 4:
     `without_the_compressor_the_channels_are_as_before`.
   - The startup upgrade takes the stage out when the file disappears, and adds it when it
     appears. Task 4: `a_compressor_that_disappears_takes_the_stage_out_on_startup`,
     `a_compressor_that_appears_adds_the_stage_on_startup`.
   - While gglinux runs, the tick takes it out within a second, restarting PipeWire only when the
     channels are already gone. Task 5:
     `a_compressor_removed_while_sonar_runs_leaves_the_dropin_at_once`,
     `a_compressor_removed_before_a_pipewire_restart_brings_the_channels_back`.
   - `sc4m_1916` is the only plugin from outside `pipewire-audio` in a channel. Task 3:
     `smart_volume_ends_every_channels_chain`.
   - **What's left**, which the owner's instructions accept: `swh-plugins` removed while gglinux
     isn't running. The next PipeWire start skips the channel modules (`nofail`, item 2); apps play
     to WirePlumber's fallback output, not through Sonar, until gglinuxd starts and takes the
     stage out with one restart. Check that nothing else widens this window.
2. **A graph PipeWire refuses would stop PipeWire from starting**, which means all audio, not just
   the channels.
   - Why: the channel modules aren't `nofail` today, and a filter-chain module whose graph fails to
     load is fatal in `context.modules` (spec Facts).
   - The stage is the first LADSPA plugin, the first `mixer` and the first `capture.volumes` in a
     channel chain, and none of it has run live. The plugin file is only a proxy for PipeWire
     loading it with these port names.
   - So the staged channel modules are `nofail`, and the stage-less drop-in stays byte-for-byte.
     Pinned by `smart_volume_ends_every_channels_chain` and
     `without_smart_volume_the_chain_is_as_before` (Task 3).
   - A refusal is caught and the stage taken out on every path that loads it: Task 5's
     `a_refused_stage_is_taken_out_when_sonar_turns_on`, `a_refused_stage_is_taken_out_on_startup`,
     `a_stage_refused_at_login_is_taken_out_on_startup` and
     `a_stage_refused_after_a_streamer_restart_is_taken_out`.
   - The rollback still works when the channels never come at all:
     `enable_rolls_back_when_the_channels_never_appear` (now three restarts).
   - The port names were read from the installed `.so` (spec Facts), and the text parses
     (`the_smart_volume_dropin_parses`).
   - What remains is hardware acceptance step 1. It includes playing into every channel, since a
     failure when the plugins are instantiated only errors that stream.
3. **Toggling must never restart PipeWire.** Pinned by the Task 4 test
   `smart_volume_switches_live_without_a_restart`.
4. **Off must be an exact passthrough.** The mixer then has the channel EQ at gain 1.0 and the
   compressor at 0. PipeWire's mixer skips a 0-gain input and copies a lone 1.0-gain one (C, SSE
   and AVX2, spec Facts). The compressor alone at 1:1 isn't exact (+0.0001–0.0002 dB, measured).
   Pinned by the Task 2 test `off_is_an_exact_passthrough`.
5. **The volume slider and the keyboard's volume keys must still work** with Smart Volume on.
   - The sink volume moves to `vol:Mult`, after the compressor; before it, the 4:1 ratio would
     squash it.
   - Pinned by `smart_volume_ends_every_channels_chain` (`capture.volumes` on `vol:Mult`, `vol`
     the last node).
   - Heard in hardware acceptance step 2.
6. **Onsets and peaks.** No test pins them; they were measured by running `sc4m` in-process (spec
   Facts).
   - A sound after a quiet stretch or after silence plays at up to the level's makeup (+3/+6/+9 dB)
     for its first 1–3 ms, and is within about 1 dB of its steady gain by 5–10 ms. The first
     draft's leveller kept such a sound above 0 dB for about 130 ms (spec, "Considered, not
     built").
   - No peak limiter: on Balanced and Loud, the first millisecond of a full-scale bang can go over
     0 dBFS.
   - Hardware acceptance step 4 compares both against Smart Volume off.
   - CPU is negligible: about 9–13 µs per side per 1024-sample block, under 1% of the cycle with
     all four channels playing, at quantum 1024 or 256.
7. **"A game is running" must not depend on the keyboard.** The lighting step can fail (no
   keyboard), so the game is published before it. Pinned by the Task 7 test
   `the_running_game_is_shared_even_without_a_keyboard`.
8. **Games-only stopping.**
   - `SIGINT` after the grace, never `SIGKILL` first; the same recorder if the game comes back;
     no restart storm after the game has gone.
   - Pinned by the Task 8 tests `games_only_waits_for_a_game_and_stops_after_it_leaves`,
     `a_game_back_within_the_grace_keeps_the_recorder` and `a_crash_after_the_game_is_not_retried`.
9. **Old settings files.** Pinned by `settings_from_before_smart_volume_still_load` (Tasks 4 and
   5) and `games_only_clips_default_off_and_old_files_still_load` (Task 1).
10. **This follows the owner's instructions** (spec, Status). The first draft built an EBU R128
    leveller instead, to avoid the optional package. It's kept in the spec under "Considered, not
    built", with the review's onset analysis; nothing of it is built here.

---

### Task 1: IPC (`gg-ipc`)

**Files:**
- `crates/gg-ipc/src/lib.rs`;
- compile shims: `crates/gg-audio/src/sonar.rs` (snapshot), `crates/gglinuxd/src/clips.rs`
  (`state()`) and `crates/gglinuxd/src/main.rs` (a placeholder arm);
- `crates/gglinuxd/examples/fakemouse.rs`.

**Interfaces:**
- Produces:
  - `SmartLevel { Quiet, Balanced (default), Loud }` (snake_case);
  - `SmartVolume { enabled: bool, level: SmartLevel }` (`#[serde(default)]`, `Copy`, `Default`);
  - `SmartVolumeState { available: bool, reason: Option<String>, settings: Vec<SmartVolume> }`;
  - `SonarState.smart_volume: SmartVolumeState`;
  - `Request::SetSmartVolume { channel: Channel, settings: SmartVolume }`;
  - `ClipsSettings.games_only: bool`;
  - `ClipsState.{game: Option<String>, waiting: bool}`.

- [ ] **Step 1: Failing tests** (append to the `gg-ipc` tests)

```rust
    #[test]
    fn smart_volume_defaults_off_and_parses() {
        let d = SmartVolume::default();
        assert_eq!((d.enabled, d.level), (false, SmartLevel::Balanced));
        let line = r#"{"id":1,"req":{"method":"set_smart_volume","params":{"channel":"game","settings":{"enabled":true,"level":"loud"}}}}"#;
        match serde_json::from_str::<RequestFrame>(line).unwrap().req {
            Request::SetSmartVolume { channel, settings } => {
                assert_eq!(channel, Channel::Game);
                assert_eq!(settings, SmartVolume { enabled: true, level: SmartLevel::Loud });
            }
            other => panic!("parsed as {other:?}"),
        }
        let partial: SmartVolume = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert_eq!(partial.level, SmartLevel::Balanced);
        assert_eq!(serde_json::to_string(&SmartLevel::Quiet).unwrap(), r#""quiet""#);
    }

    #[test]
    fn games_only_clips_default_off_and_old_files_still_load() {
        assert!(!ClipsSettings::default().games_only);
        let old: ClipsSettings = serde_json::from_str(r#"{"enabled":true,"seconds":45}"#).unwrap();
        assert!(!old.games_only);
        let on: ClipsSettings = serde_json::from_str(r#"{"enabled":true,"games_only":true}"#).unwrap();
        assert!(on.games_only);
        let s = serde_json::to_value(ClipsState::default()).unwrap();
        assert_eq!((s["waiting"].as_bool(), s["game"].is_null()), (Some(false), true));
    }
```

- [ ] **Step 2: Run** `cargo test -p gg-ipc smart_volume games_only`. Expected: compile errors
  (`SmartVolume`, `SmartLevel`, `SetSmartVolume`, `games_only` missing).

- [ ] **Step 3: Implement**

After `StreamMix`:

```rust
/// How loud Smart Volume aims a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartLevel {
    Quiet,
    #[default]
    Balanced,
    Loud,
}

/// Smart Volume on one channel: quiet sounds come up, loud ones are held down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartVolume {
    /// Off by default.
    pub enabled: bool,
    pub level: SmartLevel,
}

/// Smart Volume across the channels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartVolumeState {
    /// Whether the channels carry Smart Volume's stage, so it can be switched.
    pub available: bool,
    /// Why not, for the Sonar tab: swh-plugins is missing, or PipeWire refused the stage.
    pub reason: Option<String>,
    /// Per channel, in `Channel::ALL` order.
    pub settings: Vec<SmartVolume>,
}
```

- `SonarState` gains, after `stream`: `pub smart_volume: SmartVolumeState,`.
- `Request` gains, after `SetStreamMaster`:

```rust
    /// Turn Smart Volume on or off for a channel, or change its level. Live: no restart.
    SetSmartVolume { channel: Channel, settings: SmartVolume },
```

- `ClipsSettings` gains, after `dir`:

```rust
    /// Record only while a program from the profiles' game rules runs.
    pub games_only: bool,
```

  `Default` gains `games_only: false`.
- `ClipsState` gains, after `hotkey`:

```rust
    /// The game from the rules that's running now, if any.
    pub game: Option<String>,
    /// On and games-only, with no game from the rules running: the recorder waits.
    pub waiting: bool,
```

`SonarState.smart_volume` on its own trips clippy's `large_enum_variant` on `ResponseData` and
`Event` (352 against 144 bytes, checked); the `ClipsState` fields keep the difference under the
limit. So they land together, as here.

**Compile shims** (each replaced later):
- `gg-audio/src/sonar.rs`:
  - import `SmartVolume, SmartVolumeState` from `gg_ipc`;
  - `snapshot()` gains
    `smart_volume: SmartVolumeState { available: false, reason: None, settings: vec![SmartVolume::default(); Channel::ALL.len()] },`
    (Task 4 replaces it).
- `gglinuxd/src/clips.rs`: `state()` gains `game: None, waiting: false,` (Task 8).
- `gglinuxd/src/main.rs`: after the `SetStreamMaster` arm,
  `Request::SetSmartVolume { .. } => ResponseData::Error { message: "Smart Volume isn't wired up yet".into() },`
  (Task 6).

**`fakemouse.rs`:**
- Import `SmartVolume, SmartVolumeState`.
- `fake_sonar()` gains
  `smart_volume: SmartVolumeState { available: true, reason: None, settings: vec![SmartVolume::default(); 4] },`.
- The arm, after `SetStreamMaster`:

```rust
        Request::SetSmartVolume { channel, settings } => {
            if let Some(i) = Channel::ALL.iter().position(|&c| c == channel) {
                s.sonar.smart_volume.settings[i] = settings;
            }
            ResponseData::Success
        }
```

- The naming function gets `Request::SetSmartVolume { .. } => "set_smart_volume",`.
- `SetClipsSettings` replaces `s.clips.recording = settings.enabled;` with:

```rust
            s.clips.recording = settings.enabled && !settings.games_only;
            s.clips.waiting = settings.enabled && settings.games_only;
```

  This keeps it before `s.clips.settings = settings;`.

If `cargo build --workspace --all-targets` names another exhaustive `match` on `Request` or another
struct literal, fix it the same way and ledger it.

- [ ] **Step 4: Run** the workspace tests and clippy. Expected: `passed=281 failed=0`; clippy `0`.
- [ ] **Step 5: Commit** — `gg-ipc: Smart Volume and games-only clips`

---

### Task 2: Smart Volume's stage and controls (`gg-audio/src/smart.rs`)

**Files:** Create `crates/gg-audio/src/smart.rs`; `crates/gg-audio/src/lib.rs` (`pub mod smart;`);
`crates/gg-audio/src/mic.rs` (the LADSPA lookup, shared)

**Interfaces:**
- Produces:
  - `smart::NODES: [&str; 3]`, `smart::INPUTS: [&str; 2]`, `smart::LINKS: [(&str, &str); 2]`,
    `smart::VOLUME_CONTROL: &str`;
  - `smart::curve(SmartLevel) -> (f32 /* threshold, dB */, f32 /* makeup, dB */)`;
  - `smart::props(&SmartVolume) -> String`;
  - `smart::compressor_present() -> bool` and `smart::compressor_present_in(&[PathBuf]) -> bool`;
  - in `mic.rs`: `pub(crate) const SC4M`,
    `pub(crate) fn ladspa_dirs(Option<OsString>) -> Vec<PathBuf>` and
    `pub(crate) fn ladspa_found(&[PathBuf], &str) -> bool`. `plugins_present` and
    `plugins_present_in` keep their signatures.
- The lookup changes for the mic too: `LADSPA_PATH` now replaces the defaults instead of coming
  before them, and the defaults are PipeWire's (`/usr/lib` instead of `/usr/local/lib/ladspa`).
  On this machine both plugins are in `/usr/lib/ladspa`, so nothing changes here.

- [ ] **Step 1: Failing tests** (write the module with `todo!()` bodies and the constants empty,
  then:)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_is_an_exact_passthrough() {
        for level in [SmartLevel::Quiet, SmartLevel::Balanced, SmartLevel::Loud] {
            let p = props(&SmartVolume { enabled: false, level });
            // PipeWire's mixer skips an input at gain 0 and copies a lone input at gain 1.
            assert!(p.contains("\"sv_mix:Gain 1\" 1.0 \"sv_mix:Gain 2\" 0.0"), "{p}");
        }
        assert!(NODES[1].contains(r#""Gain 1" = 1.0 "Gain 2" = 0.0"#), "the stage starts off");
    }

    #[test]
    fn each_level_sets_its_threshold_and_makeup() {
        let p = props(&SmartVolume { enabled: true, level: SmartLevel::Balanced });
        assert!(p.starts_with("{ params = [ ") && p.ends_with(" ] }"), "{p}");
        assert!(p.contains("\"sv_comp:Threshold level (dB)\" -24.0 \"sv_comp:Makeup gain (dB)\" 6.0"), "{p}");
        assert!(p.contains("\"sv_mix:Gain 1\" 0.0 \"sv_mix:Gain 2\" 1.0"), "on hears the compressor: {p}");
        let quiet = props(&SmartVolume { enabled: true, level: SmartLevel::Quiet });
        assert!(quiet.contains("(dB)\" -30.0 ") && quiet.contains("(dB)\" 3.0 "), "{quiet}");
        let loud = props(&SmartVolume { enabled: false, level: SmartLevel::Loud });
        assert!(loud.contains("(dB)\" -22.0 ") && loud.contains("(dB)\" 9.0 "), "off keeps the level: {loud}");
        for level in [SmartLevel::Quiet, SmartLevel::Balanced, SmartLevel::Loud] {
            let (threshold, makeup) = curve(level);
            // PipeWire would clamp anything outside the plugin's ranges.
            assert!((-30.0..=0.0).contains(&threshold) && (0.0..=24.0).contains(&makeup), "{level:?}");
        }
    }

    #[test]
    fn the_stage_ends_with_the_volume_and_hears_the_eq_or_the_compressor() {
        assert!(NODES.last().unwrap().contains("name = vol label = linear"), "vol is the graph's output");
        assert!(NODES[0].contains(&format!("name = sv_comp plugin = \"{}\" label = sc4m", mic::SC4M)));
        assert!(
            NODES[0].contains(r#""RMS/peak" = 0.0 "Attack time (ms)" = 1.5 "Release time (ms)" = 800.0"#),
            "{}",
            NODES[0]
        );
        assert_eq!(INPUTS, ["sv_comp:Input", "sv_mix:In 1"], "the channel EQ feeds both");
        assert!(LINKS.contains(&("sv_comp:Output", "sv_mix:In 2")));
        assert_eq!(LINKS.last(), Some(&("sv_mix:Out", "vol:In")));
        assert_eq!(VOLUME_CONTROL, "vol:Mult");
        let from_eq = INPUTS.iter().map(|&input| ("eq_10:Out", input));
        for (output, input) in LINKS.iter().copied().chain(from_eq) {
            for port in [output, input] {
                let node = port.split(':').next().unwrap();
                assert!(node == "eq_10" || NODES.iter().any(|n| n.contains(&format!("name = {node} "))), "{port}");
            }
        }
    }

    #[test]
    fn the_compressor_is_found_by_file() {
        let dir = crate::testing::scratch("smart-ladspa");
        assert!(!compressor_present_in(std::slice::from_ref(&dir)));
        std::fs::write(dir.join("sc4m_1916.so"), b"").unwrap();
        assert!(compressor_present_in(&[dir]));
    }
}
```

In the `mic.rs` tests, add:

```rust
    #[test]
    fn plugins_are_looked_for_where_pipewire_looks() {
        let defaults: Vec<PathBuf> =
            ["/usr/lib64/ladspa", "/usr/lib/ladspa", "/usr/lib"].iter().map(PathBuf::from).collect();
        assert_eq!(ladspa_dirs(None), defaults);
        assert_eq!(
            ladspa_dirs(Some("/opt/a::/opt/b".into())),
            [PathBuf::from("/opt/a"), PathBuf::from("/opt/b")],
            "LADSPA_PATH replaces the defaults, and an empty entry is skipped"
        );
    }
```

- [ ] **Step 2: Run** `cargo test -p gg-audio smart::tests`. Expected: compile errors (`ladspa_dirs`
  missing). Once `mic.rs` has it (Step 3, first), the smart tests FAIL (`not yet implemented`, the
  empty constants).

- [ ] **Step 3: Implement**

`mic.rs`:
- `use std::ffi::OsString;` next to `PathBuf`.
- The plugin constants and the lookup become:

```rust
/// LADSPA plugins; PipeWire appends `.so` when it loads them.
const RNNOISE: &str = "librnnoise_ladspa";
/// swh's SC4 compressor, which Smart Volume uses too.
pub(crate) const SC4M: &str = "sc4m_1916";

/// Where PipeWire 1.6 looks for LADSPA plugins when `LADSPA_PATH` isn't set
/// (`/usr/lib` is its library directory on Arch).
const DEFAULT_LADSPA_DIRS: [&str; 3] = ["/usr/lib64/ladspa", "/usr/lib/ladspa", "/usr/lib"];
```

```rust
/// Whether both plugins are installed where PipeWire looks.
pub fn plugins_present() -> bool {
    plugins_present_in(&ladspa_dirs(std::env::var_os("LADSPA_PATH")))
}

/// Whether both plugins exist as files in one of `dirs`.
pub fn plugins_present_in(dirs: &[PathBuf]) -> bool {
    ladspa_found(dirs, RNNOISE) && ladspa_found(dirs, SC4M)
}

/// Where PipeWire looks for LADSPA plugins, given `LADSPA_PATH`: its entries
/// replace the defaults rather than come before them.
pub(crate) fn ladspa_dirs(ladspa_path: Option<OsString>) -> Vec<PathBuf> {
    match ladspa_path {
        Some(path) => std::env::split_paths(&path).filter(|d| !d.as_os_str().is_empty()).collect(),
        None => DEFAULT_LADSPA_DIRS.iter().map(PathBuf::from).collect(),
    }
}

/// Whether a plugin, named as PipeWire loads it (without `.so`), is a file in one of `dirs`.
pub(crate) fn ladspa_found(dirs: &[PathBuf], plugin: &str) -> bool {
    dirs.iter().any(|d| d.join(format!("{plugin}.so")).is_file())
}
```

`smart.rs`:

```rust
//! Smart Volume: evens out a channel's loudness, like Sonar's.
//!
//! It is swh's SC4 compressor (`sc4m`, from the optional `swh-plugins`) at the
//! end of every channel's chain, one per side, written only when PipeWire can
//! load it (see [`compressor_present`]):
//!
//! ```text
//!         ┌→ sv_comp ───→ In 2 ┐
//! eq_10 ──┤                    sv_mix → vol
//!         └───────────→ In 1 ──┘          ↑ the sink's volume
//! ```
//!
//! Quiet sounds come up by the level's makeup gain, and sounds above its
//! threshold are compressed 4:1. `sv_mix` hears the compressor (on) or the
//! channel EQ (off: an exact passthrough), so switching never needs a PipeWire
//! restart, and the compressor has already settled when it's switched in.

use std::path::PathBuf;

use gg_ipc::{SmartLevel, SmartVolume};

use crate::mic;

/// Where the channel EQ's output goes: into the compressor, and straight
/// into the mix for when Smart Volume is off.
pub const INPUTS: [&str; 2] = ["sv_comp:Input", "sv_mix:In 1"];

/// The stage's nodes, in chain order. `vol` must stay last: a graph without
/// explicit outputs plays its last node's output.
pub const NODES: [&str; 3] = [
    // RMS detection with an instant attack (under 2 ms): the plugin's 5 ms
    // RMS window does the smoothing. 800 ms is its slowest release. The level
    // sets the threshold and makeup; this starts at Balanced's.
    r#"{ type = ladspa name = sv_comp plugin = "sc4m_1916" label = sc4m control = { "RMS/peak" = 0.0 "Attack time (ms)" = 1.5 "Release time (ms)" = 800.0 "Threshold level (dB)" = -24.0 "Ratio (1:n)" = 4.0 "Knee radius (dB)" = 6.0 "Makeup gain (dB)" = 6.0 } }"#,
    // Starts off: all of the channel EQ (In 1), none of the compressor (In 2).
    r#"{ type = builtin name = sv_mix label = mixer control = { "Gain 1" = 1.0 "Gain 2" = 0.0 } }"#,
    r#"{ type = builtin name = vol label = linear control = { "Mult" = 1.0 } }"#,
];

/// The stage's links (output, input), after the channel EQ's into [`INPUTS`].
pub const LINKS: [(&str, &str); 2] = [("sv_comp:Output", "sv_mix:In 2"), ("sv_mix:Out", "vol:In")];

/// The control that carries the sink's volume, after the compressor, so the
/// compressor can't squash the volume slider.
pub const VOLUME_CONTROL: &str = "vol:Mult";

/// Whether PipeWire can load the compressor: `sc4m_1916.so` where it looks.
pub fn compressor_present() -> bool {
    compressor_present_in(&mic::ladspa_dirs(std::env::var_os("LADSPA_PATH")))
}

/// Whether the compressor exists as a file in one of `dirs`.
pub fn compressor_present_in(dirs: &[PathBuf]) -> bool {
    mic::ladspa_found(dirs, mic::SC4M)
}

/// A level's compressor threshold and makeup gain, in dB.
pub fn curve(level: SmartLevel) -> (f32, f32) {
    match level {
        SmartLevel::Quiet => (-30.0, 3.0),
        SmartLevel::Balanced => (-24.0, 6.0),
        SmartLevel::Loud => (-22.0, 9.0),
    }
}

/// The `pw-cli set-param` Props for one channel's stage. Off keeps the
/// level's settings on the compressor, so switching on is seamless.
pub fn props(s: &SmartVolume) -> String {
    let (threshold, makeup) = curve(s.level);
    let (dry, wet) = if s.enabled { (0.0, 1.0) } else { (1.0, 0.0) };
    format!(
        "{{ params = [ \"sv_comp:Threshold level (dB)\" {threshold:.1} \"sv_comp:Makeup gain (dB)\" {makeup:.1} \
         \"sv_mix:Gain 1\" {dry:.1} \"sv_mix:Gain 2\" {wet:.1} ] }}"
    )
}
```

(The port names and ranges were read from the installed `sc4m_1916.so`; the mixer's from
`plugin_builtin.c` 1.6.9. The levels' measured behaviour is in the spec's Facts.)

- [ ] **Step 4: Run** `cargo test -p gg-audio smart::tests`, `cargo test -p gg-audio mic::tests`,
  then the workspace tests and clippy. Expected: `4 passed`, then `6 passed`;
  `passed=286 failed=0`; clippy `0`.
- [ ] **Step 5: Commit** — `gg-audio: Smart Volume's compressor stage and its controls`

---

### Task 3: The stage in every channel's chain (`gg-audio/src/config.rs`)

**Files:** `crates/gg-audio/src/config.rs`; the callers of `config::dropin` in
`crates/gg-audio/src/sonar.rs` (code and tests)

**Interfaces:**
- Produces:
  `config::dropin(output: &str, mic_input: Option<&str>, streamer: bool, smart_volume: bool) -> Result<String>`.
- Consumes: `smart::{NODES, INPUTS, LINKS, VOLUME_CONTROL}`.

- [ ] **Step 1: Failing tests.** In the `config.rs` tests, change every existing
  `dropin(a, b, c)` to `dropin(a, b, c, false)`, then add:

```rust
    #[test]
    fn smart_volume_ends_every_channels_chain() {
        let text = dropin("alsa_output.x", None, false, true).unwrap();
        assert_eq!(text.matches(r#"name = sv_comp plugin = "sc4m_1916" label = sc4m"#).count(), 4);
        assert_eq!(
            text.matches(r#"{ output = "eq_10:Out" input = "sv_comp:Input" }"#).count(),
            4,
            "after the channel EQ"
        );
        assert_eq!(
            text.matches(r#"{ output = "eq_10:Out" input = "sv_mix:In 1" }"#).count(),
            4,
            "the mix also hears the EQ itself, for when Smart Volume is off"
        );
        assert_eq!(text.matches(r#"{ output = "sv_mix:Out" input = "vol:In" }"#).count(), 4);
        assert_eq!(
            text.matches("{ type = builtin name = vol label = linear control = { \"Mult\" = 1.0 } }\n          ]").count(),
            4,
            "the volume is each chain's last node, so it is the graph's output"
        );
        assert_eq!(text.matches("capture.volumes = [").count(), 4);
        assert_eq!(text.matches(r#"{ control = "vol:Mult" }"#).count(), 4, "the sink's volume comes after the compressor");
        assert_eq!(text.matches("input = ").count(), 4 * (22 + 4));
        assert_eq!(
            text.matches("type = ladspa").count(),
            4,
            "the compressor is the only plugin in a channel from outside pipewire-audio"
        );
        assert_eq!(
            text.matches("{ name = libpipewire-module-filter-chain\n      flags = [ nofail ]").count(),
            4,
            "a graph PipeWire refuses must cost the channels, not PipeWire"
        );
    }

    #[test]
    fn without_smart_volume_the_chain_is_as_before() {
        let text = dropin("alsa_output.x", None, false, false).unwrap();
        assert!(!text.contains("sv_") && !text.contains("capture.volumes") && !text.contains("name = vol "));
        assert!(!text.contains("type = ladspa"), "no optional plugin in a channel");
        assert!(!text.contains("nofail"), "the channel modules are as before");
        assert_eq!(
            text.matches("{ type = builtin name = eq_10 label = bq_highshelf control = { \"Freq\" = 16000.0 \"Q\" = 0.707 \"Gain\" = 0.0 } }\n          ]").count(),
            4,
            "the channel EQ still ends the chain"
        );
        assert_eq!(text.matches("input = ").count(), 4 * 22);
    }

    #[test]
    fn the_smart_volume_dropin_parses() {
        if std::process::Command::new("spa-json-dump").arg("--help").output().is_err() {
            return; // not installed here: nothing to check against
        }
        let dir = crate::testing::scratch("smart-parse");
        let texts = [dropin("hs", None, false, true).unwrap(), dropin("hs", Some("mic"), true, true).unwrap()];
        for (i, text) in texts.iter().enumerate() {
            let path = dir.join(format!("{i}.conf"));
            std::fs::write(&path, text).unwrap();
            let out = std::process::Command::new("spa-json-dump").arg(&path).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
    }
```

In `sonar.rs`:
- `enable` and `expected_dropin` pass `false` as the new last argument. Task 4 replaces it, and
  the one-task gap goes in the ledger.
- The tests that call `config::dropin(…)` (`an_outdated_dropin_is_rewritten_with_one_restart`,
  `a_current_dropin_is_left_alone`, `plugins_installed_later_add_the_mic_on_startup`) add `false`.

- [ ] **Step 2: Run** `cargo test -p gg-audio config`. Expected: compile errors (`dropin` takes 3
  arguments).

- [ ] **Step 3: Implement**
- The module doc's chain sentence gains Smart Volume:
  "Each channel is a filter chain: a virtual sink apps play into, the EQ stages, Smart Volume
  (when swh's compressor is installed), and an outlet stream that plays the result into the
  chosen physical output."
- `use crate::{eq, smart};`.
- `dropin` takes `smart_volume: bool`; its doc comment gains "…and Smart Volume at the end of
  every channel's chain when `smart_volume` is set (only when PipeWire can load its compressor;
  see [`smart`]). The channel modules are then `nofail`: if PipeWire refuses the stage, it still
  starts, without the channels, and gglinux takes the stage out." It passes the flag on as
  `graph = filter_graph(smart_volume)`.
- With `smart_volume`, each channel module gets a `flags` line, like `stream_feed`'s. The template's
  first line becomes `    {{ name = libpipewire-module-filter-chain{flags}`, with the argument:

```rust
            // A graph PipeWire refuses then costs the channels, not PipeWire. Without the
            // stage the modules are as before, so existing drop-ins stay byte-for-byte.
            flags = if smart_volume { "\n      flags = [ nofail ]" } else { "" },
```
- `filter_graph` becomes:

```rust
/// The chain every channel runs: preamp, headphone correction, channel EQ,
/// then, with `smart_volume`, Smart Volume and the channel's volume.
/// Everything starts flat and off; gglinux sets the controls live.
fn filter_graph(smart_volume: bool) -> String {
    let headphone = eq::flat_headphone().bands;
    let channel = eq::flat_channel();
    let stages: Vec<(String, gg_ipc::EqBand)> = headphone
        .iter()
        .enumerate()
        .map(|(i, b)| (format!("hp_{}", i + 1), *b))
        .chain(channel.iter().enumerate().map(|(i, b)| (format!("eq_{}", i + 1), *b)))
        .collect();

    let mut nodes =
        vec![r#"            { type = builtin name = pre label = linear control = { "Mult" = 1.0 } }"#.to_string()];
    for (name, band) in &stages {
        nodes.push(format!(
            r#"            {{ type = builtin name = {name} label = {label} control = {{ "Freq" = {freq:.1} "Q" = {q:.3} "Gain" = 0.0 }} }}"#,
            label = filter_label(band.kind),
            freq = band.freq,
            q = band.q,
        ));
    }
    let names: Vec<&str> =
        std::iter::once("pre").chain(stages.iter().map(|(n, _)| n.as_str())).collect();
    let link = |output: &str, input: &str| format!(r#"            {{ output = "{output}" input = "{input}" }}"#);
    let mut links: Vec<String> =
        names.windows(2).map(|w| link(&format!("{}:Out", w[0]), &format!("{}:In", w[1]))).collect();
    let mut volumes = String::new();
    if smart_volume {
        let last = format!("{}:Out", names.last().expect("the chain has stages"));
        nodes.extend(smart::NODES.iter().map(|n| format!("            {n}")));
        links.extend(smart::INPUTS.iter().map(|&input| link(&last, input)));
        links.extend(smart::LINKS.iter().map(|&(output, input)| link(output, input)));
        // The sink's volume moves from before the chain to after the compressor.
        volumes = format!(
            "\n          capture.volumes = [\n            {{ control = \"{}\" }}\n          ]",
            smart::VOLUME_CONTROL
        );
    }
    format!(
        "          nodes = [\n{}\n          ]\n          links = [\n{}\n          ]{volumes}",
        nodes.join("\n"),
        links.join("\n")
    )
}
```

- `dropin`'s call becomes `graph = filter_graph(smart_volume),`.
- Without `smart_volume`, the output is byte-for-byte what `dropin` printed before: no `flags`
  line, and the links built by the same format, just through the `link` closure.

- [ ] **Step 4: Run** `cargo test -p gg-audio`, then the workspace tests and clippy. Expected: all
  pass, including `the_smart_volume_dropin_parses` (`/usr/bin/spa-json-dump` is installed here);
  `passed=289 failed=0`; clippy `0`.
- [ ] **Step 5: Commit** — `gg-audio: Smart Volume at the end of every channel's chain`

---

### Task 4: Settings, the Sonar API and state (`gg-audio`)

**Files:** `crates/gg-audio/src/tools.rs`, `crates/gg-audio/src/settings.rs`,
`crates/gg-audio/src/sonar.rs`

**Interfaces:**
- Produces:
  - `PwTools::smart_volume_supported(&self) -> bool`, which defaults to
    `smart::compressor_present()`;
  - `Settings.smart_volume: BTreeMap<Channel, SmartVolume>` and
    `Settings::smart_volume(Channel) -> SmartVolume`;
  - `Sonar::set_smart_volume(Channel, SmartVolume) -> Result<()>`;
  - private `Sonar::{smart_volume_unavailable() -> Option<&'static str>, smart_volume_available() -> bool}`.
    Task 5 adds the refusal to the first;
  - `SonarState.smart_volume` filled in for real, `reason` included.

- [ ] **Step 1: Failing tests**

In `settings.rs` tests:

```rust
    #[test]
    fn settings_from_before_smart_volume_still_load() {
        let path = scratch("settings-old").join("sonar.json");
        std::fs::write(&path, r#"{"enabled":true,"output":"alsa_output.x","chatmix":0.5}"#).unwrap();
        let s = load(&path);
        assert!(s.enabled && s.smart_volume.is_empty());
        assert_eq!(s.smart_volume(Channel::Game), gg_ipc::SmartVolume::default());
    }
```

In the `sonar.rs` tests:
- `Fake` gains a field `smart_volume: RefCell<bool>`, whether swh's compressor is installed
  (`RefCell::new(true)` in `new`; Task 5 flips it mid-test), and
  `fn smart_volume_supported(&self) -> bool { *self.smart_volume.borrow() }` in its `PwTools`
  impl.
- The test import becomes `use gg_ipc::{HeadphoneEq, MicSettings, SmartLevel};`.
- The three tests from Task 3 that build drop-ins with `config::dropin(…, false)` now pass `true`.
  The fake has the compressor, so the expected drop-in carries the stage:
  `an_outdated_dropin_is_rewritten_with_one_restart`, `a_current_dropin_is_left_alone`,
  `plugins_installed_later_add_the_mic_on_startup`.

Add:

```rust
    #[test]
    fn enable_sets_every_channels_smart_volume() {
        let (settings_path, dropin) = paths("smart-enable");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        assert!(std::fs::read_to_string(&dropin).unwrap().contains("name = sv_comp"));
        let off = crate::smart::props(&SmartVolume::default());
        for id in ["20", "21", "22", "23"] {
            assert!(fake.ran(&["pw-cli", "set-param", id, "Props", &off]), "channel {id} starts off");
        }
    }

    #[test]
    fn smart_volume_switches_live_without_a_restart() {
        let (settings_path, dropin) = paths("smart-live");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        sonar.enable().unwrap();
        let restarts = fake.count("systemctl");
        let loud = SmartVolume { enabled: true, level: SmartLevel::Loud };
        sonar.set_smart_volume(Channel::Media, loud).unwrap();
        assert!(fake.ran(&["pw-cli", "set-param", "22", "Props", &crate::smart::props(&loud)]));
        let off = SmartVolume { enabled: false, ..loud };
        sonar.set_smart_volume(Channel::Media, off).unwrap();
        assert!(fake.ran(&["pw-cli", "set-param", "22", "Props", &crate::smart::props(&off)]));
        assert_eq!(fake.count("systemctl"), restarts, "never a PipeWire restart");
        assert_eq!(settings::load(&settings_path).smart_volume(Channel::Media), off);
        let s = sonar.snapshot().smart_volume;
        assert!(s.available && s.reason.is_none());
        assert_eq!(s.settings, vec![SmartVolume::default(), SmartVolume::default(), off, SmartVolume::default()]);
    }

    #[test]
    fn without_the_compressor_the_channels_are_as_before() {
        let (settings_path, dropin) = paths("smart-none");
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.smart_volume.borrow_mut() = false;
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, false).unwrap());
        let s = sonar.snapshot().smart_volume;
        assert!(!s.available, "{s:?}");
        assert!(s.reason.as_deref().is_some_and(|r| r.contains("Install swh-plugins")), "asks for the package: {s:?}");
        let on = SmartVolume { enabled: true, ..SmartVolume::default() };
        assert!(sonar.set_smart_volume(Channel::Game, on).is_err());
        assert!(
            !fake.runs.borrow().iter().any(|r| r[0] == "pw-cli" && r[4].contains("sv_mix")),
            "no Smart Volume controls sent"
        );
    }

    #[test]
    fn a_compressor_that_disappears_takes_the_stage_out_on_startup() {
        let (settings_path, dropin) = paths("smart-gone");
        let saved = Settings { enabled: true, output: Some(BT.into()), mic_input: Some(MIC.into()), ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        std::fs::write(&dropin, config::dropin(BT, Some(MIC), false, true).unwrap()).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.smart_volume.borrow_mut() = false; // swh-plugins removed while gglinux was off
        let _sonar = Sonar::new(&fake, settings_path, dropin.clone());
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, false).unwrap());
        assert_eq!(fake.count("systemctl"), 1, "one restart");
    }

    #[test]
    fn a_compressor_that_appears_adds_the_stage_on_startup() {
        let (settings_path, dropin) = paths("smart-new");
        let saved = Settings { enabled: true, output: Some(BT.into()), mic_input: Some(MIC.into()), ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        std::fs::write(&dropin, config::dropin(BT, Some(MIC), false, false).unwrap()).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        let _sonar = Sonar::new(&fake, settings_path, dropin.clone());
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, true).unwrap());
        assert_eq!(fake.count("systemctl"), 1, "one restart");
    }
```

- [ ] **Step 2: Run** `cargo test -p gg-audio`. Expected: compile errors (`smart_volume`,
  `set_smart_volume`, `smart_volume_supported` missing).

- [ ] **Step 3: Implement**

`tools.rs`, in `PwTools` after `mic_plugins_present`:

```rust
    /// Whether PipeWire can load Smart Volume's compressor (swh's `sc4m_1916`).
    fn smart_volume_supported(&self) -> bool {
        crate::smart::compressor_present()
    }
```

`settings.rs`:
- Import `SmartVolume` from `gg_ipc`.
- `Settings` gains, after `stream_master`:

```rust
    /// Smart Volume per channel; off when unset.
    pub smart_volume: BTreeMap<Channel, SmartVolume>,
```

- `Default` gains `smart_volume: BTreeMap::new(),`.
- `impl Settings` gains:

```rust
    /// A channel's Smart Volume, off if never set.
    pub fn smart_volume(&self, channel: Channel) -> SmartVolume {
        self.smart_volume.get(&channel).copied().unwrap_or_default()
    }
```

`sonar.rs`:
1. `use crate::{config, eq, mic, mix, smart};`.
2. `enable` writes
   `config::dropin(&output, mic_input.as_deref(), self.settings.streamer, self.smart_volume_available())?`.
   `expected_dropin` ends `config::dropin(output, mic, self.settings.streamer, self.smart_volume_available())`.
   Both replace Task 3's `false`.
3. After `set_stream_master`:

```rust
    /// Turn Smart Volume on or off for a channel, or change its level: live,
    /// through the stage's own controls, never a PipeWire restart.
    pub fn set_smart_volume(&mut self, channel: Channel, smart_volume: SmartVolume) -> Result<()> {
        if let Some(why) = self.smart_volume_unavailable() {
            bail!("{why}");
        }
        self.settings.smart_volume.insert(channel, smart_volume);
        self.persist();
        self.apply_smart_volume(channel)
    }
```

4. In the internals, after `choose_mic`:

```rust
    /// Why Smart Volume can't be used here, if it can't.
    fn smart_volume_unavailable(&self) -> Option<&'static str> {
        if !self.tools.smart_volume_supported() {
            Some(
                "Smart Volume needs the SC4 compressor from swh-plugins (sc4m_1916.so), \
                 which isn't installed. Install swh-plugins, then turn Sonar off and on.",
            )
        } else {
            None
        }
    }

    /// Whether the channels carry Smart Volume's stage, or will on the next write.
    fn smart_volume_available(&self) -> bool {
        self.smart_volume_unavailable().is_none()
    }
```

5. Next to `apply_chain`:

```rust
    /// Set a channel's Smart Volume controls, if PipeWire has the channel and
    /// the drop-in carries the stage.
    fn apply_smart_volume(&self, channel: Channel) -> Result<()> {
        if !self.smart_volume_available() {
            return Ok(());
        }
        if let Some(node) = self.graph.node(&config::sink_name(channel)) {
            let props = smart::props(&self.settings.smart_volume(channel));
            self.tools.run(&tools::set_params(node.id, &props))?;
        }
        Ok(())
    }
```

6. `apply_levels`: the channel loop becomes

```rust
        for channel in Channel::ALL {
            self.apply_chain(channel)?;
            self.apply_smart_volume(channel)?;
        }
```

   Its doc comment: "Push saved channel volumes, EQ, Smart Volume, the mic and ChatMix to PipeWire."
7. `snapshot()` replaces Task 1's placeholder with:

```rust
            smart_volume: SmartVolumeState {
                available: self.smart_volume_available(),
                reason: self.smart_volume_unavailable().map(str::to_owned),
                settings: Channel::ALL.iter().map(|&c| self.settings.smart_volume(c)).collect(),
            },
```

- [ ] **Step 4: Run** the workspace tests and clippy. Expected: `passed=295 failed=0`, the 6 new
  tests among them; clippy `0`.
- [ ] **Step 5: Commit** — `gg-audio: Smart Volume per channel, switched live`

---

### Task 5: A stage PipeWire can't load is taken out (`gg-audio`)

**Files:** `crates/gg-audio/src/settings.rs`, `crates/gg-audio/src/sonar.rs`

**Interfaces:**
- Produces:
  - `Settings.smart_volume_refused: bool`;
  - in `sonar.rs`, a module-level `fn channels_up(&Graph) -> bool`, and the private
    `Sonar::{dropin_mic() -> Option<String>, staged() -> bool, check_stage(), drop_refused_stage(&str, Option<&str>) -> bool, follow_compressor()}`;
  - the field `Sonar.compressor_seen: bool`, and `tick` calling `follow_compressor`;
  - `smart_volume_unavailable()` also reports a refusal.
- The test `Fake` gains `refuses_stage: RefCell<bool>` (false in `new`).
- Consumes: Task 3's `nofail` channel modules, `smart::INPUTS` to recognise a drop-in that
  carries the stage, and Task 4's `Fake.smart_volume`.

- [ ] **Step 1: Failing tests**

In `settings.rs`, `settings_from_before_smart_volume_still_load` gains
`assert!(!s.smart_volume_refused);`.

In the `sonar.rs` tests:
- `Fake` gains, after `channels_come_up`:

```rust
        /// A PipeWire that refuses Smart Volume's stage: with `sv_comp` in the
        /// drop-in it starts without the channels (their modules are `nofail`).
        refuses_stage: RefCell<bool>,
```

  `new` sets `refuses_stage: RefCell::new(false),`. Its `run` becomes:

```rust
        fn run(&self, argv: &[String]) -> Result<()> {
            if argv[0] == "systemctl" {
                let refused = *self.refuses_stage.borrow()
                    && std::fs::read_to_string(&self.dropin).is_ok_and(|t| t.contains("sv_comp"));
                *self.up.borrow_mut() = self.channels_come_up && self.dropin.exists() && !refused;
            }
            self.runs.borrow_mut().push(argv.to_vec());
            Ok(())
        }
```

- `enable_rolls_back_when_the_channels_never_appear`: its last two assertions become

```rust
        assert_eq!(fake.count("systemctl"), 3, "to apply, again without Smart Volume's stage, then to undo");
        let saved = settings::load(&settings_path);
        assert!(!saved.enabled && !saved.smart_volume_refused, "the stage wasn't to blame");
```

- `a_current_dropin_is_left_alone`: before `Sonar::new`, add
  `*fake.up.borrow_mut() = true; // PipeWire already runs this drop-in`. The startup check
  would otherwise read the fake's channels being absent as a refusal.

Add:

```rust
    #[test]
    fn a_refused_stage_is_taken_out_when_sonar_turns_on() {
        let (settings_path, dropin) = paths("smart-refused");
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.refuses_stage.borrow_mut() = true;
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        sonar.enable().unwrap();
        assert!(sonar.is_enabled(), "the channels run, without Smart Volume");
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, false).unwrap());
        assert_eq!(fake.count("systemctl"), 2, "once with the stage, once without");
        assert!(settings::load(&settings_path).smart_volume_refused);
        let s = sonar.snapshot().smart_volume;
        assert!(!s.available && s.reason.as_deref().is_some_and(|r| r.contains("off and on")), "{s:?}");
        let on = SmartVolume { enabled: true, ..SmartVolume::default() };
        assert!(sonar.set_smart_volume(Channel::Game, on).is_err());
        sonar.disable().unwrap();
        assert!(!settings::load(&settings_path).smart_volume_refused, "turning Sonar on again tries the stage again");
    }

    #[test]
    fn a_refused_stage_is_taken_out_on_startup() {
        let (settings_path, dropin) = paths("smart-refused-upgrade");
        let saved = Settings { enabled: true, output: Some(BT.into()), mic_input: Some(MIC.into()), ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        // Sonar was on before Smart Volume, so the startup upgrade adds the stage.
        let before = config::dropin(BT, Some(MIC), false, false).unwrap();
        std::fs::write(&dropin, &before).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.refuses_stage.borrow_mut() = true;
        let sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        assert!(sonar.is_enabled());
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), before);
        assert_eq!(fake.count("systemctl"), 2, "the upgrade, then the stage taken out");
        assert!(settings::load(&settings_path).smart_volume_refused);
        assert!(!sonar.snapshot().smart_volume.available);
    }

    #[test]
    fn a_stage_refused_at_login_is_taken_out_on_startup() {
        let (settings_path, dropin) = paths("smart-refused-login");
        let saved = Settings { enabled: true, output: Some(BT.into()), mic_input: Some(MIC.into()), ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        // The drop-in is current, but this PipeWire (a newer one, say) refused it at login.
        std::fs::write(&dropin, config::dropin(BT, Some(MIC), false, true).unwrap()).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.refuses_stage.borrow_mut() = true;
        let _sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, false).unwrap());
        assert_eq!(fake.count("systemctl"), 1, "one restart, without the stage");
        assert!(settings::load(&settings_path).smart_volume_refused);
    }

    #[test]
    fn a_stage_refused_after_a_streamer_restart_is_taken_out() {
        let (settings_path, dropin) = paths("smart-refused-streamer");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        sonar.enable().unwrap();
        *fake.refuses_stage.borrow_mut() = true; // PipeWire was upgraded since
        sonar.set_streamer(true).unwrap();
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), true, false).unwrap());
        assert_eq!(fake.count("systemctl"), 3, "on, streamer mode, then without the stage");
        assert!(sonar.is_enabled() && settings::load(&settings_path).smart_volume_refused);
    }

    #[test]
    fn a_compressor_removed_while_sonar_runs_leaves_the_dropin_at_once() {
        let (settings_path, dropin) = paths("smart-removed");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        let restarts = fake.count("systemctl");
        *fake.smart_volume.borrow_mut() = false; // swh-plugins removed
        sonar.tick();
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, false).unwrap());
        assert_eq!(fake.count("systemctl"), restarts, "PipeWire still runs the stage it loaded: no restart");
        assert!(!sonar.snapshot().smart_volume.available);
        sonar.tick();
        assert_eq!(fake.count("systemctl"), restarts, "once is enough");
    }

    #[test]
    fn a_compressor_removed_before_a_pipewire_restart_brings_the_channels_back() {
        let (settings_path, dropin) = paths("smart-removed-restart");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        let restarts = fake.count("systemctl");
        // swh-plugins removed, and PipeWire restarted by something else before gglinux
        // looked: it skipped the channel modules.
        *fake.smart_volume.borrow_mut() = false;
        *fake.up.borrow_mut() = false;
        sonar.tick();
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, false).unwrap());
        assert_eq!(fake.count("systemctl"), restarts + 1, "one restart, without the stage");
        assert!(sonar.snapshot().channels.iter().all(|c| c.present));
    }
```

- [ ] **Step 2: Run** `cargo test -p gg-audio`. Expected: compile errors (`smart_volume_refused`
  missing).

- [ ] **Step 3: Implement**

`settings.rs`: `Settings` gains, after `smart_volume`:

```rust
    /// PipeWire wouldn't make the channels with Smart Volume's stage, so they
    /// run without it. Only ever set while Sonar is on: turning Sonar off
    /// forgets it, so turning it on again tries the stage again.
    pub smart_volume_refused: bool,
```

`Default` gains `smart_volume_refused: false,`.

`sonar.rs`:
1. After `APPEAR_POLL`:

```rust
/// Whether PipeWire has all four channel sinks.
fn channels_up(g: &Graph) -> bool {
    Channel::ALL.iter().all(|&c| g.node(&config::sink_name(c)).is_some())
}
```

   `enable` drops its `let channels_up = …;` closure (its `all_up` now calls the function).
   `set_streamer`'s `ready` becomes
   `|g: &Graph| channels_up(g) && (!enabled || g.node(config::STREAM_SINK).is_some())`.
2. `smart_volume_unavailable` gains, before its `else`:

```rust
        } else if self.settings.smart_volume_refused {
            Some(
                "PipeWire wouldn't make the channels with Smart Volume, so they run without it. \
                 Turn Sonar off and on to try again.",
            )
```

3. `expected_dropin` takes its mic from a new helper next to it:

```rust
    /// The mic the drop-in carries: the saved one, if the plugins exist.
    fn dropin_mic(&self) -> Option<String> {
        self.settings.mic_input.clone().filter(|_| self.tools.mic_plugins_present())
    }

    /// The drop-in this version writes for the saved output and mic.
    fn expected_dropin(&self, output: &str) -> Result<String> {
        config::dropin(output, self.dropin_mic().as_deref(), self.settings.streamer, self.smart_volume_available())
    }
```

4. After `upgrade_dropin`:

```rust
    /// Whether the drop-in on disk carries Smart Volume's stage.
    fn staged(&self) -> bool {
        std::fs::read_to_string(&self.dropin_path).is_ok_and(|t| t.contains(smart::INPUTS[0]))
    }

    /// On startup with Sonar on, whether or not the drop-in was just rewritten:
    /// a PipeWire upgraded since may refuse a stage it took before.
    fn check_stage(&mut self) {
        let Some(output) = self.settings.output.clone() else { return };
        if self.staged() && !self.wait_until(channels_up) {
            let mic = self.dropin_mic();
            self.drop_refused_stage(&output, mic.as_deref());
        }
    }

    /// The channels didn't come. If the drop-in carries Smart Volume's stage
    /// and PipeWire answers, PipeWire refused the stage: the channel modules
    /// that carry it are `nofail`, so it started without them. Remember that
    /// (first, so a crash can't bring the stage back), write the drop-in
    /// without the stage and restart PipeWire once more. Returns whether the
    /// channels are up now.
    fn drop_refused_stage(&mut self, output: &str, mic: Option<&str>) -> bool {
        if !self.staged() || !self.available {
            return false;
        }
        warn!("PipeWire did not make the Sonar channels with Smart Volume's stage; going on without it");
        self.settings.smart_volume_refused = true;
        self.persist();
        let result = config::dropin(output, mic, self.settings.streamer, false)
            .and_then(|text| {
                std::fs::write(&self.dropin_path, text)
                    .with_context(|| format!("writing {}", self.dropin_path.display()))
            })
            .and_then(|()| self.tools.run(&tools::restart_pipewire()));
        if let Err(e) = result {
            warn!(error = %format!("{e:#}"), "could not take Smart Volume's stage out");
            return false;
        }
        self.wait_until(channels_up)
    }
```

5. `new`: `sonar.upgrade_dropin();` is followed by `sonar.check_stage();`. Its doc comment gains
   "With Sonar on, it also checks that PipeWire took Smart Volume's stage."
6. `enable`: after its `if self.settings.enabled { return Ok(()); }`, add
   `self.settings.smart_volume_refused = false; // Sonar is off: a refusal on record is stale`.
   The branch for missing channels becomes

```rust
            if channels_up(&self.graph) {
                // The mic module is nofail: the channels are fine without it.
                warn!("the Sonar mic did not load; continuing without it");
            } else if !self.drop_refused_stage(&output, mic_input.as_deref()) {
                warn!("Sonar channels did not appear; undoing");
                let _ = std::fs::remove_file(&self.dropin_path);
                let _ = self.tools.run(&tools::restart_pipewire());
                // Not the stage's fault: turning Sonar on again tries it again.
                self.settings.smart_volume_refused = false;
                self.persist();
                bail!(
                    "PipeWire did not create the Sonar channels within {} s, so the change was undone",
                    APPEAR_TIMEOUT.as_secs()
                );
            }
            // Otherwise PipeWire refused Smart Volume's stage: the channels run without it.
```

7. `set_streamer`: the warning after `wait_until(ready)` becomes

```rust
        if !self.wait_until(ready) {
            let mic = self.dropin_mic();
            if channels_up(&self.graph) || !self.drop_refused_stage(&output, mic.as_deref()) {
                // Never undone for this: the channels may be fine without the stream.
                warn!(enabled, "the channels or the stream mix did not appear after the restart");
            }
        }
```

8. `disable`: after `self.settings.enabled = false;`, add
   `self.settings.smart_volume_refused = false; // turning Sonar on again tries the stage again`.
9. `Sonar` gains, after `stream_levels_applied`:

```rust
    /// Whether Smart Volume's compressor was installed at the last look, so
    /// `tick` notices it going.
    compressor_seen: bool,
```

   `new` reads it before building the struct,
   `let compressor_seen = tools.smart_volume_supported();`, and stores it.
10. `tick` calls `self.follow_compressor();` first inside its `if self.settings.enabled`, before
    `keep_levels`, so levels are pushed to channels it brought back. After `check_stage`:

```rust
    /// `swh-plugins` was removed while Sonar is on: write the drop-in without
    /// Smart Volume's stage at once, so no later PipeWire start, gglinux's or
    /// anyone's, loads a plugin that isn't there. PipeWire keeps the copy it
    /// loaded until then. If it already restarted without the channels,
    /// restart it once more to bring them back.
    fn follow_compressor(&mut self) {
        let present = self.tools.smart_volume_supported();
        let gone = std::mem::replace(&mut self.compressor_seen, present) && !present;
        if !gone || !self.staged() {
            return;
        }
        let Some(output) = self.settings.output.clone() else { return };
        warn!("Smart Volume's compressor (swh-plugins) is gone; taking it out of the Sonar channels");
        let written = self.expected_dropin(&output).and_then(|text| {
            std::fs::write(&self.dropin_path, text).with_context(|| format!("writing {}", self.dropin_path.display()))
        });
        if let Err(e) = written {
            warn!(error = %format!("{e:#}"), "could not take Smart Volume's stage out");
            return;
        }
        if !channels_up(&self.graph) {
            info!("restarting PipeWire to bring the Sonar channels back without Smart Volume");
            match self.tools.run(&tools::restart_pipewire()) {
                Ok(()) => {
                    self.wait_until(channels_up);
                }
                Err(e) => warn!(error = %format!("{e:#}"), "could not restart PipeWire"),
            }
        }
    }
```

   It reads the drop-in only on the tick the compressor goes, so a machine without
   `swh-plugins` pays three `stat`s a second, not a file read.

- [ ] **Step 4: Run** the workspace tests and clippy. Expected: `passed=301 failed=0`, the 6 new
  tests among them; clippy `0`.
- [ ] **Step 5: Commit** — `gg-audio: a Smart Volume stage PipeWire can't load is taken out`

---

### Task 6: Smart Volume requests in the daemon

**Files:** `crates/gglinuxd/src/sonar.rs`, `crates/gglinuxd/src/main.rs`

**Interfaces:**
- Produces: `sonar::Command::SetSmartVolume { channel: Channel, settings: SmartVolume, reply: Reply<()> }`.
- Consumes: `Sonar::set_smart_volume`.

- [ ] **Step 1: Implement**
- `sonar.rs`:
  - import `SmartVolume`;
  - `Command` gains `SetSmartVolume { channel: Channel, settings: SmartVolume, reply: Reply<()> },`
    after `SetStreamMaster`;
  - `handle` gains:

```rust
        Command::SetSmartVolume { channel, settings, reply } => {
            let _ = reply.send(sonar.set_smart_volume(channel, settings).map_err(text));
        }
```

- `main.rs` replaces Task 1's placeholder with:

```rust
        Request::SetSmartVolume { channel, settings } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetSmartVolume { channel, settings, reply })
                .await,
        ),
```

- [ ] **Step 2: Run** the workspace tests and clippy. Expected: `passed=301 failed=0`; clippy `0`.
- [ ] **Step 3: Commit** — `gglinuxd: Smart Volume requests`

---

### Task 7: Profiles share the running game (`gglinuxd/src/profiles.rs`)

**Files:** `crates/gglinuxd/src/profiles.rs`, `crates/gglinuxd/src/main.rs`

**Interfaces:**
- Produces:
  - `profiles::spawn(event_tx, keyboard, game_tx: watch::Sender<Option<String>>) -> Handle`;
  - `Controller::new(store, path, port, event_tx, game_tx)`.
- The published value is the `process` of `pick()`'s rule, as the rule spells it, or `None`.
- `main.rs` holds the receiver as `_game_rx` until Task 8.

- [ ] **Step 1: Failing test.**
- Import `watch` alongside `broadcast, mpsc, oneshot`.
- In the tests, `controller()` passes `watch::channel(None).0` as the new last argument of
  `Controller::new`.
- Add:

```rust
    /// A keyboard that isn't there.
    struct NoKeyboard;

    impl LightingPort for NoKeyboard {
        async fn get(&self) -> Result<Lighting, String> {
            Err("no keyboard".into())
        }
        async fn set(&self, _: Lighting) -> Result<(), String> {
            Err("no keyboard".into())
        }
    }

    #[tokio::test]
    async fn the_running_game_is_shared_even_without_a_keyboard() {
        let (with_keyboard, _now) = controller();
        let (event_tx, _) = broadcast::channel(8);
        let (game_tx, game) = watch::channel(None);
        let mut c = Controller::new(with_keyboard.store, with_keyboard.path, NoKeyboard, event_tx, game_tx);
        c.follow(&running(&["game-b"])).await;
        assert_eq!(game.borrow().as_deref(), Some("game-b"), "clips hear of the game");
        assert!(c.active.is_none(), "though no lighting was applied");
        c.follow(&running(&[])).await;
        assert_eq!(*game.borrow(), None);
    }
```

- [ ] **Step 2: Run** `cargo test -p gglinuxd profiles`. Expected: compile errors (`new` takes 4
  arguments).

- [ ] **Step 3: Implement**
- The module doc gains a sentence: "The game it finds is also published for games-only clips."
- `Controller` gains, after `before`:

```rust
    /// The game from the rules that's running now, for games-only clips.
    game_tx: watch::Sender<Option<String>>,
```

- `new` takes `game_tx: watch::Sender<Option<String>>` last and stores it.
- In `follow`, right after `let picked = pick(&self.store, running).cloned();`:

```rust
        // Before the lighting, which can fail: clips follow the game regardless.
        let game = picked.as_ref().map(|r| r.process.clone());
        self.game_tx.send_if_modified(|now| {
            if *now == game {
                return false;
            }
            *now = game;
            true
        });
```

- `spawn` takes `game_tx: watch::Sender<Option<String>>` and passes it to `Controller::new`. The
  scan's skip becomes:

```rust
                    if c.store.rules.is_empty() && c.active.is_none() && c.game_tx.borrow().is_none() {
                        continue; // nothing to watch for
                    }
```

- `main.rs`, before `let workers = Workers {`:

```rust
    // The game from the profiles' rules that's running now; games-only clips follow it.
    let (game_tx, _game_rx) = watch::channel::<Option<String>>(None);
```

  and `profiles: profiles::spawn(event_tx.clone(), keyboard.clone(), game_tx),`.

- [ ] **Step 4: Run** the workspace tests and clippy. Expected: `passed=302 failed=0`; clippy `0`.
- [ ] **Step 5: Commit** — `gglinuxd: profiles share which game is running`

---

### Task 8: Games-only clips (`gglinuxd/src/clips.rs`)

**Files:** `crates/gglinuxd/src/clips.rs`, `crates/gglinuxd/src/main.rs`

**Interfaces:**
- Produces:
  - `clips::spawn(event_tx, games: watch::Receiver<Option<String>>) -> Handle`;
  - `Controller::{set_game(Option<String>), linger_over()}` (async);
  - the fields `game` and `linger_until`.
- Consumes: Task 7's channel.

- [ ] **Step 1: Failing tests** (append to the `clips.rs` tests)

```rust
    fn games_only(dir: &Path, program: PathBuf) -> (Controller, mpsc::UnboundedReceiver<Internal>) {
        let (mut c, internal) = controller(dir, program);
        c.settings.games_only = true;
        (c, internal)
    }

    #[tokio::test]
    async fn games_only_waits_for_a_game_and_stops_after_it_leaves() {
        let d = scratch("games-only");
        let (mut c, _internal) = games_only(&d, stand_in(&d, false));
        start(&mut c).await;
        let s = c.state();
        assert!(!s.recording && s.waiting, "no game yet");
        assert!(!d.join("log").exists(), "nothing was started");
        c.set_game(Some("game-a".into())).await;
        start(&mut c).await; // retried if the stand-in was busy
        assert!(log_has(&d, "start").await);
        let s = c.state();
        assert!(s.recording && !s.waiting);
        assert_eq!(s.game.as_deref(), Some("game-a"));
        c.set_game(None).await;
        assert!(c.state().recording, "kept through the grace, so the game's last moments can be saved");
        assert!(c.linger_until.is_some());
        c.linger_over().await;
        assert!(log_has(&d, "int").await, "stopped with SIGINT");
        let s = c.state();
        assert!(!s.recording && s.waiting);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn a_game_back_within_the_grace_keeps_the_recorder() {
        let d = scratch("games-back");
        let (mut c, _internal) = games_only(&d, stand_in(&d, false));
        c.set_game(Some("game-a".into())).await;
        start(&mut c).await;
        assert!(log_has(&d, "start").await);
        c.set_game(None).await;
        c.set_game(Some("game-a".into())).await;
        assert!(c.linger_until.is_none());
        c.linger_over().await; // a timer that fires late changes nothing
        assert!(c.state().recording);
        let log = std::fs::read_to_string(d.join("log")).unwrap();
        assert_eq!(log.lines().filter(|l| l.starts_with("start")).count(), 1, "one recorder throughout:\n{log}");
        assert!(!log.lines().any(|l| l == "int"), "{log}");
        c.stop().await;
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn turning_games_only_on_with_no_game_stops_the_recorder() {
        let d = scratch("games-on");
        let (mut c, _internal) = controller(&d, stand_in(&d, false));
        start(&mut c).await;
        assert!(log_has(&d, "start").await);
        c.set_settings(ClipsSettings { games_only: true, ..c.settings.clone() }).await.unwrap();
        assert!(log_has(&d, "int").await);
        assert!(c.state().waiting);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn a_crash_after_the_game_is_not_retried() {
        let d = scratch("games-crash");
        let (mut c, mut internal) = games_only(&d, stand_in(&d, true));
        c.set_game(Some("game-a".into())).await;
        start(&mut c).await;
        c.set_game(None).await;
        let exited = next_exit(&mut internal).await;
        c.on_internal(exited).await;
        assert!(c.retry_at.is_none() && c.linger_until.is_none(), "no restart without the game");
        assert!(c.state().waiting);
        std::fs::remove_dir_all(d).unwrap();
    }
```

- [ ] **Step 2: Run** `cargo test -p gglinuxd clips::tests`. Expected: compile errors (`set_game`,
  `linger_over`, `linger_until` missing).

- [ ] **Step 3: Implement** in `clips.rs`:
1. The module doc gains: "With `games_only`, it records only while a game from the profiles' rules
   runs (the profiles watcher says which)." Import `watch` with the other `tokio::sync` items.
2. After `SAVE_SETTLE`:

```rust
/// How long games-only clips keep recording after the game has gone: time to
/// save its last moments, and to bridge a launcher handing over to the game.
const GAME_GRACE: Duration = Duration::from_secs(15);
```

3. `Controller` gains, after `retry_at`:

```rust
    /// The game from the rules that's running now, from the profiles watcher.
    game: Option<String>,
    /// Games-only, the game gone: when to stop.
    linger_until: Option<Instant>,
```

   `new` starts them as `None`.
4. After `dir()`:

```rust
    /// Whether the recorder should run: clips on, and a game running if they're games-only.
    fn wanted(&self) -> bool {
        self.settings.enabled && (!self.settings.games_only || self.game.is_some())
    }
```

5. `start` begins `if self.running.is_some() || !self.wanted() { return; }`.
6. `stop` also clears `self.linger_until = None;`, next to `self.retry_at = None;`.
7. After `set_settings`:

```rust
    /// The game from the rules that's running now changed. Games-only clips
    /// start with a game, and stop [`GAME_GRACE`] after it has gone.
    pub async fn set_game(&mut self, game: Option<String>) {
        self.game = game;
        if self.game.is_some() {
            self.linger_until = None;
            self.start().await;
        } else if self.settings.games_only && self.running.is_some() && self.linger_until.is_none() {
            self.linger_until = Some(Instant::now() + GAME_GRACE);
        }
        self.publish();
    }

    /// The grace after a game is over: stop, unless the game came back.
    pub async fn linger_over(&mut self) {
        self.linger_until = None;
        if self.running.is_some() && !self.wanted() {
            info!("the game has gone; stopping the replay buffer");
            self.stop().await;
        }
    }
```

8. `on_internal`'s `Exited` arm only retries when the recorder is still wanted:

```rust
            Internal::Exited { generation } => {
                if self.running.as_ref().is_some_and(|r| r.generation == generation) {
                    let r = self.running.take().expect("checked");
                    self.linger_until = None;
                    // After the game has gone, an exit just ends it.
                    if self.wanted() {
                        self.delay = recorder::next_delay(r.started.elapsed(), self.delay);
                        self.retry_at = Some(Instant::now() + self.delay);
                        self.error = Some(format!("gpu-screen-recorder stopped; retrying in {} s", self.delay.as_secs()));
                        warn!(delay_s = self.delay.as_secs(), "gpu-screen-recorder stopped unexpectedly");
                    }
                }
            }
```

9. `state()` replaces Task 1's shim with:

```rust
            game: self.game.clone(),
            waiting: self.settings.enabled && self.settings.games_only && self.running.is_none() && self.game.is_none(),
```

10. `spawn(event_tx, mut games: watch::Receiver<Option<String>>)`: inside the task, before
    `c.start().await;`, add `c.game = games.borrow_and_update().clone();`. The loop becomes:

```rust
        loop {
            let retry = c.retry_at;
            let linger = c.linger_until;
            tokio::select! {
                command = rx.recv() => match command {
                    Some(command) => c.handle(command).await,
                    None => break,
                },
                Some(msg) = internal.recv() => c.on_internal(msg).await,
                Ok(()) = games.changed() => {
                    let game = games.borrow_and_update().clone();
                    c.set_game(game).await;
                }
                _ = async { tokio::time::sleep_until(retry.expect("guarded").into()).await }, if retry.is_some() => {
                    c.retry_at = None;
                    c.start().await;
                }
                _ = async { tokio::time::sleep_until(linger.expect("guarded").into()).await }, if linger.is_some() => {
                    c.linger_over().await;
                }
            }
        }
```

    (The `Ref` from `borrow_and_update()` is dropped at the end of its statement, before any
    `await`.)

`main.rs`: rename `_game_rx` to `game_rx`, and `clips: clips::spawn(event_tx.clone(), game_rx),`.

- [ ] **Step 4: Run** `cargo test -p gglinuxd clips`, then the workspace and clippy. Expected: the
  4 new tests pass with the existing 5 controller and 7 recorder tests; `passed=306 failed=0`;
  clippy `0`. Run `cargo test -p gglinuxd clips::tests` 10 times in a row: all green. The stand-in
  can hit `ETXTBSY`; the tests go through `start()`, which retries. Ledger any flake.
- [ ] **Step 5: Commit** — `gglinuxd: clips only while a game from the rules runs`

---

### Task 9: The UI

**Files:** `ui/src/lib/types.ts`, `ui/src/lib/daemon.svelte.ts`, `ui/src/lib/SonarPanel.svelte`,
`ui/src/lib/MomentsPanel.svelte`

- [ ] **Step 1: Types and store**

`types.ts`, after `StreamMix`:

```ts
export type SmartLevel = "quiet" | "balanced" | "loud";

/** Smart Volume on one channel: quiet sounds come up, loud ones are held down. */
export interface SmartVolume {
  enabled: boolean;
  level: SmartLevel;
}

export interface SmartVolumeState {
  /** Whether the channels carry Smart Volume's stage, so it can be switched. */
  available: boolean;
  /** Why not: swh-plugins is missing, or PipeWire refused the stage. */
  reason: string | null;
  /** Per channel, in Game, Chat, Media, Aux order. */
  settings: SmartVolume[];
}
```

- `SonarState` gains `smart_volume: SmartVolumeState;`.
- `Request` gains
  `| { method: "set_smart_volume"; params: { channel: Channel; settings: SmartVolume } }` after
  `set_stream_master`.
- `ClipsSettings` gains `/** Record only while a program from the game rules runs. */ games_only: boolean;`.
- `ClipsState` gains
  `/** The game from the rules that's running now. */ game: string | null;` and
  `/** On and games-only, with no game from the rules running. */ waiting: boolean;`.

`daemon.svelte.ts`: import `SmartVolume`, and after `setStreamMaster`:

```ts
  /** Live: the channel's own controls, never a PipeWire restart. */
  setSmartVolume(channel: Channel, settings: SmartVolume): Promise<void> {
    return this.#sonarCommand({ method: "set_smart_volume", params: { channel, settings } });
  }
```

- [ ] **Step 2: Sonar tab.**
- Import `SmartLevel`, and next to `CHANNELS`:

```ts
  const LEVELS: { value: SmartLevel; label: string }[] = [
    { value: "quiet", label: "Quiet" },
    { value: "balanced", label: "Balanced" },
    { value: "loud", label: "Loud" },
  ];
```

- Then a section right after the Channels section (inside `{#if sonar.enabled}`):

```svelte
      <section>
        <div class="head"><h4>Smart Volume</h4></div>
        {#if !sonar.smart_volume.available}
          <!-- The daemon says why: swh-plugins is missing (it asks for it), or PipeWire refused the stage. -->
          <p class="hint">{sonar.smart_volume.reason ?? "Smart Volume isn't available."}</p>
        {:else}
          {#each sonar.channels as c, i (c.channel)}
            {@const sv = sonar.smart_volume.settings[i]}
            <div class="row">
              <span class="label">{label(c.channel)}</span>
              <label class="check inline">
                <input
                  type="checkbox"
                  checked={sv.enabled}
                  onchange={(e) =>
                    quietly(() => daemon.setSmartVolume(c.channel, { ...sv, enabled: e.currentTarget.checked }))}
                />
                On
              </label>
              <select
                value={sv.level}
                disabled={!sv.enabled}
                onchange={(e) =>
                  quietly(() =>
                    daemon.setSmartVolume(c.channel, { ...sv, level: e.currentTarget.value as SmartLevel }),
                  )}
              >
                {#each LEVELS as l (l.value)}<option value={l.value}>{l.label}</option>{/each}
              </select>
            </div>
          {/each}
          <p class="hint">
            Evens out a channel's loudness: quiet sounds come up, loud ones are held down.
            Switching is instant.
          </p>
        {/if}
      </section>
```

- In `<style>`, after `.check`: `.check.inline { margin-top: 0; }`. `.check` has a top margin
  for stacked use; in a row it must line up. If the panel's selects use a class, match it.

- [ ] **Step 3: Moments page.** After the "Keep an instant-replay buffer" toggle label:

```svelte
      <label class="toggle small">
        <input
          type="checkbox"
          checked={st.settings.games_only}
          disabled={busy}
          onchange={(e) => set({ games_only: e.currentTarget.checked })}
        />
        Only while a game from your game rules runs
      </label>
```

and replace the last two lines of the `.meta` paragraph after it,
`{st.recording ? "Recording now." : ""}` and its `</p>`, with:

```svelte
        {st.recording ? (st.settings.games_only && st.game ? `Recording while ${st.game} runs.` : "Recording now.") : ""}
      </p>
      {#if st.waiting}
        <p class="meta">Waiting for a game from your game rules (Settings → Profiles) to start.</p>
      {/if}
```

- [ ] **Step 4: Check.** Run `cd ui && npm run check`. Expected: 0 errors, 0 warnings.
- If a browser check is wanted, follow M7b's ledger: vite only (no Tauri, no daemon), headless,
  with a mocked `__TAURI_INTERNALS__`.
- **Don't launch the Tauri app or the daemon.**
- [ ] **Step 5: Commit** — `UI: Smart Volume in the Sonar tab, and games-only clips`

---

### Task 10: Docs: README, the package, and the PLAN.md honesty pass

**Files:** `README.md`, `packaging/arch/PKGBUILD`, `packaging/arch/.SRCINFO`, `PLAN.md`

- [ ] **Step 1: README and the package.**
- In **Design**, the Audio bullet gains Smart Volume: "…per-app routing, hardware-dial ChatMix,
  Smart Volume, and an RNNoise mic chain — a Sonar equivalent."
- The optional-packages sentence becomes "Three packages are **optional**: two for the mic chain
  (one of them also for Smart Volume), and one for clips:", and the `swh-plugins` row's last
  column becomes "no virtual mic, no Smart Volume".
- Under the table, add:

```markdown
**Smart Volume** uses the `sc4m` compressor from `swh-plugins`. Without it, the channels stay
exactly as they were and the Sonar tab asks you to install it. If you remove it while Sonar is on,
gglinux takes Smart Volume out of the channels at once; if gglinux wasn't running then, the
channels are missing from the next login until gglinux starts and puts them back without it.
```

- `packaging/arch/PKGBUILD` and `packaging/arch/.SRCINFO`: the `swh-plugins` optdepend becomes
  `swh-plugins: compressor for the Sonar microphone and Smart Volume` (by hand in both; no
  `makepkg`).

- [ ] **Step 2: PLAN.md, M6's split "Accept:" sentence.** Today the M6 paragraph ends
  `**Accept:** a GameSense title under Proton (e.g. CS2) drives the Apex OLED/RGB live; a heartbeat timeout`,
  and its last line, `clears state after 15 s.`, sits stranded after the M6b note.
- Move that line back so the sentence reads
  `**Accept:** a GameSense title under Proton (e.g. CS2) drives the Apex OLED/RGB live; a heartbeat timeout`
  / `clears state after 15 s.`, followed by a blank line and the M6a progress note.
- Delete the stranded line after M6b.
- M7's "**Accept:**" sentence is whole at `58df557`: check it still is, and leave its wording.

- [ ] **Step 3: PLAN.md, M2 and Risks: the headset kernel backend.**
- In M2, after the "Hardware session 2026-09-27" paragraph, add:

```markdown
**Deferred:** the kernel half of the headset backend (prefer `hid-steelseries`' sysfs/ALSA when
present). No kernel here drives `0x12e0` — the `hid-steelseries` modules of every installed 7.2
kernel alias only `12c2`, `12b6` and `1410` — hidraw keeps working after one lands, and code
against an unseen sysfs layout would be untested guesswork.
```

- In **Risks & mitigations**, the `hid-steelseries` bullet's second sentence becomes:
  "Verified: hidraw stays open regardless. Preferring the kernel's sysfs/ALSA is deferred until a
  kernel supports `0x12e0` (see M2). No blocker."

- [ ] **Step 4: PLAN.md, the M7e progress note**, after the M7d paragraph:

```markdown
**M7e done in software** (spec `docs/superpowers/specs/2026-10-02-smart-volume-game-clips-design.md`):
**Smart Volume** per Sonar channel (off, Quiet, Balanced or Loud): swh's SC4 compressor at the end
of each channel's chain, switched live with no restart. It needs `swh-plugins`; without it the
channels stay exactly as they were, and gglinux takes the compressor out again if the package
goes away. **Games-only clips** keep the replay buffer only while a program from the game rules
runs. Event-based highlights (kills and the like) stay out of scope. Not yet verified: Sonar
hasn't run live, and `gpu-screen-recorder` isn't installed.
```

- [ ] **Step 5: PLAN.md, the parity checklist.** Replace everything from
  `## Feature parity checklist (vs SteelSeries GG)` up to the `---` before `## Risks & mitigations`
  with:

```markdown
## Feature parity checklist (vs SteelSeries GG)

Each row says where it stands: **verified** (seen working on the owner's hardware), **built** (in
software and tested against fakes, not yet verified on hardware), **parked** (built; the hardware
is gone), **blocked** (needs something we don't have yet), **deferred**, or **skipped** (the
owner's decision).

**Engine:**
- Apex per-key RGB, effects, and hand-back to onboard lighting: **verified** (M4a). Reactive
  lighting: **built**.
- Apex actuation, Rapid Trigger, Rapid Tap, Protection, Dual actuation, key remap and macros:
  **blocked** (M5) — needs a GG USB capture on Windows.
- Rival 3 DPI, polling, button remap and onboard save: **parked** (M1); its battery: **verified**.
- Lighting profiles and per-game switching: **built** (M7a). Profiles are lighting profiles only.
- Settings backup and restore, in place of cloud sync (which GG dropped anyway): **built** (M7d).
- Firmware update: **skipped** — bricking risk.

**Sonar** (never turned on live yet, so nothing here is verified):
- Game / Chat / Media / Aux channels, per-app routing, and the ChatMix slider: **built** (M3a).
  The base station's ChatMix dial: **skipped** — its knob is damaged.
- 10-band channel EQ, headphone correction, AutoEQ import: **built** (M3b). A shared preset
  library: not planned.
- Virtual mic with RNNoise suppression, gate and compressor (ClearCast's job): **built** (M3c).
- Smart Volume: **built** (M7e) — swh's SC4 compressor per channel; needs `swh-plugins`.
- Streamer mode: **built** (M7c).
- Virtual surround: **skipped**.

**Headset (Nova Pro Wireless):**
- Status, battery, volume readback, and base-station screen protection: **verified** (M2).
- ANC, transparency, sidetone, mic volume and light, auto-off, 2.4 GHz mode, on-device EQ:
  **built** — not yet tried from the UI.
- Kernel backend (prefer `hid-steelseries` when present): **deferred** — see M2.

**OLED:** the Apex OLED is **blocked** (M4b) on the same Windows capture as M5. The base
station's screen is kept dark on purpose (burn-in protection).

**GameSense / Engine Apps:**
- Event server, colour handlers on the keyboard, `coreProps.json` in Proton prefixes: **built**
  (M6a). Screen and tactile handlers are accepted but never drawn (OLED blocked; no tactile
  hardware).
- Built-in apps — the audio visualizer and the mic-mute light: **built** (M6b). A clock needs the
  blocked OLED.
- CS2 Game State Integration and Discord mute: **skipped**.
- PrismSync: **skipped** — the keyboard is the only RGB device.

**Moments:** an instant-replay buffer with a hotkey and a clip library: **built** (M7b;
`gpu-screen-recorder` isn't installed yet). Games-only clips: **built** (M7e). Event-based
highlights: **skipped**.

**Elsewhere:** GG Home → the UI's device dashboard, listing the three devices live: **verified**
(M0; unplugging and replugging not yet seen). Start at login, tray,
`ggctl` and an Arch `PKGBUILD`: **built** (M7d). The 3D Aim Trainer: **skipped**.
```

- Before committing, re-read each row against the milestone notes above it, and fix any row the
  notes contradict. Ledger it as a ruling.
- Keep the document's voice: plain sentences, no new emphasis styles.

- [ ] **Step 6: Final checks and commit.**
- Run the workspace tests and clippy, and `npm run check`. Expected: `passed=306 failed=0`;
  clippy `0`; 0 errors, 0 warnings.
- `git diff --stat` shows only `README.md`, `PLAN.md`, `packaging/arch/PKGBUILD` and
  `packaging/arch/.SRCINFO`.
- Commit — `docs: Smart Volume, games-only clips, and an honest parity checklist`

## Hardware acceptance (the owner)

**Smart Volume** (with Sonar's first live test; turning Sonar on restarts PipeWire, so it's the
owner's call; `swh-plugins` is installed here):
1. Turn Sonar on.
   - The four channels show as present in the Sonar tab, and Smart Volume is offered there: no
     "PipeWire wouldn't make the channels…" note, and no `"smart_volume_refused": true` in
     `sonar.json`. `pw-dump | grep -c '"gglinux_game"'` is non-zero.
   - Play a few seconds into each channel in turn, for example
     `pw-play --target gglinux_game <file>`, then `gglinux_chat`, `gglinux_media`, `gglinux_aux`.
     Each is heard. This catches a failure when a channel's plugins are instantiated, which errors
     only that stream.
   - `journalctl --user -u pipewire -b | grep -iE "filter-chain|sc4m|ladspa|mixer|can't start graph|error"`
     shows no error about the channels' graph.
   - If Smart Volume was refused, the channels run without it. Note the journal's error: it's what
     a fix needs.
2. With Smart Volume **on** for Game:
   - the Game volume slider still changes the loudness, by as much as with Smart Volume off;
   - the keyboard's volume keys (Game is the default output) do too;
   - muting Game mutes it.
3. Play something with quiet dialogue and loud bangs on Game: Balanced brings the dialogue up and
   holds the bangs down. Switching on, off and between levels is instant, with no gap in the
   sound (no PipeWire restart).
4. **Onsets, against Smart Volume off** (the same clip, at the same volume, once with Smart Volume
   off and once on):
   - Loud on Game: a loud bang after a quiet passage. Is its start louder than with Smart Volume
     off, or does it click or crackle (clipping)? Expected (spec Facts): its first 1–3 ms up to
     9 dB louder, held down within about 5 ms. With the Game volume below 100% there's more room
     before it clips.
   - Balanced on Chat: a notification ping, or a voice starting after silence (a call unmuting).
     Is it louder than with Smart Volume off? Expected: a quiet one 6 dB louder throughout (that's
     the makeup); a loud one about 4 dB louder for its first millisecond, then held down.
5. Listen for pumping on music with steady loud beats, on each level, especially Loud. Note the
   level and what was playing.
6. Quantum 256: keep something playing into each channel with Smart Volume **on**, run
   `pw-metadata -n settings 0 clock.force-quantum 256` for a few minutes, then `0` to undo. In
   `pw-top`, the four `gglinux_*` channel nodes show no new ERR (xruns), M3's bar ("xruns clean at
   quantum 512→256"). Smart Volume adds under 0.01 ms of BUSY per channel at this quantum (spec
   Facts).
7. **(Optional; `sudo`, so the owner's call)** The hard constraint, live. With Sonar on,
   `sudo pacman -R swh-plugins`:
   - within a second the Sonar tab says Smart Volume needs `swh-plugins`, and
     `grep -c sv_comp ~/.config/pipewire/pipewire.conf.d/60-gglinux-sonar.conf` prints `0`;
   - the channels keep playing (PipeWire keeps what it loaded);
   - the mic chain goes too: it needs `swh-plugins` as well;
   - `sudo pacman -S swh-plugins`, then turn Sonar off and on: Smart Volume and the mic are back.

**If a level sounds wrong** (pumping, clicks, too loud or too quiet):
- Note in PLAN.md the level, what was playing, and what was heard.
- The fix is a retune in a follow-up: the presets in `smart.rs` (threshold, makeup, release; spec,
  Levels), or a limiter after the compressor (spec, Trade-offs).
- The EBU R128 leveller (spec, "Considered, not built") isn't a fallback for sound: it's worse on
  onsets.

**Games-only clips:**
1. `sudo pacman -S gpu-screen-recorder`.
2. Add a game rule (Settings → Profiles).
3. On the Moments page, turn clips on and tick **Only while a game from your game rules runs**: it
   says it's waiting.
4. Start the game: recording begins within about 2 s ("Recording while … runs"). Press Alt+S: a
   clip is saved.
5. Quit the game: it keeps recording for about 15 s, so Alt+S still saves the last moments, then
   stops. `pgrep gpu-screen-recorder` finds nothing, and the page says it's waiting again.
