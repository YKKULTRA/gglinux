# Publish checklist

For the owner. Everything needed for a public release is on `cloud/release-prep`, except the steps
only you can take. Nothing has been merged, tagged, published or made public, and no history has
been rewritten.

What is ready on `cloud/release-prep`: fmt/clippy/tests clean, the UI checks and a Tauri release
build pass, CI workflow, README/CONTRIBUTING/SECURITY/CHANGELOG/ARCHITECTURE, SPDX headers, the
PKGBUILD (built with makepkg in an Arch container) and its AUR steps. Details in
`docs/cloud/REPORT-gglinux.md`.

## 1. Decisions only you can make

### 1a. Drop the orchestrator commit (required)

Commit `b68b0db` ("cloud-only: session settings and context pack (remove before merge)") adds
`.claude/` and `.cloud-context/`. Those files contain your home directory paths, your Obsidian
vault path, your email, your machine's setup and the assistant's memory notes. **It must
not reach a public branch, not even as a commit that a later commit deletes**: deleting the files
leaves them readable in history.

Both paths in section 2 drop it. Also decide whether `docs/cloud/` (this session's STATUS,
DECISIONS and REPORT) should be published. It contains nothing private; it is session bookkeeping.
To leave it out, add `git rm -r docs/cloud && git commit -m "Remove session notes"` on the release
branch before merging.

### 1b. Your email address in the history

205 commits, on every branch including `master`, are authored and committed with your personal
email address. Once the repository is public, anyone can read it (for example by adding `.patch`
to a commit URL). Options:

- **Keep it.** Nothing to do. Use path A in section 2.
- **Replace it with your GitHub no-reply address** before going public. This rewrites every commit
  on every branch (new hashes everywhere) and needs a force-push of all branches. Use path B in
  section 2. Find your no-reply address under GitHub **Settings > Emails** ("Keep my email
  addresses private" shows `<id>+YKKULTRA@users.noreply.github.com`).

Either way, for future commits you can set the no-reply address
(`git config --global user.email <id>+YKKULTRA@users.noreply.github.com`) and then turn on
**Settings > Emails > Block command line pushes that expose my email**.

The commits made in this session are authored as `Claude <noreply@anthropic.com>`, with
`Co-Authored-By` and session trailers. Keep or change them as you prefer.

### 1c. Merge style for the stacked branches

The stack is linear: `master` ← `m6a-gamesense` ← `m6b-effects` ← `m7a-profiles` ← `m7b-clips` ←
`m7c-streamer` ← `m7d-desktop` ← `m7e-gaps` ← `m8-bugfixes` ← `cloud/release-prep` (no merge
commits inside it). Options:

- **One `--no-ff` merge per milestone (recommended).** Matches how you merged before (`cce1fdf`
  "Merge M2 headset, M3 Sonar and M4a keyboard lighting", `4981339` "Merge reactive keyboard
  lighting..."). `git log --first-parent master` then reads as a list of milestones, and every
  commit keeps its hash, so the CHANGELOG's hash references stay valid. Commands in section 2.
- **One `--no-ff` merge of everything.** Simpler, loses the per-milestone grouping.
- **Fast-forward.** Linear history, no milestone markers.
- **Squash.** Not recommended: it throws away more than 200 well-described, test-first commits that
  the CHANGELOG points at.

### 1d. Other choices

- **When to go public.** Only keyboard lighting and base-station screen protection are verified on
  hardware. You may want one test session first (Sonar on, the headset controls from the UI,
  GameSense **Run test**, a clip with `gpu-screen-recorder` installed). The README is honest either
  way.
- **Tauri identifier** `io.github.ykk.gglinux` (in `ui/src-tauri/tauri.conf.json`). It refers to a
  `ykk` GitHub account, which isn't yours. Changing it (for example to `io.github.ykkultra.gglinux`)
  moves the app's web storage directory (`~/.local/share/<identifier>`), so existing installs lose
  the UI's stored state, and the README's uninstall line must change with it. Left as is.
- **AUR maintainer line.** `packaging/arch/PKGBUILD` says `# Maintainer: YKKULTRA` with no email.
  Add one if you want (`Name <user at example dot com>`).
- **Version and tags.** Everything is `0.0.1` (Cargo, `package.json`, `tauri.conf.json`). No tag
  exists. Tag something like `v0.1.0` only if you want a release; the CHANGELOG would then move its
  `[Unreleased]` content under that version.
- **Screenshots.** The README lists the seven it should have; take them on your desktop and add
  them under `docs/screenshots/`.

## 2. Commands, in order

Run these on your machine, in your existing clone. Nothing here was run against GitHub.

### Path A: keep your email (rebase only the release branch)

Tested in a scratch clone: the rebase applies cleanly, and the result has no `.claude/` or
`.cloud-context/` files and the same tree otherwise.

```sh
cd gglinux                                   # your existing clone
git fetch origin

# 1. A release branch without the orchestrator commit. b68b0db's parent is m8-bugfixes.
git switch -c release origin/cloud/release-prep
git rebase --onto b68b0db~1 b68b0db release
git ls-files .claude .cloud-context          # must print nothing

# 2. The rustfmt commit got a new hash; point .git-blame-ignore-revs at it.
new=$(git log --format=%H -1 --grep='^style: apply rustfmt to the whole workspace$' release)
sed -i "s/^88a6390f76378c19fc13e7a350d447178194b2a9\$/$new/" .git-blame-ignore-revs
git commit -am "Point .git-blame-ignore-revs at the rebased rustfmt commit"

# (optional) leave the session notes out
# git rm -r docs/cloud && git commit -m "Remove session notes"

# 3. Let CI run once: push the branch and open a pull request release -> master on GitHub.
git push -u origin release
```

After CI passes on the pull request, merge it locally milestone by milestone (or press the merge
button for a single merge commit):

```sh
git switch master
git pull --ff-only origin master
for b in m6a-gamesense m6b-effects m7a-profiles m7b-clips m7c-streamer m7d-desktop m7e-gaps m8-bugfixes; do
  git merge --no-ff "origin/$b" -m "Merge $b" || break
done
git merge --no-ff release -m "Merge release preparation: docs, CI, packaging, SPDX headers"
git ls-files .claude .cloud-context          # must print nothing
git push origin master
```

Then go to section 3.

### Path B: replace your email (rewrite every branch, before going public)

Tested in a scratch mirror: all 205 of your commits get the new address, the orchestrator commit
and its files disappear from every branch, the two leftover personal strings in old design-doc
versions (a scratchpad path containing your home directory name, and an ACL line naming your local
user) are replaced, and every hash
the CHANGELOG cites still resolves after step 4.

```sh
sudo pacman -S git-filter-repo
mkdir -p ~/gglinux-rewrite && cd ~/gglinux-rewrite
git clone --mirror https://github.com/YKKULTRA/gglinux.git gglinux-mirror.git

# 1. What to change. Replace ID with your number from the no-reply address. The old address is
#    read from the master merge commit, so it isn't spelled out here.
old=$(git -C gglinux-mirror.git log -1 --format=%ae 4981339)
printf 'ykk <ID+YKKULTRA@users.noreply.github.com> <%s>\n' "$old" > mailmap
cat mailmap
cat > replacements.txt <<'EOF'
regex:/tmp/claude-[0-9]+/[^ ]*/scratchpad==>$(mktemp -d)
regex:user:[a-z0-9_-]+:rw-==>user:<you>:rw-
EOF

# 2. Rewrite: new email, drop .claude/ and .cloud-context/ everywhere (b68b0db becomes empty
#    and is removed), replace the two strings.
cd gglinux-mirror.git
git filter-repo --mailmap ../mailmap --replace-text ../replacements.txt \
  --invert-paths --path .claude --path .cloud-context
git log --all --format='%ae' | sort | uniq -c   # the old address must not appear

# 3. Push every branch over the old ones. filter-repo may remove the remote; add it back if so.
git remote get-url origin || git remote add origin https://github.com/YKKULTRA/gglinux.git
git push --force --all origin
git push --force --tags origin

# 4. Hashes changed: fix the CHANGELOG's references and .git-blame-ignore-revs.
cd .. && git clone https://github.com/YKKULTRA/gglinux.git work && cd work
git switch cloud/release-prep
while read -r old new; do
  [ "$old" = old ] && continue
  sed -i "s/\b${old:0:7}\b/${new:0:7}/g" CHANGELOG.md
  sed -i "s/^$old\$/$new/" .git-blame-ignore-revs
done < ../gglinux-mirror.git/filter-repo/commit-map
git commit -am "Update commit references after the history rewrite"
git push origin cloud/release-prep
```

Your existing clone still has the old history: re-clone, or
`git fetch origin && git reset --hard origin/<branch>` on each branch you use. Then merge as in path
A from "Let CI run once" onward, using `cloud/release-prep` in place of `release` (it no longer has
the orchestrator commit).

## 3. Going public

1. **Settings > Code security**: enable **Private vulnerability reporting** (SECURITY.md tells people
   to use it).
2. **Settings > General**: add a description ("Open-source SteelSeries GG replacement for Linux")
   and topics (`steelseries`, `linux`, `pipewire`, `tauri`, `rust`, `arch-linux`).
3. **Settings > Branches**: protect `master` and require the two CI checks, "Daemon (fmt, clippy,
   tests)" and "UI (svelte-check, frontend, Tauri)".
4. **Settings > General > Danger Zone > Change repository visibility > Public.**
5. Optional: once public, Actions minutes on standard runners are free, so CI could also run on
   pushes to other branches. The workflow is limited to master/main, pull requests and manual runs
   because of the private-repo quota.
6. Optional: delete the merged branches (`m6a-gamesense` ... `m8-bugfixes`, `release`,
   `cloud/release-prep`).

## 4. The AUR

Only after the repository is public (the PKGBUILD clones it). Follow
[packaging/arch/README.md](../packaging/arch/README.md#submitting-to-the-aur-for-the-maintainer):
create an AUR account and SSH key, check that `gglinux-git` is free, clone
`ssh://aur@aur.archlinux.org/gglinux-git.git`, copy in `PKGBUILD` and `gglinux.install`, regenerate
`.SRCINFO`, build once in a clean chroot (`extra-x86_64-build`), run `namcap`, and push to `master`.

## 5. What the history scan found

Scanned on 2026-10-26 across all ten branches. Nothing was rewritten.

| Finding | Where | Handled by |
|---|---|---|
| Your personal email address as author and committer | 205 commits, all branches | your choice: path A keeps it, path B replaces it |
| `.claude/settings.json` and `.cloud-context/` (home paths, Obsidian vault path, email, memory notes) | `b68b0db`, only on `cloud/release-prep` | both paths drop it |
| Author `Hermes (orchestrator) <hermes@localhost>` | `b68b0db` only | dropped with it |
| A session scratchpad path containing your home directory name and a session id | old versions of `docs/superpowers/plans/2026-09-29-gamesense-server.md` | fixed in the tree (`0d30e25`); path B also removes it from history |
| An ACL line naming your local user | old versions of `docs/superpowers/specs/2026-09-28-reactive-lighting-design.md` | same |
| Old repository URL `github.com/ykk/gglinux` | Cargo manifests, unit, PKGBUILD (fixed); historical plans/specs (kept, harmless) | nothing more to do |
| Deleted files, blobs over 200 KB, credentials or tokens | none found | nothing to do |

The largest file ever committed is a 140 KB plan document; the PNG icons are the only binaries.
