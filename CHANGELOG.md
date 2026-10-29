# Changelog

All notable changes to gglinux are recorded in this file, in the style of
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). gglinux has not been released or
tagged yet (the Cargo version is 0.0.1), so everything sits under Unreleased, grouped by the
milestones in [PLAN.md](PLAN.md). Dates are commit dates.

## Hardware status

Most of gglinux is built and tested against fakes, not against real devices. Target devices: Apex
Pro TKL Gen 3 keyboard (`1038:1642`), Arctis Nova Pro Wireless headset and base station
(`1038:12e0`), Rival 3 Wireless Gen 2 mouse (`1038:1872`, parked).

- **Verified live on hardware:** only two features. Base-station screen burn-in protection on the
  Arctis Nova Pro Wireless (M2), and keyboard lighting on the Apex Pro TKL Gen 3 (M4a): static
  per-key colours, the wave effect, and handing lighting back to the keyboard's onboard mode.
- **Read-only checks:** the device list, the headset's status and battery, the mouse's battery, and
  the daemon's read-only headset, Sonar and keyboard queries matched the real devices. That confirms
  the transports, not the features built on them.
- **Built, untested on hardware:** everything else, including all of Sonar, every headset setting
  write, the other lighting effects, reactive lighting, GameSense, profiles, clips, streamer mode,
  Smart Volume, start at login and the Arch package.
- **Blocked:** M4b (the Apex OLED) and M5 (actuation and Rapid Trigger) need a USB capture of
  SteelSeries GG on Windows.
- **Parked:** M1 (the Rival 3 Wireless Gen 2 mouse) is built, but no write was ever confirmed on the
  device and the mouse is no longer in use.

## [Unreleased]

### Device daemon and IPC (M0)

2026-07-27 to 2026-08-12.

- `gglinuxd`, a Rust daemon that lists SteelSeries devices and follows hotplug through a udev
  monitor, with a 10-second rescan as a safety net.
- A newline-delimited JSON protocol on a unix socket at `$XDG_RUNTIME_DIR/gglinux/gglinuxd.sock`,
  with the device list and live add and remove events. Wire types live in the `gg-ipc` crate.
- A `systemd --user` unit, `scripts/install-local.sh`, a udev `uaccess` rule so no root is needed
  (later narrowed, see Security), a small `ggctl` client, and `docs/PROTOCOLS.md`.
- A Tauri 2 and Svelte 5 desktop UI with a tray icon. Its Rust side owns the socket, relays daemon
  messages to the web view, and reconnects when the daemon restarts.
- On the NVIDIA proprietary driver the UI turns off WebKit's DMABUF renderer, which otherwise
  crashed it on Wayland (Gdk protocol error 71) or left the window empty on XWayland. A value you
  set yourself still wins.
- `fakemouse`, a stand-in daemon that speaks the real protocol with no devices behind it and logs
  each request decoded, so the UI can be exercised without hardware.
- Verified on hardware: all three devices enumerate. Not yet seen: a physical unplug and replug.

### Mouse (M1, parked)

2026-07-27 to 2026-08-12. Parked because the mouse is no longer in use; only the transport and the
battery were verified on hardware.

- `gg-device-mouse` for the Rival 3 Wireless Gen 2: DPI stages, polling rate, 6-button remap,
  saving to onboard memory, and battery, driven from `ggctl mouse` and a Mouse panel in the UI.
- DPI codes are generated from rivalcfg's table, and the sensitivity packet is always padded to five
  presets, so the short packet that can disable the vertical axis (rivalcfg #273) can't be built.
- Battery reads drain stale reports first, which fixed an intermittently wrong charging flag.
- The protocol has no getters, so the daemon and panel report what gglinux has written ("applied")
  instead of claiming to know what is on the device.

### Headset and base station (M2)

2026-08-12 to 2026-09-27.

- `gg-device-headset` for the base station: a decoded status block (headset and spare-pack battery,
  connection, auto-off, wireless mode, mic light), volume readback, and setters. Replies are matched
  by opcode up to a deadline, since the base station streams unrelated reports, and a request that
  draws no reply is retried. Polling keeps unrelated reports (volume wheel, ANC, battery) as events.
- **Base-station screen burn-in protection (verified on hardware).** This unit's OLED never sleeps,
  so gglinux keeps it black and lights it for a few seconds on real events (headset power, the spare
  pack, low-battery drops, control changes). A safety re-blank runs every ten minutes, and the
  daemon blanks the screen on exit. The Headset panel sets protection, lit time and brightness.
  Waking on the volume wheel, ANC and mic mute has not been watched on hardware yet.
- Headset controls: noise mode (off, transparency or ANC), transparency level, sidetone, mic volume
  (1-10), mic mute light, auto-off (now including never), 2.4 GHz wireless mode and the on-device
  EQ. A change is validated in full first, saved on the device, and shown on the base-station
  screen for one lit period. Untested on hardware: none of these writes has been tried from the UI.

### Sonar audio (M3)

2026-09-27. Untested on hardware: Sonar has never been turned on live; only its read-only state was
compared against a real PipeWire graph.

- **Channels (M3a):** Game, Chat, Media and Aux as PipeWire devices declared in a drop-in
  (`~/.config/pipewire/pipewire.conf.d/60-gglinux-sonar.conf`), so they outlive the daemon. Turning
  Sonar on writes the drop-in, restarts PipeWire and makes Game the default output; if the channels
  never appear, gglinux removes the drop-in and restarts PipeWire again.
- A Sonar tab with per-app routing by program name (including apps that play through PipeWire's
  ALSA plugin), channel volumes, and a ChatMix slider that balances Game against Chat.
- Levels are applied again whenever the channels reappear, such as at login or after a PipeWire
  restart gglinux didn't do. Everything goes through PipeWire's own tools (`pw-dump`,
  `pw-metadata`, `pw-cli`, `wpctl`), so there is nothing native to build.
- **EQ (M3b):** each channel is a filter chain of preamp, 12-band headphone correction and 10-band
  channel EQ, changed live and saved. AutoEQ `ParametricEQ.txt` import refuses a file that needs
  more filters than the chain has, naming the line, instead of approximating it.
- An EQ curve editor with channel and headphone tabs, drawn with PipeWire's own biquad formulas. A
  dragged point no longer jumps back for a moment on release.
- **Virtual mic (M3c):** "gglinux Mic", a chain of RNNoise noise suppression, a noise gate and a
  compressor, each switched live. It is the default input while Sonar is on, and the previous
  default returns when Sonar is off. It needs the optional `noise-suppression-for-voice` and
  `swh-plugins` packages; without them Sonar runs without the mic.

### Keyboard lighting (M4a) and reactive lighting

2026-09-27 to 2026-09-28.

- `gg-device-keyboard` for the Apex Pro TKL Gen 3 with the US ANSI TKL layout (87 keys). Only seven
  vetted opcodes can be sent (see Security).
- Static per-key colours and breathing, wave and spectrum effects at 30 frames a second, with a
  per-key colour editor in the Keyboard panel. Lighting is off by default, so a fresh install sends
  the keyboard nothing, and is handed back to onboard lighting when turned off or on daemon exit.
- Verified on hardware (2026-09-27): static per-key colours, the wave effect, and hand-back to
  onboard lighting. Breathing and spectrum are built but not verified.
- **Reactive lighting** (2026-09-28): Fade (the pressed key lights and fades) or Ripple (a ring
  spreads out from it), drawn over any effect. It needs the optional `61-gglinux-reactive.rules`
  (`install-local.sh --reactive`), and the UI warns what that access allows. Untested on hardware.
- Lighting time is kept in 64-bit floats, so fades and the wave no longer stutter after the daemon
  has run for days.
- The Apex OLED (M4b) is blocked: none of the known framings changed the screen on `0x1642`.

### GameSense (M6a)

2026-09-29. Untested on hardware; no installed game uses the GameSense SDK.

- A new `gg-gamesense` crate implementing the GameSense SDK (game metadata, event registration and
  binding, events, heartbeats, stopping and removing games), with colour handlers on the Apex's
  zones, including colour forms, modes and flashing.
- An HTTP server on `127.0.0.1`, on a port picked once and saved, closed to web pages (see Security).
- While GameSense is on, gglinux writes a `coreProps.json` into every Proton prefix so Windows games
  find the server, and removes those files when it is turned off. Native Steam only, not Flatpak.
- Games draw over the lighting as an overlay. With gglinux lighting off, a game takes the keyboard
  over and hands it back after going quiet (15 seconds by default).
- A Games page with the on/off switch and a built-in test game ("Run test").
- Screen and tactile handlers are accepted but never drawn (OLED blocked, no tactile hardware).

### Built-in effects (M6b)

2026-09-29. Untested on hardware.

- **Audio visualizer:** a lighting effect that turns the keys into a spectrum analyser of what is
  playing (18 bands, bars rising from the bottom row). It captures with `pw-record` and analyses
  live; nothing is recorded.
- **Mic-mute light:** a chosen key lights (red by default) while PipeWire's default microphone is
  muted, drawn over everything else.

### Profiles and per-game switching (M7a)

2026-09-29. Untested on hardware.

- Named lighting profiles (keyboard lighting only), saved and applied from the Settings page.
- Game rules map a program name to a profile, applied while the program runs; the previous lighting
  returns when it exits. A `/proc` scan every 2 seconds finds running programs, including Proton
  games by their `.exe` name.

### Clips (M7b)

2026-09-29 to 2026-10-02. Untested; `gpu-screen-recorder` was not installed on the test machine.

- An instant-replay buffer using `gpu-screen-recorder`, supervised by the daemon and off by default.
  Length (5 to 300 seconds, 30 by default), frame rate, quality and desktop audio are settings.
- Save the replay with a hotkey through the GlobalShortcuts desktop portal (Alt+S), from the UI, or
  with `ggctl clip save`. The supervisor never signals a recorder it has already reaped.
- A Moments page that lists clips, deletes them and opens their folder.

### Streamer mode (M7c)

2026-10-02. Built on Sonar, so also untested on hardware.

- A second, independent mix of Sonar's channels on a "gglinux Stream" device for OBS, with its own
  level per channel and a master level, switched from the Sonar tab. The mix you hear keeps its own
  volumes, ChatMix and EQ.
- Each stream feed's capture and playback targets are checked, and a missing stream mix never takes
  the channels down.

### Desktop integration (M7d)

2026-10-02. Untested on hardware.

- `ggctl` is now an installed command with `clip save`, `profile list`, `profile apply <name>`,
  `gamesense test` and `gamesense on|off`, for scripts and keyboard shortcuts.
- Start at login (Settings page) enables the daemon's user service and starts the UI hidden in the
  tray. It keeps working after gglinux is upgraded while sitting in the tray.
- Settings backup and restore: a JSON bundle of the lighting, GameSense, profile, clip and Sonar
  settings, saved to `~/Documents`. A restore writes the files and restarts the daemon.
- Opening gglinux again brings up the window of the copy already running.
- `install-local.sh` installs the UI, `ggctl`, a desktop entry and an icon (`--no-build` skips a
  `ggctl` or UI it never built), and an Arch `PKGBUILD` for `gglinux-git` is in `packaging/arch/`.

### Smart Volume and games-only clips (M7e)

2026-10-02. Untested on hardware.

- **Smart Volume** per Sonar channel: off, Quiet, Balanced or Loud, using swh's SC4 compressor at the
  end of each channel's chain and switched live, without restarting PipeWire. Needs `swh-plugins`.
- A channel's audio never depends on that optional package: the stage is written only when the
  plugin is installed and taken out when PipeWire can't load it or the package goes away. After such
  a restart, EQ, volumes, ChatMix and app routes are pushed again.
- **Games-only clips:** the replay buffer runs only while a program from the game rules is running,
  and the Moments page shows when it is waiting for a game.
- PLAN.md's parity checklist now marks every feature as verified, built, parked, blocked, deferred or
  skipped.

### Fixed

2026-10-02: a pass of 64 fixes after M7e, grouped here.

- **Security-relevant** (details under Security, with the earlier udev rule fix `a07eaa3`): the
  GameSense server caps connections and times out silent ones (`844d7d8`), stored handlers have a
  size limit (`07ea570`), and clip tests run their stand-in only from a private folder (`44e2786`).
- **Daemon and IPC:** devices are enumerated and opened through hidapi one thread at a time (two
  such calls at once could free hidapi's shared error string twice). A second `gglinuxd` refuses to
  start while another is listening instead of taking its socket. A slow request no longer holds up
  others on its connection, and a request the daemon can't read is refused on its own id, not id 0.
  A client that connects before the first device scan is sent the devices once found. When one
  device's interfaces change in the same rescan as another device comes or goes (a hub or KVM
  switch), the whole list is sent. A settings file that can't be read, or isn't UTF-8, is moved
  aside instead of saved over.
- **Sonar:** a PipeWire restart, however quick, is noticed and the levels applied again. Channel and
  mic volumes changed outside gglinux are saved. A new stream that reuses an old one's node id is
  routed. If Sonar's settings say off but its drop-in is still installed (after restoring a backup,
  say), the drop-in is removed. While the chosen output is away, Sonar plays into the default in
  use. A mic added at startup becomes the default input. A non-UTF-8 name in `pw-dump` output no
  longer hides the whole PipeWire graph. A failed streamer-mode switch still pushes the levels
  again, and Smart Volume waits for channels built with its stage.
- **Lighting, effects and GameSense:** the audio visualizer, and the desktop audio in clips, hear
  every Sonar channel, not just Game. Lighting comes back as soon as a game's colours stop. An
  unplugged keyboard is noticed while gglinux lighting is off. The Menu key lights correctly for
  presses and for the `win-menu` zone, and `r-win` (not a key on this board) lights nothing. A
  bitmap handler is kept as unsupported instead of failing a game's whole bind. A value newer than
  the current frame starts its flash lit, and the test game holds its low values long enough to
  flash. Starting with GameSense off removes any `coreProps.json` files it left in Proton prefixes.
- **Profiles:** a daemon restart in the middle of a game still puts your own lighting back when the
  game ends. Programs that rewrite their process title (Chromium, Electron, node) are matched by
  program, not options, and paths or Windows file names containing " -" are no longer cut short.
- **Clips:** clips retry when the folder or recorder can't start yet, and start on their own once
  `gpu-screen-recorder` is installed; the "retrying" message goes when no retry is coming. A hotkey
  that couldn't be bound stays reported, and a shortcut KDE bound with no key shows as no hotkey.
- **Mouse and `ggctl`:** a mouse setting never written is reported as unknown, and the applied DPI is
  the rounded value the mouse runs. `ggctl` and `mousectl` no longer read the number after
  `--select` as part of the DPI list. `ggctl` counts DPI stages from 0 as `--select` does, accepts
  `--select=N`, refuses stray words and unknown flags, and prints its commands with `--help`.
- **UI:** a second Sonar or lighting change made before the first one's reply no longer undoes it.
  Key colours follow the keyboard until a key is painted, so Apply keeps the rest. DPI and stay-lit
  entries show the value actually applied, and removing an earlier DPI stage keeps the selection. A
  refused game rule keeps the program name typed, and another page's error no longer stays on the
  Engine page. Moments offers "Open folder" only once a clip exists. A restore says the daemon
  restarted only when its service is running again. A dev build runs beside an installed copy. An
  empty device list suggests the cable or port, and the reactive-lighting hint copies the packaged
  rule instead of rerunning the installer.
- **Packaging and tools:** the Arch package depends on `xdg-utils` and suggests a desktop portal for
  clips. The PKGBUILD and `install-local.sh` build where they install from, whatever
  `CARGO_TARGET_DIR` says, and rerunning `install-local.sh` restarts a running daemon onto the new
  binary. The package and README say to remove an `install-local.sh` copy, which runs instead of the
  package's. `fakemouse` logs every request with its parameters and reports the keyboard's layout.
  The visualizer builds on Rust older than 1.88, and `gg-audio` tests remove their scratch folders.
- **Documentation:** the README's status matches PLAN.md, the minimum Rust versions are stated (1.87
  for the daemon, 1.88 for the UI), and the README explains what removing `swh-plugins` does.

### Security

- **Narrowed udev rule** (`a07eaa3`, 2026-09-28). The M0 rule opened every SteelSeries hidraw node to
  the desktop user, including the Apex's two typing interfaces, so any program running as that user
  could read every keystroke typed on it. The default rule now opens only the control interfaces.
  Typing-interface access is opt-in through `61-gglinux-reactive.rules`, `install-local.sh` clears
  the access the old rule left behind, and `scripts/check-udev.sh` shows what the rules open. Rerun
  `./scripts/install-local.sh --no-build --no-ui` if you installed before 2026-09-28.
- **Key presses stay in memory** (`2e00f9f`, `bc6eb9d`). Reactive lighting reads presses only while
  lighting and reactive lighting are both on, never logs them, and drops each once drawn.
- **Keyboard firmware safety** (`7958283`). Only seven vetted opcodes can be framed or written to
  the Apex, checked again at the write, so the firmware update commands on the same interface can
  never be sent.
- **GameSense closed to web pages** (`96b7d46`). The server listens on loopback only, refuses any
  request carrying an `Origin` header, requires `Content-Type: application/json` on every POST, and
  caps request bodies at 64 KiB. It never touches a `coreProps.json` it didn't write (`f7eae84`).
- **GameSense resource limits** (`844d7d8`, `07ea570`). At most 32 connections at once; a connection
  that sends no headers within 5 seconds is dropped, and a request is ended after 10 seconds.
  Handlers kept for all games together are capped at 2 MiB.
- **Clip tests** (`44e2786`). Tests run their stand-in recorder only from a new private folder with
  an unguessable name, never one another account could have created first.
- **Build dependencies** (2026-10-26). The UI lockfile takes patched `vite`, `devalue`,
  `nanoid` and `source-map-js`, clearing four high-severity `npm audit` advisories, all in build-time
  dependencies.

### Release preparation

2026-10-26.

- The Rust workspace is formatted with rustfmt (formatting only; `.git-blame-ignore-revs` lists that
  commit), and `cargo clippy -- -D warnings` passes on toolchains that don't know the
  `chunks_exact_to_as_chunks` lint.
- SPDX `GPL-3.0-or-later` headers on all 87 source files that lacked one (Rust, TypeScript, Svelte,
  CSS, scripts, udev rules, the systemd unit and the pacman install script).
- The crates' repository URLs, the systemd unit's documentation link and the PKGBUILD's source point
  at `github.com/YKKULTRA/gglinux`. The PKGBUILD runs `cargo fetch` and `npm ci` in `prepare()`, so
  `build()` and `check()` use `--frozen` and need no network.
- CI on GitHub Actions: rustfmt, clippy and tests for the daemon and the Tauri backend, svelte-check
  and the frontend build for the UI, and a debug Tauri build. It runs on pushes to `master` or
  `main`, on pull requests and on manual dispatch.
- The Arch package builds with `options=('!lto')`. Under a stock `makepkg.conf`, LTO made hidapi's
  bundled C code into GCC bitcode and `gglinuxd` failed to link (undefined `hid_read`,
  `hid_write`); found by building the package with makepkg in an Arch container.
- The Arch package lists every library its binaries link directly (glibc, libgcc, glib2,
  gdk-pixbuf2, cairo, libsoup3, dbus, hicolor-icon-theme), as namcap asked.
- `gg-audio`'s tests no longer fail with "permission denied" for every user except the first one to
  run them on a machine: each test's scratch directory now sits directly in the temp dir instead of
  under one shared parent.
- New public documents: a README written for newcomers (with an honest verified, untested, blocked
  and parked matrix), `CONTRIBUTING.md`, `SECURITY.md`, this changelog, `docs/ARCHITECTURE.md`,
  `packaging/arch/README.md` and `docs/PUBLISH-CHECKLIST.md`.
- A session scratchpad path, a local user name and references to private notes were taken out of
  the design documents under `docs/superpowers/`.
