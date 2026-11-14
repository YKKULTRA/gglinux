# Publish checklist

For the owner. Everything needed for a public release is on `cloud/release-prep`, except the steps
only you can take. Nothing has been tagged, published or made public yet.

What is ready on `cloud/release-prep`: fmt/clippy/tests clean, the UI checks and a Tauri release
build pass, CI workflow, README/CONTRIBUTING/SECURITY/CHANGELOG/ARCHITECTURE, SPDX headers, the
PKGBUILD (built with makepkg in an Arch container) and its AUR steps. Details in
`docs/cloud/REPORT-gglinux.md`. Since then (M10, 2026-10-29) the Tauri UI was replaced by the
native Slint app: there are no UI checks or Tauri build any more, CI is one Rust job, and the
PKGBUILD has new dependencies and hasn't been built with makepkg since.

## 1. Done

- **History cleaned (2026-10-29).** Every branch was rewritten once with `git filter-repo`:
  - the owner's 205 commits now carry the GitHub no-reply address instead of a personal one;
  - the cloud orchestrator's context-pack commit (`b68b0db`, which added `.claude/` and
    `.cloud-context/` with home paths, an Obsidian vault path, an email and memory notes) is gone
    from every branch, together with its files;
  - the leftover personal strings in old design-doc versions (a session scratchpad path, an ACL
    line naming the local user, a home-directory path) were replaced.

  Commit counts, authors, dates and messages are otherwise unchanged on every branch, and every
  branch was force-pushed. Commit references in the CHANGELOG, `.git-blame-ignore-revs` and the
  docs were remapped to the new hashes. Existing clones from before 2026-10-29 still hold the old
  history: re-clone, or `git fetch origin && git reset --hard origin/<branch>` on each branch.
- **Future commits.** The repository's `user.email` is the no-reply address. Optionally also turn
  on GitHub **Settings > Emails > Block command line pushes that expose my email**.
- **Merge style decided: one `--no-ff` merge per milestone**, as `master` was merged before
  (`4ec4f9f`, `1c254c8`), so `git log --first-parent master` reads as a list of milestones and
  every commit keeps its hash:

  ```sh
  git switch master && git pull --ff-only origin master
  for b in m6a-gamesense m6b-effects m7a-profiles m7b-clips m7c-streamer m7d-desktop m7e-gaps m8-bugfixes; do
    git merge --no-ff "origin/$b" -m "Merge $b" || break
  done
  git merge --no-ff origin/cloud/release-prep -m "Merge release preparation: docs, CI, packaging, SPDX headers"
  git push origin master
  ```

## 2. Before going public

### 2a. Old commits on GitHub

A force-push does not delete the replaced commits on GitHub at once: they can stay reachable by
their old hashes until GitHub garbage-collects them. The repository has no forks, issues or stars,
so the simplest guarantee is to delete it and push the clean branches into a new repository of the
same name before switching it to public (pull requests are lost with it). The alternative is
asking GitHub Support to purge the unreachable objects.

### 2b. Session notes

Decide whether `docs/cloud/` (the release-prep session's STATUS, DECISIONS and REPORT) should be
published. It contains nothing private; it is session bookkeeping. To leave it out:
`git rm -r docs/cloud && git commit -m "Remove session notes"`.

### 2c. Other choices

- **When to go public.** Only keyboard lighting and base-station screen protection are verified on
  hardware. You may want one test session first (Sonar on, the headset controls from the UI,
  GameSense **Run test**, a clip with `gpu-screen-recorder` installed). The README is honest either
  way.
- **App id** `io.github.ykk.gglinux`. It refers to a `ykk` GitHub account, which isn't yours.
  Since M9 and M10 it is the desktop entry's name (`data/io.github.ykk.gglinux.desktop`), the app
  id the desktop and its portals know gglinux by, and the app's D-Bus name (`APP_ID` in
  `crates/gglinux-ui/src/shell.rs`). Changing it (for example to `io.github.ykkultra.gglinux`)
  means renaming all of them together, with the install scripts, the PKGBUILD and the uninstall
  steps. Left as is.
- **AUR maintainer line.** `packaging/arch/PKGBUILD` says `# Maintainer: YKKULTRA` with no email.
  Add one if you want (`Name <user at example dot com>`).
- **Version and tags.** Released as `0.1.0` (tag `v0.1.0`, 2026-11-14). Tag something
  like `v0.1.0` only if you want a release; the CHANGELOG would then move its `[Unreleased]`
  content under that version.
- **Screenshots.** `docs/screenshots/` has the six the README shows, drawn headless against the
  fake daemon by `./scripts/screenshots.sh`. Rerun it after UI changes; pictures taken on your
  desktop could replace them if you'd rather show the real thing.

## 3. Going public

1. **Settings > Code security**: enable **Private vulnerability reporting** (SECURITY.md tells people
   to use it).
2. **Settings > General**: add a description ("Open-source SteelSeries GG replacement for Linux")
   and topics (`steelseries`, `linux`, `pipewire`, `slint`, `rust`, `arch-linux`).
3. **Settings > Branches**: protect `master` and require the CI check, "Rust (fmt, clippy,
   tests)". (Before M10 there were two: "Daemon (fmt, clippy, tests)" and "UI (svelte-check,
   frontend, Tauri)".)
4. **Settings > General > Danger Zone > Change repository visibility > Public.**
5. Optional: once public, Actions minutes on standard runners are free, so CI could also run on
   pushes to other branches. The workflow is limited to master/main, pull requests and manual runs
   because of the private-repo quota.
6. Optional: delete the merged branches (`m6a-gamesense` ... `m8-bugfixes`,
   `cloud/release-prep`).

## 4. The AUR

Only after the repository is public (the PKGBUILD clones it). Follow
[packaging/arch/README.md](../packaging/arch/README.md#submitting-to-the-aur-for-the-maintainer):
create an AUR account and SSH key, check that `gglinux-git` is free, clone
`ssh://aur@aur.archlinux.org/gglinux-git.git`, copy in `PKGBUILD` and `gglinux.install`, regenerate
`.SRCINFO`, build once in a clean chroot (`extra-x86_64-build`), run `namcap`, and push to `master`.

## 5. What the history scan found

Scanned on 2026-10-26 across all ten branches; the history was cleaned on 2026-10-29 (section 1).

| Finding | Where | Handled by |
|---|---|---|
| Your personal email address as author and committer | 205 commits, all branches | replaced with the no-reply address |
| `.claude/settings.json` and `.cloud-context/` (home paths, Obsidian vault path, email, memory notes) | `b68b0db`, only on `cloud/release-prep` | commit and files dropped |
| Author `Hermes (orchestrator) <hermes@localhost>` | `b68b0db` only | dropped with it |
| A session scratchpad path containing your home directory name and a session id | old versions of `docs/superpowers/plans/2026-09-29-gamesense-server.md` | fixed in the tree (`a1d28fb`) and replaced in history |
| An ACL line naming your local user | old versions of `docs/superpowers/specs/2026-09-28-reactive-lighting-design.md` | same |
| Old repository URL `github.com/ykk/gglinux` | Cargo manifests, unit, PKGBUILD (fixed); historical plans/specs (kept, harmless) | nothing more to do |
| Deleted files, blobs over 200 KB, credentials or tokens | none found | nothing to do |

The largest file ever committed is a 140 KB plan document; the PNG icons are the only binaries.
