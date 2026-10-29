# Native desktop app (M10) — design

**Status:** decided 2026-10-29. The owner asked for the port ("i think the ui is electron right?
can we port it to native? maybe rust") and, after the comparison below, chose Slint ("do slint").
The rest of the design is the author's. **Milestone:** M10. **Replaces** the Tauri 2 + Svelte 5 UI
in `ui/` (M0 to M9). The daemon, `ggctl` and the socket protocol don't change.

The earlier specs in this folder describe the Tauri UI as it was when they were written
(`*Panel.svelte`, Tauri commands, `npm run check`). They stay as written; this record says where
each of those pieces went.

## Goal

1. **No browser engine.** The app sits in the tray most of the time, so what it holds there
   matters most. One native process, without WebKit's helper processes.
2. **Parity with the Svelte UI**, panel by panel: every control, text, state and behaviour,
   including disabled and busy states, validation ranges, empty, disconnected and error states,
   confirmation steps, hints, what happens after a request succeeds or fails or a daemon event
   arrives, and how working copies are kept or reset (`daemon.svelte.ts`). Where something can't
   apply to the new app, that's written down, not dropped silently.
3. **The same desktop behaviour:** a tray icon, one copy at a time, `--hidden` when started at
   login, closing the window into the tray, **Quit** in the tray menu, start at login, settings
   backups, and opening files (the keyboard's picture, a backup to restore).
4. **Built with cargo alone:** no Node or npm, one workspace, one lockfile, so `install-local.sh`,
   the PKGBUILD and CI each get one build step.
5. **Pictures without a display:** any page drawn to a PNG against the fake daemon, for review
   and for the README.

Out of scope: new features, protocol changes, other operating systems, and a new look (the
colours, layout and wording are the old UI's).

## Facts (measured 2026-10-29 on the owner's PC)

**The Tauri UI:**
- Hidden in the tray it held **240 MB** PSS: the UI process 74 MB, WebKitWebProcess 127 MB and
  WebKitNetworkProcess 38 MB. The daemon, for comparison: 9 MB.
- On NVIDIA's driver it had to set `WEBKIT_DISABLE_DMABUF_RENDERER=1`. Without it, WebKit's DMABUF
  renderer crashed it on Wayland (Gdk protocol error 71) or left the window empty under XWayland
  (M0).
- Building it took Node and npm, a second cargo project (`ui/src-tauri`, with its own lockfile and
  target folder) and the WebKitGTK and GTK 3 development files. CI ran it as a second job.
- The wire types existed twice: in `gg-ipc` (Rust) and in `ui/src/lib/types.ts` (TypeScript), kept
  in step by hand.

**The candidates:** Slint, GTK4 (with relm4), iced and Qt. Slint was chosen because:
- its markup is close to Svelte's (properties, bindings, `if` and `for`, components with
  callbacks), so each `.svelte` panel ports to a `.slint` file directly, and the client logic in
  `daemon.svelte.ts` becomes Rust beside it;
- it has a software renderer, so a page can be drawn headless, for screenshots and checks;
- it used the least memory of the four;
- it is offered under GPLv3 (besides its royalty-free and commercial licences), which fits
  gglinux's GPL-3.0-or-later.

**The Slint app**, as built (Slint 1.18.1):
- **49 to 59 MB** PSS, shown or hidden in the tray, on Hyprland with NVIDIA's 615 driver (open
  kernel module). It draws with OpenGL (Slint's femtovg renderer) and needs no NVIDIA setting.
- Slint has no tray and no colour picker; it has `hsv()` and `to-hsv()` to build one.
- Slint 1.18 needs Rust 1.92 (its `rust-version`); the daemon's crates still build with 1.87.
- In a scrolling page, Slint 1.18 lays out a `Text` with `wrap: word-wrap` inside an `if` or `for`
  at the wrong width, and the card around it comes out shorter than its content.
- `ScrollView` takes `content-width` and `content-height` (`viewport-*` are deprecated names).

**The desktop:**
- StatusNotifierItem is the tray protocol both desktops speak: Plasma's own tray, and on Hyprland a
  bar with a tray (Waybar's `tray` module). `ksni` 0.3 implements it on `zbus`, which the daemon
  already uses.
- Since M9 the desktop entry is `io.github.ykk.gglinux.desktop`, because xdg-desktop-portal 1.20
  and later name an app only from a desktop entry called after its app id. The window's Wayland
  app id has to be the same string.

## Design

### One crate

`crates/gglinux-ui`, package and binary `gglinux`, is a member of the cargo workspace and uses
`gg-ipc`'s types directly, so there's no second copy of the protocol. Slint is built with the
winit backend, the femtovg (OpenGL) renderer for the window, the software renderer and system
fonts for `shot`, and accessibility. The style is `fluent-dark` (`GG_STYLE` at build time
overrides it).

### The window: `ui/*.slint`

One file per page, as the Svelte UI had one component per panel:

| File | What | From |
|---|---|---|
| `app.slint` | the window, sidebar, pages, the `Gg` global (connected, parts, page), the offline view | `App.svelte`, `app.css` |
| `devices.slint` | the Engine page, device list, mouse and headset panels | `DeviceCard`, `MousePanel`, `HeadsetPanel` |
| `keyboard.slint` | lighting, switches, the keyboard's screen | `KeyboardPanel`, `SwitchesPanel`, `ScreenPanel` |
| `sonar.slint` | Sonar and the EQ editor | `SonarPanel`, `EqEditor` |
| `pages.slint` | Games, Moments, Parts, Profiles, Settings | the panels of the same names |
| `widgets.slint` | `Card`, `Button`, `Toggle`, `Segmented`, `ColorPick`, `Board`, `Shown`, ... | `app.css`, `Icon.svelte` |
| `theme.slint` | the old UI's colours and sizes | `app.css` |

Each page's values live in a Slint global (`MouseUi`, `KeyboardUi`, `SonarUi`, ...): plain
properties for what the daemon said, callbacks for what the user does. Anything that appears and
disappears and holds wrapped text goes in `Shown { shown: ...; }` (a container in `widgets.slint`
that takes no room while hidden) rather than an `if`, because of the layout fault above.

### The Rust side: `src/`

- `client.rs`: one thread holds the socket (tokio), reconnects every second while the daemon is
  away, and keeps `State`, the daemon's last answer for every part, current from its
  notifications. `command` sends a change and, once the daemon says it worked, reads that part's
  state again, since only the daemon knows what was applied. Requests time out after 5 s, or 15 s
  for those that restart PipeWire or the recorder.
- `lib.rs`: `show` fills the globals from `State`; `wire` connects each callback to a request;
  `act` and `act_then` mark a panel busy and show its error; `Forms` holds the working copies
  (DPI stages, key colours, EQ bands, ...) in Rust, so their keep-or-reset rules are unit-tested;
  `poke` runs a named step for `shot`.
- `engine.rs`, `keyboard.rs`, `sonar.rs` with `eq.rs` (the curve from PipeWire's biquad
  formulas), `pages.rs`: one module per page, each with its own `poke` steps.
- `system.rs`: the autostart entry, settings backups, opening a folder (`xdg-open`), and files
  picked through the portal's file chooser (`ashpd`).
- `shell.rs` and `main.rs`: the desktop parts below.

### Desktop parts

- **App id.** `slint::set_xdg_app_id("io.github.ykk.gglinux")` after the platform is set up and
  before the window exists, so the window gets the desktop entry's icon and the portals know it.
- **One copy at a time.** The app asks for the session-bus name `io.github.ykk.gglinux` without
  queueing and without allowing a takeover. If the name is taken, it calls
  `io.github.ykk.gglinux.App.Activate(hidden)` on `/io/github/ykk/gglinux` and exits; the running
  copy shows its window unless the new launch was `--hidden` too. With no session bus it runs
  without the guard. A debug build asks for `io.github.ykk.gglinux.dev`, so `cargo run` doesn't
  hand over to an installed copy and quit.
- **Tray.** `ksni`: clicking the icon or **Show gglinux** shows the window, **Quit** ends the event
  loop. If no tray takes the icon (no StatusNotifierWatcher), closing the window quits and
  `--hidden` shows the window anyway, so the app can't end up running with no way to reach it.
- **Close.** The close request hides the window; the event loop runs until **Quit**
  (`run_event_loop_until_quit`). Showing it again from the tray asks for a redraw, because a
  Wayland window only appears with its first frame.
- **Start at login** writes the same `~/.config/autostart/gglinux.desktop` as before
  (`Exec="<program>" --hidden`) and enables the daemon's unit; on Hyprland, which doesn't read
  autostart entries, Settings shows the line to add instead.

### Pictures without a display

`examples/shot.rs` installs a Slint platform with a `MinimalSoftwareWindow` and an event-loop
proxy that queues the client thread's work, waits for the daemon's answers, runs scripted steps
(`SHOT_DO`, through `poke`), and saves the frame as a PNG. `SHOT_SCROLL` scrolls the page first,
to a pixel offset or to a card or divider it finds in the drawn page, and `SHOT_FIT` trims the
picture to the page. `scripts/screenshots.sh` draws the README's pictures that way, each against a
fresh fake daemon in a temporary runtime folder, with no display or session bus.

### Build, install, package, CI

- `install-local.sh` builds `-p gglinuxd -p gglinux --bins` in one cargo run (`--no-ui` drops the
  app) and installs `~/.local/bin/gglinux`, the desktop entry and the icon as before.
- The PKGBUILD fetches once in `prepare()` and builds both packages with `--frozen`. Its depends
  are what the binaries link or load. Linked: glibc, libgcc, systemd-libs (the daemon's libudev)
  and fontconfig (the app's only library beyond the C library). Loaded at run time: wayland,
  libxkbcommon and libglvnd, with libx11 and libxcursor optional, for X11. No more webkit2gtk-4.1,
  gtk3, libsoup3, libayatana-appindicator, nodejs or npm.
- CI is one job: `cargo fmt --check`, clippy with `-D warnings` and the tests, for the whole
  workspace, with `libudev-dev` and `libfontconfig-dev` installed.
- Release builds are stripped (`[profile.release] strip = true`); the symbol table was most of
  the app's size.

### Kept as it was

The look, the page structure and wording, the desktop entry and app id, the autostart entry,
the backup bundle (version 1, now also carrying the switch, screen, screen-care and parts
settings), and the `gglinux` command. The old UI's web storage folder
(`~/.local/share/io.github.ykk.gglinux`) is no longer written.

## Considered, not chosen

- **GTK4 with relm4** and **iced:** both build the window in Rust code rather than in markup, so
  each panel would be rewritten in another shape instead of ported line by line.
- **Qt:** a C++ toolkit, reached from Rust through younger bindings, with a much larger runtime to
  depend on.

The memory each would hold was part of the comparison; Slint's was the lowest.

## Testing

**Unit** (`#[cfg(test)]` in the app's modules): the working copies and their keep-or-reset rules,
the EQ curve, picture conversion for the keyboard's screen, the autostart entry and its quoting,
backups, the debug build's bus name, `--hidden`, and the tray icon's pixel format.

**Checks:** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked`, as CI runs them.

**Pictures:** every page, and the states the fake daemon can reach, drawn with `shot` and looked
at, panel by panel, beside the Svelte original.

**Live (the owner's desktop):**
1. Done on Hyprland with NVIDIA 615: the window, the tray, a second launch showing the first
   copy's window, closing into the tray, memory.
2. Still to do: Plasma, an X11 session, start at login with `--hidden`, the portal's file dialogs,
   a backup and restore, and `makepkg -si` of the package.
