# gglinux

An open-source **SteelSeries GG** replacement for Linux — device control (Engine), audio mixing
(Sonar), game-event lighting (GameSense), and clip capture (Moments) — for the hardware SteelSeries
never supported on Linux.

**Status:** early. **M0 (skeleton)** is done — the daemon enumerates devices, tracks hotplug, and
serves them over its socket. **M1 (mouse)** is built but **parked**: DPI stages, polling rate, button
remap, onboard save and a battery poller for the Rival 3 Wireless Gen 2, driveable from the desktop
UI or the `ggctl` CLI, but never confirmed against the physical mouse — that hardware is no longer in
use here. See [PLAN.md](PLAN.md) for the roadmap.

## Supported hardware (initial targets)

| Device | USB ID | Highlights |
|---|---|---|
| Apex Pro TKL Gen 3 (keyboard) | `1038:1642` | OmniPoint actuation, Rapid Trigger, per-key RGB, 128×40 OLED |
| Rival 3 Wireless Gen 2 (mouse) | `1038:1872` | DPI/polling/buttons, battery *(parked — see PLAN.md)* |
| Arctis Nova Pro Wireless (headset + base) | `1038:12e0` | ANC, sidetone, EQ, ChatMix, OLED, battery |

## Design

- **Stack:** a Rust daemon (`gglinuxd`) does all HID + PipeWire + GameSense work; a Tauri 2 desktop UI
  talks to it over a local unix-socket JSON-RPC. Runs as a `systemd --user` service; needs no root
  (a udev `uaccess` rule grants the seat user hidraw access).
- **Audio:** PipeWire virtual sinks (Game/Chat/Media/Aux/Mic) with per-channel parametric EQ, per-app
  routing, hardware-dial ChatMix, and an RNNoise mic chain — a Sonar equivalent.
- **GameSense:** a compatible localhost HTTP server that also works for **Steam/Proton** games via a
  per-prefix `coreProps.json` writer.
- **Clips:** wraps [gpu-screen-recorder](https://git.dec05eba.com/gpu-screen-recorder/) (NVENC/VAAPI).

See **[PLAN.md](PLAN.md)** for the full roadmap and **[docs/PROTOCOLS.md](docs/PROTOCOLS.md)** for the
byte-level device protocol reference.

## Building

The daemon needs only a Rust toolchain (≥1.85) and libudev:

```sh
cargo build --workspace --bins
cargo test --workspace
```

Install it, the `systemd --user` unit, and the udev rule that grants your desktop session access to
the SteelSeries HID interfaces gglinux controls:

```sh
./scripts/install-local.sh          # prompts for sudo once, for the udev rule
systemctl --user enable --now gglinuxd
```

Without that udev rule the hidraw nodes stay `root:root 0600`: devices will still be *listed* (that
only reads sysfs) but cannot be opened or controlled. The rule opens only control interfaces — never
a keyboard's typing interfaces. If you installed gglinux before 2026-09-28, run
`./scripts/install-local.sh --no-build` again: the old rule opened every SteelSeries interface,
including the Apex's typing ones, and the installer clears the access that rule left behind.
`./scripts/check-udev.sh` shows which interfaces the repo's rules open, without installing anything.

**Reactive lighting (optional).** Reactive lighting needs to see which keys you press. The optional
udev rule lets any program you run read what you type on the Apex while you're logged in. gglinux
reads only while reactive lighting is on, and keeps presses in memory only until their light fades.

```sh
./scripts/install-local.sh --no-build --reactive     # opt in
sudo rm /etc/udev/rules.d/61-gglinux-reactive.rules  # opt out, then replug the keyboard
sudo udevadm control --reload
```

**Sonar** (the audio channels, EQ and virtual mic) needs PipeWire ≥ 1.0 with WirePlumber 0.5; it
drives them with PipeWire's own `pw-dump`, `pw-metadata`, `pw-cli` and `wpctl`, so there is nothing
extra to build. Three packages are **optional**: two for the mic chain, and one for clips:

| Package | Provides | Without it |
|---|---|---|
| `noise-suppression-for-voice` | RNNoise noise suppression (`librnnoise_ladspa.so`) | no virtual mic |
| `swh-plugins` | the `sc4m` compressor | no virtual mic |
| `gpu-screen-recorder` | instant-replay clips | the Moments page asks you to install it |

**GameSense** (games and apps driving the keyboard lighting) is off until you turn it on in the
Games page. Turning it on starts a server on `127.0.0.1` and writes a small `coreProps.json`
(`{"address":"127.0.0.1:<port>"}`) into every Proton prefix under
`steamapps/compatdata/*/pfx/drive_c/ProgramData/SteelSeries/SteelSeries Engine 3/`, so Windows games
under Proton find it; turning it off removes those files (gglinux never touches a coreProps.json it
didn't write). Native Arch `steam` only — Flatpak Steam isn't supported. Few games speak GameSense;
**Run test** shows what it looks like.

**Audio visualizer and mic-mute light** use PipeWire's own `pw-record` and `wpctl`; nothing extra to
install. The visualizer analyses what's playing live and never records it.

```sh
sudo pacman -S noise-suppression-for-voice swh-plugins   # Arch; both are in [extra]
```

Everything else works without them, and the mic appears on the next start once they are installed.

Poke at it without the UI, or from scripts and keyboard shortcuts, with `ggctl`. Installed, it's
plain `ggctl …`; from a checkout:

```sh
cargo run -p gglinuxd --bin ggctl                       # device list + live hotplug events
cargo run -p gglinuxd --bin ggctl -- mouse              # battery + applied settings
cargo run -p gglinuxd --bin ggctl -- mouse dpi 800,1600 --select 0 --save
cargo run -p gglinuxd --bin ggctl -- mouse polling 1000 --save
cargo run -p gglinuxd --bin ggctl -- mouse buttons button1,button2,button3,button4,button5,dpi
cargo run -p gglinuxd --bin ggctl -- clip save          # save the instant replay (bind it to a key)
cargo run -p gglinuxd --bin ggctl -- profile list       # saved lighting profiles, one per line
cargo run -p gglinuxd --bin ggctl -- profile apply Late Night
cargo run -p gglinuxd --bin ggctl -- gamesense test     # also: gamesense on, gamesense off
```

Settings apply immediately but are lost when the mouse sleeps unless you pass `--save`, which commits
them to its onboard memory. There is **no way to read settings back** off this mouse — the vendor
protocol is write-only — so `ggctl mouse` reports what *gglinux* has applied this session rather than
pretending to know what is on the device.

### UI

The desktop UI is a Tauri 2 + Svelte 5 app in [`ui/`](ui/):

```sh
cd ui && npm install
npm run tauri dev
```

Use that, or `npm run tauri build` — **not** `cargo build && ./target/debug/gglinux`. A debug binary
loads the frontend from `devUrl` (`http://localhost:1420`), not from the embedded `dist`, so without
Vite running it comes up as a blank window: no error, no log line, and the tray and daemon socket all
behave normally. Only a release build embeds the frontend.

It needs `webkit2gtk-4.1` and an appindicator for the tray. On Arch:

```sh
sudo pacman -S webkit2gtk-4.1 libayatana-appindicator
```

On the **NVIDIA proprietary driver**, WebKit's DMABUF renderer cannot allocate its buffers: on a
native Wayland session that takes the process down with `Gdk-Message: Error 71 (Protocol error)`, and
on XWayland the window comes up empty. gglinux detects that driver and sets
`WEBKIT_DISABLE_DMABUF_RENDERER=1` for itself, so this needs no action — but if you set that variable
yourself, your value wins.

The UI is optional — the daemon runs headless and keeps applying your settings with the UI closed.

### Testing without hardware

`fakemouse` stands in for the daemon: same socket, same protocol, no devices. Stop `gglinuxd` first
(it refuses to start if anything else is listening), then:

```sh
cargo run -p gglinuxd --example fakemouse                 # battery drifts down from 87%
cargo run -p gglinuxd --example fakemouse -- --battery 12 # start low
cargo run -p gglinuxd --example fakemouse -- --charging
cargo run -p gglinuxd --example fakemouse -- --fail set_mouse_dpi
cargo run -p gglinuxd --example fakemouse -- --no-mouse
```

It logs every request it receives, decoded — so it shows what the UI actually sent, not just which
method was called. Two reasons to prefer it over the real daemon while working on the UI: applying a
setting to the real mouse cannot be undone (the protocol has no getters, so the previous values are
unrecoverable), and the battery paths are otherwise only reachable by waiting for the battery to
move. It deserializes with the same `gg-ipc` types the daemon uses, so a frame it accepts is a frame
the daemon accepts.

## License

GPL-3.0-or-later. Builds on the work of rivalcfg (WTFPL), HeadsetControl (GPL-3.0), OpenRGB (GPL-2.0+),
apex-tux (Unlicense), loteran/Arctis-Sound-Manager (GPL-3.0+), Dymstro/nova-chatmix-linux (0BSD), and
others — see the reuse map in PLAN.md.
