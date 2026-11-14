# Changelog

All notable changes to gglinux are recorded in this file, in the style of
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). gglinux has not been released or
tagged yet (the Cargo version is 0.0.1), so everything sits under Unreleased, grouped by the
milestones in [PLAN.md](PLAN.md). Dates are commit dates.

## Hardware status

Most of gglinux is built and tested against fakes, not against real devices. Target devices: Apex
Pro TKL Gen 3 keyboard (`1038:1642`), Arctis Nova Pro Wireless headset and base station
(`1038:12e0`), Rival 3 Wireless Gen 2 mouse (`1038:1872`). Every feature of GG, row by row:
[docs/PARITY.md](docs/PARITY.md).

- **Verified live on hardware:** base-station screen burn-in protection on the Arctis Nova Pro
  Wireless (M2), and keyboard lighting on the Apex Pro TKL Gen 3 (M4a): static per-key colours,
  the wave effect, and handing lighting back to the keyboard's onboard mode. Since M11
  (2026-11-13): on the keyboard, a remapped key and an Fn-layer key (typed by hand), reading its
  bindings back, reading and backing up its onboard slots and writing the spare one, and the
  screen's System Monitor and animations; on the mouse, DPI stages with separate X and Y, polling
  rate, a key on a button, saving across a power cycle, the sleep timer, High-Efficiency Mode and
  Wireless Stability, all read back from the mouse.
- **Read-only checks:** the device list, the headset's status and battery, the mouse's battery, and
  the daemon's read-only headset, Sonar and keyboard queries matched the real devices. That confirms
  the transports, not the features built on them.
- **Built, untested on hardware:** everything else, including all of Sonar, every headset setting
  write and the base station's own settings, the other lighting effects, lighting zones and the
  idle scene, reactive lighting, keyboard macros, the mouse's wheel actions, GameSense, profiles,
  clips, streamer mode, Smart Volume, start at login and the Arch package.
- **On hold on the developer's keyboard:** the switch settings (actuation, Rapid Trigger, Rapid
  Tap, Protection Mode) and deep press. They went out without an error but were never checked by
  hand, and twice a key typed by itself not long after such changes; the cause isn't known.
- **Frozen as built:** Sonar and Moments stay and keep working, but nothing more is planned for
  them (the owner's choice, 2026-11-13).

## [Unreleased]

### Mouse and keyboard parity, profiles, GameSense screens, lighting zones (M11)

2026-11-06 to 2026-11-13. Built and tested against fake devices that answer late, as real ones do.
What was tried on the owner's devices on 2026-11-13 is said per item. Every new device write is
behind an experimental switch, off by default.

- **Experimental switches.** Settings → Experimental and `ggctl experimental` list eleven switches,
  one per feature that writes to a device and hasn't been tried on the owner's hardware (saved in
  `~/.config/gglinux/experimental.json`, all off). While one is off the daemon refuses its commands
  with "… hasn't been tried on your hardware yet", and the app greys its controls with that
  sentence. Seen working: the app's switch reaches the daemon and follows a change made elsewhere.
- **The Rival 3 Wireless Gen 2 is back** (un-parked 2026-11-06): the owner uses it again.
- **Mouse: its own settings read back.** Queries found in our USB captures of SteelSeries GG read
  the mouse's DPI stages, polling rate, sleep timer and wireless mode, so `ggctl mouse` and the app
  show what the mouse reports instead of what gglinux applied (with `mouse_power` on). Verified.
- **Mouse: sleep timer, High-Efficiency Mode and Wireless Stability** (`mouse_power`), as GG sends
  them. Verified: each read back from the mouse, and a 1-minute timer put it to sleep after about a
  minute.
- **Mouse: separate X and Y DPI, and any key on a button, in the app.** "Separate X and Y" above
  the DPI stages, and "Keyboard key…" / "Media key…" with a searchable list for each button.
  Verified: a button set to a key from the app typed it.
- **Mouse: an asleep mouse is told nothing.** Every write starts with a battery query; an asleep
  mouse gets "the mouse is asleep: move it, then try again" instead of a false "ok", and its last
  report is no longer shown as current. After a write the mouse's settings are read back, so one it
  dropped is reported as an error.
- **Mouse: scroll wheel actions** (`mouse_wheel`): turned up, the wheel can do anything a button can
  (buttons 1–6, DPI, scrolling, off, a key, a media key); turned down, scroll down or Back, the only
  two GG was seen sending there (captures `rival-5`, `rival-8` … `rival-10`). Scroll up / Scroll
  down rows under the mouse's buttons, and `ggctl mouse wheel`.
- **Actions on the spare keys F13–F24.** Bind a mouse button to one, then give the key an action in
  Settings → Actions or with `ggctl actions`: start a program, type text (with wtype, on Hyprland
  and other wlroots desktops), apply a profile, save the instant replay, or a media key. The daemon
  hears the key through the desktop's shortcuts portal and never reads an input device. KDE asks
  once per new key; Hyprland takes the binds from its config, and Settings shows the lines.
- **Keyboard: the real board.** The picture no longer shows Print Screen, Scroll Lock and Pause,
  which the Apex Pro TKL Gen 3 doesn't have; the button beside the screen is a key you can light
  and the new default mic-mute key. UK, German, French, Nordic, Turkish and Japanese boards get
  their own drawing, picked by the region the keyboard reports, asked only once gglinux takes the
  keyboard anyway. Verified on a US board; the other drawings are inferred.
- **Keyboard: key bindings** (`keyboard_bindings`): remap a key to a key or a combination of up to
  four, a media key, a mouse button, CPI or the wheel, or nothing, on the normal layer, with Fn held,
  or past a deep press; move the Fn key; read the keyboard's bindings back. Every write starts from
  a read, the keyboard's own Fn shortcuts are kept byte for byte, and gglinux sends the bindings
  again each time the keyboard connects. Each layer written is read back, written again up to three
  times, and put back as it was if the keyboard doesn't keep it. `ggctl bind` and a Key bindings
  panel. Verified: Caps Lock → `b` and Fn+F1 → `c` typed as set, and every layer read back.
- **Keyboard: deep press** as GG sends it (captures `apex-4`, `apex-5`, `apex-17`): the second points before the bindings,
  `00 00` for keys without a point, at most 8 deep-press bindings. On hold: GG's bytes typed `w` and
  `wxw` as they should through a probe, but through the daemon deep-layer writes were dropped at
  random, before the read-back above existed.
- **Keyboard: onboard profile slots** (`keyboard_onboard`): read all five, back each up to
  `~/.config/gglinux/onboard-backups/`, load one, write gglinux's screen picture and Rapid Tap into
  one, or put a backup back (`ggctl onboard`, and an Onboard panel). A write is one checked
  transaction: read and check the slot, save a backup and read it back, check the new image, erase,
  write, read everything back, and put the old image back on any failure. A damaged slot can be
  fixed with `ggctl onboard restore N <backup> --damaged`. Verified: all slots read twice
  identically, and two writes into the spare slot read back byte for byte. What the keyboard does
  after a load wasn't watched.
- **Keyboard: macros** (`keyboard_macros` and `keyboard_onboard`): a list of macros with steps as
  words (`a`, `ctrl+c`, `down:shift`, `wait:100`, `click:1`, `text:"hello"`), played once per press,
  a number of times, while held, or toggled on and off, with the wait between plays, written into a
  slot exactly as GG stores them (captures `apex-20` … `apex-22`, `apex-30`). Only the macro area
  and the bound keys change, after a backup; a slot holding macros gglinux can't read is left alone.
  At most two keys and 64 steps a macro for now. A write never loads the slot (run `ggctl onboard
  load N`), and the app writes into slot 5 unless another is picked. `ggctl macro` and a Macros
  panel. Untested.
- **Keyboard: Protection Mode** (`keyboard_protection`): release modes `03` and `04` in the switch
  tables, as GG sends them (captures `apex-2` … `apex-8`). On hold.
- **Keyboard: switch settings as GG sends them** (capture `apex-10`): only the table that changed,
  with GG's values for the keys the board doesn't have. All five go again after a connect or a slot
  load, when the settings are turned on, and with the first change five minutes after the last full
  send; `ggctl switches resend` sends them all now. Every report to the keyboard keeps 5 ms from the
  one before.
- **Keyboard screen: animations and a System Monitor.** The screen plays a GIF, APNG or WebP (up to
  256 pictures, each for its own delay), or shows up to four of CPU use, CPU temperature, memory,
  GPU use, GPU temperature and the time. The graphics card is only asked while it's already awake.
  `ggctl screen gif`, `ggctl screen monitor`. Verified by eye.
- **Lighting zones, an idle scene, and Reflect.** Name sets of keys (WASD, arrows, F-row, number row,
  every key, or keys you pick) and give each its own effect (one colour, painted keys, breathing, a
  wave or gradient across the zone, spectrum, audio, reactive); later zones paint over earlier ones,
  and saved lighting becomes the zone "all" unchanged. A scene of its own (or lights off) while
  you're away, some minutes after the desktop says so. Reflect (each key the colour of the screen
  above it) is built behind `lighting_reflect`, but the screen capture itself isn't yet, so it shows
  "stopped". `ggctl lighting …`. Untested.
- **Headset: the base station's own settings** (`headset_extras`): Bluetooth on at power-on, what a
  Bluetooth call does to the game sound, gain, line out (speakers, or a stream mix of main, aux and
  mic), the home screen and how soon its screen dims, as GG sends them (captures `arctis-1` …
  `arctis-3`, `arctis-5`). gglinux now shows what the base station reports for gain, line out,
  sidetone, mic volume, EQ, Bluetooth and the headset link, instead of "unknown until changed here".
  Commands to the base station keep 50 ms apart. The app's stream sliders send the other two levels
  as gglinux last set them. `ggctl headset` and new rows in the headset card. Untested.
- **GameSense: screen and bitmap handlers.** Games' screen handlers (text, progress bars, pictures,
  timed and repeated as the SDK describes) show on the keyboard's screen over your own picture,
  which comes back when the game's frames end or it goes quiet; full-keyboard `bitmap` and
  `partial-bitmap` handlers paint every key (which key takes which grid cell is gglinux's reading of
  the SDK doc). `ggctl gamesense` lists what each event draws, and `ggctl gamesense test screen` (or
  "Test the screen" in Games) plays a short test. Untested with a real game.
- **Profiles of any device settings.** A profile holds any of headset, mouse, keyboard switches, key
  bindings, keyboard screen and lighting, each ticked when saving, and applied through the same
  checks and switches as your own changes (a gated part is skipped with a note). A rule acts while
  its program runs or, as GG does, while its window has the focus (Hyprland, or KDE Plasma with
  gglinux's KWin script, which the Arch package now installs), and your own settings come back when
  you switch away. gglinux's own switch presets (Competitive FPS, MOBA, Typing, Default switches),
  import and export as JSON files. Older `profiles.json` files read as before and are kept as
  `profiles.json.v1`. Untested.
- **Sonar and Moments frozen** (the owner's choice, 2026-11-13): what is built stays and keeps
  working; no more Sonar or Moments features, and no Aim Trainer.
- **Documentation:** `docs/PARITY.md` gives every row of the GG feature audit its status;
  `docs/PROTOCOLS.md` has what our GG captures showed, each fact with its capture.

### Fixed in M11 (found on the owner's devices, 2026-11-13)

- **Mouse:** a setting sent with `--save` was lost since M1, because the save came straight after it
  and the mouse drops a command sent too soon after another. Commands now keep 35 ms apart across
  requests, and 3 s after a polling-rate or wireless-mode command, as GG waits.
- **Mouse:** a write to a sleeping mouse said "ok" and the old report stayed on screen (see the
  asleep item above).
- **Keyboard:** a bindings read right after another got the previous answer; it now waits 13 ms and
  asks again if the answer is for another layer.
- **Keyboard:** a deep-press binding written before its point was dropped by the keyboard; the points
  go first, and the bindings 20 ms after them.
- **Keyboard screen:** the GPU temperature showed `--` when `nvidia-smi` printed a fraction.

### Native desktop app (M10)

2026-10-29. Runs on the owner's desktop (Hyprland, NVIDIA's 615 driver with the open kernel
module); not yet tried on Plasma or under X11.

- The desktop app is native: Rust and [Slint](https://slint.dev) in `crates/gglinux-ui`, a member
  of the cargo workspace, in place of the Tauri 2 and Svelte 5 UI on WebKitGTK. The pages, their
  wording, the `gglinux` command and the desktop entry stay the same.
- About 55 MB of memory (49 to 59 MB, shown or hidden in the tray) instead of 240 MB, most of
  which was WebKit's web and network processes.
- No NVIDIA workaround: the old UI had to turn off WebKit's DMABUF renderer on NVIDIA's driver
  (see M0); the new one draws with OpenGL and needs nothing set.
- The tray icon (StatusNotifierItem, through `ksni`) shows the window or quits. Closing the window
  keeps gglinux in the tray; with no tray to hold it, closing quits instead. `gglinux --hidden`
  starts in the tray, as start at login does.
- One copy at a time over D-Bus: a second launch asks the running copy (`io.github.ykk.gglinux`,
  method `Activate`) to show its window and exits. A debug build answers to its own name, so it
  runs beside an installed copy.
- The keyboard's picture and a backup to restore are picked in the desktop portal's file dialog.
  Without a portal, a box to type the backup's path appears instead.
- New on the way: deleting a clip or a profile, and restoring a backup, ask first (restoring
  lists what the backup holds and when it was made); the Sonar EQ takes typed frequency, gain and
  Q for the selected point, and opens an AutoEQ file directly; Settings has an About line.
- Building needs only cargo: `install-local.sh`, the Arch package and CI build one workspace, with
  no Node, npm or WebKitGTK. The package depends on fontconfig, wayland, libxkbcommon and
  libglvnd instead of webkit2gtk-4.1, gtk3, libsoup3 and libayatana-appindicator; CI is one job.
- The app needs Rust 1.92 (Slint 1.18's minimum); the daemon and `ggctl` still build with 1.87.
- `cargo run -p gglinux --example shot` draws any page to a PNG with no display, and
  `scripts/screenshots.sh` redraws the README's screenshots that way, against the fake daemon.
- The old UI's web storage, `~/.local/share/io.github.ykk.gglinux`, is no longer used and can be
  deleted.

### Keyboard screen and switches (M4b, M5), screen care, parts, Hyprland

2026-10-29.

- The Apex's 128 × 40 screen shows text (up to five lines, as big as they fit), a clock (seconds,
  24-hour and date optional) or a picture (any image, fitted and dithered in the app, or a PBM through
  `ggctl screen image`). It's drawn while the daemon runs and handed back to the keyboard's own menus
  when set to *Keyboard's own* or when the daemon stops.
- Switches: the actuation point (0.1 to 4.0 mm) for every key and per key, Rapid Trigger on all or
  some keys with its distance (0.1 to 2.0 mm), and Rapid Tap (SOCD) pairs. The keyboard keeps them
  in RAM only, so the daemon sends them again whenever it connects; nothing is written to flash.
  Off by default.
- The keyboard's allowlist now checks sub-commands too: only the screen frame and release
  (`1F 81`, `1F 82`) and five switch tables (`38 61` primary layer only, `62`, `65`, `66`, `67`)
  are added; flash writes (`02`, `03`, `04`, `38 83`) stay impossible to frame.
- `ggctl keyboard`, `ggctl switches ...` and `ggctl screen ...`.
- The commands come from Apex Control's protocol reference (CC BY 4.0); see
  [docs/PROTOCOLS.md](docs/PROTOCOLS.md) §3b–3c. Not yet tried on this keyboard.
- **Clip hotkey fix:** current desktop portals (xdg-desktop-portal 1.20 and later) refuse a
  shortcut session from an app they can't name, and they name it only from a desktop entry called
  after its app id. The entry is now `io.github.ykk.gglinux.desktop` (was `gglinux.desktop`), and
  the hotkey's error says so when the portal refuses.
- **Hyprland:** the clip hotkey's message gives the `global` bind line for `hyprland.lua` and
  `hyprland.conf`; Settings explains that Hyprland doesn't run autostart entries and shows the
  line that starts the tray window; the README has a Hyprland section.
- **Screen burn-in care.** A picture of gglinux's moves a pixel every two minutes (a 5 × 5 walk
  around where it's drawn) and goes dark while nobody is at the computer: no active, unlocked
  session of the user on a seat (logind), the screensaver service says locked (hypridle and
  Plasma's locker answer `org.freedesktop.ScreenSaver.GetActive`; hypridle never tells logind), the
  computer going to sleep (a logind delay lock holds sleep until the screen is dark), or nobody
  logged in. Optionally the keyboard's own screen goes dark then too (off by default). Both on by
  default for gglinux's pictures; `ggctl screen care ...`.
- **Parts.** Every part (keyboard, headset, mouse, Sonar, GameSense, profiles, clips) can be
  turned off in Settings or with `ggctl parts <part> off`. One that's off isn't started: its
  device is left alone, its page is hidden, and its requests are refused with where to turn it
  back on. GameSense needs the keyboard. Applying a change restarts the daemon in place (same
  process, so systemd keeps tracking it). Turning Sonar off takes its channels out of PipeWire;
  turning GameSense off takes its files out of the Proton prefixes.
- Backups now include the switch, screen, screen-care and parts settings (they were left out).

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

### Mouse (M1, parked until M11)

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
- The Apex OLED (M4b) was blocked here: none of the known framings changed the screen on
  `0x1642` (built later, see the screen and switches section).

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
- **Gated keyboard allowlist** (M11). Key bindings, the onboard slots and Protection Mode's release
  modes are families that the open keyboard lets out only while their experimental switch is on.
  Flash commands pass only for the five profile slots, in 500-byte chunks, and gglinux sends
  them only from the checked slot transaction; the firmware's file checksum (`04`), other files and sub-commands, the
  region write (`75`), a screen picture to flash (`38 83`) and any bootloader entry stay impossible
  to frame. A random-report test runs 120 000 reports against the listed shapes.
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
