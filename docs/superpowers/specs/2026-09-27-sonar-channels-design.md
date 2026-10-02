# Sonar channels (M3a) — design

**Status:** approved in conversation 2026-09-27 ("build").
**Milestone:** M3, first of three parts (M3a channels + routing + ChatMix; M3b EQ; M3c mic chain).
Streamer mode follows M3c.

## Goal

Sonar-style audio on PipeWire: four virtual outputs — **Game, Chat, Media, Aux** — that apps are
routed to by rule, each with its own volume, balanced Game-vs-Chat by a **ChatMix** slider in the
app. Success: turning Sonar on makes Game the default output; Discord lands in Chat and Firefox in
Media without touching anything; moving an app in the UI moves it immediately and is remembered;
the ChatMix slider fades Game against Chat; turning Sonar off leaves the system as it was.

Out of scope here: EQ (M3b), mic processing (M3c), streamer mix, the base-station knob / hardware
ChatMix dial (the owner does not use it).

## Decisions (owner, 2026-09-27)

- The graph lives in **PipeWire config**, hosted by PipeWire itself, so audio keeps working when
  gglinux is not running. Enabling/disabling restarts PipeWire (1–2 s audio gap).
- ChatMix is a UI slider only.
- Enabling makes **Game** the default output; disabling restores the previous default.

## Environment facts (checked 2026-09-27)

- PipeWire 1.6.8, WirePlumber 0.5.x. WirePlumber honours `target.object` in stream props and in
  the `default` metadata, and `node.dont-fallback` (`scripts/linking/find-defined-target.lua`).
- The current default sink can be any device (it was a Bluetooth sink during design), so the
  output is chosen, not assumed to be the Arctis.
- `pw-dump` gives nodes with `info.props` (`node.name`, `media.class`,
  `application.process.binary`, …) and `info.params.Props[0].channelVolumes` (linear); the
  `default` metadata carries `default.configured.audio.sink` = `{"name": …}`.

## Design

### Graph (PipeWire drop-in)

`$XDG_CONFIG_HOME/pipewire/pipewire.conf.d/60-gglinux-sonar.conf` declares four
`libpipewire-module-loopback` instances. For channel `c` ∈ {game, chat, media, aux}:

- capture side = the virtual sink: `node.name = "gglinux_<c>"`, `media.class = "Audio/Sink"`,
  description "gglinux Game" etc., stereo.
- playback side = its **outlet**: `node.name = "gglinux_<c>.out"`, `target.object = "<output>"`,
  `node.dont-fallback = true` (an outlet whose output vanishes waits rather than falling back to
  the default — which is Game — and looping), `node.passive = true`, `stream.dont-remix = true`.

### Control

gglinux drives PipeWire with its own tools behind a `PwTools` trait (`pw-dump`; `pw-metadata`;
`wpctl`; `systemctl --user restart pipewire pipewire-pulse wireplumber`). Everything that builds a
command or parses output is pure and unit-tested; a fake `PwTools` tests the orchestration.

- **Enable:** pick the output (saved choice if present, else the current default sink if it is a
  physical one — the one WirePlumber plays into while the configured one is away — else the first
  physical sink), remember the current default, write the drop-in,
  restart PipeWire, wait up to 5 s for the four sinks, make Game the default, apply saved volumes
  and ChatMix, route all running streams. If the sinks never appear, or a step after writing the
  drop-in fails: remove the drop-in, restart again, report the error.
- **Disable:** remove the drop-in, restart PipeWire, restore the remembered default if PipeWire has
  it (one that's away isn't waited for: WirePlumber remembers it and picks it when it returns).
- **Startup:** if settings say enabled but the drop-in is gone (the user removed it), become
  disabled; otherwise resume routing and re-apply volumes/ChatMix. If settings say disabled but the
  drop-in is there (a backup from while Sonar was off was restored, say), remove it, and restart
  PipeWire if it runs the channels.
- **Routing:** while enabled, every 1 s read `pw-dump`; each app stream (`Stream/Output/Audio`,
  not one of our outlets) is sent to its channel with `pw-metadata <id> target.object gglinux_<c>`
  once per stream id (again only if its rule changes). Rules map `application.process.binary`
  (case-insensitive) to a channel; unknown apps → Game. Defaults: discord, vesktop, teamspeak,
  ts3client_linux_amd64, mumble → Chat; firefox, chromium, chrome, brave, spotify, mpv, vlc → Media.
- **Volume:** channel volume = the sink's volume (`wpctl set-volume <sink> v`, 0–1, the same cubic
  scale the desktop mixer shows); saved and re-applied on enable/startup. A change made elsewhere
  (the desktop mixer, the volume keys, an app) is saved too, so a restart brings back the level the
  user last had; the same goes for the virtual mic's volume.
- **ChatMix:** balance −1 (all Game) … 0 (both full) … +1 (all Chat). The favoured side stays at
  full, the other is turned down linearly; applied to the Game and Chat **outlets**, so it
  multiplies with, and never overwrites, the channel volumes.
- **Output change:** re-target the four outlets at runtime (`pw-metadata`) and rewrite the drop-in
  (no restart), so the next PipeWire start uses it too.

### Settings

`$XDG_CONFIG_HOME/gglinux/sonar.json`: `enabled`, `output`, `previous_default`, `chatmix`,
`volumes` (per channel), `rules`. Missing or unreadable → defaults (logged). Written atomically
(temp file + rename).

### Code layout

New crate **`gg-audio`** (depends on `gg-ipc` for the wire types): `config` (drop-in text + paths),
`graph` (pw-dump parsing), `rules`, `mix` (ChatMix gains), `settings`, `tools` (`PwTools`, the real
implementation, command builders), `sonar` (the orchestration: enable/disable/tick/setters/state).
`gglinuxd` gets a `sonar` worker thread (same shape as the device workers) that ticks every second
and serves IPC. The UI's empty **Sonar** tab becomes `SonarPanel.svelte`.

### IPC

`Channel { Game, Chat, Media, Aux }`; `SonarState { available, enabled, output, outputs:
[AudioOutput {name, description}], channels: [ChannelState {channel, present, volume}], chatmix,
apps: [AppStream {id, name, binary, channel}] }`. Requests: `get_sonar_state`,
`set_sonar_enabled {enabled}`, `set_sonar_output {name}`, `set_channel_volume {channel, volume}`,
`set_chat_mix {balance}`, `set_app_channel {binary, channel}`. Event `sonar_changed {state}`.

## Error handling

- PipeWire tools missing or failing → `available: false`, setters return the tool's error text.
- Enable failures roll back (above). Every other failure leaves settings unchanged.
- Output names are written into the config quoted; names containing `"` or `\` are rejected.

## Testing

Unit: drop-in text; pw-dump parsing against a trimmed capture from this machine; rules; ChatMix
gains; settings round-trip and corrupt-file fallback; orchestration with a fake `PwTools` (enable
writes/restarts/sets default, rollback, routing once per stream, rule change moves running
streams, outlets never routed, ChatMix sets outlet volumes, disable restores default).
Hardware (owner, asked first because it restarts PipeWire): enable → channels appear, Game is
default; Discord/Firefox land in Chat/Media; slider moves; disable restores.
