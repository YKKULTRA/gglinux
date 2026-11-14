# Actions on the spare keys F13–F24, and the Experimental panel (M11 unit a6-actions)

Date: 2026-11-06. Audit item M09 (GG's "launch application / text / media" button actions); the Settings
panel for every M11 experimental gate.

## What

- A mouse button is bound **on the mouse** to a spare key F13–F24 (the mouse panel's key picker, with the
  existing `ButtonAction::Key`). The desktop sees that key and reports it to gglinux through a
  GlobalShortcuts portal session of the actions' own: shortcut ids `action-f13` … `action-f24`, app id
  `io.github.ykk.gglinux`. gglinuxd runs the `gg_ipc::Action` bound to the key. gglinux never reads an
  input device.
- Actions: `Run` (a program and its arguments), `Type` (text), `ProfileApply` (a saved profile),
  `ClipSave` (the instant replay), `Media` (a consumer-page usage).
- Daemon: `crates/gglinuxd/src/actions.rs` (+ `actions/{store,run,portal,testing,tests}.rs`), a `Service`
  answering `get_actions`/`set_actions` and publishing `actions_changed`. Saved in
  `~/.config/gglinux/actions.json` (atomic tmp + rename) with the ids the desktop bound last time.
- `ggctl actions [set f13 run|type|profile|clip|media … | rm f13]`; the fake daemon keeps the list and says
  `shortcuts: "bound (fake)"`.
- App, Settings: an **Actions** card (rows of key, name, kind and value; Add, Save, Undo; what the desktop
  bound; whether typing works here; the Hyprland lines) and an **Experimental** card (one switch per
  `gg_ipc::Feature`, each with "Not tried on your hardware yet. Turn on only while testing, with the device
  in view."; a switch sends `set_experimental` with the whole set).

## Sources

- xdg-desktop-portal, `org.freedesktop.portal.GlobalShortcuts` (CreateSession, BindShortcuts,
  ListShortcuts, Activated): "All global shortcuts are bound to a session"; ListShortcuts "returns the
  shortcuts that were successfully bound in a previous session by the application".
- KDE bug 524138 (xdg-desktop-portal-kde): "The shortcuts dialog should not be shown if you are binding
  shortcuts that you already bound in the past."
- ashpd 0.13.13 (the pinned crate): `GlobalShortcuts::{create_session, bind_shortcuts, receive_activated}`,
  `Session::close`, `register_host_app`; the same calls `clips/hotkey.rs` makes for the clip hotkey.
- HID Usage Tables, Consumer page (0x0C): Play/Pause 0xCD, Scan Next 0xB5, Scan Previous 0xB6, Stop 0xB7,
  Mute 0xE2, Volume Increment 0xE9, Volume Decrement 0xEA.
- wtype(1) on the PC: `wtype [OPTION_OR_TEXT]... -- [TEXT]...`. playerctl (`play-pause`, `next`,
  `previous`, `stop`) and WirePlumber's `wpctl set-volume [-l limit] @DEFAULT_AUDIO_SINK@ 5%+`,
  `wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle`.

No device protocol is involved: nothing here writes to a device, so there is no allowlist family and no
experimental gate for the actions themselves.

## Decisions

- **Ask the desktop only on a change.** A portal session binds once, so a new set of keys is bound in a
  new session; the old session is closed only after the new one is bound. If the desktop refuses (KDE
  dialog cancelled, no portal), nothing changes: the old list and the old keys keep working, and the error
  is shown. The set of keys decides a rebind: a new program or name on the same keys is saved without
  asking. At start the daemon binds again only the ids recorded as bound (`bound` in `actions.json`),
  which per the portal spec and KDE bug 524138 needs no dialog; keys never bound wait for the next change.
  While the desktop isn't there yet (the daemon starts at boot), it tries again every 30 s.
- **Seams.** `Spawner` (start a program, find one in `PATH`), `Portal` (bind a set of shortcuts) and
  `Daemon` (apply a profile, save a clip) are traits; tests use `actions::testing::Fakes`, so no test
  starts a program, types text or opens a D-Bus session.
- **No shell anywhere.** `Run` spawns `argv` as given: detached (own process group), in `$HOME`, stdio to
  `/dev/null`, reaped by a thread so it doesn't linger as a zombie, never waited on. `Type` runs
  `wtype -- <text>` (the text one argument). The app splits a typed program line on spaces outside quotes
  (no expansion); `ggctl` splits each word on spaces, so a program's own `--options` go in quotes.
- **Typing** is offered where wtype works: Hyprland (`XDG_CURRENT_DESKTOP` or
  `HYPRLAND_INSTANCE_SIGNATURE`) and sway/river/wayfire/labwc, with `wtype` in `PATH`. On Plasma
  `typing = false`: "Plasma needs its remote-desktop permission for typing; not built yet". A `Type`
  action is still saved there (he uses both desktops) and says why when its key comes in.
- **Media** uses playerctl for play/pause/next/previous/stop and wpctl for volume (capped at 100 %) and
  mute; a missing tool is named in the error.
- **Hyprland** binds portal shortcuts only from its config: the app (and `ggctl actions` on Hyprland)
  shows `hl.bind("F13", hl.dsp.global("io.github.ykk.gglinux:action-f13"))` and
  `bind = , F13, global, io.github.ykk.gglinux:action-f13` for each key.
- **Lag.** The fake answers the first read after a change with the list from before it. The app shows
  what it saved until the daemon shows the same list (`experimental::Pending`), reading back twice
  300 ms apart, then the daemon's word stands. The experimental switches work the same way.

## Not verified

- Nothing here ran against a real portal, KDE or Hyprland: no session was opened (rules of this unit).
  In particular: that KDE shows no dialog when the recorded ids are bound again at start (KDE bug 524138
  suggests some versions did); that a second session from the same connection (beside the clip hotkey's)
  works on xdg-desktop-portal-kde and -hyprland; that `register_host_app` failing the second time on one
  connection is harmless; that `F13` is accepted as a `preferred_trigger`.
- The activation filter takes the session's object path from ashpd's `Serialize` of `Session` (its
  `path()` is crate-private). If that ever fails, every `action-f*` activation of the connection counts.
- Programs started by an action get the daemon's environment. Under systemd that's the user manager's
  environment when gglinuxd started, so with lingering the daemon may lack `WAYLAND_DISPLAY` and the
  desktop variables (programs can't open windows, typing reports "not on this desktop") until it's
  restarted after login. A `systemctl --user restart gglinuxd` kills programs it started (same cgroup).
- A rename on the same keys isn't sent to the desktop (no rebind), so KDE's list keeps the old name.
