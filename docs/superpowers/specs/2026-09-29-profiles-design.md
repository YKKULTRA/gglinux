# Profiles and per-game switching (M7a) — design

**Status:** decided 2026-09-29 by the author, since the owner asked not to review specs.
**Milestone:** M7, part a.

## Goal

The owner saves the keyboard's lighting as named **profiles** and switches between them by hand,
or **automatically when a game starts**: a rule "when `RDR2.exe` runs, use *Western*" applies the
profile when the process appears, and puts the previous lighting back when it exits.

Out of scope:
- profiles for Sonar or the headset (the headset keeps settings on-device; Sonar isn't verified
  live yet);
- PrismSync, since the keyboard is the only RGB device;
- matching by Steam app ID.

## Design

**Data** (`gg-ipc`):
- `Profile { name, lighting: Lighting }`;
- `GameRule { process, profile }`;
- `ProfilesState { profiles: Vec<String>, rules: Vec<GameRule>, active: Option<GameRule>, running: Vec<String> }`.
  `active` is the rule currently applied. `running` is filled only in answers to
  `get_profiles`, not in events, because it changes all the time.

**Persistence:** `~/.config/gglinux/profiles.json` = `{ profiles: [Profile], rules: [GameRule], before: Lighting | null }`,
written atomically. Unreadable → empty, with a warning, the file moved aside to
`profiles.json.unreadable-<unix seconds>` so saving can't destroy it. `before` is saved because
`SetLighting` saves a game's profile as the lighting, so a restart mid-game would otherwise lose the
owner's.

**Requests:**
- `get_profiles`;
- `save_profile { name }` — a snapshot of the current lighting; replaces a profile of the same
  name;
- `apply_profile { name }`;
- `delete_profile { name }` — refused while a rule uses it;
- `set_game_rules { rules }` — each rule's profile must exist and each process name must be
  non-empty.

**Event:** `profiles_changed { state }`.

**Validation:** names are 1–64 characters after trimming.

**Watcher** (daemon `profiles.rs`, a tokio task, every 2 s):
- Scans `/proc/<pid>` entries owned by the owner's uid.
- A process's names are its `comm` and the basename of `argv[0]`, splitting on both `/` and `\`,
  so Proton's `Z:\…\RDR2.exe` gives `RDR2.exe`. A title rewritten as the whole command line in
  one string (Chromium, Electron, node) is cut before its first option, so a renderer is `chrome`;
  a dash word with a path separator before any `=` (`Odd -Name\odd.exe`) carries on the path.
- Matching is case-insensitive. The first rule, in list order, whose process is running is
  *picked*.

**Transitions (pure):**
- nothing active → picked: save the current lighting as `before`, then apply the picked profile;
- active A → picked B (another rule): apply B, keeping the original `before`;
- active → nothing picked: restore `before`;
- after a restart mid-game, a saved `before` is kept if a rule is picked, and restored at once if
  none is;
- a rule naming a missing profile is skipped;
- a manual `apply_profile` while a rule is active changes the lighting now, and the rule's exit
  still restores `before`.

Lighting is read and set through the keyboard worker's existing `GetState` and `SetLighting`
commands, so validation and saving stay in one place.

**UI:** the **Settings** page (replacing its "coming" note) gets:
- Profiles: save the current lighting as…, apply, delete;
- Game rules: a process field with suggestions from the running processes, a profile picker, add
  and remove;
- "Now: *profile* because *process* is running".

## Testing

**Unit:**
- names from `comm` and `cmdline`, including a Windows path;
- a fake `/proc` tree: numeric entries only, and entries without `cmdline` still give `comm`;
- the picked rule follows list order and ignores case;
- every transition;
- validation and the persistence round trip.

**Integration (in process):** a fake keyboard handle is replaced by a closure-based lighting
store. Apply, rule start, rule end, and restoring `before`.

**Hardware (the owner):** save two profiles, make a rule for a game (or for `kate`), start it and
watch the lighting switch, then close it and watch it switch back.
