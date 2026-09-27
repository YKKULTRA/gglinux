# Sonar channels (M3a) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Game / Chat / Media / Aux virtual outputs hosted by PipeWire, apps routed into them by rule, per-channel volume, and a ChatMix slider — controlled from gglinux.

**Architecture:** A new `gg-audio` crate writes a PipeWire drop-in (four loopback modules) and drives PipeWire with its own CLI tools behind a `PwTools` trait; all parsing and command building is pure, and the orchestration (`Sonar`) is tested against a fake `PwTools`. A `gglinuxd` sonar worker thread ticks it every second and serves six IPC methods; the UI's Sonar tab becomes `SonarPanel.svelte`.

**Tech Stack:** Rust 2021 (serde_json), PipeWire 1.6 / WirePlumber 0.5 CLI tools, Svelte 5.

**Spec:** `docs/superpowers/specs/2026-09-27-sonar-channels-design.md`

## Global Constraints

- Drop-in: `$XDG_CONFIG_HOME/pipewire/pipewire.conf.d/60-gglinux-sonar.conf`; settings: `$XDG_CONFIG_HOME/gglinux/sonar.json` (fallback `~/.config`).
- Node names: sinks `gglinux_<game|chat|media|aux>`, outlets `gglinux_<c>.out`; every outlet has `node.dont-fallback = true`, `node.passive = true`, `stream.dont-remix = true`, `target.object = "<output>"`.
- Routing: `pw-metadata <id> target.object gglinux_<c>`, once per stream id (again only when its rule changes); rules keyed by lower-cased `application.process.binary`; unknown → Game.
- Default rules: discord, vesktop, teamspeak, ts3client_linux_amd64, mumble → Chat; firefox, chromium, chrome, brave, spotify, mpv, vlc → Media.
- Volumes: `wpctl set-volume <id> <v>` with v clamped to 0..=1, three decimals. ChatMix balance −1 (all Game)…0…+1 (all Chat): favoured side 1.0, other side `1 − |b|`, applied to the Game/Chat outlets.
- Restart: `systemctl --user restart pipewire.service pipewire-pulse.service wireplumber.service`. Channel wait: up to 5 s, polling every 250 ms. Tick: 1 s, only while enabled.
- `gg-audio` depends on `gg-ipc` (wire types), `anyhow`, `serde`, `serde_json`, `tracing` — no PipeWire bindings.
- Never run enable/disable against the real system without the owner's go-ahead (it restarts PipeWire).

## Review Focus

1. **The chosen output disappears** (Bluetooth sink turns off) → outlets must not fall back into a channel — pinned by `dropin_declares_four_loopbacks_into_the_output` (Task 2) asserting `node.dont-fallback = true` on all four.
2. **Our own outlets look like app streams** (`Stream/Output/Audio`) and must never be routed into a channel (a loop) — test `outlets_are_never_routed` (Task 5) and `our_nodes_are_neither_outputs_nor_apps` (Task 3).
3. **PipeWire comes back without the channels** (bad config, slow start) → the change is undone, not left half-applied — test `enable_rolls_back_when_the_channels_never_appear` (Task 5).
4. **A corrupt or hand-edited `sonar.json`** → defaults, logged, not a crash — test `corrupt_settings_fall_back_to_defaults` (Task 4).
5. **The user deleted the drop-in by hand while "enabled"** → Sonar reports off instead of silently re-installing — test `a_removed_dropin_means_sonar_is_off` (Task 5).

---

### Task 1: IPC — Sonar types and requests

**Files:** Modify `crates/gg-ipc/src/lib.rs`, `crates/gglinuxd/examples/fakemouse.rs`, `crates/gglinuxd/src/main.rs` (temporary arm)

**Produces:** `Channel { Game, Chat, Media, Aux }` (`ALL`, `key()`, `Ord`, `Hash`), `AudioOutput`, `ChannelState`, `AppStream`, `SonarState`, `Request::{GetSonarState, SetSonarEnabled{enabled}, SetSonarOutput{name}, SetChannelVolume{channel, volume}, SetChatMix{balance}, SetAppChannel{binary, channel}}`, `ResponseData::Sonar{state}`, `Event::SonarChanged{state}`.

- [ ] **Step 1: Failing tests** (append to gg-ipc tests):

```rust
    #[test]
    fn sonar_requests_parse_from_their_documented_json() {
        for (line, label) in [
            (r#"{"id":1,"req":{"method":"get_sonar_state"}}"#, "get"),
            (r#"{"id":2,"req":{"method":"set_sonar_enabled","params":{"enabled":true}}}"#, "enable"),
            (r#"{"id":4,"req":{"method":"set_chat_mix","params":{"balance":-0.25}}}"#, "chatmix"),
            (r#"{"id":5,"req":{"method":"set_app_channel","params":{"binary":"discord","channel":"chat"}}}"#, "app"),
            (r#"{"id":6,"req":{"method":"set_sonar_output","params":{"name":"alsa_output.x"}}}"#, "output"),
        ] {
            serde_json::from_str::<RequestFrame>(line).unwrap_or_else(|e| panic!("{label}: {e}"));
        }
        let volume = r#"{"id":3,"req":{"method":"set_channel_volume","params":{"channel":"chat","volume":0.5}}}"#;
        match serde_json::from_str::<RequestFrame>(volume).unwrap().req {
            Request::SetChannelVolume { channel, volume } => {
                assert_eq!(channel, Channel::Chat);
                assert_eq!(volume, 0.5);
            }
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn channels_have_stable_keys() {
        let keys: Vec<_> = Channel::ALL.iter().map(|c| c.key()).collect();
        assert_eq!(keys, ["game", "chat", "media", "aux"]);
        assert_eq!(serde_json::to_string(&Channel::Media).unwrap(), r#""media""#);
    }
```

- [ ] **Step 2: Run** `cargo test -p gg-ipc` — Expected: compile errors.

- [ ] **Step 3: Implement** (after `HeadsetState`):

```rust
/// A Sonar output channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Game,
    Chat,
    Media,
    Aux,
}

impl Channel {
    pub const ALL: [Channel; 4] = [Channel::Game, Channel::Chat, Channel::Media, Channel::Aux];

    /// Lower-case name, as used in node names and settings.
    pub fn key(self) -> &'static str {
        match self {
            Channel::Game => "game",
            Channel::Chat => "chat",
            Channel::Media => "media",
            Channel::Aux => "aux",
        }
    }
}

/// A physical output device the channels can play into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioOutput {
    /// PipeWire `node.name`.
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelState {
    pub channel: Channel,
    /// Whether PipeWire currently has this channel's sink.
    pub present: bool,
    /// 0..=1 on the desktop mixer's scale; `None` when absent.
    pub volume: Option<f32>,
}

/// An app playing audio, and the channel its rule sends it to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppStream {
    /// PipeWire node id; changes when the app reopens its stream.
    pub id: u32,
    pub name: String,
    /// `application.process.binary`, the key routing rules match on.
    pub binary: Option<String>,
    pub channel: Channel,
}

/// Sonar as the daemon sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SonarState {
    /// Whether PipeWire's tools answered.
    pub available: bool,
    pub enabled: bool,
    /// `node.name` of the physical output the channels play into.
    pub output: Option<String>,
    pub outputs: Vec<AudioOutput>,
    pub channels: Vec<ChannelState>,
    /// -1.0 all Game … 0.0 both full … 1.0 all Chat.
    pub chatmix: f32,
    pub apps: Vec<AppStream>,
}
```

`Request` additions (after `SetHeadset`):

```rust
    /// Sonar channels, routing, and ChatMix.
    GetSonarState,
    /// Turn Sonar on or off. Restarts PipeWire.
    SetSonarEnabled { enabled: bool },
    /// The physical output the channels play into (a `node.name`).
    SetSonarOutput { name: String },
    /// 0..=1.
    SetChannelVolume { channel: Channel, volume: f32 },
    /// -1.0 all Game … 1.0 all Chat.
    SetChatMix { balance: f32 },
    /// Route an app (by `application.process.binary`) to a channel, and remember it.
    SetAppChannel { binary: String, channel: Channel },
```

`ResponseData`: `Sonar { state: SonarState },` after `Headset`. `Event`: `/// Sonar changed: channels, apps, volumes, or on/off.` `SonarChanged { state: SonarState },`.

- [ ] **Step 4: Keep the workspace compiling.** `main.rs` `dispatch`, after the `SetHeadset` arm:

```rust
        Request::GetSonarState
        | Request::SetSonarEnabled { .. }
        | Request::SetSonarOutput { .. }
        | Request::SetChannelVolume { .. }
        | Request::SetChatMix { .. }
        | Request::SetAppChannel { .. } => ResponseData::Error {
            message: "Sonar is not wired up yet".into(),
        },
```

`fakemouse.rs`: import `AppStream, AudioOutput, Channel, ChannelState, SonarState`; add `sonar: SonarState,` to `Fake`, initialised with `fake_sonar()`; in `handle`:

```rust
        Request::GetSonarState => ResponseData::Sonar { state: s.sonar.clone() },
        Request::SetSonarEnabled { enabled } => {
            s.sonar.enabled = enabled;
            ResponseData::Success
        }
        Request::SetSonarOutput { name } => {
            s.sonar.output = Some(name);
            ResponseData::Success
        }
        Request::SetChannelVolume { channel, volume } => {
            if let Some(c) = s.sonar.channels.iter_mut().find(|c| c.channel == channel) {
                c.volume = Some(volume);
            }
            ResponseData::Success
        }
        Request::SetChatMix { balance } => {
            s.sonar.chatmix = balance;
            ResponseData::Success
        }
        Request::SetAppChannel { binary, channel } => {
            for app in s.sonar.apps.iter_mut().filter(|a| a.binary.as_deref() == Some(binary.as_str())) {
                app.channel = channel;
            }
            ResponseData::Success
        }
```

with

```rust
/// A plausible Sonar: on, two outputs, two apps.
fn fake_sonar() -> SonarState {
    SonarState {
        available: true,
        enabled: true,
        output: Some("alsa_output.fake-headset".into()),
        outputs: vec![
            AudioOutput { name: "alsa_output.fake-headset".into(), description: "Fake Headset".into() },
            AudioOutput { name: "alsa_output.fake-speakers".into(), description: "Fake Speakers".into() },
        ],
        channels: Channel::ALL
            .iter()
            .map(|&channel| ChannelState { channel, present: true, volume: Some(1.0) })
            .collect(),
        chatmix: 0.0,
        apps: vec![
            AppStream { id: 90, name: "Discord".into(), binary: Some("discord".into()), channel: Channel::Chat },
            AppStream { id: 91, name: "Firefox".into(), binary: Some("firefox".into()), channel: Channel::Media },
        ],
    }
}
```

`method_name` arms: `get_sonar_state`, `set_sonar_enabled`, `set_sonar_output`, `set_channel_volume`, `set_chat_mix`, `set_app_channel`.

- [ ] **Step 5: Run** `cargo test -p gg-ipc && cargo build --workspace --all-targets` — Expected: 11 gg-ipc tests pass; everything builds.
- [ ] **Step 6: Commit** `IPC: Sonar channels, routing, and ChatMix`

---

### Task 2: `gg-audio` crate — drop-in, rules, ChatMix, command builders

**Files:** Create `crates/gg-audio/{Cargo.toml, src/lib.rs, src/config.rs, src/rules.rs, src/mix.rs, src/tools.rs}`; modify workspace `Cargo.toml` members.

**Produces:** `config::{FILE_NAME, sink_name, outlet_name, is_ours, dropin_path, config_home, dropin}`, `rules::Rules` (`Default`, `channel_for`, `set`, serde transparent map), `mix::chatmix_gains(f32) -> (f32, f32)`, `tools::{PwTools, SystemTools, set_volume, set_target, set_default, restart_pipewire}`.

- [ ] **Step 1: Scaffold + failing tests.** `Cargo.toml`:

```toml
[package]
name = "gg-audio"
description = "Sonar-style audio channels on PipeWire"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true

[dependencies]
gg-ipc = { path = "../gg-ipc" }
anyhow.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
```

Add `"crates/gg-audio"` to the workspace members. `src/lib.rs`:

```rust
//! Sonar-style audio for gglinux, on PipeWire.
//!
//! PipeWire hosts the graph — four virtual output channels declared in a
//! drop-in config ([`config`]) — and this crate controls it with PipeWire's own
//! tools (`pw-dump`, `pw-metadata`, `wpctl`) through [`tools::PwTools`]. Every
//! action is a command a person could type, and the logic is tested against a
//! fake.

pub mod config;
pub mod mix;
pub mod rules;
pub mod tools;
```

Each module file starts with only its tests (below); implementations come in Step 3.

`config.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropin_declares_four_loopbacks_into_the_output() {
        let text = dropin("alsa_output.usb-SteelSeries_Arctis").unwrap();
        assert_eq!(text.matches("libpipewire-module-loopback").count(), 4);
        for c in Channel::ALL {
            assert!(text.contains(&format!("node.name      = \"gglinux_{}\"", c.key())), "{text}");
            assert!(text.contains(&format!("node.name          = \"gglinux_{}.out\"", c.key())), "{text}");
        }
        assert_eq!(
            text.matches("target.object      = \"alsa_output.usb-SteelSeries_Arctis\"").count(),
            4
        );
        assert_eq!(
            text.matches("node.dont-fallback = true").count(),
            4,
            "an outlet must never fall back into a channel"
        );
    }

    #[test]
    fn dropin_refuses_names_that_would_break_the_config() {
        assert!(dropin("").is_err());
        assert!(dropin("evil\" }").is_err());
        assert!(dropin("a\\b").is_err());
        assert!(dropin("a\nb").is_err());
    }

    #[test]
    fn names_identify_our_nodes() {
        assert_eq!(sink_name(Channel::Chat), "gglinux_chat");
        assert_eq!(outlet_name(Channel::Chat), "gglinux_chat.out");
        assert!(is_ours("gglinux_game.out"));
        assert!(!is_ours("alsa_output.pci-0000"));
    }
}
```

`rules.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_send_chat_apps_to_chat_and_players_to_media() {
        let rules = Rules::default();
        assert_eq!(rules.channel_for(Some("discord")), Channel::Chat);
        assert_eq!(rules.channel_for(Some("Discord")), Channel::Chat, "case-insensitive");
        assert_eq!(rules.channel_for(Some("firefox")), Channel::Media);
        assert_eq!(rules.channel_for(Some("some-game.x86_64")), Channel::Game);
        assert_eq!(rules.channel_for(None), Channel::Game);
    }

    #[test]
    fn a_set_rule_overrides_and_survives_serialisation() {
        let mut rules = Rules::default();
        rules.set("Firefox", Channel::Aux);
        let json = serde_json::to_string(&rules).unwrap();
        assert!(json.contains(r#""firefox":"aux""#), "{json}");
        let back: Rules = serde_json::from_str(&json).unwrap();
        assert_eq!(back.channel_for(Some("firefox")), Channel::Aux);
    }
}
```

`mix.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centre_leaves_both_full() {
        assert_eq!(chatmix_gains(0.0), (1.0, 1.0));
    }

    #[test]
    fn the_favoured_side_stays_full() {
        assert_eq!(chatmix_gains(-1.0), (1.0, 0.0));
        assert_eq!(chatmix_gains(1.0), (0.0, 1.0));
        assert_eq!(chatmix_gains(0.5), (0.5, 1.0));
        assert_eq!(chatmix_gains(-0.25), (1.0, 0.75));
    }

    #[test]
    fn out_of_range_and_nan_are_tamed() {
        assert_eq!(chatmix_gains(3.0), (0.0, 1.0));
        assert_eq!(chatmix_gains(f32::NAN), (1.0, 1.0));
    }
}
```

`tools.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_what_a_person_would_type() {
        assert_eq!(set_volume(57, 0.5), ["wpctl", "set-volume", "57", "0.500"]);
        assert_eq!(set_volume(57, 1.7), ["wpctl", "set-volume", "57", "1.000"], "clamped");
        assert_eq!(set_target(99, "gglinux_chat"), ["pw-metadata", "99", "target.object", "gglinux_chat"]);
        assert_eq!(set_default(20), ["wpctl", "set-default", "20"]);
        assert_eq!(
            restart_pipewire(),
            ["systemctl", "--user", "restart", "pipewire.service", "pipewire-pulse.service", "wireplumber.service"]
        );
    }
}
```

- [ ] **Step 2: Run** `cargo test -p gg-audio` — Expected: compile errors (items missing).

- [ ] **Step 3: Implementations** — prepend to each file.

`config.rs`:

```rust
//! The PipeWire drop-in that declares Sonar's channels.
//!
//! PipeWire hosts the channels itself, so they keep working while gglinux is
//! not running. Each channel is a loopback: a virtual sink apps play into, and
//! an outlet stream that plays it into the chosen physical output.

use std::path::PathBuf;

use anyhow::{ensure, Result};
use gg_ipc::Channel;

/// Sorts after the stock drop-ins.
pub const FILE_NAME: &str = "60-gglinux-sonar.conf";

/// Every node gglinux creates is named with this prefix.
const PREFIX: &str = "gglinux_";

/// The virtual sink apps play into.
pub fn sink_name(channel: Channel) -> String {
    format!("{PREFIX}{}", channel.key())
}

/// The stream that carries a channel into the physical output.
pub fn outlet_name(channel: Channel) -> String {
    format!("{PREFIX}{}.out", channel.key())
}

/// Whether a node is one of ours (a channel sink or outlet).
pub fn is_ours(node_name: &str) -> bool {
    node_name.starts_with(PREFIX)
}

fn label(channel: Channel) -> &'static str {
    match channel {
        Channel::Game => "Game",
        Channel::Chat => "Chat",
        Channel::Media => "Media",
        Channel::Aux => "Aux",
    }
}

/// `$XDG_CONFIG_HOME`, falling back to `~/.config`.
pub fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".config")
        })
}

/// Where the drop-in lives.
pub fn dropin_path() -> PathBuf {
    config_home().join("pipewire").join("pipewire.conf.d").join(FILE_NAME)
}

/// The drop-in text for channels that play into `output` (a `node.name`).
///
/// Every outlet sets `node.dont-fallback`: if the output disappears, the
/// outlet waits for it rather than falling back to the default sink — which,
/// with Sonar on, is the Game channel, and would feed audio into itself.
pub fn dropin(output: &str) -> Result<String> {
    ensure!(
        !output.is_empty() && !output.contains(['"', '\\', '\n']),
        "unusable output device name {output:?}"
    );
    let mut text = String::from(
        "# Written by gglinux: Sonar-style output channels.\n\
         # Turn Sonar off in gglinux, or delete this file and restart PipeWire, to remove them.\n\
         context.modules = [\n",
    );
    for channel in Channel::ALL {
        text.push_str(&format!(
            r#"    {{ name = libpipewire-module-loopback
      args = {{
        node.description = "gglinux {label}"
        capture.props = {{
          node.name      = "{sink}"
          media.class    = "Audio/Sink"
          audio.position = [ FL FR ]
        }}
        playback.props = {{
          node.name          = "{outlet}"
          node.description   = "gglinux {label} (output)"
          target.object      = "{output}"
          node.dont-fallback = true
          node.passive       = true
          stream.dont-remix  = true
          audio.position     = [ FL FR ]
        }}
      }}
    }}
"#,
            label = label(channel),
            sink = sink_name(channel),
            outlet = outlet_name(channel),
        ));
    }
    text.push_str("]\n");
    Ok(text)
}
```

`rules.rs`:

```rust
//! Which channel an app plays into.

use std::collections::BTreeMap;

use gg_ipc::Channel;
use serde::{Deserialize, Serialize};

/// Channel per `application.process.binary`, matched case-insensitively.
/// Apps without a rule play into Game, as Sonar's default output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rules(BTreeMap<String, Channel>);

impl Default for Rules {
    fn default() -> Self {
        let chat = ["discord", "vesktop", "teamspeak", "ts3client_linux_amd64", "mumble"];
        let media = ["firefox", "chromium", "chrome", "brave", "spotify", "mpv", "vlc"];
        Rules(
            chat.iter()
                .map(|b| (b.to_string(), Channel::Chat))
                .chain(media.iter().map(|b| (b.to_string(), Channel::Media)))
                .collect(),
        )
    }
}

impl Rules {
    pub fn channel_for(&self, binary: Option<&str>) -> Channel {
        binary
            .and_then(|b| self.0.get(&b.to_lowercase()))
            .copied()
            .unwrap_or(Channel::Game)
    }

    pub fn set(&mut self, binary: &str, channel: Channel) {
        self.0.insert(binary.to_lowercase(), channel);
    }
}
```

`mix.rs`:

```rust
//! ChatMix: one balance control across the Game and Chat channels.

/// Gains for the (Game, Chat) outlets from a balance of -1.0 (all Game) through
/// 0.0 (both full) to 1.0 (all Chat). As on the hardware dial, the favoured side
/// stays at full volume and only the other side is turned down.
pub fn chatmix_gains(balance: f32) -> (f32, f32) {
    let b = if balance.is_finite() { balance.clamp(-1.0, 1.0) } else { 0.0 };
    (1.0 - b.max(0.0), 1.0 + b.min(0.0))
}
```

`tools.rs`:

```rust
//! PipeWire's own command-line tools, behind a trait so the orchestration can
//! be tested against a fake.

use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};

pub trait PwTools {
    /// Everything PipeWire has, as `pw-dump` prints it.
    fn dump(&self) -> Result<String>;
    /// Run one control command; `argv[0]` is the program.
    fn run(&self, argv: &[String]) -> Result<()>;
    /// Wait. The fake in the tests does not.
    fn pause(&self, duration: Duration) {
        std::thread::sleep(duration)
    }
}

/// The real tools, run as child processes.
pub struct SystemTools;

impl PwTools for SystemTools {
    fn dump(&self) -> Result<String> {
        let out = Command::new("pw-dump").output().context("running pw-dump — is PipeWire installed?")?;
        if !out.status.success() {
            bail!("pw-dump failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        String::from_utf8(out.stdout).context("pw-dump printed non-UTF-8")
    }

    fn run(&self, argv: &[String]) -> Result<()> {
        let (program, args) = argv.split_first().context("empty command")?;
        let out = Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("running {program}"))?;
        if !out.status.success() {
            bail!("`{}` failed: {}", argv.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// Set a node's volume on the mixer's 0..=1 scale (what `wpctl` and the desktop show).
pub fn set_volume(id: u32, volume: f32) -> Vec<String> {
    argv(&["wpctl", "set-volume", &id.to_string(), &format!("{:.3}", volume.clamp(0.0, 1.0))])
}

/// Send a stream or outlet to a sink, by `node.name`.
pub fn set_target(id: u32, sink: &str) -> Vec<String> {
    argv(&["pw-metadata", &id.to_string(), "target.object", sink])
}

/// Make a sink the default output.
pub fn set_default(id: u32) -> Vec<String> {
    argv(&["wpctl", "set-default", &id.to_string()])
}

/// Restart PipeWire so it re-reads its config: a 1–2 s gap in all audio.
pub fn restart_pipewire() -> Vec<String> {
    argv(&["systemctl", "--user", "restart", "pipewire.service", "pipewire-pulse.service", "wireplumber.service"])
}
```

- [ ] **Step 4: Run** `cargo test -p gg-audio` — Expected: 9 pass.
- [ ] **Step 5: Commit** `gg-audio: channel drop-in, routing rules, ChatMix gains, PipeWire commands`

---

### Task 3: Graph — reading `pw-dump`

**Files:** Create `crates/gg-audio/src/graph.rs`, `crates/gg-audio/fixtures/pw-dump-2026-09-27.json`; modify `src/lib.rs` (`pub mod graph;` and a `#[cfg(test)] pub(crate) mod testing`).

**Produces:** `graph::{Node, Graph}` with `Graph::parse(&str) -> Result<Graph>`, `node(&str)`, `physical_sinks()`, `app_streams()`, `default_sink`; `testing::dump(&[(u32, &str, &str, Option<&str>)], &str) -> String`.

- [ ] **Step 1: Fixture.** Trim this machine's `pw-dump` to Node objects (keeping only `node.name`, `node.description`, `media.class`, `application.name`, `application.process.binary`, and `params.Props[].channelVolumes`) plus the `default` Metadata object — no host, user, or machine ids:

```bash
pw-dump | python3 -c '
import json, sys
KEEP = ["node.name", "node.description", "media.class", "application.name", "application.process.binary"]
out = []
for o in json.load(sys.stdin):
    if o["type"] == "PipeWire:Interface:Node" and o.get("info"):
        props = {k: v for k, v in o["info"]["props"].items() if k in KEEP}
        vols = [{"channelVolumes": p["channelVolumes"]} for p in o["info"].get("params", {}).get("Props", []) if "channelVolumes" in p]
        out.append({"id": o["id"], "type": o["type"], "info": {"props": props, "params": {"Props": vols}}})
    elif o["type"] == "PipeWire:Interface:Metadata" and o["props"].get("metadata.name") == "default":
        out.append({"id": o["id"], "type": o["type"], "props": {"metadata.name": "default"}, "metadata": o["metadata"]})
json.dump(out, sys.stdout, indent=1)
' > crates/gg-audio/fixtures/pw-dump-2026-09-27.json
```

- [ ] **Step 2: Failing tests** in `graph.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::dump;

    const CAPTURE: &str = include_str!("../fixtures/pw-dump-2026-09-27.json");

    #[test]
    fn parses_a_real_capture() {
        let g = Graph::parse(CAPTURE).unwrap();
        let arctis = g
            .node("alsa_output.usb-SteelSeries_Arctis_Nova_Pro_Wireless-00.analog-stereo")
            .expect("the Arctis sink is in the capture");
        assert_eq!(arctis.media_class, "Audio/Sink");
        assert!(arctis.volume.is_some());
        assert!(g.physical_sinks().count() >= 4);
        assert!(g.app_streams().any(|s| s.app_binary.as_deref() == Some("firefox")));
        assert!(g.default_sink.is_some(), "the capture names a default sink");
    }

    #[test]
    fn volumes_use_the_mixer_scale() {
        // Linear 0.125 is 0.5 on the cubic scale wpctl and the desktop show.
        let json = r#"[{"id":7,"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"x","media.class":"Audio/Sink"},"params":{"Props":[{"channelVolumes":[0.125,0.064]}]}}}]"#;
        let v = Graph::parse(json).unwrap().nodes[0].volume.unwrap();
        assert!((v - 0.5).abs() < 1e-6, "{v}");
    }

    #[test]
    fn our_nodes_are_neither_outputs_nor_apps() {
        let g = Graph::parse(&dump(
            &[
                (1, "alsa_output.x", "Audio/Sink", None),
                (2, "gglinux_game", "Audio/Sink", None),
                (3, "gglinux_game.out", "Stream/Output/Audio", None),
                (4, "Firefox", "Stream/Output/Audio", Some("firefox")),
            ],
            "alsa_output.x",
        ))
        .unwrap();
        assert_eq!(g.physical_sinks().map(|n| n.id).collect::<Vec<_>>(), [1]);
        assert_eq!(g.app_streams().map(|n| n.id).collect::<Vec<_>>(), [4]);
        assert_eq!(g.default_sink.as_deref(), Some("alsa_output.x"));
    }

    #[test]
    fn default_sink_may_be_a_json_string() {
        let json = r#"[{"id":39,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},
            "metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":"{\"name\":\"alsa_output.y\"}"}]}]"#;
        assert_eq!(Graph::parse(json).unwrap().default_sink.as_deref(), Some("alsa_output.y"));
    }
}
```

and in `lib.rs`:

```rust
pub mod graph;

#[cfg(test)]
pub(crate) mod testing {
    use serde_json::{json, Value};

    /// A minimal `pw-dump`: nodes as (id, node.name, media.class, binary), all at
    /// full volume, plus the configured default sink.
    pub fn dump(nodes: &[(u32, &str, &str, Option<&str>)], default_sink: &str) -> String {
        let mut objects: Vec<Value> = nodes
            .iter()
            .map(|(id, name, class, binary)| {
                let mut props = json!({ "node.name": name, "media.class": class });
                if let Some(b) = binary {
                    props["application.process.binary"] = json!(b);
                    props["application.name"] = json!(b);
                }
                json!({
                    "id": id,
                    "type": "PipeWire:Interface:Node",
                    "info": { "props": props, "params": { "Props": [{ "channelVolumes": [1.0, 1.0] }] } }
                })
            })
            .collect();
        objects.push(json!({
            "id": 39,
            "type": "PipeWire:Interface:Metadata",
            "props": { "metadata.name": "default" },
            "metadata": [{
                "subject": 0,
                "key": "default.configured.audio.sink",
                "type": "Spa:String:JSON",
                "value": { "name": default_sink }
            }]
        }));
        Value::Array(objects).to_string()
    }
}
```

- [ ] **Step 3: Run** `cargo test -p gg-audio graph` — Expected: compile errors.

- [ ] **Step 4: Implement** (prepend to `graph.rs`):

```rust
//! What PipeWire has right now, read from `pw-dump`.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::config;

/// One PipeWire node, reduced to what Sonar needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: u32,
    pub name: String,
    pub description: Option<String>,
    pub media_class: String,
    pub app_name: Option<String>,
    pub app_binary: Option<String>,
    /// Loudest channel, on the cubic scale `wpctl` and desktop mixers show.
    pub volume: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    /// `node.name` of the configured default sink.
    pub default_sink: Option<String>,
}

const SINK: &str = "Audio/Sink";
const APP_STREAM: &str = "Stream/Output/Audio";

impl Graph {
    pub fn parse(json: &str) -> Result<Graph> {
        let objects: Vec<Value> = serde_json::from_str(json).context("parsing pw-dump output")?;
        let mut graph = Graph::default();
        for object in &objects {
            match object["type"].as_str() {
                Some("PipeWire:Interface:Node") => graph.nodes.extend(parse_node(object)),
                Some("PipeWire:Interface:Metadata")
                    if object["props"]["metadata.name"] == "default" =>
                {
                    graph.default_sink = default_sink(object);
                }
                _ => {}
            }
        }
        Ok(graph)
    }

    pub fn node(&self, name: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.name == name)
    }

    /// Real output devices: sinks that are not ours.
    pub fn physical_sinks(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(|n| n.media_class == SINK && !config::is_ours(&n.name))
    }

    /// Apps playing audio. Our outlets are streams too, and are left out.
    pub fn app_streams(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(|n| n.media_class == APP_STREAM && !config::is_ours(&n.name))
    }
}

fn parse_node(object: &Value) -> Option<Node> {
    let props = &object["info"]["props"];
    let text = |key: &str| props[key].as_str().map(str::to_owned);
    Some(Node {
        id: u32::try_from(object["id"].as_u64()?).ok()?,
        name: text("node.name")?,
        description: text("node.description"),
        media_class: text("media.class").unwrap_or_default(),
        app_name: text("application.name"),
        app_binary: text("application.process.binary"),
        volume: volume(&object["info"]["params"]["Props"]),
    })
}

/// The loudest channel's volume, converted from PipeWire's linear amplitude to
/// the cubic scale the mixer shows.
fn volume(props: &Value) -> Option<f32> {
    let channels = props.as_array()?.iter().find_map(|p| p["channelVolumes"].as_array())?;
    let loudest = channels.iter().filter_map(Value::as_f64).reduce(f64::max)?;
    Some(loudest.cbrt() as f32)
}

fn default_sink(object: &Value) -> Option<String> {
    let entries = object["metadata"].as_array()?;
    let lookup = |key: &str| {
        entries
            .iter()
            .find(|e| e["subject"] == 0 && e["key"] == key)
            .and_then(|e| sink_name_of(&e["value"]))
    };
    lookup("default.configured.audio.sink").or_else(|| lookup("default.audio.sink"))
}

/// The value is `{"name": …}`, sometimes serialised as a JSON string.
fn sink_name_of(value: &Value) -> Option<String> {
    match value {
        Value::Object(_) => value["name"].as_str().map(str::to_owned),
        Value::String(s) => serde_json::from_str::<Value>(s).ok()?["name"].as_str().map(str::to_owned),
        _ => None,
    }
}
```

- [ ] **Step 5: Run** `cargo test -p gg-audio` — Expected: 13 pass.
- [ ] **Step 6: Commit** `gg-audio: read the PipeWire graph from pw-dump`

---

### Task 4: Settings

**Files:** Create `crates/gg-audio/src/settings.rs`; `lib.rs` gains `pub mod settings;` and in `testing` a `scratch(name) -> PathBuf` helper.

**Produces:** `settings::{Settings, path, load, save}`.

- [ ] **Step 1: Failing tests.** In `testing`:

```rust
    /// An empty directory unique to one test.
    pub fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join("gg-audio-tests")
            .join(format!("{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
```

`settings.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scratch;

    #[test]
    fn settings_round_trip() {
        let path = scratch("settings-rt").join("gglinux").join("sonar.json");
        let mut s = Settings::default();
        s.enabled = true;
        s.output = Some("alsa_output.x".into());
        s.chatmix = -0.5;
        s.volumes.insert(Channel::Chat, 0.4);
        s.rules.set("firefox", Channel::Aux);
        save(&path, &s).unwrap();
        assert_eq!(load(&path), s);
    }

    #[test]
    fn missing_settings_are_the_defaults() {
        assert_eq!(load(&scratch("settings-missing").join("sonar.json")), Settings::default());
    }

    #[test]
    fn corrupt_settings_fall_back_to_defaults() {
        let path = scratch("settings-corrupt").join("sonar.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(&path), Settings::default());
    }
}
```

- [ ] **Step 2: Run** `cargo test -p gg-audio settings` — Expected: compile errors.

- [ ] **Step 3: Implement** (prepend):

```rust
//! Sonar's saved state: `$XDG_CONFIG_HOME/gglinux/sonar.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gg_ipc::Channel;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{config, rules::Rules};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// `node.name` of the output the channels play into.
    pub output: Option<String>,
    /// The default sink before Sonar took over, restored when it is turned off.
    pub previous_default: Option<String>,
    /// -1.0 all Game … 1.0 all Chat.
    pub chatmix: f32,
    /// Per-channel volume on the mixer's 0..=1 scale.
    pub volumes: BTreeMap<Channel, f32>,
    pub rules: Rules,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            output: None,
            previous_default: None,
            chatmix: 0.0,
            volumes: BTreeMap::new(),
            rules: Rules::default(),
        }
    }
}

pub fn path() -> PathBuf {
    config::config_home().join("gglinux").join("sonar.json")
}

/// Read settings. A missing file means the defaults; an unreadable one is
/// logged and ignored, never fatal.
pub fn load(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            warn!(path = %path.display(), error = %e, "ignoring unreadable Sonar settings");
            Settings::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "could not read Sonar settings");
            Settings::default()
        }
    }
}

/// Write settings atomically: a sibling temp file renamed over the old one.
pub fn save(path: &Path, settings: &Settings) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(settings)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}
```

- [ ] **Step 4: Run** `cargo test -p gg-audio` — Expected: 16 pass.
- [ ] **Step 5: Commit** `gg-audio: Sonar settings file`

---

### Task 5: Orchestration — `Sonar`

**Files:** Create `crates/gg-audio/src/sonar.rs`; `lib.rs` gains `pub mod sonar;`.

**Produces:** `sonar::Sonar<T: PwTools>` with `new(T, PathBuf settings, PathBuf dropin)`, `is_enabled`, `tick`, `state` (refreshes when off), `snapshot`, `enable`, `disable`, `set_output`, `set_volume`, `set_chatmix`, `set_app_channel`.

- [ ] **Step 1: Failing tests** in `sonar.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{dump, scratch};
    use std::cell::RefCell;

    const HEADSET: &str = "alsa_output.usb-SteelSeries_Arctis";
    const BT: &str = "bluez_output.1";

    /// PipeWire stand-in. A restart brings the channels up if the drop-in exists
    /// (and `channels_come_up`), or takes them down if it does not.
    struct Fake {
        dropin: PathBuf,
        streams: RefCell<Vec<(u32, &'static str)>>,
        up: RefCell<bool>,
        channels_come_up: bool,
        runs: RefCell<Vec<Vec<String>>>,
    }

    impl Fake {
        fn new(dropin: PathBuf, streams: &[(u32, &'static str)]) -> Self {
            Fake {
                dropin,
                streams: RefCell::new(streams.to_vec()),
                up: RefCell::new(false),
                channels_come_up: true,
                runs: RefCell::new(Vec::new()),
            }
        }

        fn ran(&self, argv: &[&str]) -> bool {
            self.runs.borrow().iter().any(|r| r == argv)
        }

        fn count(&self, program: &str) -> usize {
            self.runs.borrow().iter().filter(|r| r[0] == program).count()
        }
    }

    impl PwTools for &Fake {
        fn dump(&self) -> Result<String> {
            let mut nodes: Vec<(u32, &str, &str, Option<&str>)> =
                vec![(10, HEADSET, "Audio/Sink", None), (11, BT, "Audio/Sink", None)];
            if *self.up.borrow() {
                nodes.extend([
                    (20, "gglinux_game", "Audio/Sink", None),
                    (21, "gglinux_chat", "Audio/Sink", None),
                    (22, "gglinux_media", "Audio/Sink", None),
                    (23, "gglinux_aux", "Audio/Sink", None),
                    (30, "gglinux_game.out", "Stream/Output/Audio", None),
                    (31, "gglinux_chat.out", "Stream/Output/Audio", None),
                    (32, "gglinux_media.out", "Stream/Output/Audio", None),
                    (33, "gglinux_aux.out", "Stream/Output/Audio", None),
                ]);
            }
            let streams = self.streams.borrow();
            nodes.extend(streams.iter().map(|&(id, bin)| (id, bin, "Stream/Output/Audio", Some(bin))));
            Ok(dump(&nodes, BT))
        }

        fn run(&self, argv: &[String]) -> Result<()> {
            if argv[0] == "systemctl" {
                *self.up.borrow_mut() = self.channels_come_up && self.dropin.exists();
            }
            self.runs.borrow_mut().push(argv.to_vec());
            Ok(())
        }

        fn pause(&self, _: Duration) {}
    }

    fn paths(name: &str) -> (PathBuf, PathBuf) {
        let dir = scratch(name);
        (dir.join("sonar.json"), dir.join("pipewire.conf.d").join(config::FILE_NAME))
    }

    const APPS: &[(u32, &str)] = &[(40, "Discord"), (41, "firefox"), (42, "game.bin")];

    #[test]
    fn enable_writes_the_dropin_restarts_and_makes_game_default() {
        let (settings_path, dropin) = paths("enable");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        sonar.enable().unwrap();

        let text = std::fs::read_to_string(&dropin).unwrap();
        assert!(text.contains(&format!("target.object      = \"{BT}\"")), "plays into the default output");
        assert!(fake.ran(&["systemctl", "--user", "restart", "pipewire.service", "pipewire-pulse.service", "wireplumber.service"]));
        assert!(fake.ran(&["wpctl", "set-default", "20"]), "Game becomes the default");
        assert!(fake.ran(&["pw-metadata", "40", "target.object", "gglinux_chat"]));
        assert!(fake.ran(&["pw-metadata", "41", "target.object", "gglinux_media"]));
        assert!(fake.ran(&["pw-metadata", "42", "target.object", "gglinux_game"]));

        let saved = settings::load(&settings_path);
        assert!(saved.enabled);
        assert_eq!(saved.output.as_deref(), Some(BT));
        assert_eq!(saved.previous_default.as_deref(), Some(BT));
    }

    #[test]
    fn enable_rolls_back_when_the_channels_never_appear() {
        let (settings_path, dropin) = paths("rollback");
        let mut fake = Fake::new(dropin.clone(), APPS);
        fake.channels_come_up = false;
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        assert!(sonar.enable().is_err());
        assert!(!dropin.exists(), "the drop-in is removed again");
        assert_eq!(fake.count("systemctl"), 2, "restarted once to apply, once to undo");
        assert!(!settings::load(&settings_path).enabled);
    }

    #[test]
    fn streams_are_routed_once() {
        let (settings_path, dropin) = paths("once");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        let before = fake.count("pw-metadata");
        sonar.tick();
        assert_eq!(fake.count("pw-metadata"), before, "nothing new, nothing sent");
        fake.streams.borrow_mut().push((43, "mpv"));
        sonar.tick();
        assert_eq!(fake.count("pw-metadata"), before + 1);
        assert!(fake.ran(&["pw-metadata", "43", "target.object", "gglinux_media"]));
    }

    #[test]
    fn outlets_are_never_routed() {
        let (settings_path, dropin) = paths("outlets");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.tick();
        for id in ["30", "31", "32", "33"] {
            assert!(
                !fake.runs.borrow().iter().any(|r| r[0] == "pw-metadata" && r[1] == id),
                "outlet {id} was routed into a channel"
            );
        }
    }

    #[test]
    fn changing_an_apps_channel_moves_it_now_and_is_remembered() {
        let (settings_path, dropin) = paths("move");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        sonar.enable().unwrap();
        sonar.set_app_channel("firefox", Channel::Game).unwrap();
        assert!(fake.ran(&["pw-metadata", "41", "target.object", "gglinux_game"]));
        assert_eq!(settings::load(&settings_path).rules.channel_for(Some("firefox")), Channel::Game);
    }

    #[test]
    fn chatmix_turns_down_the_other_side() {
        let (settings_path, dropin) = paths("chatmix");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_chatmix(0.5).unwrap();
        assert!(fake.ran(&["wpctl", "set-volume", "30", "0.500"]), "Game outlet down");
        assert!(fake.ran(&["wpctl", "set-volume", "31", "1.000"]), "Chat outlet full");
    }

    #[test]
    fn volume_is_saved_and_applied_to_the_channel_sink() {
        let (settings_path, dropin) = paths("volume");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        sonar.enable().unwrap();
        sonar.set_volume(Channel::Chat, 0.4).unwrap();
        assert!(fake.ran(&["wpctl", "set-volume", "21", "0.400"]));
        assert_eq!(settings::load(&settings_path).volumes.get(&Channel::Chat), Some(&0.4));
    }

    #[test]
    fn output_change_retargets_outlets_without_a_restart() {
        let (settings_path, dropin) = paths("output");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        let restarts = fake.count("systemctl");
        sonar.set_output(HEADSET).unwrap();
        for id in ["30", "31", "32", "33"] {
            assert!(fake.ran(&["pw-metadata", id, "target.object", HEADSET]));
        }
        assert_eq!(fake.count("systemctl"), restarts);
        assert!(std::fs::read_to_string(&dropin).unwrap().contains(HEADSET));
        assert!(sonar.set_output("no_such_sink").is_err());
    }

    #[test]
    fn disable_removes_the_dropin_and_restores_the_default() {
        let (settings_path, dropin) = paths("disable");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        sonar.enable().unwrap();
        sonar.disable().unwrap();
        assert!(!dropin.exists());
        assert_eq!(fake.count("systemctl"), 2);
        assert!(fake.ran(&["wpctl", "set-default", "11"]), "back to the Bluetooth output");
        assert!(!settings::load(&settings_path).enabled);
    }

    #[test]
    fn a_removed_dropin_means_sonar_is_off() {
        let (settings_path, dropin) = paths("removed");
        let saved = Settings { enabled: true, ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        let sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        assert!(!sonar.is_enabled());
        assert!(!settings::load(&settings_path).enabled);
        assert_eq!(fake.count("systemctl"), 0, "nothing re-installed behind the user's back");
    }

    #[test]
    fn state_lists_outputs_channels_and_apps() {
        let (settings_path, dropin) = paths("state");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        let state = sonar.state();
        assert!(state.available && state.enabled);
        assert_eq!(state.outputs.len(), 2);
        assert!(state.channels.iter().all(|c| c.present));
        let discord = state.apps.iter().find(|a| a.id == 40).unwrap();
        assert_eq!(discord.channel, Channel::Chat);
        assert_eq!(state.apps.len(), 3, "outlets are not apps");
    }
}
```

- [ ] **Step 2: Run** `cargo test -p gg-audio sonar` — Expected: compile errors.

- [ ] **Step 3: Implement** (prepend):

```rust
//! Sonar: the channels, routing apps into them, their volumes, and ChatMix.
//!
//! PipeWire hosts the graph (see [`config`]); this drives it through
//! [`PwTools`], so every action is a command a person could type, and the whole
//! flow is tested against a fake.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gg_ipc::{AppStream, AudioOutput, Channel, ChannelState, SonarState};
use tracing::{debug, info, warn};

use crate::graph::Graph;
use crate::settings::{self, Settings};
use crate::tools::{self, PwTools};
use crate::{config, mix};

/// How long PipeWire gets to bring nodes up after a restart.
const APPEAR_TIMEOUT: Duration = Duration::from_secs(5);
const APPEAR_POLL: Duration = Duration::from_millis(250);

pub struct Sonar<T: PwTools> {
    tools: T,
    settings: Settings,
    settings_path: PathBuf,
    dropin_path: PathBuf,
    graph: Graph,
    /// Where each app stream (by node id) was last sent, so each is routed once.
    routed: HashMap<u32, Channel>,
    available: bool,
}

impl<T: PwTools> Sonar<T> {
    /// Load saved settings and look at PipeWire once. If settings say enabled
    /// but the drop-in is gone, the user removed it by hand: become disabled
    /// rather than put it back.
    pub fn new(tools: T, settings_path: PathBuf, dropin_path: PathBuf) -> Self {
        let settings = settings::load(&settings_path);
        let mut sonar = Sonar {
            tools,
            settings,
            settings_path,
            dropin_path,
            graph: Graph::default(),
            routed: HashMap::new(),
            available: false,
        };
        sonar.refresh();
        if sonar.settings.enabled && !sonar.dropin_path.exists() {
            info!("Sonar's PipeWire drop-in is gone; treating Sonar as off");
            sonar.settings.enabled = false;
            sonar.persist();
        } else if sonar.settings.enabled {
            if let Err(e) = sonar.apply_levels() {
                warn!(error = %format!("{e:#}"), "could not restore Sonar volumes");
            }
        }
        sonar
    }

    pub fn is_enabled(&self) -> bool {
        self.settings.enabled
    }

    /// One routing pass: re-read PipeWire, send new app streams to their channels.
    pub fn tick(&mut self) {
        self.refresh();
        if self.settings.enabled {
            self.route();
        }
    }

    /// Current state. Re-reads PipeWire when Sonar is off, since no tick keeps
    /// the graph fresh then.
    pub fn state(&mut self) -> SonarState {
        if !self.settings.enabled {
            self.refresh();
        }
        self.snapshot()
    }

    /// State from the last read, without touching PipeWire.
    pub fn snapshot(&self) -> SonarState {
        SonarState {
            available: self.available,
            enabled: self.settings.enabled,
            output: self.settings.output.clone(),
            outputs: self
                .graph
                .physical_sinks()
                .map(|n| AudioOutput {
                    name: n.name.clone(),
                    description: n.description.clone().unwrap_or_else(|| n.name.clone()),
                })
                .collect(),
            channels: Channel::ALL
                .iter()
                .map(|&channel| {
                    let node = self.graph.node(&config::sink_name(channel));
                    ChannelState { channel, present: node.is_some(), volume: node.and_then(|n| n.volume) }
                })
                .collect(),
            chatmix: self.settings.chatmix,
            apps: self
                .graph
                .app_streams()
                .map(|s| AppStream {
                    id: s.id,
                    name: s.app_name.clone().unwrap_or_else(|| s.name.clone()),
                    binary: s.app_binary.clone(),
                    channel: self.settings.rules.channel_for(s.app_binary.as_deref()),
                })
                .collect(),
        }
    }

    /// Install the channels and take over the default output.
    pub fn enable(&mut self) -> Result<()> {
        if self.settings.enabled {
            return Ok(());
        }
        self.refresh();
        if !self.available {
            bail!("PipeWire is not reachable (pw-dump failed)");
        }
        let previous_default = self.graph.default_sink.clone().filter(|n| !config::is_ours(n));
        let output = self.choose_output().context("no output device for the channels to play into")?;

        let text = config::dropin(&output)?;
        if let Some(dir) = self.dropin_path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&self.dropin_path, text)
            .with_context(|| format!("writing {}", self.dropin_path.display()))?;
        info!(%output, path = %self.dropin_path.display(), "enabling Sonar; restarting PipeWire");
        self.tools.run(&tools::restart_pipewire())?;

        let game = config::sink_name(Channel::Game);
        let all_up = |g: &Graph| Channel::ALL.iter().all(|&c| g.node(&config::sink_name(c)).is_some());
        if !self.wait_until(all_up) {
            warn!("Sonar channels did not appear; undoing");
            let _ = std::fs::remove_file(&self.dropin_path);
            let _ = self.tools.run(&tools::restart_pipewire());
            bail!(
                "PipeWire did not create the Sonar channels within {} s, so the change was undone",
                APPEAR_TIMEOUT.as_secs()
            );
        }
        if let Some(node) = self.graph.node(&game) {
            self.tools.run(&tools::set_default(node.id))?;
        }

        self.settings.enabled = true;
        self.settings.output = Some(output);
        self.settings.previous_default = previous_default;
        self.persist();
        self.apply_levels()?;
        self.routed.clear();
        self.route();
        Ok(())
    }

    /// Remove the channels and give the default output back.
    pub fn disable(&mut self) -> Result<()> {
        if !self.settings.enabled {
            return Ok(());
        }
        match std::fs::remove_file(&self.dropin_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", self.dropin_path.display())),
        }
        info!("disabling Sonar; restarting PipeWire");
        self.tools.run(&tools::restart_pipewire())?;
        self.settings.enabled = false;
        self.routed.clear();
        let previous = self.settings.previous_default.take();
        self.persist();
        if let Some(name) = previous {
            if self.wait_until(|g| g.node(&name).is_some()) {
                if let Some(node) = self.graph.node(&name) {
                    self.tools.run(&tools::set_default(node.id))?;
                }
            } else {
                warn!(%name, "the previous default output did not come back");
            }
        }
        Ok(())
    }

    /// Play the channels into a different physical output.
    pub fn set_output(&mut self, name: &str) -> Result<()> {
        self.refresh();
        if !self.graph.physical_sinks().any(|n| n.name == name) {
            bail!("no output device named {name}");
        }
        let text = config::dropin(name)?;
        self.settings.output = Some(name.to_owned());
        self.persist();
        if self.settings.enabled {
            // Re-target now, and rewrite the drop-in for the next PipeWire start.
            std::fs::write(&self.dropin_path, text)
                .with_context(|| format!("writing {}", self.dropin_path.display()))?;
            for channel in Channel::ALL {
                if let Some(node) = self.graph.node(&config::outlet_name(channel)) {
                    self.tools.run(&tools::set_target(node.id, name))?;
                }
            }
        }
        Ok(())
    }

    pub fn set_volume(&mut self, channel: Channel, volume: f32) -> Result<()> {
        if !volume.is_finite() {
            bail!("volume must be a number");
        }
        let volume = volume.clamp(0.0, 1.0);
        self.settings.volumes.insert(channel, volume);
        self.persist();
        if let Some(node) = self.graph.node(&config::sink_name(channel)) {
            self.tools.run(&tools::set_volume(node.id, volume))?;
        }
        Ok(())
    }

    pub fn set_chatmix(&mut self, balance: f32) -> Result<()> {
        if !balance.is_finite() {
            bail!("ChatMix balance must be a number");
        }
        self.settings.chatmix = balance.clamp(-1.0, 1.0);
        self.persist();
        self.apply_chatmix()
    }

    /// Route an app to a channel now and from now on.
    pub fn set_app_channel(&mut self, binary: &str, channel: Channel) -> Result<()> {
        if binary.trim().is_empty() {
            bail!("no app to route");
        }
        self.settings.rules.set(binary, channel);
        self.persist();
        if self.settings.enabled {
            self.route();
        }
        Ok(())
    }

    // --- internals ------------------------------------------------------------

    /// Re-read PipeWire. A failure means PipeWire or its tools are unavailable.
    fn refresh(&mut self) {
        match self.tools.dump().and_then(|json| Graph::parse(&json)) {
            Ok(graph) => {
                self.graph = graph;
                self.available = true;
            }
            Err(e) => {
                debug!(error = %format!("{e:#}"), "PipeWire unavailable");
                self.graph = Graph::default();
                self.available = false;
            }
        }
    }

    /// Re-read PipeWire until `ready` holds, for up to [`APPEAR_TIMEOUT`].
    fn wait_until(&mut self, ready: impl Fn(&Graph) -> bool) -> bool {
        let tries = (APPEAR_TIMEOUT.as_millis() / APPEAR_POLL.as_millis()) as usize;
        for _ in 0..tries {
            self.refresh();
            if ready(&self.graph) {
                return true;
            }
            self.tools.pause(APPEAR_POLL);
        }
        false
    }

    /// The saved output if it still exists, else the current default if it is a
    /// real device, else the first real device.
    fn choose_output(&self) -> Option<String> {
        let physical = |name: &String| self.graph.physical_sinks().any(|n| &n.name == name);
        self.settings
            .output
            .clone()
            .filter(physical)
            .or_else(|| self.graph.default_sink.clone().filter(physical))
            .or_else(|| self.graph.physical_sinks().next().map(|n| n.name.clone()))
    }

    /// Send every app stream whose channel changed (or is new) to its channel.
    fn route(&mut self) {
        let present: Vec<u32> = self.graph.app_streams().map(|s| s.id).collect();
        self.routed.retain(|id, _| present.contains(id));
        let moves: Vec<(u32, Channel)> = self
            .graph
            .app_streams()
            .map(|s| (s.id, self.settings.rules.channel_for(s.app_binary.as_deref())))
            .filter(|(id, channel)| self.routed.get(id) != Some(channel))
            .collect();
        for (id, channel) in moves {
            match self.tools.run(&tools::set_target(id, &config::sink_name(channel))) {
                Ok(()) => {
                    debug!(id, ?channel, "routed app stream");
                    self.routed.insert(id, channel);
                }
                Err(e) => warn!(id, error = %format!("{e:#}"), "could not route app stream"),
            }
        }
    }

    /// Push saved channel volumes and ChatMix to PipeWire.
    fn apply_levels(&self) -> Result<()> {
        for (&channel, &volume) in &self.settings.volumes {
            if let Some(node) = self.graph.node(&config::sink_name(channel)) {
                self.tools.run(&tools::set_volume(node.id, volume))?;
            }
        }
        self.apply_chatmix()
    }

    /// ChatMix lives on the Game and Chat outlets, so it multiplies with the
    /// channel volumes instead of overwriting them.
    fn apply_chatmix(&self) -> Result<()> {
        let (game, chat) = mix::chatmix_gains(self.settings.chatmix);
        for (channel, gain) in [(Channel::Game, game), (Channel::Chat, chat)] {
            if let Some(node) = self.graph.node(&config::outlet_name(channel)) {
                self.tools.run(&tools::set_volume(node.id, gain))?;
            }
        }
        Ok(())
    }

    fn persist(&self) {
        if let Err(e) = settings::save(&self.settings_path, &self.settings) {
            warn!(error = %format!("{e:#}"), "could not save Sonar settings");
        }
    }
}
```

- [ ] **Step 4: Run** `cargo test -p gg-audio && cargo clippy -p gg-audio --all-targets` — Expected: 27 pass; no warnings.
- [ ] **Step 5: Commit** `gg-audio: Sonar enable/disable, routing, volumes, ChatMix`

---

### Task 6: Daemon — sonar worker and IPC

**Files:** Create `crates/gglinuxd/src/sonar.rs`; modify `crates/gglinuxd/src/main.rs`, `crates/gglinuxd/Cargo.toml` (`gg-audio = { path = "../gg-audio" }`).

**Consumes:** Task 5 `Sonar<SystemTools>` API, Task 1 IPC. **Produces:** `sonar::{spawn, Handle, Command}`.

- [ ] **Step 1: Worker** — `crates/gglinuxd/src/sonar.rs`:

```rust
//! Sonar worker: channel routing on its own thread.
//!
//! Every PipeWire action shells out to PipeWire's tools and can block — enabling
//! restarts PipeWire and waits for it — so it runs off the async runtime, like
//! the device workers.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use gg_audio::sonar::Sonar;
use gg_audio::tools::SystemTools;
use gg_ipc::{Channel, Event, SonarState};
use tokio::sync::{broadcast, oneshot};

/// How often to look for new app streams while Sonar is on.
const TICK: Duration = Duration::from_secs(1);

/// Reply channel for a command. `Err` carries a message meant for the user.
type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum Command {
    GetState(Reply<SonarState>),
    SetEnabled { enabled: bool, reply: Reply<()> },
    SetOutput { name: String, reply: Reply<()> },
    SetVolume { channel: Channel, volume: f32, reply: Reply<()> },
    SetChatMix { balance: f32, reply: Reply<()> },
    SetAppChannel { binary: String, channel: Channel, reply: Reply<()> },
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| "Sonar worker is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "Sonar worker dropped the request".to_string())?
    }
}

pub fn spawn(event_tx: broadcast::Sender<Event>) -> Handle {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("gg-sonar".into())
        .spawn(move || worker(rx, event_tx))
        .expect("spawn gg-sonar thread");
    Handle { tx }
}

fn worker(rx: Receiver<Command>, event_tx: broadcast::Sender<Event>) {
    let mut sonar = Sonar::new(SystemTools, gg_audio::settings::path(), gg_audio::config::dropin_path());
    let mut published: Option<SonarState> = None;
    let mut last_tick = Instant::now();
    loop {
        match rx.recv_timeout(TICK) {
            Ok(command) => handle(command, &mut sonar),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // By elapsed time, so a stream of commands cannot starve routing.
        if sonar.is_enabled() && last_tick.elapsed() >= TICK {
            sonar.tick();
            last_tick = Instant::now();
        }
        let now = sonar.snapshot();
        if published.as_ref() != Some(&now) {
            published = Some(now.clone());
            let _ = event_tx.send(Event::SonarChanged { state: now });
        }
    }
}

fn handle(command: Command, sonar: &mut Sonar<SystemTools>) {
    let text = |e: anyhow::Error| format!("{e:#}");
    match command {
        Command::GetState(reply) => {
            let _ = reply.send(Ok(sonar.state()));
        }
        Command::SetEnabled { enabled, reply } => {
            let result = if enabled { sonar.enable() } else { sonar.disable() };
            let _ = reply.send(result.map_err(text));
        }
        Command::SetOutput { name, reply } => {
            let _ = reply.send(sonar.set_output(&name).map_err(text));
        }
        Command::SetVolume { channel, volume, reply } => {
            let _ = reply.send(sonar.set_volume(channel, volume).map_err(text));
        }
        Command::SetChatMix { balance, reply } => {
            let _ = reply.send(sonar.set_chatmix(balance).map_err(text));
        }
        Command::SetAppChannel { binary, channel, reply } => {
            let _ = reply.send(sonar.set_app_channel(&binary, channel).map_err(text));
        }
    }
}
```

- [ ] **Step 2: Wire `main.rs`** — `mod sonar;` (after `mod mouse;`), `use sonar::{Command as SonarCommand, Handle as SonarHandle};`, `Workers` gains `sonar: SonarHandle`, spawned with `sonar: sonar::spawn(event_tx.clone()),`. Replace the temporary Sonar arm in `dispatch` with:

```rust
        Request::GetSonarState => match workers.sonar.send(SonarCommand::GetState).await {
            Ok(state) => ResponseData::Sonar { state },
            Err(message) => ResponseData::Error { message },
        },
        Request::SetSonarEnabled { enabled } => outcome(
            workers.sonar.send::<()>(|reply| SonarCommand::SetEnabled { enabled, reply }).await,
        ),
        Request::SetSonarOutput { name } => outcome(
            workers.sonar.send::<()>(|reply| SonarCommand::SetOutput { name, reply }).await,
        ),
        Request::SetChannelVolume { channel, volume } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetVolume { channel, volume, reply })
                .await,
        ),
        Request::SetChatMix { balance } => outcome(
            workers.sonar.send::<()>(|reply| SonarCommand::SetChatMix { balance, reply }).await,
        ),
        Request::SetAppChannel { binary, channel } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetAppChannel { binary, channel, reply })
                .await,
        ),
```

Update the crate doc's scope sentence to mention Sonar channels.

- [ ] **Step 3: Run** `cargo test --workspace && cargo build --workspace --all-targets && cargo clippy --workspace --all-targets` — Expected: all pass, no warnings.

- [ ] **Step 4: Read-only smoke run** — start `./target/debug/gglinuxd`, send `get_sonar_state` over the socket, stop it. Expected: `available: true`, `enabled: false`, outputs listing this machine's sinks, apps listing running streams. **No enable.**

- [ ] **Step 5: Commit** `gglinuxd: Sonar worker and IPC`

---

### Task 7: UI — Sonar tab

**Files:** Modify `ui/src/lib/types.ts`, `ui/src/lib/daemon.svelte.ts`, `ui/src/App.svelte`; create `ui/src/lib/SonarPanel.svelte`.

- [ ] **Step 1: Types** (`types.ts`, after the headset types):

```ts
export type Channel = "game" | "chat" | "media" | "aux";

export interface AudioOutput {
  name: string;
  description: string;
}

export interface ChannelState {
  channel: Channel;
  present: boolean;
  /** 0..=1 on the desktop mixer's scale; null when absent. */
  volume: number | null;
}

export interface AppStream {
  id: number;
  name: string;
  binary: string | null;
  channel: Channel;
}

export interface SonarState {
  available: boolean;
  enabled: boolean;
  output: string | null;
  outputs: AudioOutput[];
  channels: ChannelState[];
  /** -1 all Game … 0 both full … 1 all Chat. */
  chatmix: number;
  apps: AppStream[];
}
```

`Request` gains `get_sonar_state`, `set_sonar_enabled {enabled}`, `set_sonar_output {name}`, `set_channel_volume {channel, volume}`, `set_chat_mix {balance}`, `set_app_channel {binary, channel}`; `ResponseData` gains `{ type: "sonar"; state: SonarState }`; `DaemonEvent` gains `{ event: "sonar_changed"; data: { state: SonarState } }`.

- [ ] **Step 2: Client** — `daemon.svelte.ts`: `sonar = $state<SonarState | null>(null);` (cleared on disconnect); methods:

```ts
  async getSonarState(): Promise<SonarState> {
    const res = await this.request({ method: "get_sonar_state" });
    if (res.type !== "sonar") throw new Error(`unexpected response: ${res.type}`);
    this.sonar = res.state;
    return res.state;
  }

  /** Every Sonar change answers `success`, then the fresh state is fetched. */
  async #sonarCommand(req: Request): Promise<void> {
    const res = await this.request(req);
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getSonarState();
  }

  setSonarEnabled(enabled: boolean) {
    return this.#sonarCommand({ method: "set_sonar_enabled", params: { enabled } });
  }
  setSonarOutput(name: string) {
    return this.#sonarCommand({ method: "set_sonar_output", params: { name } });
  }
  setChannelVolume(channel: Channel, volume: number) {
    return this.#sonarCommand({ method: "set_channel_volume", params: { channel, volume } });
  }
  setChatMix(balance: number) {
    return this.#sonarCommand({ method: "set_chat_mix", params: { balance } });
  }
  setAppChannel(binary: string, channel: Channel) {
    return this.#sonarCommand({ method: "set_app_channel", params: { binary, channel } });
  }
```

and `case "sonar_changed": this.sonar = ev.data.state; break;`. Raise the request timeout for `set_sonar_enabled` — enabling restarts PipeWire and waits up to 5 s for it: make `request` take an optional `timeoutMs` (default `REQUEST_TIMEOUT_MS`) and pass `15000` from `setSonarEnabled`.

- [ ] **Step 3: Panel** — `ui/src/lib/SonarPanel.svelte`:

```svelte
<script lang="ts">
  // Sonar: Game / Chat / Media / Aux channels hosted by PipeWire, apps routed
  // into them by rule, and ChatMix between Game and Chat.

  import { daemon } from "./daemon.svelte";
  import type { Channel } from "./types";

  const CHANNELS: { value: Channel; label: string }[] = [
    { value: "game", label: "Game" },
    { value: "chat", label: "Chat" },
    { value: "media", label: "Media" },
    { value: "aux", label: "Aux" },
  ];
  const label = (c: Channel) => CHANNELS.find((x) => x.value === c)?.label ?? c;

  let sonar = $derived(daemon.sonar);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);

  $effect(() => {
    if (!daemon.connected) return;
    void daemon.getSonarState().catch((e: unknown) => {
      error = e instanceof Error ? e.message : String(e);
    });
  });

  async function run(tag: string, op: () => Promise<void>) {
    busy = tag;
    error = null;
    try {
      await op();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = null;
    }
  }

  const pct = (v: number | null) => (v === null ? "–" : `${Math.round(v * 100)}%`);
</script>

<article class="panel">
  {#if !sonar}
    <p class="hint">Loading…</p>
  {:else if !sonar.available}
    <p class="hint">PipeWire isn't reachable, so Sonar can't run.</p>
  {:else}
    <header class="top">
      <div>
        <h3>Sonar</h3>
        <p class="meta">
          {sonar.enabled
            ? "Channels are live. Apps are routed by the rules below."
            : "Off. Your apps play straight to your default output."}
        </p>
      </div>
      <button
        class="apply"
        disabled={busy !== null}
        onclick={() => run("enable", () => daemon.setSonarEnabled(!sonar!.enabled))}
      >
        {busy === "enable" ? "Restarting PipeWire…" : sonar.enabled ? "Turn off" : "Turn on"}
      </button>
    </header>
    <p class="hint">Turning Sonar on or off restarts PipeWire: a second or two of silence.</p>

    <section>
      <div class="row">
        <span class="label">Output</span>
        <select
          value={sonar.output ?? ""}
          disabled={busy !== null}
          onchange={(e) => run("output", () => daemon.setSonarOutput(e.currentTarget.value))}
        >
          {#if !sonar.output}<option value="">chosen when Sonar turns on</option>{/if}
          {#each sonar.outputs as o (o.name)}<option value={o.name}>{o.description}</option>{/each}
        </select>
      </div>
    </section>

    {#if sonar.enabled}
      <section>
        <div class="head"><h4>Channels</h4></div>
        {#each sonar.channels as c (c.channel)}
          <div class="row">
            <span class="label">{label(c.channel)}</span>
            <input
              type="range"
              min="0"
              max="100"
              step="1"
              value={Math.round((c.volume ?? 1) * 100)}
              disabled={!c.present || busy !== null}
              onchange={(e) =>
                run("volume", () => daemon.setChannelVolume(c.channel, e.currentTarget.valueAsNumber / 100))}
            />
            <span class="unit">{c.present ? pct(c.volume) : "missing"}</span>
          </div>
        {/each}
      </section>

      <section>
        <div class="head">
          <h4>ChatMix</h4>
          <button class="ghost" disabled={busy !== null} onclick={() => run("chatmix", () => daemon.setChatMix(0))}>
            Centre
          </button>
        </div>
        <div class="row">
          <span class="label">Game</span>
          <input
            type="range"
            min="-100"
            max="100"
            step="5"
            value={Math.round(sonar.chatmix * 100)}
            disabled={busy !== null}
            onchange={(e) => run("chatmix", () => daemon.setChatMix(e.currentTarget.valueAsNumber / 100))}
          />
          <span class="unit">Chat</span>
        </div>
      </section>

      <section>
        <div class="head"><h4>Apps</h4></div>
        {#if sonar.apps.length === 0}
          <p class="hint">No app is playing audio right now.</p>
        {:else}
          <ul class="apps">
            {#each sonar.apps as app (app.id)}
              <li>
                <span class="app">{app.name}</span>
                <select
                  value={app.channel}
                  disabled={!app.binary || busy !== null}
                  onchange={(e) =>
                    run("route", () => daemon.setAppChannel(app.binary!, e.currentTarget.value as Channel))}
                >
                  {#each CHANNELS as c (c.value)}<option value={c.value}>{c.label}</option>{/each}
                </select>
              </li>
            {/each}
          </ul>
          <p class="hint">A change applies now and is remembered for that app.</p>
        {/if}
      </section>
    {/if}
  {/if}

  {#if error}
    <p class="error">{error}</p>
  {/if}
</article>

<style>
  .panel {
    padding: 1rem 1.1rem;
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: 12px;
  }
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
  }
  h3 {
    margin: 0;
    font-size: 0.98rem;
    font-weight: 600;
  }
  h4 {
    margin: 0;
    font-size: 0.8rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-dim);
  }
  .meta {
    margin: 0.25rem 0 0;
    font-size: 0.82rem;
    color: var(--text-dim);
  }
  section {
    margin-top: 1.1rem;
    padding-top: 1rem;
    border-top: 1px solid var(--line);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
    margin-bottom: 0.6rem;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin-top: 0.5rem;
  }
  .label {
    width: 4.5rem;
    font-size: 0.82rem;
    color: var(--text-dim);
  }
  .row input[type="range"] {
    flex: 1;
    max-width: 18rem;
  }
  .unit {
    width: 3.5rem;
    font-size: 0.78rem;
    color: var(--text-dim);
  }
  select {
    padding: 0.3rem 0.45rem;
    font-size: 0.85rem;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 7px;
  }
  .apps {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .apps li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
    max-width: 24rem;
    padding: 0.2rem 0;
  }
  .app {
    font-size: 0.85rem;
  }
  .hint {
    margin: 0.6rem 0 0;
    font-size: 0.76rem;
    color: var(--text-dim);
  }
  .apply {
    padding: 0.4rem 0.85rem;
    font-size: 0.83rem;
    font-weight: 500;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 8px;
    cursor: pointer;
  }
  .apply:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent);
  }
  .apply:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .ghost {
    padding: 0.2rem 0.5rem;
    font-size: 0.76rem;
    color: var(--text-dim);
    background: none;
    border: 1px solid var(--line);
    border-radius: 7px;
    cursor: pointer;
  }
  .error {
    margin: 0.8rem 0 0;
    padding: 0.5rem 0.7rem;
    font-size: 0.8rem;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
    border-radius: 8px;
  }
</style>
```

- [ ] **Step 4: Mount** — in `App.svelte`: import `SonarPanel`; change `COMING` to `Record<Exclude<Page, "engine" | "sonar">, string>` without the `sonar` entry; before the generic `{:else}` page branch add:

```svelte
    {:else if page === "sonar"}
      <header class="page-head">
        <h1>Sonar</h1>
        <p>{NAV[1].blurb}</p>
      </header>
      {#if daemon.connected}
        <SonarPanel />
      {:else}
        <section class="notice"><h2>gglinuxd isn&rsquo;t running</h2></section>
      {/if}
```

and give the generic branch's `COMING[page]` a cast-free lookup (`page` is narrowed to `"moments" | "settings"` there).

- [ ] **Step 5: Run** `cd ui && npm run check && npx vite build` — Expected: 0 errors, 0 warnings; build OK.
- [ ] **Step 6: Commit** `UI: Sonar tab with channels, ChatMix, and app routing`

---

### Task 8: Docs

- [ ] **Step 1:** `PLAN.md` M3 section: add a progress paragraph — "Progress 2026-09-27 — **M3a done in software**: `gg-audio` declares Game/Chat/Media/Aux as PipeWire loopbacks in a drop-in (hosted by PipeWire, so they outlive the daemon), routes apps by `application.process.binary`, sets channel volumes, and does ChatMix on the Game/Chat outlets; the Sonar tab drives it (spec: `docs/superpowers/specs/2026-09-27-sonar-channels-design.md`). The approach moved from pipewire-rs to PipeWire's own CLI tools (`pw-dump`/`pw-metadata`/`wpctl`) behind a trait — no native bindings, and every action is a command a person could run. Next: M3b per-channel EQ, M3c mic chain, then streamer mode."
- [ ] **Step 2:** Commit `docs: M3a progress`

---

### Task 9: Hardware acceptance (owner; restarts PipeWire — ask first)

- [ ] Turn Sonar on from the UI → a short audio gap; four "gglinux …" outputs appear in KDE's audio settings; Game is the default.
- [ ] Discord (if installed) plays in Chat, Firefox in Media, a game in Game (Apps list agrees).
- [ ] Move an app in the Apps list → it moves immediately; restart the app → it stays.
- [ ] ChatMix fully to Game → chat silent; fully to Chat → game silent; Centre → both.
- [ ] Turn Sonar off → gap; the gglinux outputs vanish; the previous default output is back.
