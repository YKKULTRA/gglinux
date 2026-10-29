# Architecture

A short map of how gglinux fits together. The byte-level device protocols are in
[PROTOCOLS.md](PROTOCOLS.md); the design record for each feature is in
[superpowers/specs](superpowers/specs/) with its implementation plan in
[superpowers/plans](superpowers/plans/).

## Processes

```
 ┌──────────────┐   ┌──────────┐            ┌────────────────────────────────────────────┐
 │ gglinux      │   │  ggctl   │            │ gglinuxd (systemd --user service)          │
 │ (the app:    │   │  (CLI)   │            │                                            │
 │ Rust + Slint)│   └────┬─────┘            │  device registry (gg-hid: hidapi + udev)   │
 │              │        │                  │  keyboard ─┐                               │
 │  pages ◄─┐   │        │                  │  headset  ─┼─ one worker thread per device │
 │  client ─┴───┤        │  unix socket,    │  mouse    ─┘   (hidraw, blocking I/O)      │
 │  tray, D-Bus │        │  newline-        │  sonar      ── PipeWire CLI tools          │
 │              ├────────┴─ delimited ──────┤  gamesense  ── HTTP on 127.0.0.1           │
 └──────────────┘           JSON-RPC        │  clips      ── gpu-screen-recorder child   │
                                            │  profiles   ── /proc scan every 2 s        │
                                            └────────────────────────────────────────────┘
```

- **`gglinuxd`** owns everything that touches hardware, audio or the network. It runs as a
  `systemd --user` unit, so it outlives the UI and needs no root.
- **The app** (`crates/gglinux-ui`, binary `gglinux`) is a thin client written in Rust with
  Slint. A connection thread keeps one socket to the daemon, reconnects every second while it's
  away, and keeps the daemon's latest state for every part; after a change succeeds it reads that
  part's state again, because only the daemon knows what was applied. The window is Slint markup
  (`ui/*.slint`): each page's values live in a Slint global that the Rust side fills from the
  state, and each control's callback becomes a request. It also owns a few desktop-only jobs: the
  tray icon (StatusNotifierItem, through `ksni`), one copy at a time (a second launch asks the
  first, over D-Bus as `io.github.ykk.gglinux`, to show its window), start-at-login
  (`~/.config/autostart`), file dialogs through the desktop portal, opening folders, and settings
  export/import.
- **`ggctl`** (`crates/gglinuxd/src/bin/ggctl.rs`) is a command-line client of the same socket.

## The socket protocol

`crates/gg-ipc` defines every message, and both ends use the same types, so a frame one side
accepts is a frame the other side accepts.

- Socket: `$XDG_RUNTIME_DIR/gglinux/gglinuxd.sock`, in a directory systemd creates for the user.
- Framing: one JSON value per line. The client sends `RequestFrame { id, request }`; the daemon
  answers with a `Response` carrying the same `id`, and pushes unsolicited `Notification`s
  (device added or removed, battery, Sonar state, GameSense activity, clip saved, ...).
- Requests on one connection are answered as they finish, so a slow one doesn't hold up the rest.
- A second daemon refuses to start while one is listening on the socket.

## Crates

| Crate | Role |
|---|---|
| `gg-ipc` | Wire types for the socket, shared by the daemon, `ggctl` and the app. |
| `gg-hid` | Enumerates SteelSeries devices by vendor/product/interface/usage page, watches udev for hotplug, and diffs device lists. |
| `gg-device-keyboard` | Apex Pro TKL Gen 3 lighting: frames feature reports from an opcode allowlist, the US ANSI TKL layout and key table. |
| `gg-device-headset` | Arctis Nova Pro Wireless: packet builders and reply decoding for status, settings, EQ and the base-station screen. |
| `gg-device-mouse` | Rival 3 Wireless Gen 2: DPI, polling, buttons, save and battery, with tables generated from rivalcfg. |
| `gg-audio` | Sonar: the PipeWire drop-in that declares the channels, EQ and mic chain as filter chains; reading the graph from `pw-dump`; routing rules; settings. All PipeWire access goes through a `PwTools` trait, so tests run against a fake. |
| `gg-gamesense` | GameSense: request and handler parsing, zones, painting events onto the key grid, the registry of games, and the `coreProps.json` files written into Proton prefixes. |
| `gglinuxd` | The daemon: socket server, device workers, Sonar controller, GameSense HTTP server, clips supervisor, profiles watcher, and the `ggctl` binary. |
| `gglinux-ui` | The desktop app, `gglinux`: the daemon client, the Slint pages, the tray and the one-copy guard. Because it uses `gg-ipc`'s types directly, there's no second copy of the protocol to keep in step. |

The app replaced a Tauri 2 + Svelte UI on 2026-10-29: the same pages, without a browser engine,
at about a quarter of the memory (60 MB in the tray against 240 MB). Why, and how it's put
together: [the design record](superpowers/specs/2026-10-29-native-app-design.md).

## Device workers

Each device type has its own worker thread, started with the daemon. A worker opens its device's
control interface while the device is connected, takes commands from a channel, and publishes
state changes as events. HID I/O is blocking, so it stays off the async runtime.

- **Keyboard**: renders lighting at 30 fps (static per-key colours, breathing, wave, spectrum,
  the audio visualizer, reactive presses, GameSense overlays, the mic-mute light) and hands the
  keyboard back to its onboard lighting when lighting is off or the daemon exits. Every write is
  checked against the opcode allowlist in `gg-device-keyboard`, at framing time and again at the
  write.
- **Headset**: polls status, listens for the base station's unsolicited events, applies settings,
  and runs screen protection (keeps the OLED dark, lights it briefly on real events, re-blanks).
- **Mouse**: applies settings and polls the battery. The protocol has no getters, so the daemon
  reports what it applied, never what it guesses is on the device.

## Sonar (audio)

Sonar is built from PipeWire's own pieces, driven through its command-line tools rather than
native bindings, so every action is something a person could run by hand:

1. Turning Sonar on writes `~/.config/pipewire/pipewire.conf.d/60-gglinux-sonar.conf` and
   restarts PipeWire. The drop-in declares each channel (Game, Chat, Media, Aux, and the Stream
   mix) as a filter chain: preamp, headphone correction, 10-band EQ, and optionally Smart Volume's
   compressor. The virtual mic (RNNoise, gate, compressor) is added only when its LADSPA plugins
   exist.
2. Because PipeWire hosts the channels, they keep working while gglinux is stopped.
3. Live changes (volumes, EQ bands, ChatMix, routing) use `pw-cli set-param`, `pw-metadata` and
   `wpctl`, with no restart.
4. Turning Sonar off removes the drop-in and restarts PipeWire.

## GameSense

An HTTP server on `127.0.0.1` implements the GameSense SDK endpoints. It refuses requests with an
`Origin` header and requires `application/json` on every POST, so web pages can't drive it, and it
caps connections (32) and times out slow headers (5 s) and requests (10 s). While it is on,
gglinux writes `coreProps.json` into every Steam Proton prefix so Windows games find it; it only
ever removes files it wrote itself. Game events are painted onto the keyboard as an overlay.

## Clips and profiles

- **Clips** supervise a `gpu-screen-recorder` replay buffer, restarting it when it dies. Saving
  comes from the GlobalShortcuts portal hotkey, the UI or `ggctl clip save`. "Games only" keeps
  the buffer running only while a game from the profile rules is running.
- **Profiles** are named lighting setups. A watcher scans `/proc` every 2 s and applies the
  profile whose rule matches a running program (including Proton games' `.exe` names), then
  restores the previous lighting when it exits.

## Settings and files

Settings are JSON files in `~/.config/gglinux/` (`lighting.json`, `profiles.json`,
`gamesense.json`, `clips.json`, `sonar.json`, ...). An unreadable file is moved aside as
`<name>.unreadable-<seconds>` rather than overwritten. Files gglinux writes outside that
directory:

| Path | When |
|---|---|
| `~/.config/pipewire/pipewire.conf.d/60-gglinux-sonar.conf` | while Sonar is on |
| `.../compatdata/*/pfx/drive_c/ProgramData/SteelSeries/SteelSeries Engine 3/coreProps.json` | while GameSense is on |
| `~/.config/autostart/gglinux.desktop` | when Start at login is on |
| your videos folder, `gglinux/` by default | saved clips |

## Safety rules for hardware code

- Only the exact USB IDs and interfaces listed in the udev rule are opened.
- The keyboard can only be sent opcodes on its allowlist. The firmware-update commands share that
  interface and can brick it, so they are impossible to frame.
- No device's onboard settings are saved to flash unless the user asks for it.
- Packet builders validate their ranges and are unit-tested byte for byte.

See [CONTRIBUTING.md](../CONTRIBUTING.md#hardware-writes) for the rules on changing any of this.
