# gglinux

An open-source **SteelSeries GG** replacement for Linux — device control (Engine), audio mixing
(Sonar), game-event lighting (GameSense), and clip capture (Moments) — for the hardware SteelSeries
never supported on Linux.

**Status:** early. **M0 (skeleton)** is done — the daemon enumerates all three devices, tracks hotplug,
and serves them over its socket. **M1 (mouse)** is built: DPI stages, polling rate, button remap,
onboard save, and a battery poller for the Rival 3 Wireless Gen 2, driveable from either the desktop
UI or the `ggctl` CLI. Its acceptance test — confirming a write on the physical mouse — is still
outstanding. See [PLAN.md](PLAN.md) for the roadmap.

## Supported hardware (initial targets)

| Device | USB ID | Highlights |
|---|---|---|
| Apex Pro TKL Gen 3 (keyboard) | `1038:1642` | OmniPoint actuation, Rapid Trigger, per-key RGB, 128×40 OLED |
| Rival 3 Wireless Gen 2 (mouse) | `1038:1872` | DPI/polling/buttons, battery |
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
SteelSeries HID nodes:

```sh
./scripts/install-local.sh          # prompts for sudo once, for the udev rule
systemctl --user enable --now gglinuxd
```

Without that udev rule the hidraw nodes stay `root:root 0600`: devices will still be *listed* (that
only reads sysfs) but cannot be opened or controlled.

Poke at it without the UI, via the `ggctl` client:

```sh
cargo run -p gglinuxd --example ggctl                    # device list + live hotplug events
cargo run -p gglinuxd --example ggctl -- mouse           # battery + applied settings
cargo run -p gglinuxd --example ggctl -- mouse dpi 800,1600 --select 0 --save
cargo run -p gglinuxd --example ggctl -- mouse polling 1000 --save
cargo run -p gglinuxd --example ggctl -- mouse buttons button1,button2,button3,button4,button5,dpi
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

## License

GPL-3.0-or-later. Builds on the work of rivalcfg (WTFPL), HeadsetControl (GPL-3.0), OpenRGB (GPL-2.0+),
apex-tux (Unlicense), loteran/Arctis-Sound-Manager (GPL-3.0+), Dymstro/nova-chatmix-linux (0BSD), and
others — see the reuse map in PLAN.md.
