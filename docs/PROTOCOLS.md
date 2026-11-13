# gglinux — Device Protocol Reference

Byte-level reference for controlling the three target devices over USB HID, compiled from
open-source prior art. **Protocol facts are not copyrightable**; per-source licenses are noted
where we port *code*. All three devices are USB VID `0x1038` (SteelSeries).

> Legend: `hidraw` = `/dev/hidrawN`; "feature report" = `SET_FEATURE` (report id byte first);
> "output report" = interrupt/`write()` (report id byte first). "iface N" = USB `bInterfaceNumber`.

> **⚠ hidraw framing — byte 0 is always a report id, never protocol data.** A `write()` to hidraw
> must begin with the report id. The kernel (`hidraw_send_report`) treats `0x00` as "this device
> doesn't use numbered reports" and **strips** that byte before putting the rest on the wire; any
> non-zero byte 0 is kept *and* sent. So the framing differs per device, and getting it wrong fails
> silently (the write succeeds, the device just ignores it):
>
> | Device | Descriptor | Buffer to write | Wire payload |
> |---|---|---|---|
> | Mouse `1038:1872`, keyboard `1038:1642` | **unnumbered** | `[0x00] + payload`, pad to **65** | 64 B, starts at the opcode |
> | Headset `1038:12e0` | **numbered** (`0x06`/`0x07`) | `[0x06] + payload`, pad to **64** | 63 B after the id |
>
> For the headset the leading `0x06` really *is* a report id, so it is both the buffer's byte 0 and
> what the descriptor declares — do not additionally repeat it in the payload.

---

## 1. Rival 3 Wireless Gen 2 — mouse — `1038:1872`

**Status: fully solved, and verified on real hardware (2026-07-27).** Port
`rivalcfg/devices/rival3_wireless_gen2.py` (WTFPL). Transport: **HID output reports to iface 3**
(the 4th of 4 dongle HID interfaces, `/dev/hidraw3` here, usage page `0xFFC0`). rivalcfg selects
purely by `interface_number == 3`, no usage-page filter. Only works over the 2.4 GHz dongle, not
Bluetooth.

**Framing (measured, supersedes an earlier note in this file):** the descriptor is **unnumbered**
(Input 64 B, Output 64 B, Feature 514 B). Write `[0x00] + payload` padded to **65 bytes**; the kernel
strips byte 0 and the device receives a 64-byte report beginning with the opcode. Responses arrive as
**input reports** — plain `read()` on the same fd, 64 bytes, first payload byte at index 0.

> An earlier draft of this file claimed output writes are prefixed with `0x02` (and features `0x03`).
> That was wrong: `0x02`/`0x03` are rivalcfg's *internal* `HID_REPORT_TYPE_OUTPUT`/`_FEATURE` enum
> values naming which report type to use, not bytes that go on the wire. Sending `02 aa 01` gets no
> reply; `aa 01` gets one. Feature reports are not used for reads at all here — `GET_FEATURE`
> returns 0 bytes.

| Function | Command bytes | Notes |
|---|---|---|
| Sensitivity / DPI | `0x2C` | `multidpi_range_choice_xy`, `xy_mapping="xxyy"`, `dpi_length_byte=1`, range `[100,18000,100]`, ≤5 presets, default `800:800,1600:1600`. TrueMove Air 18K. |
| Polling rate | `0x17` | `{1000:0x00, 500:0x01, 250:0x02, 125:0x03}` |
| Buttons (6) | `0x19` | `button_field_length=5`, offsets `0x00/05/0A/0F/14/19`; codes: disable `0x00`, keyboard `0x51`, multimedia `0x61`, dpi-cycle `0x30`, scroll-up `0x31`, scroll-down `0x32` |
| Battery read | `0xAA 0x01` | response: `level=data[0]` (%), `charging=bool(data[2])`. **Verified:** reply `10 00 00 …` = 16%, not charging. |
| Save / commit | `0x09` | no args |

- **⚠ Bug to replicate the *fix* for (rivalcfg #273):** setting fewer than 5 DPI presets with the
  naïve `xxyy` encoder corrupts the report and **kills the vertical axis** (unrecoverable). Port the
  fixed `handlers/multidpi_range_choice_xy.py` (pad to `max_preset_count`).
- **No RGB** on the wireless Gen 2 (only the *wired* `1038:1870` has 3-zone RGB). Do not add LEDs.
- Kernel `hid-steelseries` does **not** export this mouse's battery to upower → poll `0xAA 0x01`
  ourselves (Solaar-style). Treat timeout/`0` as "asleep/unknown".
- Original 2020 model `1038:1830` deltas: sensitivity `0x20` with `dpi_length_byte=2`; everything else same.
- **Not covered by any public RE (fresh-RE candidates):** macros, Bluetooth-side config. The sleep
  timer, High-Efficiency Mode and Wireless Stability are now captured (below).

### 1a. Captured from GG 123 (2026-11-08)

Our own USB captures of GG 123.0.0 in a Windows VM with the mouse passed through (pcaps, action logs
and the full write-up: `RIVAL-PROTOCOL.md` in the M11 capture folder). Same transport as above; answers
to queries are 64-byte input reports, first byte at index 0.

| Function | Bytes | Notes |
|---|---|---|
| Sleep timer | `2B 00 00 00 00 <seconds u16 LE>` | GG sends it only when saving, then `09`; GG offers 1–20 min (seen: `58 02` = 10 min, `B0 04` = 20 min). **Built (gated `mouse_power`), untested here.** |
| Read sleep timer | `AB` → `00 00 00 00 <seconds u16 LE>` | 300, 600, 1200 read back as set. **Built (gated `mouse_power`).** |
| High-Efficiency / Wireless Stability | `2E 00` (High-Efficiency on), `2E 02 00` (normal), `2E 02 01` (normal + stability) | sent live; read back with `AE` → `<mode> <stability>`. **Built (gated `mouse_power`).** |
| Wheel bindings | `1A 31 00 00 00 00 32 …` | wheel up/down, same codes as `19`; only the default was captured, so not built |
| Read DPI stages | `AC` → `<count> <current> <X×5> <Y×5>` | the layout of `2C`. **Built (gated `mouse_power`).** |
| Read polling rate | `97` → `<code>` | the codes of `17`. **Built (gated `mouse_power`).** |

**Pacing (found on hardware, 2026-11-13):** a command sent straight after another makes the mouse drop
the first. A setting followed at once by the save `09` was lost every time (DPI, polling rate and the sleep
timer alike; the mouse kept reporting its old value), 5 ms apart it was kept. GG leaves 30–40 ms between
its commands; gglinux now keeps 35 ms (`COMMAND_PACE`). After a polling-rate command (`17`, even with the
rate unchanged) or a radio-mode command (`2E`) the dongle answers nothing, or late enough to mix answers
up, for about a second, and a command sent 0.5–0.6 s after one was lost (1.5 s: kept). GG waits 3.0 s after
`17` in both captures and 1.4–2.7 s after `2E`; gglinux waits 3 s after either (`LINK_PACE`). A save needs
no extra time: a timer sent 40 ms after `09` was kept.
`2B` on its own takes effect at once, without `09`: a 60 s timer sent to a mouse that
had lain still for minutes put it to sleep on the spot, after which it answers no query until it's moved.

A mouse that isn't answering reads as all zeros, which would decode as 1000 Hz (`97` → `00`) and
High-Efficiency Mode (`AE` → `00 00`). gglinux believes those two only next to an `AC` answer with a
stage count of 1 to 5, which zeros can't be.
| Unknown queries | `D0`, `90` → `01 05`; `E8` → `04`/`01` | GG polls `E8` every ~2 s |

Acceleration, deceleration and angle snapping send nothing to the mouse (only `09` on save): GG
applies them on the PC.

Reuse: `flozz/rivalcfg` `devices/rival3_wireless_gen2.py`, `usbhid.py`, `mouse.py`, `handlers/multidpi_range_choice_xy.py`, `devices/dpi.py` (all WTFPL).

---

## 2. Arctis Nova Pro Wireless — headset + base station (GameDAC Gen 2) — `1038:12e0`

**Status: best-documented SteelSeries device on Linux.** One vendor HID protocol on **iface 4**
(OUT `0x04` / IN `0x84`). TX prefix byte0 = `0x06`. Output reports zero-padded (HeadsetControl uses
31 bytes, Dymstro 63, loteran 64 — all work). Sibling PIDs: `0x12e5` (Nova Pro Wireless X), `0x225d`.

> ### ⚠ Verified on hardware 2026-08-12 — two corrections to the reference implementations
>
> **1. Command replies come back on report `0x06`, not `0x07`.** The sources describe the status
> readback as `07 b0 ..`; the real device answers `06 b0 ..`, echoing the opcode. Report `0x07` is a
> *separate, unsolicited* stream. **Decode by opcode, not by report id or arrival order.**
>
> **2. `07 25 <n>` is the volume wheel, sent only while it moves.** On 2026-08-12 it arrived every
> ~16 ms with `n` wandering 2–8 and was logged as a possible level meter; on 2026-09-27 it was
> silent for minutes and burst only while the headset wheel turned. `n` is attenuation on the
> 0–56 scale (0 = loudest), the same value `06 20` reports at byte 3.
>
> Together these break any request/response loop that reads the *next* report after a request: the
> next report is almost never the reply. Worse, it fails intermittently rather than outright — a
> fixed retry budget succeeds or fails depending on what the stream is doing. Read to a **deadline**,
> and match the opcode. (`Headset::status` does both; 10/10 reads after, versus intermittent before.)
>
> **Report descriptor** (iface 4, one hidraw node, two usage collections):
> `85 06` on usage page `0xFFC0` — In/Out 63 B + report id = **64 B writes**;
> `85 07` on usage page `0xFF00`. Reports are genuinely **numbered**, so byte 0 of a hidraw write is
> `0x06` and is transmitted (unlike the Rival 3, where byte 0 is `0x00` and the kernel strips it).
>
> Sample decode, headset docked: `06 b0 00 00 01 00 08 08 08 00 02 0a 05 00 08 08`
> → battery 8/8 = 100%, spare 8/8 = 100%, mic LED 10, auto-off `05` = 30 min, mode `00` = speed,
> power `08` = online.

### Setters (TX `0x06 ..`)
| Function | Bytes | Notes |
|---|---|---|
| Save to flash | `06 09` | HeadsetControl appends after each setting (`sendCommandWithSave`) |
| Battery/status request | `06 b0` | see status map below |
| Sidetone | `06 39 <lvl>` | `0=off,1=low,2=med,3=high` |
| EQ preset select | `06 2e <preset>` | presets `0–3` stored on base; `4` = custom slot |
| EQ 10-band custom | `06 33 <b0..b9>` | select custom first (`06 2e 04`); each byte = `0x14 + 2*dB`, range −10..+10 dB, 0.5 dB step |
| Noise mode | `06 bd <0\|1\|2>` | 0 off, 1 transparency, 2 ANC (loteran; status byte 10, event `07 bd`) |
| Transparency level | `06 b9 <1-10>` | status byte 8, event `07 b9` |
| Mic volume/gain | `06 37 <1-10>` | 1 = muted, 10 = 100% (loteran's shipped slider; the `0xa0` figure elsewhere is contradicted by it — confirm by ear). Not in the status readback |
| Analog gain (hi-Z) | `06 27 <1=low\|2=high>` | hardware gain toggle, **not** a sample-rate change |
| Mic-mute LED brightness | `06 bf <1-10>` | 1 is the minimum (HeadsetControl "off" = 1, "on" = 10); status byte 11 |
| Auto-shutoff timer | `06 c1 <0-6>` | `0`=never, `1=1m,2=5m,3=10m,4=15m,5=30m,6=60m`; status byte 12 |
| 2.4 GHz mode | `06 c3 <0=speed\|1=range>` | |
| ChatMix enable | `06 49 01` | disable `06 49 00`; enables the dial interrupt (below) |
| Sonar icon on OLED | `06 8d 01` | |
| Volume set | `06 25 <atten 0-56>` | 0=max, 56=mute |
| Line-out | `06 43 <v>` / per-stream `06 47 <mainL> <mainR> <aux>` | |
| OLED frame | `06 93 <x> 00 <w> <h> <pixels>` | **1024-byte feature report** per strip, strips ≤ 64 px wide (x = 0, 64), column-major LSB-top, 128×64 panel. One all-black frame blanks the panel and holds 3+ min; re-sending every 2 s flashes the stock screen through. **Verified 2026-09-27** |
| OLED brightness | `06 85 <1-10>` | 64-byte output report. **No off level**: `0` behaves like `1` (ggoled rejects it). Verified 2026-09-27 |
| OLED return-to-UI | `06 95` | stock screen back immediately. A headset-wheel volume change also returns to it, permanently. Verified 2026-09-27 |
| Audio settings | `06 20` | reply byte 2 = audio input, **byte 3 = volume** (attenuation 0–56). Verified 2026-09-27 |

### Status readback — `06 b0` reply byte map (RX `07 b0 ..`)
`2`=bt_powerup, `3`=bt_auto_mute, `4`=bt_power, `5`=bt_connection, **`6`=headset_battery (0–8 → %)**,
**`7`=spare/charge-slot battery** (hot-swap), `8`=transparency_level, `9`=mic_status,
`10`=noise_cancelling, `11`=mic_led_brightness, `12`=auto_off_minutes, `13`=wireless_mode,
`14`=wireless_pairing, **`15`=headset_power_status (`0x01` offline / `0x02` cable-charging / `0x08` online)**.

### Interrupt-IN events (RX `0x07 ..`, read continuously from iface 4)
- **ChatMix dial:** `07 45 <game 0x00-0x64> <chat 0x00-0x64>` — both `0x64` at center detent.
  **Host must do the fade** (no hardware mixing this generation → drives the PipeWire ChatMix engine).
- **Battery: `07 b7 <headset> <spare>`**, both in eighths (observed 2026-09-27): `07 b7 08 00` when a full pack
  went into the headset with the slot empty; `07 b7 00 05`→`06`→`07`→`08` about every 20 min while a pack
  charged in the slot with the headset off. `07 b5 01` accompanied headset power changes (meaning unconfirmed).
- Master volume: `07 25 <atten>` (only while the wheel moves) · EQ band readback: `07 31 <band> <val>` (dB=`(val-20)/2`) ·
  ANC `07 bd <nc>` · transparency `07 b9 <lvl>` · line-out `07 43`/`07 47`.

### Init handshake (loteran `device_init`, before applying settings)
`06 20` / `06 10` / `06 80` / `06 3b` / `06 b2` / `06 b3` / `06 b7` / `06 83 01` / `06 69 00`.

### Notes
- **USB audio:** Linux sees ONE analog-stereo sink + ONE mono source. Windows Sonar's Game/Chat/Media
  and 8ch/96 kHz "spatial" outputs are **software virtual devices**, not extra USB endpoints — there is
  **no ALSA UCM / alternate-setting / quirk** to unlock more channels. The whole gap is userspace.
- **Kernel:** `hid-steelseries` support for `0x12e0` (Sriman Achanta series, v3 2026-02-27) is **NOT
  merged** as of 2026-07-20 (earliest ~7.3). On 7.1.4-arch1 the device binds `hid-generic` on all
  interfaces → **uncontended hidraw ownership, no unbind needed.** Even after it lands, hidraw stays
  fully open (verified in `hid-core.c`: `hidraw_report_event()` runs first, unconditionally). The
  kernel driver will own battery (`power_supply`) + sidetone/ChatMix/mic-mute (ALSA mixer) but **never
  EQ or OLED** → build a runtime-detected backend (prefer kernel sysfs/ALSA when present, else hidraw).
- **Not covered (fresh-RE candidates):** mic noise-gate opcode, firmware update, BT control opcodes
  (pairing/auto-mute), dual-USB source switching.

Reuse: `Sapd/HeadsetControl` `lib/devices/steelseries_arctis_nova_pro_wireless.hpp` +
`protocols/steelseries_protocol.hpp` (GPL-3.0); `loteran/Arctis-Sound-Manager`
`devices/nova_pro_wireless.yaml` + `oled_protocol.py` (GPL-3.0-or-later, authoritative spec);
`Dymstro/nova-chatmix-linux` `nova-chatmix.py` + udev/systemd units (0BSD, copy freely).

---

## 3. Apex Pro TKL Gen 3 — keyboard — `1038:1642`

Three sub-systems, three very different maturity levels.

### 3a. RGB — **solved** (OpenRGB master, 2026-01-05, author filipesn)
Transport: **feature reports on iface 1** (leading report-id `0x00`). Uses the `APEX_GEN3` quirk.

| Op | Byte | Purpose |
|---|---|---|
| INIT | `0x4B` | once, 65-byte report |
| DIRECT (wired) | `0x40` | per-frame streaming; `buf[0]=0x00, buf[1]=0x40, buf[2]=count`, then `{HID_keycode,R,G,B}` × from the 112-entry `keys[]` table; total length `APEX_PACKET_LENGTH=643` |
| ONBOARD/reset | `0x41` | hand lighting back to firmware |
| Region/serial | `0xF5` | read via `hid_write(65)`+`hid_read_timeout(64)` |
| Profile select | `0x89` | 5 onboard profiles |
| Firmware version | `0x90` | read-only |

**Verified on this unit 2026-09-27**: `0x4B` init, a `0x40` frame (all red, then only G green) and
`0x41` hand-back behaved as sent; `0xF5` replied `f5 00 01` (US). Packet sizes: `PACKET_IN=64`, `OUT=65`. Reuse: `OpenRGB Controllers/SteelSeriesController/`
(`SteelSeriesApexController.cpp`, `SteelSeriesApexBaseController.h`, `SteelSeriesDevices.h`,
`SteelSeriesControllerDetect.cpp`) — GPL-2.0-or-later, upgrades cleanly into GPL-3.0.

### 3a′. Typing interfaces (read for reactive lighting) — verified 2026-09-28

Report descriptors read from sysfs on `1038:1642` (no keystrokes were read):

- **iface 0** — boot keyboard, no report ID, 8 bytes: modifier bitmask (usages `E0–E7`), a
  reserved byte, six key slots (usages `00–FF`; all `0x01` = ErrorRollOver); LED output report.
- **iface 2** — N-key rollover, no report ID, 32 bytes: one bit per usage `00–FF` (modifiers at
  bits `E0–E7`); LED output report.

Both use HID keyboard usage codes, the numbering of the lighting table, so a press maps straight
to its LED — all but Menu, which types Application `0x65` (the HID usage; not yet captured on this
unit): the table has no `0x65` and lights Menu as `0xE7`. gglinux opens these read-only, and only
with the optional `61-gglinux-reactive.rules` installed; the default udev rule leaves them root-only.

### 3b. OLED — 128×40 monochrome — command `1F 81` (built 2026-10-29, untested here)

> **Source, adapted:** Apex Control's protocol reference (github.com/Rikearon/apex-control,
> `docs/PROTOCOL.md`, © 2026 Henrique Aron, CC BY 4.0, <https://creativecommons.org/licenses/by/4.0/>),
> checked by its author on one `0x1642` at firmware 1.19.7; apex-pro-tkl-gen3-linux (MIT) gives the
> same live-frame command. Condensed and reworded here.
>
> - Live frame: feature report on iface 1, report id 0, then `1F 81` and 640 bytes of pixels,
>   zero-padded to the descriptor's 644 bytes (645 with the report id on Linux). Held until the next
>   frame or a power cycle; doesn't touch flash.
> - Release to the keyboard's own screen: 65-byte output report `00 1F 82`.
> - Pixels: SSD1306 page order, 5 pages × 128 columns, the byte at `page*128 + x` holds rows
>   `page*8 … page*8+7` of column `x`, bit 0 the top row.
> - **Not used:** `38 83 00 <640 px>` stores an image in flash (apex-pro-tkl-gen3-linux sends it as
>   `1F 83`). gglinux's allowlist refuses both.
>
> This also explains the 2026-09-27 attempts below: the device command is `1F`, not apex-tux's `61`.

Earlier notes (apex-tux):
apex-tux covers the Gen3 *family* but **not `0x1642` specifically** (it lists `0x1640` full-size,
`0x1644` dongle, `0x1646` wireless-TKL). Two framings exist — **must confirm which `0x1642` uses on-device:**
- **Single-report** (`ApexProGen3` 0x1640): one 642-byte feature report on **iface 1**, SSD1306
  page-major, header `0x61`. `FB_SIZE = 40*128/8 + 2 = 642`, `BitArray<[u8;642],Msb0>`.
- **Chunked** (`ChunkedGen3` 0x1644/0x1646): **8 feature reports of 641 bytes on iface 3**;
  `report[0]=cmd` (`0x0C` wired / `0x4C` dongle), `report[1]=0x01`, `report[2..4]=offset LE`,
  `report[4]=80` chunk len, payload `report[6..86]`; offsets `0x0000,0050,00A0,00F0,0140,0190,01E0,0230`.

Reuse: `not-jan/apex-tux` `apex-hardware/src/usb.rs` + `device.rs` (Unlicense) — add `0x1642` to
`SupportedDevice`, pick the variant after descriptor inspection.

> **Tested on `0x1642` 2026-09-27 — none of the known framings drives its OLED.**
> The HID descriptor of iface 1 (the only writable vendor interface) declares **no report IDs** and a
> **644-byte feature report** (plus 64-byte input/output); iface 4 (`0xFFC1`) is input-only, iface 3 is
> consumer/mouse. With the owner watching the OLED, each of these left it **unchanged**:
> 1. apex-tux single-report: 642 bytes `[0x61, 640 page-major px, 0]` (0x61 lands in the report-id slot);
> 2. the same frame unnumbered, shaped like this keyboard's working RGB frames: `[0x00, 0x61, px…, 0]`;
> 3. (2) after a `0x4B` init;
> 4. apex-tux chunked Gen 3: 8 × `[0x00, 0x0C, 0x01, offset LE, 80, 0, 80 px]` (tried once, owner's choice).
>
> None had any visible effect; lighting was handed back (`0x41`) afterwards. **The OLED command for
> `0x1642` is unknown** — capture it from SteelSeries GG on Windows alongside the M5 actuation
> captures rather than guessing further on this interface.

### 3c. Actuation / switch behavior — command `38` (built 2026-10-29, untested here)

> **Source, adapted:** the same Apex Control reference (CC BY 4.0). All of these write the
> keyboard's RAM only (checked there: the flash profile is unchanged afterwards), so a power cycle
> brings the onboard profile back; gglinux re-sends on every connect.
>
> - 68 analog keys, by HID code: `04–28`, `2A–39`, `64`, `87–8B`, `E0–E7`, `F0` (no Escape, F-row,
>   navigation cluster or arrows). Every table lists all 68.
> - Actuation (feature, 645 bytes): `38 61 44 00` then `code, h, l` × 68. Byte 4 is the layer: `00`
>   primary, `02` the second (dual) point, which gglinux doesn't write. Points are 0.1–4.0 mm in
>   40 steps; the raw Hall thresholds per step (`l` release, `h` actuate) are the vendor's, as Apex
>   Control lists them, from `(4,4)` at 0.1 mm to `(213,219)` at 4.0 mm (table in
>   `crates/gg-device-keyboard/src/proto.rs`).
> - Release mode (feature): `38 62 44` then `code, mode` × 68: `0` normal, `2` Rapid Trigger
>   (`3`, `4` accepted, meaning unknown; not used).
> - Rapid Trigger distance (feature): `38 65 44` then `code, tenths of a mm` × 68 (default 2).
> - Rapid Tap (output, 65 bytes): `38 66 <0|1>` on/off; `38 67 <count>` then
>   `index, key1, key2, mode | 0x80 if both report, 0, 0` per pair, at most 10. Modes 0–3 are
>   documented as last-wins, neither, first-wins, second-wins; not verified anywhere.
> - **Not built:** key bindings (`36`, which also replaces the Fn layer's shortcuts if written
>   blank). Onboard profile images (`02` erase, `03` write) are §3d's, behind a gate.

Earlier notes:
No public project implements OmniPoint / Rapid Trigger / Rapid Tap / Protection Mode for *any* Apex
Pro generation on Linux. This is the project's one real reverse-engineering effort.
- Vendor/config interface: **usage page `0xFFC0`, usage 1 → iface 1 (wired)**. Report length 64.
- Feature spec to reproduce: OmniPoint 3.0 per-key actuation **0.1–4.0 mm, 40 levels (0.1 mm steps)**;
  Rapid Trigger (sensitivity/deadzone); Rapid Tap/SOCD (≤5 key pairs); Protection Mode; Dual
  Actuation (2-in-1 action point); Dual Bindings; 5 onboard profiles; 1000 Hz polling (NOT 8000).
- **Method:** capture GG↔keyboard traffic with usbmon/Wireshark (Windows host or Windows-in-VM/Wine),
  diff reports while toggling one setting at a time, decode opcodes + per-key byte layout.

### 3d. Onboard profile slots — `83` read, `02`/`03` erase and write, `B1`, `B2` (built M11, gated off, untested here)

> **Source, adapted:** the same Apex Control reference (CC BY 4.0), "Onboard profiles & the flash
> filesystem", "Image layout", "CRC", "Writing safely" and "Queries & misc". Its author reports a
> full slot read, and an erase + 25-chunk write + `B1` + full read-back, as confirmed on one keyboard
> (fw 1.19.7), first on spare slot 4 (0-based: file `0x84`, "Config 5", which ggctl and the app call
> slot 5). Where the document leaves the shape or timing open, gglinux does what Apex Control's own
> code does at the same commit (MIT; pinned in `gglinux-audit-2026-11-05/evidence/apexkit-f7db8c5/`,
> cited `AK:<file>:<line>`), as review R1-onboard found. gglinux builds them behind `keyboard_onboard`
> (off by default); none of it has been sent to this keyboard yet.
>
> - Every setting command above writes RAM only; at power-up the keyboard loads the active slot's
>   flash image. Slots 0–4 (shown 1–5) are files `0x80 + slot`. Integers are little-endian.
> - Read a chunk (feature, write then read): request `83 01 <file> F4 01 <offset u32>`; the feature
>   read answers `83 <err> <data…>`. gglinux asks for 25 × 500 bytes at offsets 0, 500 … 12000 and
>   drops the last chunk's 176 bytes past the image, as Apex Control's code reads (AK
>   `ApexDevice.swift:214-245`, `OnboardProfile.swift:136-148`). It waits 13 ms between the request
>   and the read (the feature report still holds the last command's answer until then; AK
>   `HIDTransport.swift:247-253`) and tries each chunk up to 4 times, 50 ms apart (AK
>   `ApexDevice.swift:223-237`).
> - Erase (output → input): `02 01 <file>`; the input report comes when the erase is done (up to
>   6 s). What it holds isn't documented: gglinux only waits for it (one that starts with `02`).
> - Write a chunk (feature): `03 01 <file> F4 01 <offset u32> <data…>`, 500-byte chunks, 13 ms after
>   each one, the last too. The 25th is sent exactly as Apex Control's code sends it: size 500 at
>   offset 12000, the image's last 324 bytes, then 176 × `0xFF` (AK `OnboardProfile.swift:155-177`,
>   pinned by its test `OnboardProfileTests.swift:302-318`). Nothing but `0xFF` goes past byte 12323.
> - Check (output → input): `B1 <slot>`. The reference says only that the firmware CRC-checks the
>   slot, and documents no answer. gglinux waits up to 2 s, logs what came, and lets the full
>   read-back decide, as Apex Control's code does (AK `ApexDevice.swift:274-277`). An answer other
>   than `B1 00`, or none, is kept as a note in the result until a session on the keyboard shows
>   what a good answer looks like.
> - Load (output): `B2 <slot>`, no answer.
> - The input queue: the kernel keeps every input report the interface sends for the open file.
>   Before the erase and before `B1`, gglinux throws away the ones nobody read, and while it waits
>   it skips reports that don't start with the opcode it waits for, so an old or late answer to
>   something else is never taken for the erase's or the check's (Apex Control waits only for the
>   report after its own send: AK `HIDTransport.swift:191-197, 259-286`).
> - Image (profile schema 3, device schema 8, fw ≥ 1.19.0), 12 324 bytes = 12 320-byte body + CRC.
>   Offsets: 0 schema `3` (u32) · 4 name, 24 bytes ASCII · 28 onboard lighting, 48 (Fn-lit mask at
>   40) · 76 Fn key, u32 (factory `0xF0`) · 80 normal-layer bindings, 100 × 5 · 580 Fn-layer
>   bindings, 100 × 5 · 1080 device schema `8` (u32) · 1084 primary actuation, 70 `h` then 70 `l` ·
>   1224 three zone levels + a 70-byte zone list · 1306 second actuation `h`, `l` (`FF FF` off),
>   release modes, Rapid Trigger distance, 70 each · 1586 protection ms (u32) + 70 distances ·
>   1660 second-actuation bindings, 70 × 5 · 2010 screen picture, 640 bytes as `1F 81` · 2650 Rapid
>   Tap, 10 × `key1, key2, mode | 0x80 both, 0, 0` + the on byte · 2701 padding · 2704 macros, 9 600
>   bytes · 12304 GUID, 16 · 12320 CRC. A test pins each as a number.
> - The per-key tables (bindings, actuation, release, distance, protection) are in the firmware's
>   own key order (`deviceKeyIndexToHID` in ApexKit), which the reference doesn't list. Until
>   gglinux has it, a write leaves those tables as the slot has them.
> - CRC: the STM32 CRC unit's CRC-32, polynomial `0x04C11DB7`, init `0xFFFFFFFF`, no reflection,
>   no final XOR, fed as 32-bit little-endian words over the 12 320 body bytes (the byte-wise
>   equivalent is CRC-32/MPEG-2 with each word's bytes reversed). ApexKit's vectors (`C704DD7B`,
>   `1DABE74F`, `8DCBFB33`, `908EDE87`; AK `OnboardProfileTests.swift:64-70`) are in the tests.
> - Writing safely, as gglinux does it (`crates/gg-device-keyboard/src/onboard.rs`,
>   `Keyboard::write_slot`): read the whole slot and check size, schemas and CRC → save a backup
>   file (`~/.config/gglinux/onboard-backups/slot<N>-<UTC>.bin`, folder 700, file 600, synced with
>   its folder) and read it back; it must equal the slot or nothing is erased → change the image in
>   memory and check it the same way → erase → 25 chunks → `B1` → read everything back and compare
>   (once more if the first read differs, as the first read after a change may lag). On any failure
>   from the erase on, the old image goes back, and that restore never stops once its erase is out:
>   each send is tried 3 times, an erase that doesn't answer is waited out for its whole 6 s, `B1`
>   decides nothing, the read-back always runs, and the whole restore is done up to 3 times before
>   it reports failure. The worker sends nothing else meanwhile, so the lighting stream pauses
>   around the write; stopping or restarting the daemon waits for a write to end. An unchanged
>   image isn't written. Most of a write takes about 2 s; with every wait run out, about 37 s.
> - A slot that doesn't check out any more (a write cut short, a restore that failed) is never
>   written that way. `ggctl onboard restore N <backup file> --damaged` (its own question first)
>   puts a backup into it (`Keyboard::recover_slot`): the slot is read and saved raw as
>   `slot<N>-<UTC>-damaged.bin`, the backup file is checked (size, schemas, CRC), then erase,
>   25 chunks, `B1` and read-back as a restore does them. It's refused for a slot that checks out,
>   and for one that holds a whole image with its CRC right but other schemas (other firmware's
>   layout). A slot that reads bad on a plain read is saved the same way, to look at later.

### 3e. Captured from GG 123 (2026-11-13), firmware 1.19.7

Our own USB captures of GG 123.0.0 with the keyboard passed through to a Windows VM (pcaps `apex-2…8`,
action logs and screenshots in the M11 capture folder). All on interface 1; answers to output reports
come as input reports on endpoint `0x82`.

| What | Bytes | Notes |
|---|---|---|
| Protection Mode | release-mode table `38 62 44 [key, mode] × 68` | mode `00` neither, `02` Rapid Trigger, `03` Protection Mode, `04` both. **Built (gated `keyboard_protection`).** |
| Deep press, "none" | `38 61 44 02 [key, h, l] × 68` | GG sends `00 00` for keys never set and `FF FF` for a key switched off; Apex Control sends `FF FF`. gglinux now sends what GG does. His flash images hold `00 00`. |
| Deep-press bindings | (GG's text) | "Maximum of 8 dual actuation keybinds permitted": enforced in `Bindings::validate`. |
| Slot write | `02 01 <file>` → input `02 00` (125 ms); 25 × `03 01 <file> F4 01 <offset u32>` ~15 ms apart, the last at 12000 with size 500; `B1 <slot>` → `B1 00 <slot> 01`; `B2 <slot>` → `B2 00 <slot>` | GG's Save. The shapes of §3d; `B2` does answer. GG doesn't read back; gglinux does. |
| Slot read | `83 01 <file> F4 01 <offset u32>` (feature), read 15–17 ms later → `83 00 <data>` | GG reads all five slots when it opens the keyboard. |
| Queries | `90` → `90 00 "1.19.7"`; `F5` → `F5 00 01` (region); `F2` → `F2 00 00` (layout); `34` → `34 00 00` (unknown) | GG's connect sequence: `90 F5 90 F2 F5`, the five slots, `1F 82`, `B2 <its config>`, `34`, `90`. |

**Deep press on his keyboard (firmware 1.19.7, 2026-11-13, finger tests with `hwtest/apex_probe.py`):**

- A deep-press binding (`36 02`) is kept only on a key that already has a second point. Written before the point, the
  keyboard drops it without an error (read back empty 5 times of 5; point first, kept 4 of 4). So gglinux sends the
  points, then the deep layer, and writes the deep layer again after every points table.
- With W at 3.0 mm and W → x: a gentle press types `w`, a full press `wxw` (the key's own action comes back as it
  rises past the point again).
- In RAM no table takes a point away: not `00 00` (which leaves a key as it is), not GG's `FF FF` for that key, not
  `FF FF` for all 68 keys. A profile load (`B2`) or unplugging does. A point with nothing bound past it does nothing (a
  full press types the key once), so when gglinux takes a point away it sends GG's reset for that key and clears the
  key's deep binding.
- Switch tables (`38 61 44 00`, `38 62`, `38 65`, `38 66`, `38 67`) and points tables don't touch the deep layer; `B2`
  clears it.
- Once, right after gglinux's first deep-press attempt (bindings, then switch tables at 1.5 mm without Rapid Trigger,
  then the points table with `FF FF` for 67 keys), the keyboard sent `` ` `` over and over with nobody touching it,
  until unplugged. Not seen again; the cause is unknown.

On his keyboard (2026-11-13): the firmware key order of every per-key table in a slot image is Apex
Control's `deviceKeyIndexToHID` (pinned `Key.swift`), which the images' own normal layers confirm; the 91
bindable keys are exactly `bindings::KEYS`; a bindings read (`B6`) needs ~13 ms before its answer is
there (read at once, the Fn layer's read still returned the normal layer's answer).

### What gglinux sends this keyboard: the allowlist

Every report is checked right before it goes out, by
[`crates/gg-device-keyboard/src/allow.rs`](../crates/gg-device-keyboard/src/allow.rs) (with
`ALLOWED`/`ALLOWED_SUB` in `proto.rs`). Shape is checked before the gate, and every refusal says
which rule refused it.

- **Always:** `40 41 4B 23 A3 90 F5`, the screen's `1F 81`/`1F 82`, and the switches' `38 61` (primary
  layer `00` only), `38 62`, `38 65`, `38 66`, `38 67`. RAM only.
- **With `keyboard_bindings` on:** `36`/`B6 <layer 0–2> <count 1–106>`, `35 <key>`, `3C <mask>`, and the
  second actuation table `38 61 44 02`. RAM only.
- **With `keyboard_onboard` on, flash:** `02 01 <file>`, `03 01 <file> F4 01 <offset>` and
  `83 01 <file> F4 01 <offset>` with `file` `0x80`–`0x84` and `offset` a multiple of 500 from 0 to 12000
  (500-byte chunks, as Apex Control's code sends them; `03`'s data is the image's bytes there, `0xFF` up to 500,
  then zeros), and `B1`/`B2 <slot 0–4>`. Nothing but zeros may follow their fields.
- **With `keyboard_protection` on:** release modes `03` and `04` in the `38 62` table (Protection Mode,
  and with Rapid Trigger). Without it the table takes only `00` and `02`; no other mode ever passes.
- **Never, whatever is on:** `04` (the firmware's file checksum), `02`/`03`/`83` with another
  sub-command or another file (the other files hold what the keyboard needs to run), `75` (writes the
  region to flash on every call), `38 83` (a screen picture to flash), any bootloader entry
  (`custom_enter_boot_bytes`, unpublished) and anything unlisted. A random-report property test runs
  120 000 reports against the listed shapes.

Background: these opcodes share the `0xFFC0` interface with SteelSeries' "FIZZ" update protocol
(9-byte header `cmd, fsid, fileid, block-size u16, offset u32, payload`; fwupd discussion #10308),
which is why the old rule here was "never send `0x02`, `0x03`, `0x04`". The narrow rule keeps the
update path closed (`04`, other files and sub-commands, the bootloader) while letting the five profile
slots through, gated. Probing beyond the listed shapes stays off limits on this interface.

---

## 4. GameSense (localhost HTTP JSON) — for RGB/OLED/tactile game events

Server discovery via `coreProps.json` = `{"address":"127.0.0.1:<port>"}` (dynamic port; any loopback
port works). Windows path `%PROGRAMDATA%\SteelSeries\SteelSeries Engine 3\coreProps.json`; macOS
`/Library/Application Support/SteelSeries Engine 3/coreProps.json`; **no native-Linux path exists.**

**Endpoints** (POST `application/json`): `/game_metadata`, `/register_game_event`, `/bind_game_event`,
`/game_event`, `/multiple_game_events` (+`/supports_multiple_game_events`, Engine 3.15.4+),
`/game_heartbeat`, `/remove_game_event`, `/remove_game`, `/stop_game`. Event/game names uppercase
`[A-Z0-9-_]`; heartbeat/deactivation timeout 15 s.

**Handlers** target `device-type` + `zone` with color/tactile/screen handlers. Keyboard zones:
`function-keys/main-keyboard/keypad/number-keys/macro-keys`; mouse `wheel/logo/base`; headset `earcups`.
Screen device-types include `screened-128x40` (Apex 7 / Apex Pro). **OLED frames:** `frame.image-data-128x40`
= **640-byte** bit-packed array (1 bit/px, MSB-first, upper-left origin) — same geometry as apex-tux.

### Proton bridge (the key finding — makes GameSense flagship, not niche)
pressure-vessel shares the **host network namespace** (it's a mount/filesystem container; never passes
`--unshare-net`), so `127.0.0.1:<port>` inside a Proton game **is** the host loopback. Wine maps
`C:\ProgramData` → `$WINEPREFIX/drive_c/ProgramData`, and for Proton `$WINEPREFIX =
<library>/steamapps/compatdata/<APPID>/pfx`. So `gglinuxd` must **write coreProps.json into every
prefix**:
```
<library>/steamapps/compatdata/<APPID>/pfx/drive_c/ProgramData/SteelSeries/SteelSeries Engine 3/coreProps.json
```
Enumerate all libraries via `libraryfolders.vdf`; `inotify`-watch for new `compatdata/*` so a
freshly-installed game gets the file **before first launch** (games read it once at startup; "absent"
= "Engine not running"). **Caveat:** breaks only under **Flatpak Steam** (can `--unshare-network`) —
document "use native Steam for GameSense". Native Arch `steam` is fine.

Reuse: `SteelSeries/gamesense-sdk` docs (protocol facts); `Ven0m0/steelseriesgg-rs` axum server
skeleton (MIT) — note it lacks the per-prefix writer, which is new work.

**Implemented 2026-09-29** in `gg-gamesense` + `gglinuxd/src/gamesense*`: the prefix writer never
overwrites a coreProps.json that isn't gglinux's (only an `address` key on `127.0.0.1`), and removes
its own files when GameSense is turned off. Requests with an `Origin` header are refused and POSTs
must be `application/json`, so browsers can't reach it.

---

## 5. Clips — gpu-screen-recorder (GPL-3.0)

Persistent replay-buffer child: `gpu-screen-recorder -w portal -f 60 -r 60 -c mp4 -o ~/Videos`
(`-w portal` for KDE-Wayland; NVENC auto-selected on the RTX 4090; `-replay-storage disk` to spare RAM;
`-bm cbr -q` for steady size). **Save buffer = `SIGUSR1`**, pause = `SIGUSR2`. Bind the save hotkey via
`org.freedesktop.portal.GlobalShortcuts` (Plasma 6 supports it) and signal the child — cleaner than
gsr's own keyboard-grab. Fallback: OBS replay buffer via `obs-websocket`.
