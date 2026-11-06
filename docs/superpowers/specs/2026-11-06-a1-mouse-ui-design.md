# a1-mouse-ui: separate X and Y DPI, and a key picker for mouse buttons

Audit items M06 and M07. App only: `crates/gglinux-ui/src/mouse.rs` and `ui/mouse.slint`.

## What

- **Separate X and Y DPI.** The panel's working copy (`MouseForm`) keeps both axes of each stage
  (`Stage { x, y }`) and a `split` flag. A "Separate X and Y" checkbox in the DPI STAGES heading
  sets it. Off (the default), each stage has one box that sets both axes, as before. On, each stage
  has an X box and a Y box. Apply sends both axes.
- **Key picker.** The button menu keeps its 15 actions and gains "Keyboard key…" and "Media key…".
  Each opens a list under that button's row: a search box over `gg_ipc::mouse_keys::KEYS` or
  `MEDIA` labels. Clicking a key (or Enter, for the first match) makes it the button's name and
  closes the list. A button bound to a key shows its menu at "Keyboard key…" and a line under it,
  "Key: F13".

## Protocol source

Nothing new is framed. Both features use requests that already exist and already reach the mouse
through ggctl:

- `Request::SetMouseDpi { stages: Vec<DpiStage { x, y }> }`. The packet is
  `gg-device-mouse::proto::sensitivity_packet`, from rivalcfg's Rival 3 Wireless Gen 2 profile
  (pinned rivalcfg 779ea52, `sensitivity`: `value_type: multidpi_range_choice_xy`,
  `xy_mapping: xxyy`; evidence `f8b07b7eda7f2122.txt` lines 15-24). Its byte test, including the
  fixed-offset Y block (rivalcfg #273), lives in gg-device-mouse.
- `Request::SetMouseButtons { actions: Vec<String> }`. Names are `ButtonAction::parse` names. The
  lists are `gg_ipc::mouse_keys`. A gglinuxd test (`the_apps_key_names_are_the_mouse_codes_own`)
  checks that each name there is one the mouse code takes, as the right kind.

No experimental gate applies: these are the same writes the panel and ggctl already make, and
nothing is written until the user clicks Apply.

## Decisions

- **Seeding.** The form is seeded from what the daemon applied, Y included. `split` turns on when
  any applied stage has x ≠ y, so an asymmetric setup isn't flattened by showing one box.
- **Linking again.** Turning "Separate X and Y" off sets each stage's Y to its X. Otherwise the one
  box would show X while a hidden, different Y was sent.
- **Editing.** While linked, a box (and the Y setter, if called) sets both axes. While split, each
  box sets its own axis. Typing is only kept in range; snapping to 100 happens per axis on commit
  and before sending, as before.
- **The list is inline, not a popup.** A `PopupWindow` isn't drawn by the headless `shot` example,
  and an inline list keeps the button it belongs to in view. It sits in a `Shown` under its row.
  Each row is a `spacing: 0` layout, so hidden lines take no room and the default layout is
  unchanged.
- **The "Key:" line goes under the menu, not beside it.** Beside the 220 px menu there is about
  58 px left in the side-by-side layout, too narrow for "Key: Print Screen".
- **While a list is open,** its button's menu shows "Keyboard key…" or "Media key…". Cancel puts
  the menu back on the button's real action. Picking an action from the menu closes that button's
  list. Opening a list on another button moves it there. Reset to factory closes it.
- **Search.** The search matches label or name, any case. Results are ordered: the exact match
  first, then labels or names that start with the search (or have a word that does: "alt" finds
  "Left Alt"), then any other match. Enter picks the first one.
- **Names the app doesn't know** (daemon aliases such as `bksp`, set with ggctl) keep menu index -1
  and show "Set elsewhere: bksp". They're sent back unchanged on Apply. The media keys are all in
  the action list too, so a media key picked from its list shows as that action.

## Unverified

- Whether the Rival 3 Wireless Gen 2 really tracks differently per axis with asymmetric stages
  hasn't been tried on the owner's mouse. The packet layout is rivalcfg's. gglinuxd already sends
  asymmetric stages from ggctl.
- The key list is drawn by `shot`. Typing into it, keyboard focus on open, and scrolling the list
  inside the page have not been tried with a real pointer.
