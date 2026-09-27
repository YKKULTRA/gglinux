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
equivalent. `gglinux` fills that gap: **one Rust daemon + one Tauri UI** that reaches parity with GG
for these devices, published under GPL-3.0 so it can build directly on the GPL-licensed prior art.

**Decisions (locked):** Rust daemon `gglinuxd` + Tauri 2 UI over unix-socket JSON-RPC · GPL-3.0-or-later ·
gpu-screen-recorder as the clip engine · build order = **quick wins first** (mouse + headset → Sonar
audio → keyboard RGB/OLED → keyboard actuation RE → GameSense → polish).

**Environment (verified):** Arch Linux, kernel 7.1.4-arch1, KDE Plasma 6 / Wayland, PipeWire 1.6.8 +
WirePlumber 0.5.15, Intel UHD 770 + NVIDIA RTX 4090 (VAAPI + NVENC), Rust/Node/Python present. All three
devices bind `hid-generic` on every interface → **uncontended hidraw access today**; no kernel
`hid-steelseries` conflict on this kernel (earliest ~7.3), and hidraw stays open even after it lands.

Byte-level protocol detail for every device lives in **[docs/PROTOCOLS.md](docs/PROTOCOLS.md)**.

---

## Architecture

Cargo workspace; the daemon owns all hardware/audio, the Tauri app is a thin client.

```
gglinux/
  Cargo.toml                     # [workspace]
  crates/
    gglinuxd/        # bin: tokio runtime, unix-socket JSON-RPC server, orchestrator, device registry
    gg-hid/          # shared hidapi plumbing: enumerate/open by (vid,pid,iface,usage_page), hotplug (udev)
    gg-ipc/          # serde JSON-RPC request/response types — shared with the Tauri backend
    gg-device-mouse/    # Rival 3 Wireless Gen 2   (port rivalcfg, WTFPL)
    gg-device-headset/  # Arctis Nova Pro Wireless (port HeadsetControl GPL-3 + loteran + Dymstro)
    gg-device-keyboard/ # Apex Pro TKL Gen 3 + OLED (port OpenRGB RGB + apex-tux OLED; actuation = fresh RE)
    gg-audio/        # Sonar-style PipeWire graph (pipewire-rs): virtual sinks, EQ, routing, ChatMix, mic
    gg-gamesense/    # axum HTTP server + coreProps.json writer (native + per-Proton-prefix)
    gg-clips/        # gpu-screen-recorder supervisor + GlobalShortcuts portal
  ui/                # Tauri 2 app: src-tauri/ (Rust IPC client) + frontend/ (web UI); depends on gg-ipc
  data/udev/70-gglinux.rules
  data/systemd/gglinuxd.service  # systemd --user unit
  packaging/aur/PKGBUILD
```

**Process model.** `gglinuxd` runs as a **systemd `--user` unit** (persistent, single instance, survives
UI close) — *not* a Tauri sidecar (those die with the app). The Tauri backend opens a
`tokio::net::UnixStream` to the daemon and bridges JSON-RPC to the webview via Tauri commands/events.

**IPC.** unix-socket JSON-RPC for daemon↔UI (our protocol, versioned, no D-Bus coupling). `zbus` inside
the daemon only for the things that *are* D-Bus: `org.freedesktop.portal.GlobalShortcuts` (clip hotkey)
and StatusNotifierItem (tray). `pipewire-rs` for audio-graph objects.

**HID.** `hidapi` crate with the **hidraw backend** (coexists with `hid-generic`, no driver detach; gives
`usage_page`/`interface_number` for selecting the right interface among the many each device exposes).
`udev` crate `MonitorBuilder` on `hidraw`+`usb` for hotplug. Blocking reads (base-station dial/battery
interrupts) run on `tokio::task::spawn_blocking`.

**System integration.** One udev rule `/usr/lib/udev/rules.d/70-gglinux.rules`:
`KERNEL=="hidraw*", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1038", TAG+="uaccess"` (logind grants the
active seat user; **no root, no polkit, no plugdev**). Must sort before `73-seat-late.rules`.

**Packaging.** **AUR PKGBUILD is the primary channel.** Flatpak is not viable as primary (a sandbox can't
install udev rules, and hidraw + PipeWire-node-creation + spawning gpu-screen-recorder fight the sandbox).

**Tauri on KDE/Wayland/NVIDIA.** Ship env defaults `WEBKIT_DISABLE_DMABUF_RENDERER=1` and
`__NV_DISABLE_EXPLICIT_SYNC=1` (the #1 Tauri-Linux blank-screen/Error-71 issue on the RTX 4090). Tray via
StatusNotifierItem works well on Plasma 6.

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
`docs/superpowers/specs/2026-09-27-headset-controls-design.md`). Still to do for M2: owner
acceptance on hardware; the ChatMix dial moves to M3, which drives it; the base station's
damaged knob is out of scope by the owner's choice.

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
`docs/superpowers/specs/2026-09-27-keyboard-lighting-design.md`). Next: M4b OLED (framing to confirm
on-device), then reactive lighting (needs an input-access decision).

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

### M7 — Suite features & polish
Profiles + per-game auto-switch (QuickSet-style `/proc` exec watcher swaps profiles on game launch/exit);
**PrismSync** cross-device RGB sync (Active/Reactive/Idle layers); **clips** via gpu-screen-recorder
(replay child, GlobalShortcuts save hotkey, clip library UI, per-game auto-clip); **Streamer Mode** (second
Personal/Stream sink graph); virtual surround (spatializer + SOFA, optional); tray/autostart; settings
backup (local export/import — no cloud); AUR PKGBUILD + install docs.
**Accept:** launching a game swaps profiles and restores on exit; Alt+S saves a replay clip; RGB syncs
across all three devices; `makepkg -si` installs a working daemon+UI from clean.

---

## Feature parity checklist (vs SteelSeries GG)

**Engine:** device config ✓(M1–M5) · macros ✓(M1/M5) · per-game auto-switch ✓(M7) · onboard profiles
✓(M1/M4) · firmware update ✗(out of scope — bricking risk) · cloud sync → local backup only (GG
discontinued cloud anyway).
**Sonar:** Game/Chat/Media/Aux/Mic sinks ✓(M3) · 10-band parametric EQ ✓(M3) · per-app mixer ✓(M3) ·
ChatMix ✓(M3) · ClearCast denoise → RNNoise ✓(M3) · Smart Volume ✓(M3, compressor/limiter) · virtual
surround ✓(M7) · Streamer Mode ✓(M7) · preset library ✓(AutoEQ import; community sharing later).
**Devices:** Apex actuation/RapidTrigger/RapidTap/Protection/Dual ✓(M5) · per-key RGB ✓(M4) · OLED
✓(M4) · Rival DPI/polling/buttons ✓(M1) · Nova ANC/sidetone/EQ/mic/battery ✓(M2).
**GameSense/Engine Apps:** event server ✓(M6) · OLED/RGB/tactile handlers ✓(M6) · built-in apps ✓(M6) ·
**works under Proton** ✓(M6). **PrismSync** ✓(M7). **Moments** → gpu-screen-recorder ✓(M7).
**3D Aim Trainer** ✗ (out of scope). **GG Home dashboard** → the UI's device dashboard ✓.

---

## Risks & mitigations

- **Bricking the Apex (M5).** Hard-coded DENYLIST of FIZZ/erase/bootloader opcodes with a code-level
  assertion; probe only known-reversible opcodes; capture-before-write; test on the user's single unit
  carefully. This is the project's one genuinely dangerous surface.
- **Actuation RE stalls (M5).** It gates only keyboard *switch* config, not the rest — M1–M4/M6 ship
  independently. Worst case, the keyboard still gets full RGB/OLED/remap without actuation tuning.
- **Kernel `hid-steelseries` lands mid-project.** Verified: hidraw stays open regardless; build the
  headset backend to detect + prefer kernel sysfs/ALSA when present. No blocker.
- **PipeWire latency stacking (M3).** Keep the whole chain at 48 kHz, collapse hops (filter-chain node
  *is* the sink), `clock.force-quantum 256`; measure with `pw-top`.
- **Proton coreProps reachability (M6).** Confirmed reachable (shared host netns); the only failure mode
  is Flatpak Steam — documented, not engineered around.
- **Tauri/webkit2gtk on Wayland/NVIDIA.** Ship the env workarounds; fall back to X11 backend if needed.

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

Reference clients to validate the GameSense server: `SteelSeries/unity-gamesense-client`, the PyPI
`gamesense` package. Prior art to diff audio behavior against: `loteran/Arctis-Sound-Manager`.
