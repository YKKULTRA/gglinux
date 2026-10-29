# gglinux — Implementation Plan

An open-source, feature-complete **SteelSeries GG** replacement for Linux, targeting the
Apex Pro TKL Gen 3 keyboard, Rival 3 Wireless Gen 2 mouse, and Arctis Nova Pro Wireless headset.

---

## Context

SteelSeries GG (Engine + Sonar + Moments + GameSense) is **Windows/macOS only**. On Linux the three
devices work as generic HID/USB-audio, but every configurable feature — key actuation, RGB, OLED, DPI,
sidetone, ANC, the per-app audio mixer, ChatMix, EQ, game-event lighting — is inaccessible. Piecemeal
tools exist (rivalcfg, HeadsetControl, OpenRGB, apex-tux, a handful of ChatMix scripts) but nothing
unifies them, none touch the Apex Pro Gen 3 switch behavior, and there is no Sonar or GameSense
equivalent. `gglinux` fills that gap: **one Rust daemon + one desktop app** that reaches parity with GG
for these devices, published under GPL-3.0 so it can build directly on the GPL-licensed prior art.
The app was planned and built as a Tauri UI; since M10 (2026-10-29) it is a native Rust + Slint app.

**Decisions (locked):** Rust daemon `gglinuxd` + Tauri 2 UI over unix-socket JSON-RPC · GPL-3.0-or-later ·
gpu-screen-recorder as the clip engine · build order = **quick wins first** (mouse + headset → Sonar
audio → keyboard RGB/OLED → keyboard actuation RE → GameSense → polish).

**Changed 2026-10-29 (M10):** the UI is now a native Rust + Slint app (`crates/gglinux-ui`), not
Tauri 2 + Svelte. The Tauri UI held 240 MB in the tray, two thirds of it in WebKit's own processes;
the Slint one holds about 55 MB. The daemon, the socket protocol and everything else above stand.

**Environment (verified):** Arch Linux, kernel 7.1.4-arch1, KDE Plasma 6 / Wayland, PipeWire 1.6.8 +
WirePlumber 0.5.15, Intel UHD 770 + NVIDIA RTX 4090 (VAAPI + NVENC), Rust/Node/Python present. All three
devices bind `hid-generic` on every interface → **uncontended hidraw access today**; no kernel
`hid-steelseries` conflict on this kernel (earliest ~7.3), and hidraw stays open even after it lands.

Byte-level protocol detail for every device lives in **[docs/PROTOCOLS.md](docs/PROTOCOLS.md)**.

---

## Architecture

Cargo workspace; the daemon owns all hardware/audio, the desktop app is a thin client.

```
gglinux/
  Cargo.toml                     # [workspace]
  crates/
    gglinuxd/        # bin: tokio runtime, unix-socket JSON-RPC server, orchestrator, device registry
    gg-hid/          # shared hidapi plumbing: enumerate/open by (vid,pid,iface,usage_page), hotplug (udev)
    gg-ipc/          # serde JSON-RPC request/response types — shared with ggctl and the app
    gg-device-mouse/    # Rival 3 Wireless Gen 2   (port rivalcfg, WTFPL)
    gg-device-headset/  # Arctis Nova Pro Wireless (port HeadsetControl GPL-3 + loteran + Dymstro)
    gg-device-keyboard/ # Apex Pro TKL Gen 3 + OLED (port OpenRGB RGB + apex-tux OLED; actuation = fresh RE)
    gg-audio/        # Sonar-style PipeWire graph (pipewire-rs): virtual sinks, EQ, routing, ChatMix, mic
    gg-gamesense/    # axum HTTP server + coreProps.json writer (native + per-Proton-prefix)
    gg-clips/        # gpu-screen-recorder supervisor + GlobalShortcuts portal
    gglinux-ui/      # the desktop app since M10: Rust + Slint (ui/*.slint), a client of gg-ipc
  ui/                # until M10: Tauri 2 app, src-tauri/ (Rust IPC client) + frontend/ (web UI)
  data/udev/70-gglinux.rules
  data/systemd/gglinuxd.service  # systemd --user unit
  packaging/aur/PKGBUILD
```

This is the layout as planned in M0. What was built differs in places: clips live in `gglinuxd`
(no `gg-clips` crate), `gg-audio` drives PipeWire's command-line tools rather than pipewire-rs
(M3), the udev rule is `60-gglinux.rules`, the package is in `packaging/arch/`, and `ui/` was
deleted in M10. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) describes the tree as it is.

**Process model.** `gglinuxd` runs as a **systemd `--user` unit** (persistent, single instance, survives
UI close) — *not* a child of the app, which would die with it (the plan ruled out a Tauri sidecar
for that reason). The app keeps one `tokio::net::UnixStream` to the daemon on a thread of its own
and fills the Slint window's globals from it; until M10 the Tauri backend held that socket and
bridged JSON-RPC to the webview via Tauri commands/events.

**IPC.** unix-socket JSON-RPC for daemon↔UI (our protocol, versioned, no D-Bus coupling). `zbus` only
for the things that *are* D-Bus: in the daemon, `org.freedesktop.portal.GlobalShortcuts` (clip
hotkey) and asking logind and the screensaver whether anyone is there (screen care); in the app
(since M10), the tray (StatusNotifierItem, through `ksni`), one copy at a time (the name
`io.github.ykk.gglinux` and its `Activate` method) and the file-chooser portal. (The plan had the
tray in the daemon; the Tauri UI drew it itself.) `pipewire-rs` was planned for audio-graph
objects; M3 used PipeWire's CLI tools instead.

**HID.** `hidapi` crate with the **hidraw backend** (coexists with `hid-generic`, no driver detach; gives
`usage_page`/`interface_number` for selecting the right interface among the many each device exposes).
`udev` crate `MonitorBuilder` on `hidraw`+`usb` for hotplug. Blocking reads (base-station dial/battery
interrupts) run on `tokio::task::spawn_blocking`.

**System integration.** One udev rule `/usr/lib/udev/rules.d/70-gglinux.rules`:
`KERNEL=="hidraw*", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1038", TAG+="uaccess"` (logind grants the
active seat user; **no root, no polkit, no plugdev**). Must sort before `73-seat-late.rules`.

**Packaging.** **AUR PKGBUILD is the primary channel.** Flatpak is not viable as primary (a sandbox can't
install udev rules, and hidraw + PipeWire-node-creation + spawning gpu-screen-recorder fight the sandbox).

**The app on KDE/Wayland/NVIDIA.** The plan for Tauri was to ship env defaults
`WEBKIT_DISABLE_DMABUF_RENDERER=1` and `__NV_DISABLE_EXPLICIT_SYNC=1` (the #1 Tauri-Linux
blank-screen/Error-71 issue on the RTX 4090); the Tauri UI set the first one itself on NVIDIA's
driver, from M0 until M10. The Slint app (M10) draws with OpenGL through Slint's femtovg renderer,
with no browser engine, and needs neither: it ran on Hyprland with the NVIDIA 615 driver (open
kernel module) with nothing set. Tray via StatusNotifierItem: the plan expected it to work well on
Plasma 6. The Slint app's tray (`ksni`) was tried on Hyprland; Plasma is still to be tried.

---

## Reuse map (what we port, and its license)

| From | License | We take |
|---|---|---|
| **Ven0m0/steelseriesgg-rs** | MIT | Whole-daemon skeleton — our exact stack (hidapi+tokio+axum GameSense server, systemd+udev). Lift into GPL-3. |
| **not-jan/apex-tux** | Unlicense | Workspace shape; OLED framebuffer + feature-report framing (128×40, header `0x61`) |
| **OpenRGB** `SteelSeriesController/` | GPL-2.0-or-later | Apex Gen 3 RGB: opcodes `0x4B/40/41/F5/89/90`, 643-byte Direct packet, 112-key table (`0x1642` already supported) |
| **Sapd/HeadsetControl** `…nova_pro_wireless.hpp` | GPL-3.0 | Nova Pro byte builders: sidetone/battery/EQ/inactive/save |
| **loteran/Arctis-Sound-Manager** | GPL-3.0+ | Authoritative Nova Pro command+status spec (YAML); Sonar-EQ-as-filter-chain; OLED protocol |
| **Dymstro/nova-chatmix-linux** | 0BSD | Nova Pro ChatMix loop (iface 4, `07 45` dial), udev/systemd units — copy freely |
| **flozz/rivalcfg** `rival3_wireless_gen2.py` | WTFPL | Entire Rival 3 W Gen 2 command table (verified `0x1872` profile) |
| **sonusmix** `pipewire_api/mainloop.rs` | MPL-2.0 | pipewire-rs `create_object` patterns for null sinks + links |
| **werman/noise-suppression-for-voice** | GPL-3.0 | RNNoise LADSPA mic filter-chain config |
| **gpu-screen-recorder** | GPL-3.0 | Replay-buffer child (`-w portal`, `SIGUSR1` save) |

Reference-only (do **not** copy code): **OpenRGB SDK** design (GPL-2.0 network server pattern);
**Solaar** (daemon-less hidraw+uaccess validation); **EasyEffects** (APO→AutoEQ import logic).
Protocol *facts* from any source are always fair game.

---

## Milestones

Ordered for quick wins first. **M1–M2 and M3 are largely independent** (device HID vs PipeWire) and can
progress in parallel. Each milestone lists its acceptance test.

### M0 — Skeleton & plumbing  ✅ *done*
Cargo workspace; `gg-hid` (enumerate/open by vid/pid/iface/usage_page, udev hotplug); `gg-ipc` types;
`gglinuxd` JSON-RPC server + systemd user unit; udev rule; Tauri shell with tray + daemon connection;
device registry that detects the three PIDs and reports connect/disconnect.
**Accept:** `systemctl --user start gglinuxd`; UI lists the 3 devices live; unplug/replug reflects in UI.

Outcome — enumeration confirmed on hardware, and the interface map **matches `docs/PROTOCOLS.md`**,
so M1/M2/M4 can open the right node immediately:

| Device | control interface | usage page | node |
|---|---|---|---|
| Rival 3 W Gen2 `1038:1872` | iface 3 | `0xFFC0` | `/dev/hidraw3` |
| Arctis Nova Pro W `1038:12e0` | iface 4 | `0xFFC0` | `/dev/hidraw14` |
| Apex Pro TKL Gen3 `1038:1642` | iface 1 | `0xFFC0` | `/dev/hidraw5` |

Two findings worth carrying forward:
- The Apex exposes a **second** vendor page, `0xFFC1` on iface 4 (`/dev/hidraw8`). Its iface 3 is
  consumer-control + mouse, **not** vendor — which rules out the "OLED is chunked over iface 3"
  framing hypothesis in PROTOCOLS.md. For M4, probe OLED on iface 1 and `0xFFC1`/iface 4.
- None of the three report a USB serial, so logical ids are `vid:pid`. Two identical devices would
  currently collide; revisit if that ever matters.

Still unverified: physical unplug/replug (no one at the machine). `diff()` is unit-tested for
add/remove/swap/interface-churn instead, and the udev wake path is confirmed live (the monitor thread
parks in `poll()`, no busy-spin).

### M1 — Mouse: Rival 3 Wireless Gen 2  *(quick win)*  — **built; parked, hardware retired**

> **Parked 2026-07-27.** The Rival 3 is no longer in use — the owner moved to a Razer mouse — so the
> acceptance test below cannot be run and M1 stops here rather than counting as complete. The code is
> finished, tested and reachable from the UI and CLI; only the on-hardware confirmation is missing.
> Nothing else depends on it. Unparking needs the mouse back for one session.
>
> Razer support is a separate question, not a continuation of this: different vendor protocol, and
> [OpenRazer](https://openrazer.github.io/) (GPL-2.0) already covers that hardware on Linux — so the
> honest options are to integrate it or to leave mice to it entirely.
Port the rivalcfg `0x1872` table into `gg-device-mouse`: DPI stages (X/Y, the **fixed** `xxyy` encoder —
avoid the #273 vertical-axis bug), polling rate, 6-button remap, save. Battery poller (`0xAA 01`,
Solaar-style, publish % + charging over IPC). Mouse UI panel.
**Accept:** set DPI/polling/bindings from the UI and confirm on-device (e.g. DPI change measurable);
battery % shows and updates; settings persist across replug.

Done: `gg-device-mouse` (DPI/polling/buttons/save + battery, 12 unit tests over the packet builders,
DPI codes machine-generated from rivalcfg rather than guessed); the `gglinuxd` mouse worker thread;
the `get_mouse_state` / `set_mouse_*` / `save_mouse` IPC methods; the `ggctl mouse` client; and the
`MousePanel` UI (battery, DPI stages, polling rate, button remap, onboard save).

The panel separates *what will be written* (the form) from *what has been written* (`applied`), and
says so on screen, because there is no third thing — the state of the hardware — that either one can
stand in for.

Verified on hardware: transport (open iface 3, write, read input report) and battery, repeatedly and
stably. **Not yet verified: any write.** The protocol has no getters, so a write cannot be confirmed
in software — it needs a human watching the pointer speed change. Two hazards make that a deliberate
step rather than a casual one:
- Applying settings is **not restorable** — we cannot read the current values first, so whatever was
  configured before is gone.
- Bug #273 territory. `sensitivity_packet` pads to five presets using fixed-size arrays, so a short
  packet is structurally unrepresentable (test `short_preset_lists_still_pad_the_x_block`), but the
  failure mode it guards against — permanent loss of the vertical axis — is not one to test casually.

Also outstanding: settings do not yet persist across a daemon restart (no config file — that is M7),
so `applied` starts empty each run.

### M2 — Headset: Arctis Nova Pro Wireless  *(quick win)*
Port HeadsetControl + loteran/Dymstro into `gg-device-headset` (iface 4, `06 …` TX / `07 …` RX). Setters:
sidetone, ANC, transparency, mic volume, mic-mute-LED, auto-off timer, 2.4 GHz mode, on-device 10-band EQ
+ preset slots, save. Interrupt reader for battery/dial/ANC events. Backend abstraction that prefers
kernel `power_supply`/ALSA if a future `hid-steelseries` is present, else pure hidraw. Headset UI panel.
**Accept:** change sidetone/ANC/EQ from UI and see it reflected on the base-station OLED menu; battery
(headset + spare) reads; turning the physical dial emits `07 45` events the daemon logs.

Progress 2026-09-27: `gg-device-headset` reads status and volume (verified on hardware
2026-08-12) and a setter is confirmed by readback. The daemon now has a headset worker, IPC and a
UI panel, whose first feature is **base-station screen burn-in protection** — this unit's OLED
never sleeps on its own, so gglinux keeps it black and lights it briefly on events (spec:
`docs/superpowers/specs/2026-09-27-headset-screen-protection-design.md`). The UI now also sets noise mode, transparency, sidetone, mic volume and
mute light, auto-off, 2.4 GHz mode and the on-device EQ (spec:
`docs/superpowers/specs/2026-09-27-headset-controls-design.md`). The ChatMix dial moves to M3,
which drives it; the base station's damaged knob is out of scope by the owner's choice.

Hardware session 2026-09-27 (merged to master afterwards): **verified** — the screen blanks and
stays black through polling; headset power-on and battery (`07 b7`) events light it, then it
re-blanks; the 10-minute safety re-blank ran for about two hours. **Untested** — wake on the headset wheel, ANC and mic mute (not watched); every headset
setting write from the UI, including whether mic volume 1–10 sounds right by ear. Known nit: a
spare battery charging in the base station steps `07 b7` about every 20 minutes, and each step
briefly lights the screen.

**Deferred:** the kernel half of the headset backend (prefer `hid-steelseries`' sysfs/ALSA when
present). No kernel here drives `0x12e0` — the `hid-steelseries` modules of every installed 7.2
kernel alias only `12c2`, `12b6` and `1410` — hidraw keeps working after one lands, and code
against an unseen sysfs layout would be untested guesswork.

### M3 — Sonar audio core
`gg-audio` on pipewire-rs. Virtual sinks **Game / Chat / Media / Aux / Mic** (hybrid: static
`context.objects` for stable sinks + daemon-controlled gains/links/routing). Per-channel 10-band biquad
EQ (`bq_lowshelf`+8×`bq_peaking`+`bq_highshelf`, runtime-settable Freq/Q/Gain) + AutoEQ preset import via
`module-parametric-equalizer`. **ChatMix** engine (Game/Chat loopback to headset, complementary gains
driven by the M2 `07 45` dial). Per-app routing (watch `node-added`, set `target.object` by
`application.process.binary`, persist rules). Mic chain: RNNoise + noise-gate + compressor (ClearCast
parity). Mixer + EQ UI (curve editor).
**Accept:** apps route to channels from the UI and persist; dial fades Game vs Chat; EQ audibly changes
sound; mic denoise works in a call; xruns clean at quantum 512→256.

Progress 2026-09-27 — **M3a done in software**: `gg-audio` declares Game / Chat / Media / Aux as
PipeWire loopbacks in a drop-in (hosted by PipeWire, so they outlive the daemon), routes apps by
`application.process.binary` (or the `alsa_playback.<app>` name for plain ALSA clients), sets
channel volumes, and does ChatMix on the Game/Chat outlets; the Sonar tab drives it (spec:
`docs/superpowers/specs/2026-09-27-sonar-channels-design.md`). The approach moved from pipewire-rs to
PipeWire's own CLI tools (`pw-dump` / `pw-metadata` / `wpctl`) behind a trait — no native bindings,
and every action is a command a person could run. **M3b done in software**: each channel is a
filter chain (preamp → 12-band headphone correction → 10-band channel EQ), set live with
`pw-cli set-param`; AutoEQ `ParametricEQ.txt` import; a draggable curve editor that uses PipeWire's
own biquad formulas (spec: `docs/superpowers/specs/2026-09-27-sonar-eq-design.md`). **M3c done in
software**: a virtual gglinux Mic (RNNoise → noise gate → compressor, each live-switchable) that
becomes the default input while Sonar is on; its module is `nofail` and only written when its
plugins exist (spec: `docs/superpowers/specs/2026-09-27-sonar-mic-design.md`). Next: streamer mode.

Hardware status 2026-09-27: **nothing in M3 has run live yet.** The daemon's read-only Sonar state
matched the real graph, and the generated drop-ins parse with `spa-json-dump`, but Sonar has
never been enabled (it writes the drop-in and restarts PipeWire — ask the owner first). Still to
confirm live: routing, volumes, ChatMix, which node accepts the EQ `Props`, and the mic chain.

### M4 — Keyboard RGB + OLED: Apex Pro Gen 3
Port OpenRGB `0x1642` RGB (iface 1 feature reports; INIT `0x4B`, DIRECT `0x40`, ONBOARD `0x41`, 643-byte
packet + 112-key table) and apex-tux OLED (**add `0x1642`**, confirm single-642 vs 8×641 framing on-device
first). Per-key RGB editor + software effects (reactive/streamed) with onboard handoff; OLED content
(images/GIFs/text/system stats). Keyboard UI panel.
**Accept:** set individual key colors + run an effect from the UI; push a custom image and live stats to
the OLED; hand back to onboard lighting cleanly.

Progress 2026-09-27 — **M4a done in software**: `gg-device-keyboard` (allowlisted protocol: only
`0x23 0x40 0x41 0x4B 0x90 0xA3 0xF5` can be framed or written; US ANSI TKL layout), a keyboard
worker running static per-key colours and breathing / wave / spectrum at 30 fps, handing lighting
back to the keyboard when off or on exit; a Keyboard panel with a per-key editor (spec:
`docs/superpowers/specs/2026-09-27-keyboard-lighting-design.md`). **Accepted on hardware
2026-09-27**: static per-key, wave, and hand-back to onboard lighting all seen by the owner.

M4b OLED was **blocked**: none of apex-tux's framings (`0x61` numbered, unnumbered, after init;
`0x0C` chunked) changed the OLED on `0x1642`. **Built 2026-10-29** from Apex Control's published
protocol reference (the live frame is `1F 81`; see `docs/PROTOCOLS.md` §3b), together with M5's
actuation, Rapid Trigger and Rapid Tap (§3c). Neither has been tried on this keyboard yet.
**Reactive lighting done in software** (2026-09-28; spec:
`docs/superpowers/specs/2026-09-28-reactive-lighting-design.md`): Fade or Ripple over any effect,
from key presses read on the Apex's typing interfaces. The same change narrowed the udev rule,
which had opened every SteelSeries interface (so any program could read Apex keystrokes). Access
to the typing interfaces is now opt-in via `61-gglinux-reactive.rules`. Not yet verified on
hardware: the owner reinstalls the rules, confirms with `getfacl` that the hole is closed, and
tries both modes.

### M5 — Keyboard actuation: OmniPoint 3.0  *(fresh reverse-engineering — highest risk)*
No public RE exists. Sub-steps: (1) capture GG↔keyboard HID traffic (usbmon/Wireshark, Windows host or
VM) while toggling one setting at a time; (2) decode per-key actuation (0.1–4.0 mm / 40 levels), Rapid
Trigger, Rapid Tap/SOCD (≤5 pairs), Protection Mode, Dual Actuation, Dual Bindings, key remap + macros;
(3) implement in `gg-device-keyboard` on the `0xFFC0`/iface-1 vendor interface; (4) per-key actuation-map
UI. **Strictly honor the firmware/bootloader DENYLIST** (`0x02/03/04`, boot-entry) — see PROTOCOLS.md.
**Accept:** set a per-key actuation point + Rapid Trigger from the UI and verify the changed feel/behavior;
GG-set values survive our writes and vice-versa; zero firmware-range opcodes ever sent (assert in code).

### M6 — GameSense server + Proton bridge
`gg-gamesense`: axum server on `127.0.0.1:<port>`; write `coreProps.json` to the native path **and into
every Steam/Proton prefix** (`compatdata/<APPID>/pfx/drive_c/ProgramData/…`, parse `libraryfolders.vdf`,
inotify new prefixes). Implement all endpoints + the handler model (color/tactile/screen, zones); render
events onto keyboard RGB/OLED via M4. Built-in Engine Apps: clock, audio visualizer, Discord (mute/talk),
CS2 (health/ammo). Document "native Steam only (not Flatpak Steam)".
**Accept:** a GameSense title under Proton (e.g. CS2) drives the Apex OLED/RGB live; a heartbeat timeout
clears state after 15 s.

Progress 2026-09-29 — **M6a GameSense server done in software** (spec:
`docs/superpowers/specs/2026-09-29-gamesense-server-design.md`): new `gg-gamesense` crate (requests,
handlers, zones, painting, registry, Proton-prefix files), an axum server on loopback closed to web
pages, `coreProps.json` in every Proton prefix while enabled, games drawn over the lighting as an
overlay (taking the keyboard over when gglinux lighting is off, handing back after the game goes
quiet), and a Games page with a built-in test game. None of the owner's installed games speak the
SDK. Not yet verified on hardware. Next: M6b game-independent effects on the same overlay interface.

**M6b built-in effects done in software** (2026-09-29; spec
`docs/superpowers/specs/2026-09-29-builtin-effects-design.md`): an **Audio** visualizer effect (18
bands from `pw-record` of the default output, bars rising from the bottom row) and a **mic-mute
light** (a chosen key, red by default, while PipeWire's default microphone is muted). Not yet
verified on hardware.

### M7 — Suite features & polish
Profiles + per-game auto-switch (QuickSet-style `/proc` exec watcher swaps profiles on game launch/exit);
**PrismSync** cross-device RGB sync (Active/Reactive/Idle layers); **clips** via gpu-screen-recorder
(replay child, GlobalShortcuts save hotkey, clip library UI, per-game auto-clip); **Streamer Mode** (second
Personal/Stream sink graph); virtual surround (spatializer + SOFA, optional); tray/autostart; settings
backup (local export/import — no cloud); AUR PKGBUILD + install docs.
**Accept:** launching a game swaps profiles and restores on exit; Alt+S saves a replay clip; RGB syncs
across all three devices; `makepkg -si` installs a working daemon+UI from clean.

Progress 2026-09-29 — **M7a profiles done in software** (spec
`docs/superpowers/specs/2026-09-29-profiles-design.md`): named lighting profiles and game rules
(process name → profile) applied while the program runs, from a 2 s `/proc` scan that also
recognises Proton games' `.exe` names. Not yet verified on hardware.

**M7b clips done in software** (spec `docs/superpowers/specs/2026-09-29-clips-design.md`): a
supervised `gpu-screen-recorder` replay buffer (off by default), saved by the GlobalShortcuts
portal hotkey (Alt+S), the UI or `ggctl`, and a clip library on the Moments page. Needs
`sudo pacman -S gpu-screen-recorder` (not installed yet). Not yet verified.

**M7c streamer mode done in software** (spec `docs/superpowers/specs/2026-09-29-streamer-mode-design.md`):
a second, independent Stream mix of Sonar's channels on a "gglinux Stream" device (per-channel and
master levels) for OBS to record. Builds on Sonar, which is still unverified live.

**M7d done in software** (spec `docs/superpowers/specs/2026-09-29-desktop-integration-design.md`):
`ggctl` installed as a command (clip/profile/GameSense), start at login (user service + tray
autostart), settings backup/restore, `install-local.sh` installs the UI, and an Arch `PKGBUILD`
(`gglinux-git`, for when the repo is published — publishing is the owner's call). Skipped:
PrismSync (one RGB device) and virtual surround (optional).

**M7e done in software** (spec `docs/superpowers/specs/2026-10-02-smart-volume-game-clips-design.md`):
**Smart Volume** per Sonar channel (off, Quiet, Balanced or Loud): swh's SC4 compressor at the end
of each channel's chain, switched live with no restart. It needs `swh-plugins`; without it the
channels stay exactly as they were, and gglinux takes the compressor out again if the package
goes away. **Games-only clips** keep the replay buffer only while a program from the game rules
runs. Event-based highlights (kills and the like) stay out of scope. Not yet verified: Sonar
hasn't run live, and `gpu-screen-recorder` isn't installed.

### M8 and M9

Not planned up front, so recorded where they landed: **M8** (2026-10-02) was a bug hunt, 64
confirmed bugs fixed test-first (CHANGELOG, "Fixed"); **M9** (2026-10-29) built the Apex screen and
switches (see M4 and M5), screen burn-in care, turning parts off, and the Hyprland notes.

### M10 — Native desktop app: Rust + Slint  — **done 2026-10-29; live-tested on Hyprland**

Asked for by the owner on 2026-10-29, who took the UI for an Electron app and asked for a native
one, maybe in Rust. Measured first, hidden in the tray: the Tauri UI held **240 MB** PSS (its own
process 74 MB, WebKitWebProcess 127 MB, WebKitNetworkProcess 38 MB) next to the daemon's 9 MB.
Slint, GTK4 (relm4), iced and Qt were compared, and the owner chose **Slint**: its markup is close
to Svelte, so each panel ports directly; it draws headless, for screenshots; it used the least
memory; and its GPLv3 licence option fits gglinux's. Design record:
`docs/superpowers/specs/2026-10-29-native-app-design.md`.

Built: `crates/gglinux-ui` (package and binary `gglinux`), a member of the cargo workspace that uses
`gg-ipc`'s types directly. Every page of the old UI is ported (Engine with the mouse, headset and
keyboard panels, Sonar with the EQ editor, Games, Moments, Settings with parts and profiles).
Desktop parts: the tray through `ksni` (StatusNotifierItem); one copy at a time through the D-Bus
name `io.github.ykk.gglinux` (a second launch calls the first one's `Activate` and exits);
`--hidden` for starting at login; closing the window hides it to the tray, and **Quit** in the
tray menu ends it. The `shot` example draws any page to a PNG with Slint's software renderer
against the fake daemon; `scripts/screenshots.sh` makes the README's pictures with it. The `ui/`
tree (Svelte, Tauri, npm) is deleted: `install-local.sh`, the PKGBUILD and CI build one cargo
workspace with no Node.

Parity pass (2026-10-29): each page was checked against its Svelte panel line by line, and the
gaps were fixed. The largest fixes:
- repeated rows are now updated in place rather than rebuilt on every daemon event, which had cut
  off slider and EQ drags;
- list boxes now follow the daemon's state after a pick or a refused change;
- working copies are reset when the daemon goes away;
- the client keeps answers and notifications in the order they arrived;
- deleting a clip or a profile, and restoring a backup, now ask first;
- without a tray, closing the window quits the app, so it is never left unreachable.

The app has 96 unit tests, including the daemon client against a fake socket.

Measured on the owner's desktop (Hyprland, NVIDIA 615 with the open kernel module):
- **49–59 MB** PSS, shown or hidden in the tray;
- **no CPU time** over 30 s hidden in the tray;
- no NVIDIA workaround needed (the Tauri UI needed WebKit's DMABUF renderer off there).

Live-tested there:
- a `--hidden` start puts the icon in the tray with no window;
- a second `--hidden` launch exits at once, and a plain second launch shows the first copy's
  window;
- closing the window hides it, and the tray icon shows it again;
- restarting `gglinuxd` shows the offline card, then the panels come back by themselves;
- **Quit** exits with status 0 and leaves the daemon running.

**Accept:** every control, text, state and behaviour of the Svelte panels exists in the Slint ones
or is noted as not applying; the tray, start at login, the hand-over, the portal's file dialogs and
backups work on Plasma and Hyprland; `makepkg -si` installs it.

Not tried yet:
- Plasma and X11;
- the portal file dialogs (keyboard picture, restoring a backup);
- the colour picker and EQ dragging with a real mouse;
- `makepkg -si`.

---

## Feature parity checklist (vs SteelSeries GG)

Each row says where it stands: **verified** (seen working on the owner's hardware), **built** (in
software and tested against fakes, not yet verified on hardware), **parked** (built; the hardware
is gone), **blocked** (needs something we don't have yet), **deferred**, or **skipped** (the
owner's decision).

**Engine:**
- Apex per-key RGB, the wave effect, and hand-back to onboard lighting: **verified** (M4a).
  Breathing, spectrum and reactive lighting: **built**.
- Apex actuation (global and per key), Rapid Trigger and Rapid Tap: **built** (M5, RAM only,
  re-sent on connect). Protection mode, dual actuation, key remap, macros and saving to the
  keyboard's flash: **not built** (the commands are documented, but remapping can wipe the Fn
  layer's shortcuts and flash writes share opcodes with the firmware updater).
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
- Status, battery, volume readback, and base-station screen protection: **verified** (M2; waking
  the screen on the wheel, ANC and mic mute not yet watched).
- ANC, transparency, sidetone, mic volume and light, auto-off, 2.4 GHz mode, on-device EQ:
  **built** — not yet tried from the UI.
- Kernel backend (prefer `hid-steelseries` when present): **deferred** — see M2.

**OLED:** the Apex screen shows text, a clock or a picture: **built** (M4b). The base station's
screen is kept dark on purpose (burn-in protection).

**GameSense / Engine Apps:**
- Event server, colour handlers on the keyboard, `coreProps.json` in Proton prefixes: **built**
  (M6a). Screen and tactile handlers are accepted but not drawn yet (the screen works since M4b;
  drawing GameSense screen events on it is still to do; no tactile hardware).
- Built-in apps — the audio visualizer and the mic-mute light: **built** (M6b); a clock on the
  keyboard screen: **built** (M4b).
- CS2 Game State Integration and Discord mute: **skipped**.
- PrismSync: **skipped** — the keyboard is the only RGB device.

**Moments:** an instant-replay buffer with a hotkey and a clip library: **built** (M7b;
`gpu-screen-recorder` isn't installed yet). Games-only clips: **built** (M7e). Event-based
highlights: **skipped**.

**Elsewhere:** GG Home → the UI's device dashboard, listing the three devices live: **verified**
(M0; unplugging and replugging not yet seen). Start at login, tray, `ggctl` and an Arch
`PKGBUILD`: **built** (M7d; the tray, start at login and the one-copy guard were built again for
the native app in M10). The 3D Aim Trainer: **skipped**.

---

## Risks & mitigations

- **Bricking the Apex (M5).** Hard-coded DENYLIST of FIZZ/erase/bootloader opcodes with a code-level
  assertion; probe only known-reversible opcodes; capture-before-write; test on the user's single unit
  carefully. This is the project's one genuinely dangerous surface.
- **Actuation RE stalls (M5).** It gates only keyboard *switch* config, not the rest — M1–M4/M6 ship
  independently. Worst case, the keyboard still gets full RGB/OLED/remap without actuation tuning.
- **Kernel `hid-steelseries` lands mid-project.** Verified: hidraw stays open regardless. Preferring
  the kernel's sysfs/ALSA is deferred until a kernel supports `0x12e0` (see M2). No blocker.
- **PipeWire latency stacking (M3).** Keep the whole chain at 48 kHz, collapse hops (filter-chain node
  *is* the sink), `clock.force-quantum 256`; measure with `pw-top`.
- **Proton coreProps reachability (M6).** Confirmed reachable (shared host netns); the only failure mode
  is Flatpak Steam — documented, not engineered around.
- **Tauri/webkit2gtk on Wayland/NVIDIA.** Ship the env workarounds; fall back to X11 backend if needed.
  It happened: WebKit's DMABUF renderer crashed the UI on Wayland (Gdk protocol error 71) or left
  the window empty on XWayland, so the UI turned it off on NVIDIA's driver (M0). Gone with WebKit
  in M10.
- **The native app (M10).** Slint has no tray and no colour picker of its own, so gglinux has its
  own of both. The tray needs a StatusNotifierItem host; without one, closing the window quits
  instead of hiding it, so the app is never lost. Slint 1.18 measures wrapped text inside an `if`
  in a scrolling page at the wrong height; such pieces use the `Shown` widget instead.

---

## Verification strategy

- **Per device (M1/M2/M4):** cross-check our writes against the reference tools (install `rivalcfg` /
  `headsetcontrol` in a throwaway env and confirm identical device state), plus on-device observation
  (OLED menu, DPI feel, LED changes).
- **Audio (M3):** `pw-top`/`pw-link`/`wpctl` to inspect the graph; route a test app per channel; verify
  ChatMix fade and EQ with a tone/sweep; `pw-top` for xruns.
- **Actuation (M5):** capture-replay diffing; a key-tester page in the UI showing live actuation depth;
  assert no denylisted opcode is ever emitted (unit test over the command encoder).
- **GameSense (M6):** a local test client POSTing the SDK example events; then CS2 under Proton driving
  the OLED; verify per-prefix coreProps is written and picked up.
- **End-to-end (M7):** `makepkg -si` on a clean checkout; game-launch profile switch; hotkey clip save.
- **The app (M10):** every page drawn headless against the fake daemon (`shot`,
  `scripts/screenshots.sh`) and checked by eye; each panel compared with its Svelte original; then
  live on Plasma and Hyprland: the tray, start at login, the hand-over, file dialogs.

Reference clients to validate the GameSense server: `SteelSeries/unity-gamesense-client`, the PyPI
`gamesense` package. Prior art to diff audio behavior against: `loteran/Arctis-Sound-Manager`.
