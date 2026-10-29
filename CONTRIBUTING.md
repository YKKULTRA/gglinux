# Contributing to gglinux

Thanks for looking. gglinux is a small project with one maintainer and one set of devices, so the
most useful contributions are reports from real hardware, protocol captures, and focused fixes.

## What helps most

1. **Testing on hardware.** Most features have never run on a real device (see the matrix in the
   [README](README.md#feature-matrix)). If you own an Apex Pro TKL Gen 3 or an Arctis Nova Pro
   Wireless, trying a feature and reporting what happened is worth a lot. Include your
   distribution, kernel, PipeWire version, `ggctl` output and the relevant part of
   `journalctl --user -u gglinuxd`.
2. **USB captures of SteelSeries GG on Windows.** The Apex OLED and the actuation / Rapid Trigger
   settings are blocked because nobody has published their protocol. A Wireshark/USBPcap capture
   of GG changing one setting at a time on an Apex Pro TKL Gen 3 (`1038:1642`) would unblock both.
   Say which setting you changed, from what to what, and when.
3. **Bug fixes with a test.** See below.

Support for other devices is welcome in principle, but every device has its own protocol and needs
someone who owns it to test it. Please open an issue to discuss it before writing code.

## Building and checking

Install the build dependencies listed in the [README](README.md#requirements), then:

```sh
# The daemon, ggctl and the desktop app: one cargo workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

These are the same checks CI runs ([.github/workflows/ci.yml](.github/workflows/ci.yml)). Please run
them before opening a pull request.

The code is formatted with stock `rustfmt`. One commit reformatted the whole tree; to keep
`git blame` useful, run once:

```sh
git config blame.ignoreRevsFile .git-blame-ignore-revs
```

## Running the app while developing

```sh
cargo run -p gglinux
```

The app is `crates/gglinux-ui`: the window is Slint markup in `ui/*.slint` (one file per page,
`widgets.slint` for the shared pieces), and the Rust in `src/` fills each page's Slint global from
the daemon's state and answers its callbacks with requests. Where a panel keeps a working copy (a
form you edit before applying), the copy is Rust, so it's tested as Rust.

A debug build answers to its own D-Bus name, so it runs beside an installed gglinux instead of
handing over to it, and you may see two tray icons.

One Slint behaviour to know about: inside the scrolling page, a `Text` with `wrap: word-wrap` in an
`if` or `for` is measured at the wrong width, and the card around it comes out shorter than its
content. Put wrapped text in a `Shown { }` (in `widgets.slint`) instead of an `if`.

### Pictures of the app without a screen

`shot` draws the window to a PNG with Slint's software renderer. Pointed at the fake daemon below,
it shows every page with made-up devices, from a machine with no display:

```sh
mkdir -p -m 700 /tmp/gg-fake
XDG_RUNTIME_DIR=/tmp/gg-fake cargo run -p gglinuxd --example fakemouse &
XDG_RUNTIME_DIR=/tmp/gg-fake cargo run -p gglinux --example shot -- sonar sonar.png 1180 1400
```

The page is `engine`, `sonar`, `games`, `moments` or `settings`. Some variables change the
picture:

- `SHOT_DO` clicks first, for example
  `SHOT_DO=keyboard.set-enabled:1,keyboard.set-effect:0,keyboard.key:35,keyboard.paint-selected`.
  Each page module names its steps in its `poke` function (`src/keyboard.rs`, `src/sonar.rs`, ...).
- `SHOT_SCROLL` scrolls the page as its scroll bar would: by pixels, to `card:N` (the page's Nth
  card at the top), `end:N` (its bottom at the bottom) or `line:N` (the Nth divider inside the
  cards), each with an optional `+px` or `-px`. It prints the cards and lines it found.
- `SHOT_FIT=1` makes the picture no taller than the page.
- `SHOT_WAIT_MS` is how long to wait for the daemon's answers first (1500 by default).

`./scripts/screenshots.sh` redraws the README's pictures in `docs/screenshots/` this way, each
against a fresh fake daemon in a temporary folder, without touching a running gglinux. Run it
after changing how a page looks (`./scripts/screenshots.sh keyboard eq` redraws just those two),
and look at the pictures before committing them: when a page gains or loses a card, a picture's
`SHOT_SCROLL` in the script may need the new number.

## Working without hardware

`fakemouse` stands in for the daemon: same socket, same protocol, fake devices (a keyboard, a
headset and a mouse). Stop `gglinuxd` first, since only one process can listen on the socket:

```sh
systemctl --user stop gglinuxd
cargo run -p gglinuxd --example fakemouse                  # battery drifts down from 87%
cargo run -p gglinuxd --example fakemouse -- --battery 12  # start low
cargo run -p gglinuxd --example fakemouse -- --charging
cargo run -p gglinuxd --example fakemouse -- --fail set_mouse_dpi
cargo run -p gglinuxd --example fakemouse -- --no-mouse
```

It logs every request it receives, decoded, so you can see exactly what the app sent. It uses the
same `gg-ipc` types as the daemon, so a request it accepts is one the daemon accepts.

The Sonar code reaches PipeWire only through the `PwTools` trait in `gg-audio`, and its tests run
against a fake. `crates/gg-audio/fixtures/` holds a trimmed real `pw-dump` capture.

## Code and commit conventions

- **Fix bugs test-first.** Write a test that fails for the bug, then fix it. Most of the history
  works this way.
- **Don't refactor working code** as part of an unrelated change.
- **New source files** start with `SPDX-License-Identifier: GPL-3.0-or-later` in a comment.
- **Commit messages** start with the area and say what changes, in plain words, from the user's
  point of view. Examples from the history:
  - `gglinuxd: a second daemon refuses to start while one listens, not take its socket`
  - `UI: removing an earlier DPI stage keeps the selected stage selected`
  - `gg-gamesense: win-menu lights the Menu key, and r-win lights nothing`
- Larger features get a short design note in `docs/superpowers/specs/` and a plan in
  `docs/superpowers/plans/`. Read the existing ones before changing a feature: they record why
  things are the way they are.

## Hardware writes

Sending the wrong bytes to a device can break it. A malformed DPI packet is known to permanently
kill the Rival 3's vertical axis (rivalcfg issue #273), and the Apex's firmware-update commands
share the interface gglinux uses for lighting. So these rules are strict:

- **Never add a new opcode, report or write path without a source.** That means a published
  reference implementation or a USB capture of the vendor software doing the same thing. Guessing
  opcodes on a live device is not acceptable.
- **The keyboard's opcode allowlist** (`ALLOWED` in `crates/gg-device-keyboard/src/proto.rs`) is
  checked when a report is framed and again at the write. Extending it needs a capture and a
  reviewer. Firmware-update and bootloader opcodes must never be added.
- **Every packet builder gets a byte-for-byte unit test**, including tests for the range checks
  on its inputs.
- **No writes to a device's flash by default.** Saving to onboard memory only happens when the
  user explicitly asks for it (for example `ggctl mouse ... --save`).
- **Say what you tested on.** A pull request that changes device code must say which device and
  firmware it ran on, or say plainly that it was not tested on hardware.
- **Udev access stays narrow.** Don't widen `data/udev/60-gglinux.rules`. Keyboard typing
  interfaces must never be opened by default.

## Pull requests

- Keep each pull request to one change, with its tests.
- Update the README, `PLAN.md`'s parity checklist or `docs/PROTOCOLS.md` if your change affects
  them, and add an entry under `[Unreleased]` in `CHANGELOG.md`.
- CI must pass.

## License

gglinux is licensed under GPL-3.0-or-later. By contributing, you agree that your contribution is
licensed under the same terms. If you port code from another project, say where it came from and
check that its license is compatible (see the reuse map in [PLAN.md](PLAN.md)).

## Security

Please don't report security problems in public issues. See [SECURITY.md](SECURITY.md).
