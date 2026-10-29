# Desktop integration and packaging (M7d) — design

**Status:** decided 2026-09-29 by the author (the owner asked not to review specs). **Milestone:**
M7, part d, the last of the buildable roadmap. PrismSync is skipped, since the keyboard is the only
RGB device. Virtual surround stays optional and unbuilt.

## Goal

gglinux behaves like an installed desktop app:
- it can start with the desktop (the daemon as a user service, the UI hidden in the tray);
- its settings can be backed up and restored;
- `ggctl` is a real command for scripts and KDE shortcuts;
- `install-local.sh` installs the UI too;
- an Arch `PKGBUILD` (`gglinux-git`) builds and installs everything.

Publishing the repository or the package is **not** part of this. It needs the owner's decision;
memory notes the git identity is repo-local and unconfirmed.

## Design

**`ggctl` becomes a binary** (`crates/gglinuxd/src/bin/ggctl.rs`, replacing the example):
- The argument parsing becomes a pure `parse(args) -> Result<Action>`, where `Action` is `Watch`
  or `Send(Request)`.
- It keeps the mouse commands, and adds:
  - `clip save`;
  - `profile list`;
  - `profile apply <name>`;
  - `gamesense test`;
  - `gamesense on|off`.
- `profile list` prints one name per line.
- `default-run` stays `gglinuxd`.

**The UI starts hidden when asked:** the main window is created invisible, and `setup` shows it
unless the arguments include `--hidden`. The tray is always there.

**Autostart** (Tauri commands `get_autostart` / `set_autostart(enabled)`):
- On:
  - write `~/.config/autostart/gglinux.desktop` (`Exec="<this executable>" --hidden`);
  - run `systemctl --user enable gglinuxd.service`.
- Off: remove that file and run `systemctl --user disable gglinuxd.service`.
- Reading: on iff the desktop file exists.
- A `systemctl` failure (unit not installed, as in a dev checkout) comes back as an error message,
  and the desktop file part still happens.

**Settings backup** (Tauri commands `export_settings` / `import_settings(text)`):
- **The bundle:** `{"gglinux_backup": 1, "created": <unix>, "files": {"lighting.json": {…}, …}}`,
  covering the known files `lighting.json`, `gamesense.json`, `profiles.json`, `clips.json` and
  `sonar.json` wherever they exist and parse.
- **Export** writes to `~/Documents/gglinux-backup-<YYYY-MM-DD_HH-MM-SS>.json` (to `$HOME` when
  there's no `Documents`), using a UTC timestamp, and returns the path.
- **Import:**
  - rejects anything that isn't version 1 or names an unknown file;
  - writes each file atomically, pretty-printed;
  - then runs `systemctl --user try-restart gglinuxd.service` so the daemon loads them;
  - answers which files it wrote and whether the daemon restarted.
- The UI picks the file with a plain `<input type=file>` and reads it in the page, so no dialog
  plugin is needed.

**The Settings page** gets, under Profiles:
- "Start gglinux when I log in";
- "Back up settings" (shows the path, with "Open folder");
- "Restore from a backup…".

**`install-local.sh`:**
- also builds the UI (`npm ci`, then `npm run tauri build -- --no-bundle`), unless `--no-ui`;
- installs `~/.local/bin/gglinux`, `~/.local/bin/ggctl`, a desktop entry in
  `~/.local/share/applications` (Exec made absolute) and the icon in `~/.local/share/icons`.

**Packaging** (`packaging/arch/`):
- `PKGBUILD` for `gglinux-git`, from `git+https://github.com/ykk/gglinux.git` (the URL already in
  `Cargo.toml`, for when it's published). It builds the daemon, `ggctl` and the UI, and runs the
  workspace tests in `check()`.
- It installs:
  - `/usr/bin/{gglinuxd,ggctl,gglinux}`;
  - the user unit with `ExecStart=/usr/bin/gglinuxd`;
  - `/usr/lib/udev/rules.d/60-gglinux.rules`;
  - the optional reactive rule under `/usr/share/gglinux/`;
  - the desktop entry, icon and licence.
- `depends`: `systemd-libs`, `webkit2gtk-4.1`, `gtk3`, `libayatana-appindicator`, `pipewire`,
  `wireplumber`. `optdepends`: `noise-suppression-for-voice`, `swh-plugins`,
  `gpu-screen-recorder`.
- `gglinux.install` prints how to enable the user service and opt in to reactive lighting.
- `data/gglinux.desktop` is the shared desktop entry.

## Testing

**Unit:**
- `ggctl` parsing for every command, and the errors;
- the autostart desktop entry's content (quoting of an executable path with spaces);
- the backup: a round trip of the known files, unknown names refused, the wrong version refused,
  bad JSON refused, and files that don't exist left out of an export;
- the UTC timestamp format for known instants.

**Checks:**
- `desktop-file-validate data/gglinux.desktop`;
- `bash -n` for the scripts;
- `makepkg --printsrcinfo` and `namcap PKGBUILD` in `packaging/arch`;
- a full `makepkg` build is the owner's acceptance, since it compiles everything in release mode.

**Hardware (the owner):**
1. Settings → Start at login: log out and in, and the tray icon is there with the daemon running.
2. Export, change the lighting, import: it comes back.
3. Bind a KDE shortcut to `ggctl clip save`.
