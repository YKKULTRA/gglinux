# Reactive keyboard lighting (M4a follow-up) — design

**Status:** approved in conversation 2026-09-28. **Milestone:** M4, after M4a lighting.

## Goal

Keys react to being pressed on the Apex Pro TKL Gen 3: either the pressed key lights and fades
(**Fade**) or a ring spreads out from it (**Ripple**), drawn over whatever base effect is running.
Getting key presses means reading the keyboard's typing interfaces. That access is opt-in, scoped
to this one keyboard, and closed by default. The same change closes a hole we opened in M0: today
the udev rule lets any program running as the desktop user read every keystroke typed on the Apex.

Success:
- after reinstalling the default rule, no program running as the user can open the Apex's typing
  interfaces;
- with the optional rule installed and reactive on, presses light up with no delay you can see;
- typing is unaffected;
- key codes never leave the daemon's memory.

Out of scope: any other keyboard; per-key reactive colours; key statistics or heatmaps; reactive
content on the OLED; evdev input.

## Facts (verified on this machine 2026-09-28)

- The Apex exposes five HID interfaces. gglinux drives only **iface 1** (lighting, `0xFFC0`).
- **iface 0** (`hidraw6`) descriptor: boot keyboard, no report ID, 8 bytes: modifier bitmask
  (usages `E0–E7`), a reserved byte, six key slots (usages `00–FF`), plus an LED output report.
- **iface 2** (`hidraw8`) descriptor: N-key rollover, no report ID, a 256-bit bitmap (usages
  `00–FF`, modifiers at bits `E0–E7`), 32 bytes, plus an LED output report.
- Both carry HID keyboard usage codes: the numbering of the lighting frame's `KEYS` table and of
  `layout::ANSI_TKL`. No translation is needed.
- It's not known which of the two interfaces the keyboard sends typing reports on. Each report
  carries the full pressed-key state, so reading both and diffing each against its own last state
  works either way.
- `data/udev/60-gglinux.rules` (installed, identical to the repo copy) tags **every**
  `hidraw*` under vendor `1038` with `uaccess`. `hidraw6` and `hidraw8` are `user:<you>:rw-` right
  now, and hidraw gets a copy of every input report.
- udev sets `ID_VENDOR_ID`, `ID_MODEL_ID` and `ID_USB_INTERFACE_NUM` on the hidraw node after
  `IMPORT{builtin}="usb_id"`, so one rule line can match vendor, product and interface together.
  Several `ATTRS{}` keys can't do that: they must all match on the same parent device, but
  `idVendor` lives on the USB device while `bInterfaceNumber` lives on the USB interface.
- Removing a udev tag doesn't revoke an ACL already on a node. It lasts until the node is
  re-created (replug or reboot) or the ACL is cleared.

## Design

### 1. Access

**`data/udev/60-gglinux.rules` (narrowed):** `IMPORT{builtin}="usb_id"` on `hidraw*` under
vendor `1038`, then `TAG+="uaccess"` only for:
- `1038:1642` interface `01` — Apex lighting;
- `1038:12e0` interface `04` — base station;
- `1038:1872` interface `03` — mouse.

Every other SteelSeries node stays root-only. Listing devices still works, because it only reads
sysfs.

**`data/udev/61-gglinux-reactive.rules` (new, optional):** `TAG+="uaccess"` for `1038:1642`
interfaces `00` and `02` only. The file header and the README say what it grants: any program you
run can read what you type on the Apex, as long as you're logged in at the seat.

**`scripts/install-local.sh`:**
- installs the narrowed 60 rule as today;
- a new `--reactive` flag also installs the 61 rule;
- without `--reactive`, it runs `setfacl -b` on the Apex's iface 00/02 hidraw nodes, found by
  `udevadm info` properties. This clears the ACLs the old rule left behind, so the hole closes
  without a replug.

The README documents the flag and how to undo it:

```
sudo rm /etc/udev/rules.d/61-gglinux-reactive.rules
sudo udevadm control --reload
# then replug the keyboard
```

gglinux never runs `sudo` itself.

### 2. Reading presses

**`gg-device-keyboard::keys` (pure):**
- `boot_keys(&[u8]) -> Option<KeySet>` — 8-byte report → pressed set, with modifiers as `E0–E7`.
  Returns `None` for a rollover-error report (any slot `0x01`) and for a wrong length.
- `nkro_keys(&[u8]) -> Option<KeySet>` — 32-byte bitmap → pressed set. Returns `None` for a wrong
  length.
- `pressed(prev, now) -> impl Iterator<Item = u8>` — codes in `now` but not in `prev`.
- `KeySet` is a 256-bit set.

**Reader (daemon, `keyboard/reader.rs`):**
- Finds the Apex's iface 0 and 2 hidraw paths by enumeration and opens them **read-only**.
- A thread polls both with a 200 ms timeout, so a stop flag is checked promptly. Each report goes
  to the decoder for its interface and is diffed against that interface's last set. Each new press
  is sent to the keyboard worker as `Command::Pressed { code, at: Instant }`, where `at` is when the reader
  received the report.
- It runs only while lighting and reactive are **both** on and the keyboard is connected; in any
  other state the nodes are closed.
- If either open fails with permission denied, the status is `no_access` and both nodes are closed.
- Any other failure (node missing, read error) closes both, and the worker retries every 2 s, like
  the lighting reopen.

**Privacy invariants:**
- key codes are never logged at any level;
- they're never written to disk and never put in IPC state or events;
- `Command::Pressed`'s `Debug` doesn't print the code;
- a press lives in memory only until its effect ends, in a buffer of at most 32 presses.

### 3. Engine (`keyboard/effects.rs`)

**Positions:** keys get 2-D centres in key units, `(x + w/2, y + 0.5)` from `ANSI_TKL`. The
wave keeps using `x / WIDTH`.

**`color(lighting, positions, code, t, presses)`:** computes the base colour as today, then:
- **Fade:** for a press of key `k` at age `a < duration`, key `k` gets weight `1 − a/duration`.
- **Ripple:** radius `r = 20·a` key units. Every key at distance `d` from the pressed key gets
  weight `(1 − a/duration) · max(0, 1 − |d − r|)`. At `a = 0` that's only the pressed key.
- A key's weight is the **max** over all live presses.

**Output:** `base·(1 − w) + colour·w` per channel, then brightness, as today.

**Frames:** at 30 fps while the base effect is animated **or** any press is still live. A static
base returns to on-change plus the 2 s refresh once presses expire.

### 4. IPC and UI

**`gg-ipc`:**
- `Lighting` gains `reactive: Reactive`, marked `#[serde(default)]` so an existing `lighting.json`
  still loads. `Reactive` is `{ enabled: bool, mode: ReactiveMode, color: Rgb, duration_s: f32 }`,
  with `ReactiveMode` = `fade` | `ripple`.
- The default is off, fade, white, 0.6 s.
- `Lighting::validate` rejects a `duration_s` outside `0.1..=3.0` or not finite.
- `KeyboardState` gains `reactive: ReactiveStatus` = `off` | `reading` | `no_access`, sent with the
  existing `keyboard_changed` event.
- The `fakemouse` example is updated to match.

**UI (`KeyboardPanel.svelte`):** a Reactive section with an on/off switch, Fade/Ripple, colour and
duration. With `no_access` it shows the install command, and the warning, in the same words as the
README.

## Testing

**Unit:**
- `boot_keys`: modifiers, six keys, rollover error → `None`, wrong length → `None`;
- `nkro_keys`: bits map to codes, wrong length → `None`;
- `pressed`: new keys only, releases ignored;
- Fade: weight 1 at `a = 0`, 0 at `duration`, other keys untouched;
- Ripple: a far key lights later than a near one, and nothing lights after `duration`;
- blend endpoints, and brightness still applied;
- max over presses;
- an old `lighting.json` without `reactive` loads with reactive off;
- `validate` bounds;
- `Command::Pressed` `Debug` omits the code.

**Rules:** `udevadm verify data/udev/*.rules`.

**Hardware (the owner):**
1. Reinstall the default rule. `getfacl /dev/hidraw6 /dev/hidraw8` shows no `user:` entry.
2. Lighting still works.
3. Turn reactive on: the panel shows `no_access`.
4. Install with `--reactive`: the panel shows `reading`.
5. Fade and Ripple both show over a wave.
6. Typing is unaffected.
7. Turning reactive off closes the nodes.
