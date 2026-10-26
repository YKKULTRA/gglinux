# Decisions made in the release-prep cloud session

The owner was away, so judgement calls were made here and recorded. Each one says what was
decided and why, so it can be reversed.

## D1. Hardware status follows the brief, not the old README

The old README listed "the headset's status, battery" and "the mouse's battery" as verified.
The brief and the project memory say only two things were verified live on hardware: M2
base-station screen protection and M4a keyboard lighting. The public docs now use that stricter
list. Device enumeration (M0) is described as "seen working" only where PLAN.md records it.

## D2. Stock rustfmt for the whole workspace, in one formatting-only commit

`cargo fmt --check` failed on 53 files (689 hunks): the workspace had never been formatted.
A `rustfmt.toml` matching the house style (`max_width = 120`, `use_small_heuristics = "Max"`)
would still have touched 46 files, so the default style was applied instead, which is what a
contributor's `cargo fmt` does with no setup. The commit (`88a6390`) is formatting only and is
listed in `.git-blame-ignore-revs`.

## D3. npm audit: lockfile-only bumps for four build-time packages

`npm ci` reported four high-severity advisories (vite 6.4.3, devalue, nanoid, source-map-js),
all build-time. `npm audit fix` also stripped every `libc` field from the lockfile (an npm
version difference), so only the four bumped entries were patched into the original lockfile.
No `package.json` range changed. svelte-check and the frontend build pass.

## D4. Unknown clippy lint: allow `unknown_lints` beside it

`crates/gglinuxd/src/keyboard/audio.rs` allowed `clippy::chunks_exact_to_as_chunks`, which
clippy 1.97 (and the declared MSRV 1.87) do not know, so `clippy -D warnings` failed. The
attribute now also allows `unknown_lints`. Code unchanged.

## D5. Repository URL is github.com/YKKULTRA/gglinux

`Cargo.toml`, `ui/src-tauri/Cargo.toml`, the systemd unit and the PKGBUILD named
`github.com/ykk/gglinux`. All now use the real remote. Historical plan/spec documents under
`docs/superpowers/` keep the old URL as written at the time. The Tauri identifier
`io.github.ykk.gglinux` was **not** changed: it names the UI's data directory
(`~/.local/share/io.github.ykk.gglinux`), so changing it would orphan existing web storage.
Listed as an owner decision in `docs/PUBLISH-CHECKLIST.md`.

## D6. SPDX headers on every source file and the shipped config

Added `SPDX-License-Identifier: GPL-3.0-or-later` to 87 files that lacked it: Rust, TS,
Svelte, CSS, the HTML shell, the scripts (after the shebang), the udev rules, the systemd unit
and the pacman install script. `.desktop`, PKGBUILD and `.SRCINFO` were left alone (metadata,
the PKGBUILD has its own `license=`).

## D7. CI: two jobs, debug Tauri build

`.github/workflows/ci.yml` runs on push to master/main, pull requests and manual dispatch only.
The `daemon` job runs fmt, clippy `-D warnings` and tests; the `ui` job runs npm ci,
svelte-check, the Vite build, fmt/clippy/tests of the Tauri backend and `tauri build --debug
--no-bundle`. The debug Tauri build was chosen over a release build to save Actions minutes
(release uses LTO with one codegen unit); the release build is exercised by the PKGBUILD.
Actions are pinned to major tags (checkout v6, setup-node v6, rust-cache v2), all on Node 24.
actionlint could not be run here (downloading it was blocked); the YAML was parsed and checked
by hand.

## D8. PKGBUILD follows the Arch Rust/Node guidelines

Network access (`cargo fetch`, `npm ci`) moved to `prepare()`; `build()` and `check()` use
`--frozen`. The maintainer line is `# Maintainer: YKKULTRA` without an email: publishing an
email address is the owner's call (see the publish checklist).
