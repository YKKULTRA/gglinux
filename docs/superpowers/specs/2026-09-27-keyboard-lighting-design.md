# Keyboard lighting (M4a) — design

**Status:** approved in conversation 2026-09-27; transport verified on the owner's keyboard the
same day. **Milestone:** M4 part 1 (M4b is the OLED).

## Goal

gglinux runs the Apex Pro TKL Gen 3's per-key RGB: a per-key static editor, animated effects
(breathing, wave, spectrum), and brightness — or hands lighting back to the keyboard's own
onboard profile. Success: colours painted in the UI appear on the keys; effects animate smoothly
without affecting typing; turning gglinux lighting off, or stopping the daemon, restores the
keyboard's own lighting; nothing outside a fixed allowlist of commands can ever be sent.

Out of scope here: the OLED (M4b); reactive-on-keypress lighting (needs read access to the
keyboard's input events — a security decision for the owner, asked separately); audio visualizer;
writing lighting into the keyboard's onboard memory (no public protocol); ISO picture (the owner's
keyboard reports US ANSI).

## Facts (OpenRGB `SteelSeriesApexController`, verified on `1038:1642` 2026-09-27)

- Interface 1, usage page `0xFFC0`. Command reports are 65 bytes with report id `0x00`.
- `0x4B` init (feature report), then `0x40` direct frames: a **643-byte feature report**
  `[0x00, 0x40, n, {hid_code, r, g, b} × n]` over OpenRGB's 112-entry HID-code table. The owner saw
  all-red, then only G green — as sent.
- `0x41` (output report) hands lighting back to the onboard profile — observed.
- `0xF5` region read: reply `f5 00 01 …` → US ANSI on this unit. `0x23`/`0xA3` set/get onboard
  brightness (not used: direct colours are absolute). `0x90` firmware version.
- The firmware-update (FIZZ) commands `0x02`/`0x03`/`0x04` share this interface and can brick it.

## Design

**`gg-device-keyboard` crate:** `proto` — the opcode **allowlist** (`0x4B 0x40 0x41 0x23 0xA3 0xF5
0x90`; every builder checks it, so nothing else can be framed), the 112-code table, the direct
frame builder, command reports, region decode; `layout` — the US ANSI TKL picture (87 keys: HID
code, label, x, y, width in key units); `Keyboard` — open, init, show a frame, hand back, read
region.

**Lighting engine (daemon, pure `effects` module):** colour of a key at time `t`:
static = its saved colour (unlisted keys dark); breathing = colour × (1 − cos 2πt/T)/2;
wave = rainbow hue by the key's horizontal position minus t/T; spectrum = one hue for all keys
cycling every T. Every frame is scaled by brightness (0–100 %). Periods 0.5–60 s.

**Worker:** a `gg-keyboard` thread owns the device (reopen every 2 s when absent). With gglinux
lighting on: init once per open, then frames at 30 fps for animated effects, or on change (and a
2 s refresh) for static. With it off: hand back once. On daemon exit: hand back (awaited with the
headset's shutdown). Settings persist in `$XDG_CONFIG_HOME/gglinux/lighting.json`; default is
**off** — gglinux never takes over the lighting until asked.

**IPC:** `Rgb`, `Effect` (tagged: `static`, `breathing {color, period_s}`, `wave {period_s}`,
`spectrum {period_s}`), `KeyColor {code, color}`, `Lighting {enabled, effect, keys, brightness}`,
`KeyInfo {code, label, x, y, w}`, `KeyboardState {connected, layout, lighting}`; requests
`get_keyboard_state`, `set_lighting {lighting}`; event `keyboard_changed`. `gg-ipc` gains
`config_dir()`.

**UI:** a Keyboard panel for the Apex: on/off ("gglinux controls the lighting"), effect picker
with its parameters, brightness, and — for static — the keyboard picture drawn from `layout`:
click keys to select, pick a colour, paint the selection or all keys, apply.

## Testing

Unit: allowlist (exactly the seven opcodes over all 256 bytes; FIZZ refused), frame layout and
size, command padding, region decode; layout (87 unique codes, all in the 112-code table, rows
within the TKL width); effects (HSV conversion, static lookup, breathing endpoints, wave varies
with position, spectrum uniform, brightness 0 is black); IPC JSON and validation.
Hardware (owner): paint keys, run each effect, typing unaffected, turn off → onboard lighting,
stop daemon → onboard lighting.
