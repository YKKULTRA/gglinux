# Arch Linux package: `gglinux-git`

This directory holds the AUR package for gglinux:

| File | What it is |
|---|---|
| `PKGBUILD` | Builds `gglinuxd`, `ggctl` and the desktop app from the latest `master` of `github.com/YKKULTRA/gglinux`. |
| `.SRCINFO` | Package metadata the AUR reads. Generated with `makepkg --printsrcinfo`; never edit it by hand. |
| `gglinux.install` | Messages pacman prints after install and upgrade: removing an old `install-local.sh` copy, enabling the user service, and the opt-in reactive-lighting rule. |

## What the package installs

| Path | |
|---|---|
| `/usr/bin/gglinuxd`, `/usr/bin/ggctl`, `/usr/bin/gglinux` | the daemon, the CLI, the desktop app |
| `/usr/lib/systemd/user/gglinuxd.service` | the user unit, with `ExecStart=/usr/bin/gglinuxd` |
| `/usr/lib/udev/rules.d/60-gglinux.rules` | the narrow device-access rule (control interfaces only) |
| `/usr/share/gglinux/61-gglinux-reactive.rules` | the opt-in reactive-lighting rule, **not** active until the user copies it to `/etc/udev/rules.d/` |
| `/usr/share/applications/io.github.ykk.gglinux.desktop`, `/usr/share/icons/hicolor/128x128/apps/gglinux.png` | menu entry and icon |
| `/usr/share/licenses/gglinux-git/LICENSE` | GPL-3.0 text |

The user still runs `systemctl --user enable --now gglinuxd` (the install message says so).

## Notes on the PKGBUILD

- **Network only in `prepare()`.** `cargo fetch` runs there; `build()` and `check()` use
  `--frozen`, following the Arch [Rust](https://wiki.archlinux.org/title/Rust_package_guidelines)
  package guidelines. The app is part of the same cargo workspace, so there's one fetch and one
  build.
- **`options=('!lto')` is required.** The `hidapi` crate compiles its bundled C library with the
  `CFLAGS` makepkg exports. With makepkg's default `lto` option those objects become GCC LTO
  bitcode, which the Rust linker can't use, and `gglinuxd` fails to link with undefined `hid_read`,
  `hid_write`, and so on. This was reproduced with a stock `makepkg.conf` in an `archlinux`
  container.
- `check()` runs the workspace's tests (the daemon's and the app's) in release mode.
- `arch=('x86_64')` only: no other architecture has been built.

## Testing a change to the PKGBUILD

The source is the public GitHub repository, so the repository must be public before anyone else can
build this package.

```sh
cd packaging/arch
makepkg --printsrcinfo > .SRCINFO   # after every PKGBUILD change
makepkg -sf                          # build (add -i to install)
namcap PKGBUILD gglinux-git-*.pkg.tar.zst
```

To build in a clean chroot, as the AUR's users effectively do, use `devtools`:

```sh
sudo pacman -S --needed devtools namcap
extra-x86_64-build
```

To build a branch other than `master` while testing, temporarily change the source to, for
example, `gglinux::git+https://github.com/YKKULTRA/gglinux.git#branch=my-branch`, or to a local
clone with `git+file:///path/to/gglinux#branch=my-branch`. Don't commit or push that change.

## Submitting to the AUR (for the maintainer)

Do these once the GitHub repository is public. Nothing here has been done yet.

1. **Account.** Register at <https://aur.archlinux.org/register>. The AUR shows your user name on
   the package page. The account settings have a **Hide Email Address** option; check that it is
   set the way you want before submitting.
2. **SSH key.** Create a key just for the AUR and add the public half under **My Account > SSH
   Public Key**:

   ```sh
   ssh-keygen -t ed25519 -f ~/.ssh/aur -C "aur"
   cat ~/.ssh/aur.pub
   ```

   Then tell SSH to use it:

   ```sh
   cat >> ~/.ssh/config <<'EOF'
   Host aur.archlinux.org
     IdentityFile ~/.ssh/aur
     User aur
   EOF
   ```

3. **Check the name is free:** <https://aur.archlinux.org/packages/gglinux-git> should say the
   package does not exist (also check `gglinux`, since this package `provides` it).
4. **Clone the empty AUR repository** (git warns that it is empty; that is expected):

   ```sh
   git -c init.defaultBranch=master clone ssh://aur@aur.archlinux.org/gglinux-git.git ~/aur/gglinux-git
   ```

5. **Copy the package files in** and set the maintainer line. The AUR convention is
   `# Maintainer: Name <user at example dot com>`; the email part is optional, and leaving it out is
   fine.

   ```sh
   cp packaging/arch/PKGBUILD packaging/arch/gglinux.install ~/aur/gglinux-git/
   cd ~/aur/gglinux-git
   $EDITOR PKGBUILD                     # the "# Maintainer:" line
   makepkg --printsrcinfo > .SRCINFO
   ```

6. **Build it once more from the public repository**, ideally in a clean chroot, and run namcap:

   ```sh
   extra-x86_64-build                   # or: makepkg -sf
   namcap PKGBUILD gglinux-git-*.pkg.tar.zst
   rm -f gglinux-git-*.pkg.tar.zst
   ```

7. **Push.** The AUR only accepts the `master` branch, and only `PKGBUILD`, `.SRCINFO` and the files
   the PKGBUILD names belong in it:

   ```sh
   git add PKGBUILD .SRCINFO gglinux.install
   git commit -m "Initial import: gglinux-git"
   git push origin master
   ```

8. The package appears at <https://aur.archlinux.org/packages/gglinux-git>. Copy any maintainer-line
   change back into this directory so the two stay the same.

## Updating the AUR package later

A `-git` package rebuilds from the newest commit whenever a user reinstalls it, so the AUR copy only
needs a push when the PKGBUILD itself changes (new dependencies, build steps, files):

```sh
cp packaging/arch/PKGBUILD packaging/arch/gglinux.install ~/aur/gglinux-git/
cd ~/aur/gglinux-git
# bump pkgrel if pkgver didn't change
makepkg --printsrcinfo > .SRCINFO
git commit -am "Describe the change" && git push
```

Keep `packaging/arch/` in this repository and the AUR repository identical.
