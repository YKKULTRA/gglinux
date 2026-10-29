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
# Daemon and ggctl (the cargo workspace)
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

# Desktop app (ui/ and its separate cargo project ui/src-tauri)
cd ui
npm ci
npm run check            # svelte-check
npx vite build           # frontend
(cd src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked)
npx tauri build --debug --no-bundle
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
cd ui
npm run tauri dev
```

Use that or `npm run tauri build`. **Don't** run `cargo build && ./target/debug/gglinux`: a debug
binary loads the frontend from the Vite dev server (`http://localhost:1420`), not from the embedded
files, so without Vite running you get a blank window and no error.

A dev build runs beside an installed gglinux instead of handing over to it, so you may see two tray
icons.

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

It logs every request it receives, decoded, so you can see exactly what the UI sent. It uses the
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
