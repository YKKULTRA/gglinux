# Architecture

A short map of how gglinux fits together. The byte-level device protocols are in
[PROTOCOLS.md](PROTOCOLS.md); the design record for each feature is in
[superpowers/specs](superpowers/specs/) with its implementation plan in
[superpowers/plans](superpowers/plans/). Where each GG feature stands is in [PARITY.md](PARITY.md).

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
                                            │  profiles   ── /proc scan every 2 s, and   │
                                            │                the focused window          │
                                            │  actions    ── F13–F24 through the         │
                                            │                shortcuts portal            │
                                            │  experimental gates (experimental.json)    │
                                            └────────────────────────────────────────────┘
```

- **`gglinuxd`** owns everything that touches hardware, audio or the network. It runs as a
  `systemd --user` unit, so it outlives the UI and needs no root.
- **The app** (`crates/gglinux-ui`, binary `gglinux`) is a thin client written in Rust with
  Slint. A connection thread keeps one socket to the daemon, reconnects every second while it's
  away, and keeps the daemon's latest state for every part; after a change succeeds it reads that
  part's state again, because only the daemon knows what was applied. The window is Slint markup
  (`ui/*.slint`): each page's values live in a Slint global that the Rust side fills from the
  state, and each control's callback becomes a request. Each panel has its own pair of files
  (`mouse.rs`/`mouse.slint`, `switches.*`, `screen.*`, `bindings.*`, `onboard.*`, `macros.*`,
  `actions.*`, `experimental.*`, ...). It also owns a few desktop-only jobs: the tray icon
  (StatusNotifierItem, through `ksni`), one copy at a time (a second launch asks the first, over
  D-Bus as `io.github.ykk.gglinux`, to show its window), start-at-login (`~/.config/autostart`),
  file dialogs through the desktop portal, opening folders, and settings export/import.
- **`ggctl`** (`crates/gglinuxd/src/bin/ggctl/`, one file per command group) is a command-line
  client of the same socket. Commands that edit part of a state read it twice first, because a
  device (or the fake daemon) may still answer the first read after a change with the old state.

## The socket protocol

`crates/gg-ipc` defines every message, and both ends use the same types, so a frame one side
accepts is a frame the other side accepts.

- Socket: `$XDG_RUNTIME_DIR/gglinux/gglinuxd.sock`, in a directory systemd creates for the user.
- Framing: one JSON value per line. The client sends `RequestFrame { id, request }`; the daemon
  answers with a `Response` carrying the same `id`, and pushes unsolicited `Notification`s
  (device added or removed, battery, Sonar state, GameSense activity, clip saved, a gate turned
  on or off, ...).
- Requests on one connection are answered as they finish, so a slow one doesn't hold up the rest.
- A second daemon refuses to start while one is listening on the socket.
- New fields are added with serde defaults, so an older client's requests and an older settings
  file still read. For example, a lighting object may carry `zones` and `idle` (below); without
  `zones`, its `effect` covers every key as before.

## Crates

| Crate | Role |
|---|---|
| `gg-ipc` | Wire types for the socket, shared by the daemon, `ggctl` and the app, including the experimental gates (`experimental.rs`), profiles, lighting zones, macros and GameSense summaries. |
| `gg-hid` | Enumerates SteelSeries devices by vendor/product/interface/usage page, watches udev for hotplug, and diffs device lists. |
| `gg-device-keyboard` | Apex Pro TKL Gen 3: lighting frames, the screen's live frame, switch tables, key bindings, onboard profile slots and the macro codec, each report checked against a gated allowlist (`allow.rs`) and sent through one paced function. Board layouts per region (US, ISO, JIS). A transport seam with a lagging fake for tests. |
| `gg-device-headset` | Arctis Nova Pro Wireless: packet builders and reply decoding for status, settings, EQ, the base station's own settings (`base.rs`) and its screen. Commands keep 50 ms apart. |
| `gg-device-mouse` | Rival 3 Wireless Gen 2: DPI, polling, buttons, the scroll wheel, sleep timer, wireless mode, save and battery, with tables generated from rivalcfg, read-backs of the mouse's own settings, and a process-wide pace between commands. |
| `gg-audio` | Sonar: the PipeWire drop-in that declares the channels, EQ and mic chain as filter chains; reading the graph from `pw-dump`; routing rules; settings. All PipeWire access goes through a `PwTools` trait, so tests run against a fake. |
| `gg-gamesense` | GameSense: request and handler parsing, zones, painting events onto the key grid, screen handlers drawn for the 128 × 40 screen, bitmap handlers, the registry of games, and the `coreProps.json` files written into Proton prefixes. |
| `gglinuxd` | The daemon: socket server, device workers, Sonar controller, GameSense HTTP server, clips supervisor, profiles controller, actions service, experimental gates, and the `ggctl` binary. |
| `gglinux-ui` | The desktop app, `gglinux`: the daemon client, the Slint pages, the tray and the one-copy guard. Because it uses `gg-ipc`'s types directly, there's no second copy of the protocol to keep in step. |

The app replaced a Tauri 2 + Svelte UI on 2026-10-29: the same pages, without a browser engine,
at about a quarter of the memory (60 MB in the tray against 240 MB). Why, and how it's put
together: [the design record](superpowers/specs/2026-10-29-native-app-design.md).

## Experimental gates

Every device write that hasn't been tried on the owner's hardware sits behind a gate in
`~/.config/gglinux/experimental.json`, all off by default (`crates/gglinuxd/src/experimental.rs`).
While a gate is off the daemon refuses the request with "… hasn't been tried on your hardware yet:
turn it on in Settings → Experimental first", and the app shows the control greyed with that
sentence. A change is saved, put in a `watch` channel the workers follow, pushed to the keyboard
worker in the same step and sent to every client. On the keyboard the gate is enforced twice: the
worker refuses the request, and the open keyboard lets a gated command family (bindings, onboard
slots, Protection Mode's release modes) out of the allowlist only while its gate is on. The list
of gates is in the README.

## Device workers

Each device type has its own worker thread, started with the daemon. A worker opens its device's
control interface while the device is connected, takes commands from a channel, and publishes
state changes as events. HID I/O is blocking, so it stays off the async runtime. Devices answer
late, so the workers (and their fakes in tests) never take the first read after a change as
proof that it took.

- **Keyboard** (`gglinuxd/src/keyboard.rs` and one module per area under `keyboard/`):
  - *Lighting* renders at 30 fps (on change for static scenes). Since M11 it is a stack of zones
    (`zones.rs`): each zone is a set of keys (every key, a preset such as WASD, or picked keys)
    with its own effect (one colour, painted keys, breathing, wave, gradient, spectrum, the audio
    visualizer, reactive presses, Reflect), and later zones paint over earlier ones. A saved
    lighting with no zones reads as one zone `all` with its old effect. GameSense overlays and the
    mic-mute light draw on top. The lighting is handed back to the keyboard's onboard profile when
    turned off or when the daemon exits.
  - *Idle scene* (`idle.rs`): a second stack (empty means every key dark) shown some minutes after
    the desktop says nobody is there. It reuses screen care's presence signal (`presence.rs`:
    logind's Active, LockedHint and IdleHint, the screensaver's `GetActive`, and sleep), sent to
    the worker as `Command::SetAway`. No input device is read.
  - *Reflect* (`reflect.rs`): a zone effect that takes each key's colour from the part of the
    screen above it, at most 15 frames a second (4 × 4 points a key, each key moving halfway to its
    new colour per frame). Frames come from a `ScreenSampler` trait. The real sampler (the
    ScreenCast portal with a PipeWire video stream) isn't built: `start_portal()` says so and opens
    nothing, so a Reflect zone shows "stopped" until it is; tests use a fake screen. Behind the
    `lighting_reflect` gate.
  - *Screen* (`screen.rs`, `screen/`): text, clock, picture, animations (`anim.rs`) and the System
    Monitor (`sysmon.rs`, `gpu.rs`: the graphics card is only asked while it's already awake), all
    through the screen's live frame, with screen care (a pixel's shift every two minutes, dark
    while away). A GameSense game's screen frames come in through the overlay (`Overlay::screen`)
    and show over the user's own screen setting while they're due, at most one picture every
    100 ms (`OVERLAY_GAP`); the user's screen comes back when they end.
  - *Switches* (`switches.rs`): the five switch reports go only when they differ from what this
    connection last sent, or all five after a connect, a slot load, turning them on, or the first
    change five minutes after the last full send. The deep-press points table follows, only with
    `keyboard_bindings` on.
  - *Bindings* (`bindings.rs`): read, merged onto the keyboard's own and written back on connect,
    each layer read back after it's written. *Onboard* (`onboard.rs`): slot reads, backups, loads
    and the checked write transaction. *Macros* (`macros.rs`): the macro library, written into a
    slot through that transaction (never loaded by it).
  - The board drawn follows the keyboard's region, asked once a connection, the first time gglinux
    takes the keyboard anyway, so a fresh install still sends nothing.
- **Headset**: polls status and audio settings every second, listens for the base station's
  unsolicited events, applies settings, and runs screen protection (keeps the OLED dark, lights it
  briefly on real events, re-blanks). It follows the gates on the event bus (`headset/gates.rs`);
  the base station's own settings go only with `headset_extras` on, and its screen settings are
  asked for only then (`headset/ux.rs`).
- **Mouse**: applies settings and polls the battery. Every write starts with a battery query made
  for it, so an asleep mouse is sent nothing and the client is told to move it. With `mouse_power`
  on, the mouse's own DPI stages, polling rate, sleep timer and wireless mode are read back after
  a write and on each poll; the buttons and the wheel have no read, so for those the daemon
  reports what it applied. Commands keep a process-wide pace (35 ms; 3 s after a polling-rate or
  radio-mode command), because the mouse drops a command sent too close to the one before.

## Sonar (audio)

Sonar is built from PipeWire's own pieces, driven through its command-line tools rather than
native bindings, so every action is something a person could run by hand. It is frozen as built
since 2026-11-13 (the owner's choice): it keeps working, and nothing is added to it.

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
ever removes files it wrote itself. Game events are painted onto the keyboard as an overlay, and
screen handlers draw on its screen over the user's own screen picture: the registry works out
which picture is due (`Registry::screen`), the overlay hands it to the keyboard worker's screen,
and the user's screen comes back when the frames end or the game goes quiet, stops or is removed.
Bitmap handlers paint every key through the same overlay. `ggctl gamesense` lists each game's
events and what each one draws on.

## Profiles

Profiles (`gglinuxd/src/profiles.rs` and `profiles/`) are named sets of device settings: any of
six sections, headset, mouse, keyboard switches, key bindings, keyboard screen and lighting, each
ticked when saving. There is no Sonar section.

- **Applying** sends each section in the order headset, mouse, switches, bindings, screen,
  lighting, through the daemon's own request handler (`profiles/daemon.rs` gives the controller
  the workers as a `Daemon` port that calls `dispatch`). So a section is refused exactly when the
  user's own request would be, and a gated part is skipped with a note instead of sent. Settings
  the device saves itself (the headset's, the mouse's sleep timer) go only where they differ.
- **Rules** match a program, or a window class, and act while the program runs or while its window
  has the focus. The first rule that holds wins. Your own settings for the sections a rule's
  profile replaces are kept first (the Default, saved in `profiles.json`) and put back when no
  rule holds.
- **Focus sources** (`profiles/focus.rs`) start only while a rule acts on focus. Hyprland: its
  event socket (`.socket2.sock`, `activewindowv2`), with `j/clients` for the window's class and
  process. KDE Plasma: the daemon serves `io.github.ykk.gglinux.Focus` at
  `/io/github/ykk/gglinux/Focus` (method `WindowActivated(s class, i pid)`) on the session bus,
  and gglinux's KWin script (`crates/gglinuxd/src/profiles/kwin/gglinux-focus`, installed by the
  Arch package) calls it whenever a window is activated. A window counts once it has held the
  focus for half a second. Where no source reports the focus, a focus rule acts while its program
  runs. The `/proc` scan every 2 s finds running programs, including Proton games by their `.exe`
  name.
- **Presets** are gglinux's own switch presets (Competitive FPS, MOBA, Typing, Default switches),
  shipped as profile files in `profiles/presets/`; they are gglinux's choices, not SteelSeries'.
  Profiles export to and import from JSON files (`{"format":"gglinux-profile","version":2,…}`).
- The daemon hands the profiles controller its workers at start (`profiles::connect`), so rules act
  before any client has asked.

## Clips and actions

- **Clips** supervise a `gpu-screen-recorder` replay buffer, restarting it when it dies. Saving
  comes from the GlobalShortcuts portal hotkey, the UI or `ggctl clip save`. "Games only" keeps
  the buffer running only while a game from the profile rules is running. Like Sonar, clips are
  frozen as built since 2026-11-13.
- **Actions** (`gglinuxd/src/actions.rs`): the spare keys F13–F24 (a mouse button can send one)
  run a program, type text, apply a profile, save a clip or press a media key. The daemon binds
  the keys through the desktop's GlobalShortcuts portal and hears them from it; it never reads an
  input device. KDE asks once to confirm each new key; Hyprland takes the binds from its config.

## Settings and files

Settings are JSON files in `~/.config/gglinux/` (`lighting.json`, `switches.json`,
`screen.json`, `screen-care.json`, `bindings.json`, `macros.json`, `profiles.json`,
`actions.json`, `experimental.json`, `parts.json`, `gamesense.json`, `clips.json`,
`sonar.json`, ...). An unreadable file is moved aside as `<name>.unreadable-<seconds>` rather
than overwritten. The first time a version-1 `profiles.json` is read, a copy is kept as
`profiles.json.v1`. Onboard slot backups go to `~/.config/gglinux/onboard-backups/`
(folder 700, files 600). Files gglinux writes outside that directory:

| Path | When |
|---|---|
| `~/.config/pipewire/pipewire.conf.d/60-gglinux-sonar.conf` | while Sonar is on |
| `.../compatdata/*/pfx/drive_c/ProgramData/SteelSeries/SteelSeries Engine 3/coreProps.json` | while GameSense is on |
| `~/.config/autostart/gglinux.desktop` | when Start at login is on |
| your videos folder, `gglinux/` by default | saved clips |

## Safety rules for hardware code

- Only the exact USB IDs and interfaces listed in the udev rule are opened.
- The keyboard can only be sent reports on its allowlist; gated families pass only while their
  gate is on. The firmware-update commands share that interface and can brick it, so they are
  impossible to frame.
- No device's onboard settings are saved to flash unless the user asks for it. On the keyboard,
  flash is written only through one checked transaction: read and check the slot, save a backup
  and read it back, check the new image, erase, write, read everything back, and put the old
  image back on any failure.
- Every new device write starts behind an experimental gate, off by default.
- Packet builders validate their ranges and are unit-tested byte for byte, against our own GG
  captures where we have them.

See [CONTRIBUTING.md](../CONTRIBUTING.md#hardware-writes) for the rules on changing any of this.
