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

## D9. Hygiene sweep: what was removed or changed

Searched every tracked file (outside the orchestrator's `.claude/` and `.cloud-context/`) for home
paths, usernames, hostnames, IPs, private-note references, logs, large binaries and tracked
`.remember/` or `.superpowers/` content, and searched the full history for deleted files, blobs
over 200 KB and credential-like strings.

Removed or changed in the tree:

- `docs/superpowers/plans/2026-09-29-gamesense-server.md`: a session scratchpad path containing
  the owner's home directory name and a session id, replaced by `S=$(mktemp -d)`; a step that
  said to update the owner's Obsidian note and assistant memory, reworded to "the list of files
  gglinux writes".
- `docs/superpowers/plans/2026-09-28-reactive-lighting.md`: "system-changes memory" step reworded
  the same way.
- `docs/superpowers/specs/2026-09-28-reactive-lighting-design.md`: an ACL line naming the local
  user now reads `user:<you>:rw-`.
- Repository URLs `github.com/ykk/gglinux` → `github.com/YKKULTRA/gglinux` (see D5).

Checked and kept:

- `crates/gg-audio/fixtures/pw-dump-2026-09-27.json`: a trimmed `pw-dump` used by tests. It names
  generic audio devices (the headset, a USB audio interface, "AirPods Pro", an HDMI monitor model)
  and apps (Firefox, Elisa); the Bluetooth address is already the placeholder `00:11:22:33:44:55`.
  Nothing personal enough to justify breaking the test fixture.
- Test paths like `/home/u/...` and `/home/you/...` are placeholders.
- Hardware descriptions in `PLAN.md` and the specs (RTX 4090, KDE, Arch/CachyOS) are development
  context, not private.
- No logs, no large binaries (largest tracked file is a 140 KB plan), nothing tracked under
  `.remember/` or `.superpowers/`. `.gitignore` already covers `target/`, `node_modules/`,
  `ui/dist/`, makepkg output and `*.log`.
- History: no file was ever deleted, no blob over 200 KB, no credentials. The two personal strings
  fixed above remain in old commits; harmless, and rewriting published history is the owner's call.

Not touched (orchestrator-owned): the commit `b68b0db` "cloud-only: session settings and context
pack" adds `.claude/` and `.cloud-context/`, which contain the owner's home paths, Obsidian vault
path and email. **That commit must not reach a public branch**; the publish checklist says how to
drop it.

## D10. Orchestrator correction: no deadline, no history rewrite

Mid-session the owner's orchestrator cancelled the 23:45 UTC deadline and said not to rewrite
history or force-push, but to record the history findings with exact cleanup commands. Nothing was
rewritten or force-pushed. The findings and two cleanup paths (A: rebase only the release branch to
drop `b68b0db`; B: `git filter-repo` to also replace the email everywhere) are in
`docs/PUBLISH-CHECKLIST.md`. Both paths were dry-run in throwaway clones in the session scratchpad
(never pushed): A rebases cleanly and the per-milestone `--no-ff` merges reproduce the release
tree exactly; B rewrites all 205 owner commits, prunes `b68b0db` and its files from every branch,
scrubs the two leftover strings, and the hash-remap loop leaves every CHANGELOG hash resolvable.

## D11. The public docs don't spell out what they tell the owner to scrub

The checklist will be public too, so it never writes the owner's email, home directory path or
local user name literally: the cleanup commands read the address from a commit and use regex
patterns for the two strings. The CHANGELOG avoids citing hashes of this session's commits, which
path A would change.

## D12. gg-audio test scratch directories (test-only fix)

Running the suite as a non-root user, after root had run it, failed 57 `gg-audio` tests with
"permission denied": every test's scratch directory sat under one shared `/tmp/gg-audio-tests`,
owned by whoever ran the tests first. CI on a fresh runner would not hit it, but any contributor on
a machine another account had tested on would. Fixed test-first: a new test asserts the scratch
directory sits directly in the temp dir (failed before the fix), and the helper now uses
`gg-audio-test-<pid>-<name>` directly under the temp dir, like every other crate's tests. No
non-test code changed.

## D13. PKGBUILD depends lists directly linked libraries

namcap on the built package reported eight directly linked libraries (glibc, libgcc, glib2,
gdk-pixbuf2, cairo, libsoup3, dbus, hicolor-icon-theme) satisfied only transitively. The Arch
package guidelines ask not to rely on transitive dependencies, so they were added. `libgcc` is the
current Arch package that owns `libgcc_s.so.1` (namcap names it). namcap's "may not be needed"
warnings for `libayatana-appindicator`, `pipewire`, `wireplumber` and `xdg-utils` are false
positives (loaded or executed at run time), so they stay. The `-debug` package's symlink errors
are namcap's known false positive for split debug packages.
