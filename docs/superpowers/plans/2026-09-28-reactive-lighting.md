# Reactive keyboard lighting — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keys on the Apex Pro TKL Gen 3 react to presses (Fade or Ripple over the base effect). The
same change narrows the udev rule so no program can read Apex keystrokes unless the owner opts in.

**Architecture:**
- **udev:** the default rule opens only the control interfaces gglinux drives. An optional second
  rule opens the Apex's two typing interfaces.
- **Keyboard crate:** pure decoders for the typing reports go in `gg-device-keyboard::keys`.
- **Reader:** a thread in `gglinuxd` opens the typing nodes read-only, but only while reactive is
  on, and sends key-down edges to the keyboard worker.
- **Engine:** the pure effects engine blends Fade/Ripple over the base colour.
- **IPC and UI:** IPC gains the reactive settings and status; the UI gains a Reactive section.

**Tech Stack:** Rust 2021 (hidapi for enumeration, `libc::poll` for reading), udev rules and bash,
Svelte 5.

**Spec:** `docs/superpowers/specs/2026-09-28-reactive-lighting-design.md`

**Branch:** create `m4-reactive-lighting` from `master` before Task 1.

## Global Constraints

- **Privacy:**
  - key codes are never logged at any level, never written to disk, and never put in IPC state or
    events;
  - `Command::Pressed`'s `Debug` prints `Pressed` only;
  - `KeySet`'s `Debug` prints only a count;
  - at most 32 presses are held in memory, each only until its effect ends.
- **Read-only:** the typing nodes are opened read-only (`O_RDONLY | O_NONBLOCK`). Nothing is ever
  written to interfaces 0 or 2. The lighting allowlist (`0x23 0x40 0x41 0x4B 0x90 0xA3 0xF5`)
  stays exactly as it is.
- **The reader runs only while** `lighting.enabled && lighting.reactive.enabled` and the keyboard
  is connected. In any other state the nodes are closed.
- **Default rule opens exactly:**
  - `1038:1642` iface `01`
  - `1038:12e0` iface `04`
  - `1038:1872` iface `03`
- **Optional rule opens exactly:** `1038:1642` ifaces `00` and `02`.
- **Reactive defaults:** off, `fade`, white `{255,255,255}`, `duration_s` 0.6. It's valid for
  `0.1..=3.0` and finite.
- **Ripple:** radius `20·age` key units; ring half-width 1 key unit. Weight is
  `(1 − age/duration) · max(0, 1 − |d − r|)`. Fade weight is `1 − age/duration` on the pressed key
  only. A key's weight is the max over live presses. Output is `base·(1 − w) + colour·w`, then
  brightness.
- **Frames:** 30 fps while the base effect is animated or any press is live. Reader open retries
  every 2 s (`REOPEN_INTERVAL`), including after `no_access`.
- **Never run `sudo`.** The owner installs rules. The agent writes files and runs root-free checks
  only.
- **Checks:** `cargo test --workspace --all-targets`, `cargo clippy --workspace --all-targets` (no
  warnings), `cd ui && npm run check`.
- **Style:** match the surrounding hand formatting; don't run `cargo fmt` repo-wide (the originals
  were never rustfmt-clean).

## Review Focus

1. **Keyboard unplugged while reading.** `poll` reports `POLLHUP`/`POLLERR` or `read` returns 0 or
   an error. The reader must end and report `ReaderStopped`, not spin at 100 % CPU. Pinned by the
   Task 5 test `peer_closing_stops_the_reader_and_reports_it`.
2. **Rollover error in the middle of typing.** A report of `0x01` slots must leave the last state
   unchanged. Treating it as "all released" would re-fire every held key on the next report.
   Pinned by the Task 5 test `held_keys_and_rollover_errors_do_not_re_fire`.
3. **A held key plus a new key.** The new report contains the held key again; only the new key
   fires. Pinned by the Task 2 test `pressed_reports_only_new_keys` and the Task 5 sequence test.
4. **Access on only one interface.** If iface 0 opens but iface 2 is denied, the reader must close
   both and report `no_access`, never read half the keyboard. Pinned by the Task 5 test
   `one_denied_node_means_no_access_and_nothing_stays_open`.
5. **A bad duration in a hand-edited `lighting.json`** (0, negative, NaN). `load` doesn't
   validate, so the engine must return weight 0, never NaN colours. Pinned by the Task 4 test
   `a_bad_duration_lights_nothing`.

---

### Task 1: Narrow the udev rule; add the optional reactive rule

**Files:**
- Create: `scripts/check-udev.sh`
- Modify: `data/udev/60-gglinux.rules` (whole file)
- Create: `data/udev/61-gglinux-reactive.rules`
- Modify: `scripts/install-local.sh`
- Modify: `README.md` (the install section, lines ~44–53)

**Interfaces:**
- Produces: the rule files and `./scripts/check-udev.sh [--reactive]` (exit 0 = every SteelSeries
  hidraw node is opened exactly as expected; 1 = a mismatch; 2 = no SteelSeries node present).
- Produces: the warning sentence the UI copies in Task 7:
  > Reactive lighting needs to see which keys you press. The optional udev rule lets any program
  > you run read what you type on the Apex while you're logged in. gglinux reads only while
  > reactive lighting is on, and keeps presses in memory only until their light fades.

- [ ] **Step 1: Write the check script (the test)**

`scripts/check-udev.sh`:

```bash
#!/usr/bin/env bash
# Check which SteelSeries hidraw nodes the repo's udev rules open to the desktop user,
# without installing them: `udevadm test` loads the repo copies from a scratch directory,
# where a file of the same name shadows any installed copy. CURRENT_TAGS is what this run
# tags; TAGS also carries sticky tags from earlier events, so it is not used.
#
# Needs the devices plugged in; needs no root.
#
# Usage: ./scripts/check-udev.sh [--reactive]
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
reactive=0
for arg in "$@"; do
  case "$arg" in
    --reactive) reactive=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

dir="$(mktemp -d)"
trap 'rm -rf "$dir"' EXIT
cp "$repo/data/udev/60-gglinux.rules" "$dir/"
if [[ $reactive -eq 1 ]]; then
  cp "$repo/data/udev/61-gglinux-reactive.rules" "$dir/"
else
  printf '# shadows an installed copy: checking the default rule alone\n' >"$dir/61-gglinux-reactive.rules"
fi

# Whether gglinux should open this product:interface to the desktop user.
expected() {
  case "$1:$2" in
    1642:01 | 12e0:04 | 1872:03) echo yes ;;
    1642:00 | 1642:02) if [[ $reactive -eq 1 ]]; then echo yes; else echo no; fi ;;
    *) echo no ;;
  esac
}

prop() { sed -n "s/^$1=//p" <<<"$2"; }

seen=0
fail=0
for sys in /sys/class/hidraw/hidraw*; do
  props="$(udevadm info --query=property "$sys" 2>/dev/null || true)"
  [[ "$(prop ID_VENDOR_ID "$props")" == 1038 ]] || continue
  pid="$(prop ID_MODEL_ID "$props")"
  ifn="$(prop ID_USB_INTERFACE_NUM "$props")"
  seen=$((seen + 1))
  tags="$(udevadm test -D "$dir" --action=add "$sys" 2>/dev/null | sed -n 's/^ *CURRENT_TAGS=//p')"
  got=no
  [[ "$tags" == *:uaccess:* ]] && got=yes
  want="$(expected "$pid" "$ifn")"
  mark=ok
  [[ "$got" == "$want" ]] || { mark=WRONG; fail=1; }
  printf '%-9s 1038:%s iface %s  opened=%-3s want=%-3s %s\n' "$(basename "$sys")" "$pid" "$ifn" "$got" "$want" "$mark"
done

if [[ $seen -eq 0 ]]; then
  echo "no SteelSeries hidraw nodes found — plug a device in" >&2
  exit 2
fi
exit $fail
```

Run: `chmod +x scripts/check-udev.sh`

- [ ] **Step 2: Run it against the current (broad) rule to verify it fails**

Run: `./scripts/check-udev.sh; echo "exit=$?"`
Expected: `exit=1`. Lines for `1642` ifaces `00`, `02`, `03`, `04` and `12e0` iface `03` show
`opened=yes want=no WRONG`; `1642:01` and `12e0:04` show `ok`.

- [ ] **Step 3: Write the narrowed default rule**

Replace `data/udev/60-gglinux.rules` entirely:

```
# gglinux — grant the logged-in desktop user access to the SteelSeries HID interfaces
# gglinux controls, and nothing else.
#
# Install:
#   ./scripts/install-local.sh
# or by hand:
#   sudo install -Dm644 60-gglinux.rules /etc/udev/rules.d/60-gglinux.rules
#   sudo udevadm control --reload
#   sudo udevadm trigger --subsystem-match=hidraw --action=add
#
# TAG+="uaccess" asks systemd-logind to place an ACL on the device node for whoever
# owns the active local session. No root, no polkit, no plugdev group — and the ACL
# is revoked automatically when the session ends. This must be evaluated *before*
# /usr/lib/udev/rules.d/70-uaccess.rules, hence the 60- prefix.
#
# Only control interfaces are opened. A keyboard's typing interfaces carry every
# keystroke, so they stay root-only here; reactive lighting opts in to the Apex's
# separately, with 61-gglinux-reactive.rules.
#
# Vendor, product and interface are matched as properties from the usb_id builtin:
# several ATTRS{} keys must all match on one parent device, but idVendor lives on
# the USB device and bInterfaceNumber on the USB interface.
#
# Fallback for systems without systemd-logind: replace TAG+="uaccess" with
#   MODE="0660", GROUP="plugdev"
# and add yourself to the plugdev group.

ACTION=="remove", GOTO="gglinux_end"
SUBSYSTEM!="hidraw", GOTO="gglinux_end"
IMPORT{builtin}="usb_id"
ENV{ID_VENDOR_ID}!="1038", GOTO="gglinux_end"

# Apex Pro TKL Gen 3 — lighting (vendor usage page 0xFFC0)
ENV{ID_MODEL_ID}=="1642", ENV{ID_USB_INTERFACE_NUM}=="01", TAG+="uaccess"
# Arctis Nova Pro Wireless base station — control
ENV{ID_MODEL_ID}=="12e0", ENV{ID_USB_INTERFACE_NUM}=="04", TAG+="uaccess"
# Rival 3 Wireless Gen 2 — control
ENV{ID_MODEL_ID}=="1872", ENV{ID_USB_INTERFACE_NUM}=="03", TAG+="uaccess"

LABEL="gglinux_end"
```

- [ ] **Step 4: Write the optional reactive rule**

`data/udev/61-gglinux-reactive.rules`:

```
# gglinux — OPTIONAL: lets reactive lighting see key presses on the Apex Pro TKL Gen 3.
#
# Reactive lighting needs to see which keys you press. This rule lets any program you
# run read what you type on the Apex while you're logged in. gglinux reads only while
# reactive lighting is on, and keeps presses in memory only until their light fades.
# Install it only if you want reactive lighting:
#   ./scripts/install-local.sh --reactive
# Undo:
#   sudo rm /etc/udev/rules.d/61-gglinux-reactive.rules
#   sudo udevadm control --reload
#   then replug the keyboard (an ACL already on a node outlives its rule).

ACTION=="remove", GOTO="gglinux_reactive_end"
SUBSYSTEM!="hidraw", GOTO="gglinux_reactive_end"
IMPORT{builtin}="usb_id"
ENV{ID_VENDOR_ID}!="1038", GOTO="gglinux_reactive_end"
ENV{ID_MODEL_ID}!="1642", GOTO="gglinux_reactive_end"

# Interface 0: the boot keyboard report. Interface 2: the N-key-rollover report.
ENV{ID_USB_INTERFACE_NUM}=="00|02", TAG+="uaccess"

LABEL="gglinux_reactive_end"
```

- [ ] **Step 5: Run the checks to verify they pass**

Run: `udevadm verify data/udev/*.rules && ./scripts/check-udev.sh; echo "exit=$?"; ./scripts/check-udev.sh --reactive; echo "exit=$?"`
Expected: `udevadm verify` reports 2 files, `Fail: 0`. The first check prints every line `ok` with
only `1642:01` and `12e0:04` opened, then `exit=0`. The second prints every line `ok` with
`1642:00`, `1642:01`, `1642:02` and `12e0:04` opened, then `exit=0`.

- [ ] **Step 6: Teach the installer `--reactive` and the ACL cleanup**

In `scripts/install-local.sh`:

Change the usage line and option parsing:

```bash
# Usage: ./scripts/install-local.sh [--no-udev] [--no-build] [--reactive]
#
# --reactive also installs the optional rule that lets reactive lighting see key presses on
# the Apex Pro TKL Gen 3 — which lets any program you run read what you type on it.
```

```bash
do_reactive=0
for arg in "$@"; do
  case "$arg" in
    --no-udev)  do_udev=0 ;;
    --no-build) do_build=0 ;;
    --reactive) do_reactive=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done
```

Replace the `if [[ $do_udev -eq 1 ]]; then … fi` block with:

```bash
if [[ $do_udev -eq 1 ]]; then
  echo "==> installing udev rule (needs root)"
  sudo install -Dm644 "$repo/data/udev/60-gglinux.rules" /etc/udev/rules.d/60-gglinux.rules
  if [[ $do_reactive -eq 1 ]]; then
    echo "==> installing the optional reactive-lighting rule:"
    echo "    programs you run will be able to read what you type on the Apex"
    sudo install -Dm644 "$repo/data/udev/61-gglinux-reactive.rules" /etc/udev/rules.d/61-gglinux-reactive.rules
  fi
  sudo udevadm control --reload
  # Re-apply to already-plugged devices so you don't have to replug.
  sudo udevadm trigger --subsystem-match=hidraw --action=add
  if [[ ! -e /etc/udev/rules.d/61-gglinux-reactive.rules ]]; then
    # Earlier gglinux rules opened the Apex's typing interfaces too, and a node keeps an
    # ACL after its rule stops granting it — clear those so the old access really ends.
    for sys in /sys/class/hidraw/hidraw*; do
      props="$(udevadm info --query=property "$sys" 2>/dev/null || true)"
      if grep -qx 'ID_VENDOR_ID=1038' <<<"$props" && grep -qx 'ID_MODEL_ID=1642' <<<"$props" \
        && grep -qxE 'ID_USB_INTERFACE_NUM=0[02]' <<<"$props"; then
        sudo setfacl -b "/dev/$(basename "$sys")"
      fi
    done
  fi
fi
```

Run: `bash -n scripts/install-local.sh && echo syntax-ok`
Expected: `syntax-ok`. **Do not run the installer** (it uses sudo; the owner runs it).

- [ ] **Step 7: README**

In `README.md`, replace the paragraph that starts "Install it, the `systemd --user` unit, and the
udev rule…" through "…cannot be opened or controlled." with:

````markdown
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
````

- [ ] **Step 8: Commit**

```bash
git add scripts/check-udev.sh scripts/install-local.sh data/udev/60-gglinux.rules data/udev/61-gglinux-reactive.rules README.md
git commit -m "udev: open only the interfaces gglinux controls; reactive access is opt-in

The old rule opened every SteelSeries hidraw node, including the Apex's two
typing interfaces, so any program running as the desktop user could read
every keystroke on it. The default rule now opens only the control
interfaces; 61-gglinux-reactive.rules opts in to the typing ones.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Decode the typing reports (`gg-device-keyboard::keys`)

**Files:**
- Create: `crates/gg-device-keyboard/src/keys.rs`
- Modify: `crates/gg-device-keyboard/src/lib.rs` (add `pub mod keys;` after `pub mod layout;`)
- Modify: `docs/PROTOCOLS.md` (append to section 3, before `### 3b.`)

**Interfaces:**
- Produces:
  - `pub struct KeySet` (`Copy`, `Default`, `PartialEq`, `Eq`; manual `Debug` that prints only
    the count), with `insert(&mut self, u8)`, `contains(&self, u8) -> bool` and
    `iter(self) -> impl Iterator<Item = u8>`;
  - `pub const BOOT_LEN: usize = 8` and `pub const NKRO_LEN: usize = 32`;
  - `pub fn boot_keys(report: &[u8]) -> Option<KeySet>` and
    `pub fn nkro_keys(report: &[u8]) -> Option<KeySet>`;
  - `pub fn pressed(prev: KeySet, now: KeySet) -> impl Iterator<Item = u8>`.

- [ ] **Step 1: Write the failing tests**

`crates/gg-device-keyboard/src/keys.rs`, tests first (the functions come in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn codes(set: KeySet) -> Vec<u8> {
        set.iter().collect()
    }

    fn set(codes: &[u8]) -> KeySet {
        let mut s = KeySet::default();
        codes.iter().for_each(|&c| s.insert(c));
        s
    }

    #[test]
    fn boot_reports_give_modifiers_and_keys() {
        // Left Ctrl + Right Shift held, A and Enter pressed.
        let report = [0b0010_0001, 0, 0x04, 0x28, 0, 0, 0, 0];
        assert_eq!(codes(boot_keys(&report).unwrap()), vec![0x04, 0x28, 0xE0, 0xE5]);
        assert_eq!(codes(boot_keys(&[0; 8]).unwrap()), Vec::<u8>::new(), "all released");
    }

    #[test]
    fn boot_rollover_errors_and_wrong_lengths_are_not_a_state() {
        assert!(boot_keys(&[0, 0, 1, 1, 1, 1, 1, 1]).is_none(), "ErrorRollOver says nothing about which keys");
        assert!(boot_keys(&[0; 7]).is_none());
        assert!(boot_keys(&[0; 9]).is_none());
    }

    #[test]
    fn nkro_bits_are_usage_codes() {
        let mut report = [0u8; NKRO_LEN];
        report[0x04 / 8] |= 1 << (0x04 % 8); // A
        report[0xE1 / 8] |= 1 << (0xE1 % 8); // Left Shift
        assert_eq!(codes(nkro_keys(&report).unwrap()), vec![0x04, 0xE1]);
        assert!(nkro_keys(&[0; 31]).is_none());
        assert!(nkro_keys(&[0; 33]).is_none());
    }

    #[test]
    fn pressed_reports_only_new_keys() {
        assert_eq!(pressed(set(&[]), set(&[0x04])).collect::<Vec<_>>(), vec![0x04]);
        assert_eq!(pressed(set(&[0x04]), set(&[0x04, 0x05])).collect::<Vec<_>>(), vec![0x05], "a held key does not fire again");
        assert_eq!(pressed(set(&[0x04, 0x05]), set(&[0x05])).count(), 0, "releases are not presses");
    }

    #[test]
    fn debug_never_shows_which_keys() {
        let s = set(&[0x04, 0x05]);
        assert_eq!(format!("{s:?}"), "KeySet(2 keys)");
    }
}
```

With only the test module present, add stubs so the module compiles and the tests fail on
behaviour:

```rust
//! The keyboard's typing reports, decoded to the set of keys held down.

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct KeySet([u64; 4]);

impl KeySet {
    pub fn insert(&mut self, _code: u8) {}
    pub fn contains(&self, _code: u8) -> bool { false }
    pub fn iter(self) -> impl Iterator<Item = u8> { std::iter::empty() }
}

impl std::fmt::Debug for KeySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("KeySet") }
}

pub const BOOT_LEN: usize = 8;
pub const NKRO_LEN: usize = 32;

pub fn boot_keys(_report: &[u8]) -> Option<KeySet> { None }
pub fn nkro_keys(_report: &[u8]) -> Option<KeySet> { None }
pub fn pressed(_prev: KeySet, _now: KeySet) -> impl Iterator<Item = u8> { std::iter::empty() }
```

Add `pub mod keys;` to `crates/gg-device-keyboard/src/lib.rs` after `pub mod layout;`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p gg-device-keyboard keys`
Expected: FAIL. `boot_reports_give_modifiers_and_keys`, `nkro_bits_are_usage_codes`,
`pressed_reports_only_new_keys` and `debug_never_shows_which_keys` panic.
`boot_rollover_errors_and_wrong_lengths_are_not_a_state` passes against the stub; that's fine,
it pins the `None` cases.

- [ ] **Step 3: Implement**

Replace the stubs (keep the test module):

```rust
//! The keyboard's typing reports, decoded to the set of keys held down.
//!
//! Interface 0 sends the 8-byte boot report (a modifier bitmask, a reserved
//! byte, six key slots); interface 2 sends a 32-byte N-key-rollover bitmap,
//! one bit per usage. Both use HID keyboard usage codes — the numbering of
//! [`crate::proto::KEYS`] and [`crate::layout::ANSI_TKL`].
//!
//! Which keys someone types is private: nothing here logs, and `Debug` on a
//! [`KeySet`] prints only how many keys it holds.

/// A set of HID keyboard usages, 0..=255.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct KeySet([u64; 4]);

impl KeySet {
    pub fn insert(&mut self, code: u8) {
        self.0[usize::from(code >> 6)] |= 1 << (code & 63);
    }

    pub fn contains(&self, code: u8) -> bool {
        self.0[usize::from(code >> 6)] & (1 << (code & 63)) != 0
    }

    pub fn iter(self) -> impl Iterator<Item = u8> {
        (0..=255u8).filter(move |&c| self.contains(c))
    }
}

impl std::fmt::Debug for KeySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n: u32 = self.0.iter().map(|w| w.count_ones()).sum();
        write!(f, "KeySet({n} keys)")
    }
}

pub const BOOT_LEN: usize = 8;
pub const NKRO_LEN: usize = 32;

/// HID usage `ErrorRollOver`: too many keys held for the boot report to say which.
const ERROR_ROLL_OVER: u8 = 0x01;
const FIRST_MODIFIER: u8 = 0xE0;

/// The keys held down in a boot report; `None` for a rollover error or a wrong length.
pub fn boot_keys(report: &[u8]) -> Option<KeySet> {
    if report.len() != BOOT_LEN {
        return None;
    }
    let slots = &report[2..];
    if slots.contains(&ERROR_ROLL_OVER) {
        return None;
    }
    let mut set = KeySet::default();
    for bit in 0..8 {
        if report[0] & (1 << bit) != 0 {
            set.insert(FIRST_MODIFIER + bit);
        }
    }
    slots.iter().filter(|&&c| c != 0).for_each(|&c| set.insert(c));
    Some(set)
}

/// The keys held down in an N-key-rollover bitmap; `None` for a wrong length.
pub fn nkro_keys(report: &[u8]) -> Option<KeySet> {
    if report.len() != NKRO_LEN {
        return None;
    }
    let mut set = KeySet::default();
    for (i, &byte) in report.iter().enumerate() {
        for bit in 0..8 {
            if byte & (1 << bit) != 0 {
                set.insert((i * 8 + bit) as u8);
            }
        }
    }
    Some(set)
}

/// Keys down in `now` that were not down in `prev`.
pub fn pressed(prev: KeySet, now: KeySet) -> impl Iterator<Item = u8> {
    KeySet(std::array::from_fn(|i| now.0[i] & !prev.0[i])).iter()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p gg-device-keyboard`
Expected: PASS, all keyboard crate tests including the five new ones.

- [ ] **Step 5: Record the typing interfaces in PROTOCOLS.md**

In `docs/PROTOCOLS.md`, insert before the `### 3b. OLED` heading:

```markdown
### 3a′. Typing interfaces (read for reactive lighting) — verified 2026-09-28

Report descriptors read from sysfs on `1038:1642` (no keystrokes were read):

- **iface 0** — boot keyboard, no report ID, 8 bytes: modifier bitmask (usages `E0–E7`), a
  reserved byte, six key slots (usages `00–FF`; all `0x01` = ErrorRollOver); LED output report.
- **iface 2** — N-key rollover, no report ID, 32 bytes: one bit per usage `00–FF` (modifiers at
  bits `E0–E7`); LED output report.

Both use HID keyboard usage codes, the numbering of the lighting table, so a press maps straight
to its LED. gglinux opens these read-only, and only with the optional
`61-gglinux-reactive.rules` installed; the default udev rule leaves them root-only.
```

- [ ] **Step 6: Commit**

```bash
git add crates/gg-device-keyboard/src/keys.rs crates/gg-device-keyboard/src/lib.rs docs/PROTOCOLS.md
git commit -m "gg-device-keyboard: decode the Apex's typing reports

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Reactive settings and status in IPC

**Files:**
- Modify: `crates/gg-ipc/src/lib.rs` (the lighting types around lines 378–457; tests at the end)
- Modify: `crates/gglinuxd/src/keyboard/effects.rs` (test helper `lit`)
- Modify: `crates/gglinuxd/src/keyboard.rs` (`snapshot`)
- Modify: `crates/gglinuxd/examples/fakemouse.rs` (the `KeyboardState` literal, ~line 91)

**Interfaces:**
- Produces (in `gg_ipc`):
  - `pub enum ReactiveMode { Fade, Ripple }` (snake_case in JSON);
  - `pub struct Reactive { pub enabled: bool, pub mode: ReactiveMode, pub color: Rgb, pub duration_s: f32 }`
    with `Default` (off, Fade, white, 0.6) and `pub const DURATION_S: RangeInclusive<f32> = 0.1..=3.0`;
  - `Lighting.reactive: Reactive` with `#[serde(default)]`;
  - `pub enum ReactiveStatus { Off, Reading, NoAccess }` (snake_case, `Default` = `Off`);
  - `KeyboardState.reactive: ReactiveStatus`.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `crates/gg-ipc/src/lib.rs`:

```rust
    #[test]
    fn lighting_saved_before_reactive_still_loads_with_reactive_off() {
        let old = r#"{"enabled":true,"effect":{"kind":"static"},"keys":[],"brightness":70}"#;
        let l: Lighting = serde_json::from_str(old).unwrap();
        assert!(l.enabled && l.brightness == 70);
        assert_eq!(l.reactive, Reactive::default());
        assert!(!l.reactive.enabled);
    }

    #[test]
    fn reactive_defaults_json_and_bounds() {
        let d = Reactive::default();
        assert_eq!((d.mode, d.color, d.duration_s), (ReactiveMode::Fade, Rgb { r: 255, g: 255, b: 255 }, 0.6));
        let json = serde_json::to_value(Reactive { mode: ReactiveMode::Ripple, ..d }).unwrap();
        assert_eq!(json["mode"], "ripple");
        assert_eq!(serde_json::to_value(ReactiveStatus::NoAccess).unwrap(), "no_access");
        assert_eq!(ReactiveStatus::default(), ReactiveStatus::Off);

        let with = |duration_s| Lighting { reactive: Reactive { duration_s, ..d }, ..Lighting::default() };
        assert!(with(0.1).validate().is_ok());
        assert!(with(3.0).validate().is_ok());
        assert!(with(0.05).validate().is_err());
        assert!(with(3.5).validate().is_err());
        assert!(with(f32::NAN).validate().is_err());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p gg-ipc reactive`
Expected: FAIL to compile, with `cannot find type Reactive` / `ReactiveMode` / `ReactiveStatus`.

- [ ] **Step 3: Implement**

In `crates/gg-ipc/src/lib.rs`, after `pub struct KeyColor { … }`:

```rust
/// How a key press shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactiveMode {
    /// The pressed key lights and fades back.
    Fade,
    /// A ring spreads out from the pressed key.
    Ripple,
}

/// Key presses drawn over the base effect.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Reactive {
    pub enabled: bool,
    pub mode: ReactiveMode,
    pub color: Rgb,
    /// How long one press shows, in seconds.
    pub duration_s: f32,
}

impl Reactive {
    pub const DURATION_S: std::ops::RangeInclusive<f32> = 0.1..=3.0;
}

impl Default for Reactive {
    fn default() -> Self {
        Reactive { enabled: false, mode: ReactiveMode::Fade, color: Rgb { r: 255, g: 255, b: 255 }, duration_s: 0.6 }
    }
}
```

In `pub struct Lighting`, after `pub brightness: u8,`:

```rust
    /// Presses drawn over the effect; absent in files saved before it existed.
    #[serde(default)]
    pub reactive: Reactive,
```

In `Lighting::validate`, before the final `Ok(())`:

```rust
        let d = self.reactive.duration_s;
        if !(d.is_finite() && Reactive::DURATION_S.contains(&d)) {
            return Err(format!("reactive duration {d} s out of range (0.1..=3)"));
        }
```

In `impl Default for Lighting`, add `reactive: Reactive::default()` to the literal.

After `pub struct KeyInfo { … }`:

```rust
/// Whether the daemon is reading key presses for reactive lighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactiveStatus {
    /// Not reading: reactive or lighting is off, or the keyboard is away.
    #[default]
    Off,
    Reading,
    /// The typing interfaces can't be opened: the optional udev rule isn't installed.
    NoAccess,
}
```

In `pub struct KeyboardState`, after `pub lighting: Lighting,`:

```rust
    pub reactive: ReactiveStatus,
```

Fix the other constructors so the workspace builds:
- `crates/gglinuxd/src/keyboard/effects.rs`, in `fn lit`: `Lighting { enabled: true, effect, keys: Vec::new(), brightness: 100, reactive: gg_ipc::Reactive::default() }`.
- `crates/gglinuxd/src/keyboard.rs`, in `snapshot`: add `reactive: gg_ipc::ReactiveStatus::Off,` after
  `lighting: state.lighting.clone(),`. Task 6 replaces this with the live status.
- `crates/gglinuxd/examples/fakemouse.rs` (~line 91): `KeyboardState { connected: true, layout: Vec::new(), lighting: Lighting::default(), reactive: ReactiveStatus::Off }`,
  and add `ReactiveStatus` to its `gg_ipc` import list.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --all-targets 2>&1 | grep -E "^test result|FAILED|error" `
Expected: every `test result: ok`, no `FAILED`, no `error`.

- [ ] **Step 5: Commit**

```bash
git add crates/gg-ipc/src/lib.rs crates/gglinuxd/src/keyboard/effects.rs crates/gglinuxd/src/keyboard.rs crates/gglinuxd/examples/fakemouse.rs
git commit -m "gg-ipc: reactive lighting settings and reading status

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Fade and Ripple in the effects engine

**Files:**
- Modify: `crates/gglinuxd/src/keyboard/effects.rs`
- Modify: `crates/gglinuxd/src/keyboard.rs` (callers of `positions`/`color`; the `State.positions` type)

**Interfaces:**
- Consumes: `gg_ipc::{Reactive, ReactiveMode}` (Task 3).
- Produces:
  - `pub struct Pos { pub x: f32, pub y: f32 }` — a key centre in key units;
  - `pub fn positions() -> HashMap<u8, Pos>`;
  - `pub struct Hit { pub code: u8, pub t: f32 }` — a press at `t` seconds of effect time
    (`Clone`, `Copy`, no `Debug`);
  - `pub fn color(lighting: &Lighting, positions: &HashMap<u8, Pos>, code: u8, t: f32, hits: &[Hit]) -> Rgb`;
  - `pub fn reactive_weight(r: &Reactive, positions: &HashMap<u8, Pos>, code: u8, t: f32, hits: &[Hit]) -> f32`;
  - `pub const RIPPLE_SPEED: f32 = 20.0`.

- [ ] **Step 1: Write the failing tests**

In the `tests` module of `effects.rs`, change every existing `color(…, t)` call to pass a fifth
argument `&[]`. Add the imports and tests below:

```rust
    use gg_ipc::{Reactive, ReactiveMode};

    const BLUE: gg_ipc::Rgb = gg_ipc::Rgb { r: 0, g: 0, b: 255 };

    fn reactive(mode: ReactiveMode, duration_s: f32) -> Lighting {
        let r = Reactive { enabled: true, mode, color: gg_ipc::Rgb { r: 255, g: 255, b: 255 }, duration_s };
        Lighting { reactive: r, ..lit(Effect::Static) }
    }

    fn dist(p: &HashMap<u8, Pos>, a: u8, b: u8) -> f32 {
        ((p[&a].x - p[&b].x).powi(2) + (p[&a].y - p[&b].y).powi(2)).sqrt()
    }

    #[test]
    fn fade_lights_the_pressed_key_and_fades_back() {
        let l = reactive(ReactiveMode::Fade, 0.6);
        let p = positions();
        let hits = [Hit { code: 0x04, t: 1.0 }];
        assert_eq!(color(&l, &p, 0x04, 1.0, &hits), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(color(&l, &p, 0x04, 1.3, &hits), Rgb { r: 128, g: 128, b: 128 });
        assert_eq!(color(&l, &p, 0x04, 1.6, &hits), Rgb::default(), "gone after the duration");
        assert_eq!(color(&l, &p, 0x05, 1.0, &hits), Rgb::default(), "other keys untouched");
    }

    #[test]
    fn ripple_starts_on_the_pressed_key_and_reaches_far_keys_later() {
        let l = reactive(ReactiveMode::Ripple, 1.0);
        let p = positions();
        let hits = [Hit { code: 0x29, t: 0.0 }]; // Esc
        let w = |code, t| reactive_weight(&l.reactive, &p, code, t, &hits);
        assert_eq!(w(0x29, 0.0), 1.0);
        assert_eq!(w(0x3A, 0.0), 0.0, "F1 is dark at the moment of the press");
        let far = dist(&p, 0x29, 0x48) / RIPPLE_SPEED; // Pause
        assert!(far > 0.5, "Pause is far from Esc");
        assert_eq!(w(0x48, 0.1), 0.0, "not reached yet");
        assert!(w(0x48, far) > 0.1, "reached later");
        assert_eq!(w(0x48, 1.0), 0.0, "gone after the duration");
    }

    #[test]
    fn the_strongest_press_wins_and_nothing_overflows() {
        let l = reactive(ReactiveMode::Fade, 0.6);
        let p = positions();
        let hits = [Hit { code: 0x04, t: 0.0 }, Hit { code: 0x04, t: 0.3 }];
        assert_eq!(reactive_weight(&l.reactive, &p, 0x04, 0.3, &hits), 1.0);
        assert_eq!(color(&l, &p, 0x04, 0.3, &hits), Rgb { r: 255, g: 255, b: 255 });
    }

    #[test]
    fn presses_blend_over_the_base_then_brightness_applies() {
        let mut l = reactive(ReactiveMode::Fade, 1.0);
        l.keys = vec![KeyColor { code: 0x04, color: RED }];
        l.reactive.color = BLUE;
        let p = positions();
        let hits = [Hit { code: 0x04, t: 0.0 }];
        assert_eq!(color(&l, &p, 0x04, 0.5, &hits), Rgb { r: 128, g: 0, b: 128 });
        l.brightness = 50;
        assert_eq!(color(&l, &p, 0x04, 0.5, &hits), Rgb { r: 64, g: 0, b: 64 });
    }

    #[test]
    fn reactive_off_ignores_presses_and_unknown_keys_are_harmless() {
        let mut l = reactive(ReactiveMode::Ripple, 1.0);
        let p = positions();
        let unknown = [Hit { code: 0x7F, t: 0.0 }]; // not on the TKL
        assert_eq!(color(&l, &p, 0x29, 0.0, &unknown), Rgb::default());
        l.reactive.enabled = false;
        assert_eq!(color(&l, &p, 0x29, 0.0, &[Hit { code: 0x29, t: 0.0 }]), Rgb::default());
    }

    #[test]
    fn a_bad_duration_lights_nothing() {
        let p = positions();
        let hits = [Hit { code: 0x04, t: 0.0 }];
        for d in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            for mode in [ReactiveMode::Fade, ReactiveMode::Ripple] {
                let l = reactive(mode, d);
                assert_eq!(reactive_weight(&l.reactive, &p, 0x04, 0.0, &hits), 0.0, "{mode:?} {d}");
                assert_eq!(color(&l, &p, 0x04, 0.0, &hits), Rgb::default());
            }
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p gglinuxd effects`
Expected: FAIL to compile: `cannot find type Pos` / `Hit`, `reactive_weight`, `RIPPLE_SPEED`, and
`color` takes 4 arguments.

- [ ] **Step 3: Implement**

In `effects.rs`, replace `positions` and `color` and add the rest. Keep `animated`, `hsv`,
`rgb` and `scale` as they are:

```rust
use gg_ipc::{Effect, Lighting, Reactive, ReactiveMode};

/// How fast a ripple's ring spreads, in key units per second.
pub const RIPPLE_SPEED: f32 = 20.0;
/// Half the ring's width, in key units.
const RING: f32 = 1.0;

/// A key's centre, in key units from the top-left of the board.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
}

/// A key press, `t` seconds into the effect. No `Debug`: which keys someone
/// presses is never printed.
#[derive(Clone, Copy)]
pub struct Hit {
    pub code: u8,
    pub t: f32,
}

/// The centre of every key on the board.
pub fn positions() -> HashMap<u8, Pos> {
    ANSI_TKL.iter().map(|&(code, _, x, y, w)| (code, Pos { x: x + w / 2.0, y: y + 0.5 })).collect()
}

/// The colour of the LED with HID code `code`, `t` seconds into the effect,
/// with recent key presses drawn over it.
pub fn color(lighting: &Lighting, positions: &HashMap<u8, Pos>, code: u8, t: f32, hits: &[Hit]) -> Rgb {
    let base = match lighting.effect {
        Effect::Static => lighting
            .keys
            .iter()
            .find(|k| k.code == code)
            .map(|k| rgb(k.color))
            .unwrap_or_default(),
        Effect::Breathing { color, period_s } => scale(rgb(color), (1.0 - (TAU * t / period_s).cos()) / 2.0),
        Effect::Wave { period_s } => {
            let x = positions.get(&code).map_or(0.0, |p| p.x / WIDTH);
            hsv(x - t / period_s)
        }
        Effect::Spectrum { period_s } => hsv(t / period_s),
    };
    let r = &lighting.reactive;
    let w = if r.enabled { reactive_weight(r, positions, code, t, hits) } else { 0.0 };
    scale(mix(base, rgb(r.color), w), f32::from(lighting.brightness) / 100.0)
}

/// How strongly key `code` shows the reactive colour at `t`: the strongest
/// live press, 0 (none) to 1 (full).
pub fn reactive_weight(r: &Reactive, positions: &HashMap<u8, Pos>, code: u8, t: f32, hits: &[Hit]) -> f32 {
    let duration = r.duration_s;
    if !(duration.is_finite() && duration > 0.0) {
        return 0.0; // a hand-edited file; never divide by it
    }
    let here = positions.get(&code);
    hits.iter()
        .map(|h| {
            let age = t - h.t;
            if !(0.0..duration).contains(&age) {
                return 0.0;
            }
            let fade = 1.0 - age / duration;
            match r.mode {
                ReactiveMode::Fade => if h.code == code { fade } else { 0.0 },
                ReactiveMode::Ripple => {
                    let (Some(a), Some(b)) = (here, positions.get(&h.code)) else { return 0.0 };
                    let d = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
                    fade * (1.0 - (d - RIPPLE_SPEED * age).abs() / RING).max(0.0)
                }
            }
        })
        .fold(0.0, f32::max)
}

/// `a` moved toward `b` by `w` (0..=1).
fn mix(a: Rgb, b: Rgb, w: f32) -> Rgb {
    let w = w.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (f32::from(x) * (1.0 - w) + f32::from(y) * w).round() as u8;
    Rgb { r: f(a.r, b.r), g: f(a.g, b.g), b: f(a.b, b.b) }
}
```

In `crates/gglinuxd/src/keyboard.rs`:
- `State.positions: HashMap<u8, f32>` becomes `HashMap<u8, effects::Pos>`.
- In `render`, the call becomes
  `effects::color(&state.lighting, &state.positions, code, t, &[])`. Task 6 passes the real hits.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p gglinuxd effects && cargo clippy -p gglinuxd --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: all `effects` tests PASS (the six old ones and the six new ones); clippy count `0`.

- [ ] **Step 5: Commit**

```bash
git add crates/gglinuxd/src/keyboard/effects.rs crates/gglinuxd/src/keyboard.rs
git commit -m "gglinuxd: fade and ripple key presses over the lighting effect

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: The key-press reader

**Files:**
- Create: `crates/gglinuxd/src/keyboard/reader.rs`
- Modify: `crates/gglinuxd/src/keyboard.rs` (`mod reader;`; two `Command` variants; manual `Debug`)
- Modify: `crates/gglinuxd/Cargo.toml` (add `libc.workspace = true`)

**Interfaces:**
- Consumes: `gg_device_keyboard::keys::{boot_keys, nkro_keys, pressed, KeySet}` (Task 2);
  `gg_device_keyboard::{VENDOR_ID, PRODUCT_ID}`.
- Produces:
  - new `Command` variants: `Pressed { code: u8, at: Instant }` and `ReaderStopped` (the reader
    ended on its own: device gone or read error);
  - `impl Debug for Command` that prints only the variant name;
  - in `keyboard::reader`:
    - `pub enum OpenError { NoAccess, Unavailable(String) }`;
    - `pub struct Reader` (stops and joins on `Drop`);
    - `Reader::start(api: &HidApi, tx: Sender<Command>) -> Result<Reader, OpenError>`.

- [ ] **Step 1: Add the command variants and the Debug test**

In `crates/gglinuxd/src/keyboard.rs`, add to `enum Command` (before `Shutdown`):

```rust
    /// A key went down on the Apex, as the reader saw it. Never logged.
    Pressed { code: u8, at: Instant },
    /// The reader ended by itself (keyboard gone, read error); it retries later.
    ReaderStopped,
```

After the enum:

```rust
/// Names only: a `Pressed` never prints its key.
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Command::GetState(_) => "GetState",
            Command::SetLighting { .. } => "SetLighting",
            Command::Pressed { .. } => "Pressed",
            Command::ReaderStopped => "ReaderStopped",
            Command::Shutdown(_) => "Shutdown",
        })
    }
}
```

So `handle` compiles, add two arms to its `match` (Task 6 gives them behaviour):

```rust
        Command::Pressed { .. } | Command::ReaderStopped => {}
```

Add `mod reader;` under `mod effects;`, and `libc.workspace = true` to `crates/gglinuxd/Cargo.toml`
`[dependencies]`.

- [ ] **Step 2: Write the failing reader tests**

`crates/gglinuxd/src/keyboard/reader.rs`, with the test module and stubs:

```rust
//! Reads key presses from the Apex's typing interfaces, for reactive lighting.

use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;

use hidapi::HidApi;

use super::Command;

#[derive(Debug)]
pub enum OpenError {
    NoAccess,
    Unavailable(String),
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Boot,
    Nkro,
}

pub struct Reader {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Reader {
    pub fn start(_api: &HidApi, _tx: Sender<Command>) -> Result<Reader, OpenError> {
        Err(OpenError::Unavailable("not written yet".into()))
    }

    fn spawn(_files: Vec<(File, Kind)>, _tx: Sender<Command>) -> Reader {
        Reader { stop: Arc::new(AtomicBool::new(false)), thread: None }
    }
}

fn open_all(_paths: &[(PathBuf, Kind)]) -> Result<Vec<(File, Kind)>, OpenError> {
    Err(OpenError::Unavailable("not written yet".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
    use std::time::{Duration, Instant};

    /// A connected pair of packet sockets: one write is one read, like a hidraw report.
    fn packet_pair() -> (File, File) {
        let mut fds = [0; 2];
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "socketpair");
        unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
    }

    fn next(rx: &Receiver<Command>) -> Option<Command> {
        match rx.recv_timeout(Duration::from_millis(600)) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => panic!("reader dropped its sender"),
        }
    }

    fn code(c: Option<Command>) -> u8 {
        match c {
            Some(Command::Pressed { code, .. }) => code,
            other => panic!("expected a press, got {other:?}"),
        }
    }

    #[test]
    fn held_keys_and_rollover_errors_do_not_re_fire() {
        let (ours, mut kb) = packet_pair();
        let (tx, rx) = mpsc::channel();
        let _reader = Reader::spawn(vec![(ours, Kind::Boot)], tx);
        kb.write_all(&[0, 0, 0x04, 0, 0, 0, 0, 0]).unwrap(); // A down
        assert_eq!(code(next(&rx)), 0x04);
        kb.write_all(&[0, 0, 0x04, 0x05, 0, 0, 0, 0]).unwrap(); // B down, A held
        assert_eq!(code(next(&rx)), 0x05);
        kb.write_all(&[0, 0, 1, 1, 1, 1, 1, 1]).unwrap(); // rollover error
        kb.write_all(&[0, 0, 0x04, 0x05, 0x06, 0, 0, 0]).unwrap(); // C down
        assert_eq!(code(next(&rx)), 0x06, "only C: the rollover error changed nothing");
        kb.write_all(&[0; 8]).unwrap(); // all up
        assert!(next(&rx).is_none(), "releases are not presses");
    }

    #[test]
    fn nkro_reports_are_read_from_their_own_interface() {
        let (boot, _kb0) = packet_pair();
        let (nkro, mut kb2) = packet_pair();
        let (tx, rx) = mpsc::channel();
        let _reader = Reader::spawn(vec![(boot, Kind::Boot), (nkro, Kind::Nkro)], tx);
        let mut report = [0u8; 32];
        report[0x2C / 8] |= 1 << (0x2C % 8); // Space
        kb2.write_all(&report).unwrap();
        assert_eq!(code(next(&rx)), 0x2C);
    }

    #[test]
    fn peer_closing_stops_the_reader_and_reports_it() {
        let (ours, kb) = packet_pair();
        let (tx, rx) = mpsc::channel();
        let reader = Reader::spawn(vec![(ours, Kind::Boot)], tx);
        drop(kb); // the keyboard went away
        assert!(matches!(next(&rx), Some(Command::ReaderStopped)));
        let started = Instant::now();
        drop(reader);
        assert!(started.elapsed() < Duration::from_secs(1), "the thread had already ended");
    }

    #[test]
    fn dropping_the_reader_stops_it_quietly() {
        let (ours, _kb) = packet_pair();
        let (tx, rx) = mpsc::channel();
        let reader = Reader::spawn(vec![(ours, Kind::Boot)], tx);
        let started = Instant::now();
        drop(reader);
        assert!(started.elapsed() < Duration::from_secs(1), "stops within a poll interval");
        assert!(matches!(rx.recv_timeout(Duration::from_millis(100)), Err(RecvTimeoutError::Disconnected)),
            "no ReaderStopped for a stop we asked for");
    }

    #[test]
    fn one_denied_node_means_no_access_and_nothing_stays_open() {
        if unsafe { libc::geteuid() } == 0 {
            return; // root reads anything; the check means nothing
        }
        let dir = std::env::temp_dir().join(format!("gglinux-reader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let open = dir.join("iface0");
        let denied = dir.join("iface2");
        std::fs::write(&open, b"").unwrap();
        std::fs::write(&denied, b"").unwrap();
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = open_all(&[(open, Kind::Boot), (denied, Kind::Nkro)]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(result, Err(OpenError::NoAccess)), "got {result:?}");
    }

    #[test]
    fn a_press_never_prints_its_key() {
        let c = Command::Pressed { code: 0x04, at: Instant::now() };
        assert_eq!(format!("{c:?}"), "Pressed");
    }
}
```

`{result:?}` needs `Debug` on `(File, Kind)`: `File` has it, and `Kind` derives it (it prints only
`Boot`/`Nkro`, never a key).

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p gglinuxd reader`
Expected: FAIL. The spawn-based tests time out waiting for presses (`expected a press, got None`)
or never see `ReaderStopped`, and `one_denied_node…` gets `Unavailable`. `a_press_never_prints_its_key`
and `dropping_the_reader_stops_it_quietly` pass; that's fine, since the stub spawns no thread.

- [ ] **Step 4: Implement**

Replace the stubs in `reader.rs` (keep the test module):

```rust
//! Reads key presses from the Apex's typing interfaces, for reactive lighting.
//!
//! Only while reactive lighting is on: the worker starts a [`Reader`] then,
//! and dropping it closes both nodes. They're opened read-only; nothing is
//! ever written to them. Which keys are pressed is private — the codes go to
//! the worker and nowhere else, and nothing here logs them.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use gg_device_keyboard::keys::{self, KeySet};
use gg_device_keyboard::{PRODUCT_ID, VENDOR_ID};
use hidapi::HidApi;

use super::Command;

/// How long one poll waits before checking whether to stop.
const POLL_MS: i32 = 200;
/// The boot keyboard interface and the N-key-rollover one.
const TYPING_INTERFACES: [(i32, Kind); 2] = [(0, Kind::Boot), (2, Kind::Nkro)];

#[derive(Debug)]
pub enum OpenError {
    /// Permission denied: the optional udev rule isn't installed.
    NoAccess,
    /// Anything else (not plugged in, node missing); worth retrying.
    Unavailable(String),
}

/// Which report an interface sends.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Boot,
    Nkro,
}

pub struct Reader {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Reader {
    /// Open the Apex's typing interfaces and start reading them.
    pub fn start(api: &HidApi, tx: Sender<Command>) -> Result<Reader, OpenError> {
        let files = open_all(&typing_paths(api)?)?;
        Ok(Self::spawn(files, tx))
    }

    fn spawn(files: Vec<(File, Kind)>, tx: Sender<Command>) -> Reader {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("gg-keys".into())
            .spawn(move || {
                if !run(&files, &flag, &tx) {
                    let _ = tx.send(Command::ReaderStopped);
                }
            })
            .expect("spawn gg-keys thread");
        Reader { stop, thread: Some(thread) }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The hidraw paths of both typing interfaces, or why not.
fn typing_paths(api: &HidApi) -> Result<Vec<(PathBuf, Kind)>, OpenError> {
    TYPING_INTERFACES
        .iter()
        .map(|&(iface, kind)| {
            api.device_list()
                .find(|d| d.vendor_id() == VENDOR_ID && d.product_id() == PRODUCT_ID && d.interface_number() == iface)
                .and_then(|d| d.path().to_str().ok().map(PathBuf::from))
                .map(|p| (p, kind))
                .ok_or_else(|| OpenError::Unavailable(format!("typing interface {iface} not found")))
        })
        .collect()
}

/// Open every path read-only, or none: one denied node means no access at all.
fn open_all(paths: &[(PathBuf, Kind)]) -> Result<Vec<(File, Kind)>, OpenError> {
    paths
        .iter()
        .map(|(path, kind)| {
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)
                .map(|f| (f, *kind))
                .map_err(|e| match e.kind() {
                    io::ErrorKind::PermissionDenied => OpenError::NoAccess,
                    _ => OpenError::Unavailable(format!("{}: {e}", path.display())),
                })
        })
        .collect() // on the first error the files opened so far are dropped, so closed
}

/// Read until told to stop (`true`) or the device fails (`false`).
fn run(files: &[(File, Kind)], stop: &AtomicBool, tx: &Sender<Command>) -> bool {
    let mut last = vec![KeySet::default(); files.len()];
    let mut buf = [0u8; 64];
    while !stop.load(Ordering::Relaxed) {
        let mut fds: Vec<libc::pollfd> = files
            .iter()
            .map(|(f, _)| libc::pollfd { fd: f.as_raw_fd(), events: libc::POLLIN, revents: 0 })
            .collect();
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, POLL_MS) };
        if n < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        for (i, p) in fds.iter().enumerate() {
            if p.revents & libc::POLLIN != 0 {
                let len = match (&files[i].0).read(&mut buf) {
                    Ok(0) => return false, // end of file: the device is gone
                    Ok(len) => len,
                    Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => continue,
                    Err(_) => return false,
                };
                let now = match files[i].1 {
                    Kind::Boot => keys::boot_keys(&buf[..len]),
                    Kind::Nkro => keys::nkro_keys(&buf[..len]),
                };
                let Some(now) = now else { continue }; // rollover error or odd length: no change
                let at = Instant::now();
                for code in keys::pressed(last[i], now) {
                    if tx.send(Command::Pressed { code, at }).is_err() {
                        return true; // the worker is gone; nobody to tell
                    }
                }
                last[i] = now;
            } else if p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                return false;
            }
        }
    }
    true
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p gglinuxd reader && cargo clippy -p gglinuxd --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: all six `reader` tests PASS; clippy count `0`. `reader` and `Reader::start` aren't used
by the worker yet, so if clippy/rustc reports `dead_code` for `Reader::start`, `OpenError`
variants or `typing_paths`, add `#[allow(dead_code)] // wired up in the next task` on
`mod reader;` and remove it in Task 6.

- [ ] **Step 6: Commit**

```bash
git add crates/gglinuxd/src/keyboard/reader.rs crates/gglinuxd/src/keyboard.rs crates/gglinuxd/Cargo.toml Cargo.lock
git commit -m "gglinuxd: read key presses from the Apex's typing interfaces

Read-only, and never logged: presses go to the keyboard worker as
Command::Pressed, whose Debug prints no key.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Wire reactive into the keyboard worker

**Files:**
- Modify: `crates/gglinuxd/src/keyboard.rs`

**Interfaces:**
- Consumes: `reader::{Reader, OpenError}` and `Command::{Pressed, ReaderStopped}` (Task 5);
  `effects::{Hit, color}` (Task 4); `gg_ipc::ReactiveStatus` (Task 3).
- Produces: `KeyboardState.reactive` carries the live status; pure helpers
  `fn wants_reader(lighting: &Lighting, connected: bool) -> bool`,
  `fn push_hit(hits: &mut Vec<Hit>, hit: Hit)` (keeps at most `MAX_HITS = 32`, dropping the
  oldest), and `fn interval(lighting: &Lighting, live_hits: bool) -> Duration`.

Design note: the worker keeps a clone of its own `Sender<Command>` to hand to each `Reader`. That
means the `RecvTimeoutError::Disconnected` branch can no longer fire, so the worker stops only on
`Command::Shutdown`, which `main.rs` already sends on exit (`shutdown_devices`). The branch stays
as a harmless fallback. Leave a comment saying so.

- [ ] **Step 1: Write the failing tests**

Add at the end of `crates/gglinuxd/src/keyboard.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::{Effect, Reactive};

    fn lighting(enabled: bool, reactive: bool, effect: Effect) -> Lighting {
        Lighting { enabled, effect, reactive: Reactive { enabled: reactive, ..Reactive::default() }, ..Lighting::default() }
    }

    #[test]
    fn the_reader_runs_only_with_lighting_and_reactive_on_and_the_keyboard_here() {
        assert!(wants_reader(&lighting(true, true, Effect::Static), true));
        assert!(!wants_reader(&lighting(false, true, Effect::Static), true), "lighting off");
        assert!(!wants_reader(&lighting(true, false, Effect::Static), true), "reactive off");
        assert!(!wants_reader(&lighting(true, true, Effect::Static), false), "keyboard away");
    }

    #[test]
    fn at_most_32_presses_are_kept_oldest_dropped_first() {
        let mut hits = Vec::new();
        for i in 0..40u8 {
            push_hit(&mut hits, effects::Hit { code: i, t: f32::from(i) });
        }
        assert_eq!(hits.len(), MAX_HITS);
        assert_eq!(hits[0].code, 8, "the eight oldest went");
        assert_eq!(hits[MAX_HITS - 1].code, 39);
    }

    #[test]
    fn live_presses_animate_even_a_static_effect() {
        let still = lighting(true, true, Effect::Static);
        assert_eq!(interval(&still, false), STATIC_REFRESH);
        assert_eq!(interval(&still, true), FRAME);
        assert_eq!(interval(&lighting(true, true, Effect::Wave { period_s: 4.0 }), false), FRAME);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p gglinuxd keyboard::tests`
Expected: FAIL to compile: `cannot find function wants_reader` / `push_hit`, `MAX_HITS`, and
`interval` takes 1 argument.

- [ ] **Step 3: Implement**

In `crates/gglinuxd/src/keyboard.rs`:

1. Imports: add `use gg_ipc::ReactiveStatus;` and `use reader::{OpenError, Reader};`. Remove any
   `#[allow(dead_code)]` put on `mod reader;` in Task 5.

2. Constants, after `IDLE_WAIT`:

```rust
/// The most key presses held for reactive lighting; older ones are dropped first.
const MAX_HITS: usize = 32;
```

3. `spawn`: hand the worker a sender for its readers:

```rust
pub fn spawn(event_tx: broadcast::Sender<Event>) -> Handle {
    let (tx, rx) = mpsc::channel();
    let readers_tx = tx.clone();
    std::thread::Builder::new()
        .name("gg-keyboard".into())
        .spawn(move || worker(rx, readers_tx, event_tx))
        .expect("spawn gg-keyboard thread");
    Handle { tx }
}
```

4. `State` gains, after `published`:

```rust
    /// Hands key presses to this worker; cloned into each reader.
    tx: Sender<Command>,
    /// Reading key presses, while reactive lighting wants them.
    reader: Option<Reader>,
    last_reader_attempt: Option<Instant>,
    reactive: ReactiveStatus,
    /// Recent presses, newest last; never logged, never saved.
    hits: Vec<effects::Hit>,
```

`worker(rx: Receiver<Command>, tx: Sender<Command>, event_tx: …)` initialises them:
`tx, reader: None, last_reader_attempt: None, reactive: ReactiveStatus::Off, hits: Vec::new()`.
Replace the `Err(RecvTimeoutError::Disconnected)` arm's body with the same code plus the comment:

```rust
            // Unreachable while `state.tx` lives — the worker stops on Shutdown,
            // which main sends on exit. Kept as a fallback.
            Err(RecvTimeoutError::Disconnected) => {
                hand_back(&mut state);
                return;
            }
```

The loop body after `try_open` becomes:

```rust
        if state.device.is_none() {
            try_open(&mut state);
        }
        sync_reader(&mut state);
        render(&mut state);
        publish(&mut state, &event_tx);
```

5. `interval` and `next_wait`:

```rust
/// How often frames are due: fast for animated effects and while presses show.
fn interval(lighting: &Lighting, live_hits: bool) -> Duration {
    if effects::animated(&lighting.effect) || live_hits {
        FRAME
    } else {
        STATIC_REFRESH
    }
}
```

In `next_wait`, `interval(&state.lighting)` becomes `interval(&state.lighting, !state.hits.is_empty())`.
In `render`, likewise.

6. Helpers, after `next_wait`:

```rust
/// Whether key presses should be read right now.
fn wants_reader(lighting: &Lighting, connected: bool) -> bool {
    lighting.enabled && lighting.reactive.enabled && connected
}

/// Keep a press, dropping the oldest past [`MAX_HITS`].
fn push_hit(hits: &mut Vec<effects::Hit>, hit: effects::Hit) {
    if hits.len() == MAX_HITS {
        hits.remove(0);
    }
    hits.push(hit);
}

/// Start or stop the key reader to match the settings.
fn sync_reader(state: &mut State) {
    if !wants_reader(&state.lighting, state.device.is_some()) {
        if state.reader.take().is_some() {
            debug!("stopped reading key presses");
        }
        state.hits.clear();
        state.reactive = ReactiveStatus::Off;
        return;
    }
    if state.reader.is_some() || !is_older_than(state.last_reader_attempt, REOPEN_INTERVAL) {
        return;
    }
    state.last_reader_attempt = Some(Instant::now());
    match Reader::start(&state.api, state.tx.clone()) {
        Ok(reader) => {
            info!("reading key presses for reactive lighting");
            state.reader = Some(reader);
            state.reactive = ReactiveStatus::Reading;
        }
        Err(OpenError::NoAccess) => {
            if state.reactive != ReactiveStatus::NoAccess {
                info!("reactive lighting needs the optional udev rule 61-gglinux-reactive.rules");
            }
            state.reactive = ReactiveStatus::NoAccess;
        }
        Err(OpenError::Unavailable(why)) => {
            debug!(%why, "keyboard typing interfaces not available");
            state.reactive = ReactiveStatus::Off;
        }
    }
}
```

7. In `handle`, replace the placeholder arm from Task 5:

```rust
        Command::Pressed { code, at } => {
            if state.reader.is_some() {
                let t = at.saturating_duration_since(state.started).as_secs_f32();
                push_hit(&mut state.hits, effects::Hit { code, t });
                state.last_frame = None; // show it now
            }
        }
        Command::ReaderStopped => {
            state.reader = None;
            state.reactive = ReactiveStatus::Off;
            state.last_reader_attempt = Some(Instant::now()); // retry after REOPEN_INTERVAL
        }
```

8. In `render`, after `let t = …;`, drop expired presses and pass the rest. The existing guard
   already uses the `interval` from step 5:

```rust
    let t = state.started.elapsed().as_secs_f32();
    let duration = state.lighting.reactive.duration_s;
    state.hits.retain(|h| t - h.t < duration);
    let hits = &state.hits;
```

The frame call becomes
`device.show(|code| effects::color(&state.lighting, &state.positions, code, t, hits))`.
If `retain` runs where `device` already borrows `state`, move the `retain` above
`let Some(device) = …`.

9. `snapshot`: `reactive: state.reactive,` replaces `ReactiveStatus::Off`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --all-targets 2>&1 | grep -E "^test result|FAILED|panicked"; cargo clippy --workspace --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: every `test result: ok`, no `FAILED`/`panicked`; clippy count `0`.

Also check the privacy invariant by grep:
Run: `grep -nE "(debug|info|warn|error|trace)!\(.*code" crates/gglinuxd/src/keyboard.rs crates/gglinuxd/src/keyboard/*.rs`
Expected: no output. No log line mentions a key code.

- [ ] **Step 5: Commit**

```bash
git add crates/gglinuxd/src/keyboard.rs
git commit -m "gglinuxd: reactive lighting in the keyboard worker

The reader runs only while lighting and reactive are both on and the
keyboard is here; presses are drawn for their duration and then dropped.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Reactive in the UI; the fake daemon; progress notes

**Files:**
- Modify: `ui/src/lib/types.ts` (the lighting types, lines ~193–225)
- Modify: `ui/src/lib/KeyboardPanel.svelte`
- Modify: `crates/gglinuxd/examples/fakemouse.rs` (the `SetLighting` arm, ~line 265)
- Modify: `PLAN.md` (the M4 progress paragraph)

**Interfaces:**
- Consumes: the JSON from Task 3: `lighting.reactive = { enabled, mode: "fade"|"ripple", color, duration_s }`,
  `state.reactive = "off"|"reading"|"no_access"`; the warning sentence from Task 1.

- [ ] **Step 1: Types (the failing check)**

In `ui/src/lib/types.ts`, after `KeyColor`:

```ts
export type ReactiveMode = "fade" | "ripple";

export interface Reactive {
  enabled: boolean;
  mode: ReactiveMode;
  color: Rgb;
  /** seconds one press shows, 0.1..=3 */
  duration_s: number;
}
```

Add `reactive: Reactive;` to `Lighting`, and to `KeyboardState` add:

```ts
  /** Whether the daemon is reading key presses for reactive lighting. */
  reactive: "off" | "reading" | "no_access";
```

Run: `cd ui && npm run check 2>&1 | tail -3`
Expected: `0 ERRORS` (the panel spreads `lighting`, so the new field flows through). This step
sets the types that Step 2 consumes; it's a compile gate, not a failing test.

- [ ] **Step 2: The Reactive section**

In `KeyboardPanel.svelte` `<script>`, import `ReactiveMode` and `Reactive` from `./types`, and add:

```ts
  const MODES: { mode: ReactiveMode; label: string }[] = [
    { mode: "fade", label: "Fade" },
    { mode: "ripple", label: "Ripple" },
  ];
  const STATUS = { off: "waiting for the keyboard", reading: "on", no_access: "no access" } as const;
  /** Same words as the README and 61-gglinux-reactive.rules. */
  const ACCESS_WARNING =
    "Reactive lighting needs to see which keys you press. The optional udev rule lets any program " +
    "you run read what you type on the Apex while you're logged in. gglinux reads only while " +
    "reactive lighting is on, and keeps presses in memory only until their light fades.";

  function setReactive(patch: Partial<Reactive>) {
    if (!lighting) return;
    void apply({ reactive: { ...lighting.reactive, ...patch } });
  }
```

In the markup, inside `{#if lighting.enabled}` and after the Effect `</section>` (before
`{#if lighting.effect.kind === "static"}`):

```svelte
      <section>
        <div class="head">
          <h4>Reactive</h4>
          {#if lighting.reactive.enabled}<span class="unit">{STATUS[kb.reactive]}</span>{/if}
        </div>
        <label class="toggle">
          <input
            type="checkbox"
            checked={lighting.reactive.enabled}
            disabled={busy}
            onchange={(e) => setReactive({ enabled: e.currentTarget.checked })}
          />
          Light keys as you press them
        </label>
        {#if lighting.reactive.enabled}
          <div class="row">
            <div class="segmented">
              {#each MODES as m (m.mode)}
                <button class:on={lighting.reactive.mode === m.mode} disabled={busy} onclick={() => setReactive({ mode: m.mode })}>
                  {m.label}
                </button>
              {/each}
            </div>
          </div>
          <div class="row">
            <span class="label">Colour</span>
            <input
              type="color"
              value={hex(lighting.reactive.color)}
              onchange={(e) => setReactive({ color: rgbOf(e.currentTarget.value) })}
            />
          </div>
          <div class="row">
            <span class="label">Duration</span>
            <input
              type="range"
              min="0.1"
              max="3"
              step="0.1"
              value={lighting.reactive.duration_s}
              onchange={(e) => setReactive({ duration_s: e.currentTarget.valueAsNumber })}
            />
            <span class="unit">{lighting.reactive.duration_s.toFixed(1)} s</span>
          </div>
          {#if kb.reactive === "no_access"}
            <div class="notice">
              <p>{ACCESS_WARNING}</p>
              <p>To allow it: <code>./scripts/install-local.sh --no-build --reactive</code></p>
            </div>
          {/if}
        {/if}
      </section>
```

In `<style>`:

```css
  .notice { margin-top: 0.8rem; padding: 0.55rem 0.75rem; font-size: 0.8rem; color: var(--text-dim); background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; }
  .notice p { margin: 0; }
  .notice p + p { margin-top: 0.45rem; }
  .notice code { font-size: 0.78rem; color: var(--text); }
```

Run: `cd ui && npm run check 2>&1 | tail -3`
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: Let the fake daemon show `no_access`**

In `crates/gglinuxd/examples/fakemouse.rs`, the `SetLighting` arm becomes:

```rust
        Request::SetLighting { lighting } => {
            // Like a machine without the optional udev rule, so the UI's hint shows.
            s.keyboard.reactive = if lighting.enabled && lighting.reactive.enabled {
                ReactiveStatus::NoAccess
            } else {
                ReactiveStatus::Off
            };
            s.keyboard.lighting = lighting;
            ResponseData::Success
        }
```

Run: `cargo build -p gglinuxd --examples 2>&1 | tail -1 && cargo test --workspace --all-targets 2>&1 | grep -E "FAILED|panicked" ; echo done`
Expected: `Finished …` and then `done` with no `FAILED`/`panicked` line.

- [ ] **Step 4: PLAN.md progress**

In `PLAN.md`, replace the sentence "Reactive lighting waits on an input-access decision." (end of
the M4 progress block) with:

```markdown
**Reactive lighting done in software** (2026-09-28; spec:
`docs/superpowers/specs/2026-09-28-reactive-lighting-design.md`): Fade or Ripple over any effect,
from key presses read on the Apex's typing interfaces. The same change narrowed the udev rule,
which had opened every SteelSeries interface (so any program could read Apex keystrokes). Access
to the typing interfaces is now opt-in via `61-gglinux-reactive.rules`. Not yet verified on
hardware: the owner reinstalls the rules, confirms with `getfacl` that the hole is closed, and
tries both modes.
```

- [ ] **Step 5: Commit**

```bash
git add ui/src/lib/types.ts ui/src/lib/KeyboardPanel.svelte crates/gglinuxd/examples/fakemouse.rs PLAN.md
git commit -m "UI: reactive lighting controls, with the access warning

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Hardware acceptance (the owner, after the branch is reviewed)

1. `./scripts/install-local.sh --no-build`, then `getfacl -p /dev/hidraw6 /dev/hidraw8` shows no
   `user:` line, and `./scripts/check-udev.sh` prints all `ok`.
2. Restart the daemon. Lighting still works (wave); the headset screen still blanks.
3. Turn reactive on: the panel shows "no access" with the warning.
4. `./scripts/install-local.sh --no-build --reactive`; within about 2 s the panel shows "on".
5. Fade, then Ripple, over a wave: presses light up at once; typing is unaffected.
6. Turn reactive off; `lsof /dev/hidraw6 /dev/hidraw8` shows gglinuxd holds neither.
7. Record `61-gglinux-reactive.rules` (if kept) in the list of files gglinux installs.
