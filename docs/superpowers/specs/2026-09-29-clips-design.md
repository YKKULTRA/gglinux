# Clips (M7b) — design

**Status:** decided 2026-09-29 by the author (the owner asked not to review specs). **Milestone:**
M7, part b. **Needs** `gpu-screen-recorder` (CachyOS `cachyos-extra-v3`, 6.1.3), which isn't
installed. gglinux builds and tests without it, and the feature switches itself on once it's there.

## Goal

Instant replay, like GG's Moments:
- gglinux keeps the last N seconds of the screen and audio in a replay buffer;
- a hotkey, a button in the UI, or `ggctl clip save` writes them to a file;
- the Moments page lists the clips, deletes them and opens their folder.

Out of scope:
- automatic per-game clips;
- editing or trimming;
- uploading;
- recording without the replay buffer.

## Facts

From PROTOCOLS.md §5, plus the gpu-screen-recorder docs:
- **Replay mode:** `gpu-screen-recorder -w portal -f <fps> -r <seconds> -c mp4 -o <dir>`.
- **Signals:** `SIGUSR1` saves the buffer as a file in `<dir>`; `SIGINT` stops it cleanly.
- **Capture and encoding:** `-w portal` is the KDE-Wayland capture (a one-time "share screen"
  dialog; `-restore-portal-session yes` remembers the choice). NVENC is picked automatically on the
  RTX 4090.
- **Other flags:** `-q <medium|high|very_high|ultra>`; `-a default_output` / `-a default_input`
  add audio tracks; `-replay-storage disk` keeps the buffer off RAM (not used; RAM is the
  default).
- **The hotkey:** the right mechanism on Plasma 6 is `org.freedesktop.portal.GlobalShortcuts`,
  since the daemon can't see keys (M4a's security choice). KDE asks the user once to confirm the
  binding. Crate: `ashpd`.

## Design

**Settings** (`gg-ipc::ClipsSettings`, saved in `~/.config/gglinux/clips.json`):

| Field | Default | Range |
|---|---|---|
| `enabled` | **false** | |
| `seconds` | 30 | 5–300 |
| `fps` | 60 | 15–240 |
| `quality` | `high` | medium, high, very_high, ultra |
| `desktop_audio` | true | |
| `mic` | false | |
| `dir` | `None` → `~/Videos/gglinux` | |

**The recorder** (daemon `clips.rs`, a tokio task):
- **Command line:** a pure `argv(&settings, dir)`, so it can be tested.
- **Desktop audio:** `-a default_output`. With Sonar on, the default output is the Game channel
  alone, so it's `-a device:<node.name>.monitor` for the output Sonar's channels play into, whose
  monitor carries every channel. Sonar's worker says when that changes (Sonar turned on or off, or
  another output chosen), and a recorder taking desktop audio starts again there.
- **Starting:** while enabled and the program is found on `PATH`, it runs the recorder as a child
  process, creating `dir` if needed.
- **Crashes:** a child that exits on its own is restarted after 5 s, doubling to at most 60 s. A
  child that has run for 60 s resets the delay. A start that fails (`dir` can't be created yet, the
  program can't be run) is retried on the same backoff.
- **Settings changes:** stop with `SIGINT` (waiting up to 3 s, then `kill`), then start again with
  the new flags.
- **Save:** `SIGUSR1` to the child. After 1.5 s the clip list is re-read, and the new file is
  announced in a `clips_changed` event.
- **Status:** `available` (on `PATH`), `recording` (child running), `error` (the last failure
  message).

**The clip library:**
- The files in `dir` with extension `mp4`, `mkv` or `webm`, newest first, at most 200:
  `{name, bytes, modified}` (seconds since the epoch).
- `delete_clip { name }` accepts only a plain file name (no `/`, not `.` or `..`) with a video
  extension that exists in `dir`.

**The hotkey** (`clips/hotkey.rs`):
- While clips are enabled, it binds a GlobalShortcuts portal session with the shortcut
  `save-clip` ("Save the last clip"), preferred trigger `ALT+S`.
- Each activation saves. The trigger KDE reports is shown in the UI.
- Failures (no portal, the owner declined) are logged once and shown as "no hotkey; use the button
  or `ggctl clip save`".

**IPC:**
- `ClipInfo { name, bytes, modified }`;
- `ClipsState { available, enabled, recording, settings, dir, clips, error: Option<String>, hotkey: Option<String> }`;
- requests: `get_clips_state`, `set_clips_settings { settings }`, `save_clip`,
  `delete_clip { name }`;
- event: `clips_changed { state }`.

**UI:** the **Moments** page (replacing "coming"):
- when `gpu-screen-recorder` is missing, the install command
  (`sudo pacman -S gpu-screen-recorder`);
- the on/off switch, with a note that the screen stays in a rolling buffer in memory while on;
- the settings, **Save clip**, and the hotkey status;
- the clip list with size and date, Delete, and **Open folder** (a Tauri command running
  `xdg-open` on the directory, only if it's an existing directory).

## Testing

**Unit:**
- `argv` for each setting combination, including audio tracks and the quality flag;
- settings validation and defaults, and old or unreadable files;
- the restart backoff (pure: the delay after an exit, given the run time and the previous delay);
- the clip listing: extensions, order, limit;
- delete-name safety (`../x.mp4`, `a/b.mp4` and `notes.txt` refused).

**In process:** the recorder is started with a stand-in program — a shell script that records the
signals it receives into a file — to test start, save (`SIGUSR1`), restart after an exit, and
`SIGINT` on stop.

**Hardware (the owner):**
1. Install `gpu-screen-recorder` and turn clips on; accept KDE's screen-share and shortcut
   dialogs.
2. Play something, then press Alt+S: a file appears in `~/Videos/gglinux` and in the list.
3. Delete it from the UI.
