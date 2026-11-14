# C3 profiles: every device section, switching on the focused window, gglinux's own presets

Unit c3-profiles of M11 wave C (audit rows P01, P02, P03). Built and tested against fakes only: no device was
touched, no real Hyprland socket was opened, no session bus was used. Sonar is not a section (the owner's decision of
2026-11-13).

## What

- **Profiles v2 (P01).** A profile holds any of six sections, each optional: headset (the settings `HeadsetChange`
  has), mouse (DPI stages, polling rate, buttons; sleep timer and radio only with `mouse_power`), keyboard switches,
  key bindings (with on/off), keyboard screen, lighting. Saving takes the current state of the ticked sections;
  applying sends each included section through the daemon's own request handlers, in the fixed order headset, mouse,
  switches, bindings, screen, lighting.
- **Switching on the focused window (P02).** A rule now says what it matches (a program, or a window class) and when
  it acts (while the program runs, or while its window has the focus). The focused window comes from Hyprland's event
  socket, or from gglinux's KWin script on KDE Plasma; where neither reports it, a focus rule acts while its program
  runs (the old process mode).
- **Presets (P03).** gglinux's own presets, shipped as profile files: Competitive FPS, MOBA, Typing, Default
  switches. Each sets only the keyboard switches and says in its own words that it is gglinux's own, not
  SteelSeries'. They can be applied as they are, or copied into a saved profile to change.
- **Sharing.** A profile is exported as a JSON file (`{"format":"gglinux-profile","version":2,"about":…,
  "profile":…}`) and imported from one (a bare profile, as `profiles.json` holds them, reads too).

## Sources

- QuickSet, SteelSeries' blog (audit evidence `83acfbb6c4b05636.txt`, source id `quickset`): "As soon as you switch
  windows or close your game or close, your settings return to their default state", and the default "DESKTOP
  Profile" covering keyboards, mice and headsets. This is the behaviour of focus rules and of the Default.
- SteelSeries Engine page (`a8b5abea53fb2b66.txt`, id `engine`): profiles "load automatically when a familiar game
  application is in the foreground".
- The half-second wait before a focus change counts (`focus::DEBOUNCE`) is gglinux's own choice; GG documents none.
- Every preset value is gglinux's own (none is copied from GG or a vendor), and each preset file says so.

## Decisions

1. **Sections go through `dispatch`.** Profiles get the daemon's workers as a `Daemon` port (`profiles/daemon.rs`:
   each request is serialized and answered by `crate::dispatch`, the same function a client's request reaches). So a
   section is refused exactly when the user's own request would be (the part is off, the gate is off, the device is
   away), with the same words. Profiles requests are refused through the port, so profiles never wait on themselves.
2. **A gated section is skipped with a note, never sent.** Before applying, profiles read the gates
   (`get_experimental`): bindings without `keyboard_bindings`, switches with Protection Mode on without
   `keyboard_protection`, the mouse's sleep timer and radio without `mouse_power`, the wheel's two actions (when not
   the factory pair) without `mouse_wheel` (the six buttons still go), and each headset change whose
   `HeadsetChange::feature` gate is off. Each skip or refusal is a line in `ProfilesState::applied.notes`, shown in the
   panel and by `ggctl profile apply`.
3. **Only what the device saves is sent only when it differs.** The headset saves each change on its base station,
   and the mouse saves its sleep timer; those are compared with what the device reports and sent only where they
   differ. Everything else (RAM settings: lighting, switches, screen, bindings, mouse DPI/polling/buttons/radio, all
   with `save: false`) is sent whole each time. Nothing else is saved to a device's memory.
4. **Readings lag; what was just sent counts.** The headset's controls and the mouse's sleep timer are readings that
   come a second or more after a change, and fakes lag on every read. For 5 s after profiles sent a section, what it
   sent counts over what the daemon reads back (`sections::Sent`), for diffs, for keeping your own settings and for
   saving. Tested with a fake daemon that answers the pre-change state on the first read after a change.
5. **The Default.** When a rule's profile goes on, your own settings for each section it holds are read first and
   kept (`Store::before`, saved in `profiles.json`, as v1 kept the lighting). Switching from rule A to rule B applies
   B over the Default, so a section A had and B hasn't comes back as yours. When no rule holds, the Default is applied
   and forgotten. A manual apply doesn't touch it. A daemon that stops while a rule holds puts the Default back when
   it starts and no rule holds.
6. **Rules in list order; the first that holds wins.** A focus rule holds while its window has the focus (by any of
   the program's names, or by window class, ignoring case); a while-running rule holds while the program runs. A class
   rule must act on focus. While the focused window isn't followed (no rule on focus, no desktop reporting it, the
   source stopped), focus rules act while their program runs, and class rules don't act; the panel says why.
7. **Focus sources.** Started only while a rule acts on focus, and stopped when none does. Hyprland: the folder named
   by `HYPRLAND_INSTANCE_SIGNATURE`, else the newest `$XDG_RUNTIME_DIR/hypr/*` with a `.socket2.sock` (a daemon started
   by systemd often lacks Hyprland's variables). `activewindowv2>>ADDR` is looked up in `j/clients` on `.socket.sock`
   (class and pid), the class of the last `activewindow>>` line standing in when the lookup fails; `j/activewindow`
   gives the window it starts on. The pid's names come from `/proc/<pid>/comm`, `cmdline` and `exe`, so Proton games
   match by their `.exe`. KDE Plasma: the daemon serves `io.github.ykk.gglinux.Focus` at `/io/github/ykk/gglinux/Focus`
   (method `WindowActivated(s class, i pid)`) on the session bus; the KWin script
   (`crates/gglinuxd/src/profiles/kwin/gglinux-focus`) calls it on `workspace.windowActivated` (Plasma 6) or
   `clientActivated` (Plasma 5). Plasma counts as reporting only after the first call. A source that stops is looked
   for again after 10 s.
8. **Debounce.** A reported window counts once it has held for 0.5 s, so Alt-Tabbing past a window doesn't apply its
   profile. A source stopping counts at once.
9. **No new experimental gate.** Profiles add no new device command: every section is an existing request, gated as
   it is. The new behaviours are opt-in by what the user saves and picks (sections ticked, a rule set to "on focus"),
   and version-1 files keep their meaning exactly (lighting only, while running). Revisit if the owner wants focus
   following itself behind a flag.
10. **Version 1 reads losslessly.** `Profile.lighting` is the same field, now optional; v1 rules have no `by`/`when`
    and read as program/while-running; v1's `before` (a bare `Lighting`) reads as a Default holding lighting. The first
    time a v1 file is read, a copy is kept as `profiles.json.v1`, since a v1 gglinux can't read v2 files. Tested with
    a v1 file in the exact shape v1's types serialize to.
11. **Wiring.** `main.rs`'s profile routing arm hands profiles the workers on each profiles request (only the first
    one sends them). Until then automatic switching updates games-only clips but applies nothing. The lead's one-line
    snippet in the report connects them at start instead, so rules act before any client has asked.

## Wire

- Requests: `save_profile {name, sections?}`, `apply_profile {name, preset?}`, `get_profile {name, preset?}` →
  `profile {file}`, `import_profile {profile, replace?}`, `copy_profile {from, preset?, to?}`; `get_profiles`,
  `delete_profile`, `set_game_rules` as before. New fields are `#[serde(default)]`, so v1 clients' requests still read.
- `GameRule {process, profile, by: program|class, when: running|focused}`.
- `ProfilesState` adds `sections` (per profile), `presets`, `focus {source, window {class, program}, note}`,
  `applied {profile, notes}`, `default_sections`; all `#[serde(default)]`.

## Not done / open

- Nothing here was tried on the owner's desktop: the Hyprland path is proven with a fake socket pair, the KWin
  receiver by calling its method; the script itself has never run in KWin.
- Installing the KWin script is a hands-on step (report); packaging it is the lead's.
- Profiles hold no Sonar settings, no keyboard macros (they live in onboard slots), no screen care, no mouse sensor
  settings.
