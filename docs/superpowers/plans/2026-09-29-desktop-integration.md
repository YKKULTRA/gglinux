# Desktop integration and packaging (M7d) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:**
- `ggctl` as a real command, with clip/profile/GameSense commands;
- the UI starts hidden in the tray;
- start at login;
- settings backup and restore;
- `install-local.sh` installs the UI;
- an Arch `PKGBUILD`.

**Architecture:**
- **`ggctl`:** moves from `examples/` to `src/bin/`, with its argument parsing made pure.
- **Autostart and backup:** Tauri commands whose logic lives in a pure module in the Tauri app
  (`ui/src-tauri/src/system.rs`), unit-tested with `cargo test` in that crate.
- **Packaging:** files under `data/` and `packaging/arch/`.

**Tech Stack:** Rust (Tauri 2 app crate, gglinuxd), bash, makepkg, Svelte 5.

**Spec:** `docs/superpowers/specs/2026-09-29-desktop-integration-design.md`

**Depends on:** M7c done (stacked branch `m7d-desktop`).

## Global Constraints

- **Backup files:** exactly `lighting.json`, `gamesense.json`, `profiles.json`, `clips.json`,
  `sonar.json`. The bundle is `{"gglinux_backup":1,"created":<unix secs>,"files":{name: json}}`.
  Import refuses another version or an unknown name, and writes each file atomically.
- **Export path:** `~/Documents/gglinux-backup-YYYY-MM-DD_HH-MM-SS.json` (UTC), or `$HOME/…` when
  there's no `~/Documents`.
- **After import:** `systemctl --user try-restart gglinuxd.service`.
- **Autostart:** `~/.config/autostart/gglinux.desktop` with `Exec="<exe>" --hidden`, plus
  `systemctl --user enable|disable gglinuxd.service`. On iff the file exists.
- **Window:** the main window is `visible: false` in `tauri.conf.json`; `setup` shows it unless
  `--hidden`.
- **`ggctl` commands:** `(none)` watch; `mouse …` (unchanged); `clip save`; `profile list`;
  `profile apply <name>`; `gamesense test`; `gamesense on|off`.
- **`PKGBUILD`:** `pkgname=gglinux-git`. Never run a full `makepkg` build in the session (long),
  and never publish anything.
- **Checks and style:** as before, plus `cargo test --manifest-path ui/src-tauri/Cargo.toml`.

## Review Focus

1. **An import bundle with `"../../.bashrc"` as a file name.** Refused, because only the five known
   names are allowed. Pinned by the Task 2 test `unknown_or_path_like_names_are_refused`.
2. **An executable path with spaces** in the autostart `Exec=`. It must be quoted per the Desktop
   Entry spec. Pinned by the Task 2 test `the_autostart_entry_quotes_the_program`.
3. **`systemctl` missing the unit** (a dev checkout). Autostart still writes the desktop file and
   reports the service part failed. Pinned by the Task 2 test
   `a_failing_service_step_is_reported_not_fatal`, via an injected runner.
4. **Starting hidden with the daemon down.** The tray still works: the UI's reconnect loop is
   unchanged. No test; the ledger notes the decision.
5. **`ggctl profile apply` with a name containing spaces.** The rest of the arguments join into the
   name. Pinned by the Task 1 test `profile_names_may_contain_spaces`.

---

### Task 1: `ggctl` becomes a binary

**Files:** `git mv crates/gglinuxd/examples/ggctl.rs crates/gglinuxd/src/bin/ggctl.rs`; `README.md`
(its `ggctl` lines)

**Interfaces:**
- Produces `enum Action { Watch, Send(Request) }` and `fn parse(args: &[String]) -> Result<Action>`
  in the binary, with tests.

- [ ] **Step 1: Move it and write the failing tests**

After the `git mv`, add to the end of `ggctl.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn sends(line: &str) -> Request {
        match parse(&args(line)).unwrap() {
            Action::Send(r) => r,
            Action::Watch => panic!("{line} parsed as watch"),
        }
    }

    #[test]
    fn no_arguments_watches() {
        assert!(matches!(parse(&[]).unwrap(), Action::Watch));
    }

    #[test]
    fn the_new_commands_map_to_their_requests() {
        assert!(matches!(sends("clip save"), Request::SaveClip));
        assert!(matches!(sends("profile list"), Request::GetProfiles));
        assert!(matches!(sends("gamesense test"), Request::GamesenseTest));
        assert!(matches!(sends("gamesense on"), Request::SetGamesenseEnabled { enabled: true }));
        assert!(matches!(sends("gamesense off"), Request::SetGamesenseEnabled { enabled: false }));
        assert!(matches!(sends("mouse"), Request::GetMouseState));
        assert!(matches!(sends("mouse polling 1000"), Request::SetMousePolling { hz: 1000, save: false }));
    }

    #[test]
    fn profile_names_may_contain_spaces() {
        match sends("profile apply Late Night") {
            Request::ApplyProfile { name } => assert_eq!(name, "Late Night"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mistakes_are_errors() {
        for bad in ["clip", "clip delete", "profile", "profile apply", "gamesense maybe", "frobnicate"] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }
}
```

- [ ] **Step 2: Run** `cargo test -p gglinuxd --bin ggctl`. Expected: compile errors (`parse`,
  `Action` missing).

- [ ] **Step 3: Implement**

- The doc comment at the top lists the new commands. Replace the `cargo run … --example ggctl`
  line with "Installed as `ggctl`; from a checkout: `cargo run -p gglinuxd --bin ggctl`", and add:

```text
//! ggctl clip save                         # save the instant replay (bind this to a key)
//! ggctl profile list                      # saved lighting profiles
//! ggctl profile apply <name>
//! ggctl gamesense test|on|off
```

- Add:

```rust
/// What the command line asks for.
enum Action {
    /// Stream everything the daemon sends until it goes quiet.
    Watch,
    Send(Request),
}

fn parse(args: &[String]) -> Result<Action> {
    let save = args.iter().any(|a| a == "--save");
    let positional: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with("--")).collect();
    Ok(Action::Send(match positional.split_first() {
        None => return Ok(Action::Watch),
        Some((&"mouse", rest)) => mouse_request(rest, args, save)?,
        Some((&"clip", ["save"])) => Request::SaveClip,
        Some((&"clip", _)) => bail!("usage: ggctl clip save"),
        Some((&"profile", ["list"])) => Request::GetProfiles,
        Some((&"profile", ["apply", name @ ..])) if !name.is_empty() => Request::ApplyProfile { name: name.join(" ") },
        Some((&"profile", _)) => bail!("usage: ggctl profile list | ggctl profile apply <name>"),
        Some((&"gamesense", ["test"])) => Request::GamesenseTest,
        Some((&"gamesense", ["on"])) => Request::SetGamesenseEnabled { enabled: true },
        Some((&"gamesense", ["off"])) => Request::SetGamesenseEnabled { enabled: false },
        Some((&"gamesense", _)) => bail!("usage: ggctl gamesense test|on|off"),
        Some((other, _)) => bail!("unknown command '{other}' (try: mouse, clip, profile, gamesense, or nothing to watch)"),
    }))
}
```

- `main` becomes: collect `args`; `let action = parse(&args)?;`; connect; `match action { Action::Watch => watch(stream), Action::Send(request) => { … existing response printing … } }`.
- In the printing, add an arm before `other =>`:

```rust
        ResponseData::Profiles { state } => {
            for name in state.profiles {
                println!("{name}");
            }
        }
```

- The `--save` note keeps working: compute `save` and `positional` in `main` the same way, for
  the note only.
- README: every `cargo run -p gglinuxd --example ggctl` becomes `cargo run -p gglinuxd --bin ggctl`,
  and the section mentions the installed `ggctl` and the new commands.

- [ ] **Step 4: Run** `cargo test -p gglinuxd --bin ggctl` (4 pass), the workspace tests and
  clippy (`0`). Also run `cargo run -q -p gglinuxd --bin ggctl -- frobnicate; echo "exit=$?"`.
  Expected: the error message and `exit=1`, without connecting first. Parse before connecting.
- [ ] **Step 5: Commit** — `ggctl: a real command, with clip, profile and GameSense commands`

---

### Task 2: Autostart and backup logic (`ui/src-tauri/src/system.rs`)

**Files:** Create `ui/src-tauri/src/system.rs`; `ui/src-tauri/src/main.rs` (`mod system;`)

**Interfaces:**
- Produces (pure or path-injected):
  - `autostart_entry(exe: &Path) -> String`;
  - `set_autostart(dir: &Path, exe: &Path, on: bool, run: &dyn Fn(&[&str]) -> Result<(), String>) -> Result<(), String>`;
  - `autostart_on(dir: &Path) -> bool`;
  - `BACKUP_FILES: [&str; 5]`;
  - `export(config: &Path, created: u64) -> serde_json::Value`;
  - `import(config: &Path, text: &str) -> Result<Vec<String>, String>`;
  - `stamp(unix: u64) -> String` (`YYYY-MM-DD_HH-MM-SS`, UTC).

- [ ] **Step 1: Failing tests** (`system.rs` with `todo!()` bodies, then):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gglinux-ui-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_autostart_entry_quotes_the_program() {
        let e = autostart_entry(Path::new("/opt/my apps/gglinux"));
        assert!(e.starts_with("[Desktop Entry]\n"));
        assert!(e.contains("\nExec=\"/opt/my apps/gglinux\" --hidden\n"), "{e}");
        assert!(e.contains("\nType=Application\n") && e.contains("\nName=gglinux\n"));
    }

    #[test]
    fn autostart_writes_and_removes_the_entry_and_the_service_follows() {
        let d = scratch("autostart");
        let calls = RefCell::new(Vec::new());
        let run = |argv: &[&str]| {
            calls.borrow_mut().push(argv.join(" "));
            Ok(())
        };
        set_autostart(&d, Path::new("/usr/bin/gglinux"), true, &run).unwrap();
        assert!(autostart_on(&d));
        set_autostart(&d, Path::new("/usr/bin/gglinux"), false, &run).unwrap();
        assert!(!autostart_on(&d));
        assert_eq!(*calls.borrow(), [
            "systemctl --user enable gglinuxd.service",
            "systemctl --user disable gglinuxd.service",
        ]);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_failing_service_step_is_reported_not_fatal() {
        let d = scratch("autostart-fail");
        let err = set_autostart(&d, Path::new("/usr/bin/gglinux"), true, &|_: &[&str]| Err("Unit not found".into())).unwrap_err();
        assert!(err.contains("Unit not found"));
        assert!(autostart_on(&d), "the tray part still happened");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn backups_round_trip_the_known_files() {
        let from = scratch("backup-from");
        std::fs::write(from.join("lighting.json"), r#"{"enabled":true}"#).unwrap();
        std::fs::write(from.join("profiles.json"), r#"{"profiles":[]}"#).unwrap();
        std::fs::write(from.join("notes.json"), r#"{"x":1}"#).unwrap();
        std::fs::write(from.join("clips.json"), "{ broken").unwrap();
        let bundle = export(&from, 1_790_000_000);
        assert_eq!(bundle["gglinux_backup"], 1);
        let names: Vec<&String> = bundle["files"].as_object().unwrap().keys().collect();
        assert_eq!(names, ["lighting.json", "profiles.json"], "unknown and unreadable files are left out");
        let to = scratch("backup-to");
        let wrote = import(&to, &bundle.to_string()).unwrap();
        assert_eq!(wrote, ["lighting.json", "profiles.json"]);
        let back: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(to.join("lighting.json")).unwrap()).unwrap();
        assert_eq!(back["enabled"], true);
        std::fs::remove_dir_all(from).unwrap();
        std::fs::remove_dir_all(to).unwrap();
    }

    #[test]
    fn unknown_or_path_like_names_are_refused() {
        let to = scratch("backup-bad");
        for bad in [
            r#"{"gglinux_backup":1,"created":0,"files":{"../../.bashrc":{}}}"#,
            r#"{"gglinux_backup":1,"created":0,"files":{"other.json":{}}}"#,
            r#"{"gglinux_backup":2,"created":0,"files":{}}"#,
            r#"not json"#,
        ] {
            assert!(import(&to, bad).is_err(), "{bad}");
        }
        assert_eq!(std::fs::read_dir(&to).unwrap().count(), 0, "nothing written");
        std::fs::remove_dir_all(to).unwrap();
    }

    #[test]
    fn stamps_are_utc_date_and_time() {
        assert_eq!(stamp(0), "1970-01-01_00-00-00");
        assert_eq!(stamp(1_790_606_404), "2026-09-28_14-40-04");
        assert_eq!(stamp(951_782_400), "2000-02-29_00-00-00", "a leap day");
    }
}
```

- [ ] **Step 2: Run** `cargo test --manifest-path ui/src-tauri/Cargo.toml system`. Expected: FAIL,
  `not yet implemented`. The first build compiles Tauri, which takes a while.

- [ ] **Step 3: Implement**

```rust
//! Desktop chores for the UI: starting at login, and backing settings up.
//! Paths and the command runner are passed in, so all of it is testable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const BACKUP_FILES: [&str; 5] = ["lighting.json", "gamesense.json", "profiles.json", "clips.json", "sonar.json"];
const ENTRY: &str = "gglinux.desktop";
const SERVICE: &str = "gglinuxd.service";

/// The autostart entry: the UI, hidden in the tray.
pub fn autostart_entry(exe: &Path) -> String {
    let exe = exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "[Desktop Entry]\nType=Application\nName=gglinux\nComment=SteelSeries control, in the tray\n\
         Exec=\"{exe}\" --hidden\nIcon=gglinux\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n"
    )
}

pub fn autostart_on(dir: &Path) -> bool {
    dir.join(ENTRY).is_file()
}

/// Start gglinux at login, or stop doing so: the UI's autostart entry, and the
/// daemon's user service. The entry is handled even if `systemctl` fails.
pub fn set_autostart(dir: &Path, exe: &Path, on: bool, run: &dyn Fn(&[&str]) -> Result<(), String>) -> Result<(), String> {
    let path = dir.join(ENTRY);
    if on {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        std::fs::write(&path, autostart_entry(exe)).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    } else if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    }
    run(&["systemctl", "--user", if on { "enable" } else { "disable" }, SERVICE])
        .map_err(|e| format!("the tray part is done, but the daemon's service: {e}"))
}

/// Every known settings file that exists and parses.
pub fn export(config: &Path, created: u64) -> Value {
    let files: BTreeMap<&str, Value> = BACKUP_FILES
        .iter()
        .filter_map(|&name| {
            let text = std::fs::read_to_string(config.join(name)).ok()?;
            serde_json::from_str(&text).ok().map(|v| (name, v))
        })
        .collect();
    json!({ "gglinux_backup": 1, "created": created, "files": files })
}

/// Write a backup's files into `config`. Nothing is written unless the whole
/// bundle checks out. Returns the names written.
pub fn import(config: &Path, text: &str) -> Result<Vec<String>, String> {
    let bundle: Value = serde_json::from_str(text).map_err(|e| format!("not a gglinux backup: {e}"))?;
    if bundle["gglinux_backup"] != 1 {
        return Err("not a gglinux backup this version understands".into());
    }
    let files = bundle["files"].as_object().ok_or("the backup has no files")?;
    if let Some(bad) = files.keys().find(|k| !BACKUP_FILES.contains(&k.as_str())) {
        return Err(format!("the backup names an unknown file {bad:?}"));
    }
    std::fs::create_dir_all(config).map_err(|e| e.to_string())?;
    let mut wrote = Vec::new();
    for (name, value) in files {
        let path = config.join(name);
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &path)).map_err(|e| format!("cannot write {name}: {e}"))?;
        wrote.push(name.clone());
    }
    Ok(wrote)
}

/// `YYYY-MM-DD_HH-MM-SS` in UTC (days-to-civil, after Howard Hinnant).
pub fn stamp(unix: u64) -> String {
    let (days, secs) = ((unix / 86_400) as i64, unix % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}_{:02}-{:02}-{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Where an export goes: `~/Documents`, or home when there's none.
pub fn export_dir(home: &Path) -> PathBuf {
    let docs = home.join("Documents");
    if docs.is_dir() { docs } else { home.to_path_buf() }
}
```

`serde_json` is already a dependency of the Tauri crate. Add `mod system;` to `main.rs`.

- [ ] **Step 4: Run** `cargo test --manifest-path ui/src-tauri/Cargo.toml`. Expected: 6 passed. If
  `stamp(1_790_606_404)` isn't `2026-09-28_14-40-04`, check it with
  `date -u -d @1790606404 +%F_%H-%M-%S` and fix the expectation, not the algorithm, if `date` agrees
  with the code; ledger it.
- [ ] **Step 5: Commit** — `UI: start-at-login and settings-backup logic`

---

### Task 3: Tauri commands, hidden start, and the Settings panel

**Files:** `ui/src-tauri/src/main.rs`, `ui/src-tauri/tauri.conf.json`, `ui/src/lib/app.ts` (new),
`ui/src/lib/SettingsPanel.svelte` (new), `ui/src/App.svelte`

- [ ] **Step 1: Commands and the hidden start** (`main.rs`)

```rust
fn home() -> std::path::PathBuf {
    std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default()
}

fn run_quietly(argv: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new(argv[0]).args(&argv[1..]).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[tauri::command]
fn get_autostart() -> bool {
    system::autostart_on(&home().join(".config/autostart"))
}

#[tauri::command]
fn set_autostart(enabled: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    system::set_autostart(&home().join(".config/autostart"), &exe, enabled, &run_quietly)
}

/// Write a backup; returns its path.
#[tauri::command]
fn export_settings() -> Result<String, String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
    let bundle = system::export(&gg_ipc::config_dir(), now);
    let path = system::export_dir(&home()).join(format!("gglinux-backup-{}.json", system::stamp(now)));
    std::fs::write(&path, serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Restore a backup and restart the daemon so it loads it. Returns what happened, for the UI.
#[tauri::command]
fn import_settings(text: String) -> Result<String, String> {
    let wrote = system::import(&gg_ipc::config_dir(), &text)?;
    let restarted = run_quietly(&["systemctl", "--user", "try-restart", "gglinuxd.service"]).is_ok();
    Ok(format!(
        "Restored {}. {}",
        wrote.join(", "),
        if restarted { "gglinuxd restarted to load them." } else { "Restart gglinuxd to load them." }
    ))
}
```

- Register them: `generate_handler![daemon_status, daemon_send, open_folder, get_autostart, set_autostart, export_settings, import_settings]`.
- In `setup`, after `build_tray`:

```rust
            // Autostart passes --hidden: stay in the tray until asked.
            if !std::env::args().any(|a| a == "--hidden") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                }
            }
```

  This needs `use tauri::Manager;` if it isn't imported yet.
- `tauri.conf.json`: add `"visible": false` to the main window.

Run `cargo build --manifest-path ui/src-tauri/Cargo.toml` and `cargo test --manifest-path ui/src-tauri/Cargo.toml`.
Expected: builds; 6 pass.

- [ ] **Step 2: `app.ts`**

```ts
// Desktop chores the UI does itself, not through the daemon.
import { invoke } from "@tauri-apps/api/core";

export const getAutostart = () => invoke<boolean>("get_autostart");
export const setAutostart = (enabled: boolean) => invoke<void>("set_autostart", { enabled });
export const exportSettings = () => invoke<string>("export_settings");
export const importSettings = (text: string) => invoke<string>("import_settings", { text });
export const openFolder = (path: string) => invoke<void>("open_folder", { path });
```

Keep `daemon.openFolder` from M7b if it exists, or point it at this. Don't have two
implementations: re-export or remove one, and ledger which.

- [ ] **Step 3: `SettingsPanel.svelte`**

```svelte
<script lang="ts">
  // Start at login, and backups of every setting.

  import { exportSettings, getAutostart, importSettings, openFolder, setAutostart } from "./app";

  let autostart = $state<boolean | null>(null);
  let busy = $state(false);
  let note = $state<string | null>(null);
  let error = $state<string | null>(null);
  let lastExport = $state<string | null>(null);

  $effect(() => {
    void getAutostart().then((on) => (autostart = on));
  });

  async function run(action: () => Promise<void>) {
    busy = true;
    error = null;
    note = null;
    try {
      await action();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function pick(e: Event & { currentTarget: HTMLInputElement }) {
    const file = e.currentTarget.files?.[0];
    e.currentTarget.value = "";
    if (!file) return;
    void run(async () => {
      note = await importSettings(await file.text());
    });
  }

  const folderOf = (p: string) => p.slice(0, p.lastIndexOf("/")) || "/";
</script>

<article class="panel">
  <section class="first">
    <label class="toggle">
      <input
        type="checkbox"
        checked={autostart ?? false}
        disabled={busy || autostart === null}
        onchange={(e) => {
          const on = e.currentTarget.checked;
          void run(async () => {
            await setAutostart(on);
            autostart = on;
          });
        }}
      />
      Start gglinux when I log in (the daemon, and this window hidden in the tray)
    </label>
  </section>
  <section>
    <div class="head"><h4>Backup</h4></div>
    <div class="row">
      <button class="ghost" disabled={busy} onclick={() => run(async () => { lastExport = await exportSettings(); })}>Back up settings</button>
      <label class="ghost file">
        Restore from a backup…
        <input type="file" accept=".json,application/json" onchange={pick} />
      </label>
    </div>
    {#if lastExport}
      <p class="meta">Saved to <code>{lastExport}</code> <button class="link" onclick={() => run(() => openFolder(folderOf(lastExport!)))}>Open folder</button></p>
    {/if}
    {#if note}<p class="meta">{note}</p>{/if}
  </section>
  {#if error}<p class="error">{error}</p>{/if}
</article>

<style>
  .panel { margin-top: 1rem; padding: 1rem 1.1rem; background: var(--surface); border: 1px solid var(--line); border-radius: 12px; }
  section { margin-top: 1.1rem; padding-top: 1rem; border-top: 1px solid var(--line); }
  section.first { margin-top: 0; padding-top: 0; border-top: none; }
  h4 { margin: 0; font-size: 0.8rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-dim); }
  .head { margin-bottom: 0.6rem; }
  .toggle { display: flex; align-items: center; gap: 0.5rem; font-size: 0.85rem; cursor: pointer; }
  .row { display: flex; gap: 0.6rem; flex-wrap: wrap; }
  .meta { margin: 0.6rem 0 0; font-size: 0.8rem; color: var(--text-dim); }
  code { font-size: 0.78rem; }
  .ghost { padding: 0.3rem 0.65rem; font-size: 0.8rem; color: var(--text-dim); background: none; border: 1px solid var(--line); border-radius: 7px; cursor: pointer; }
  .ghost:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .ghost:disabled { opacity: 0.4; cursor: default; }
  .file input { display: none; }
  .link { padding: 0; font-size: 0.8rem; color: var(--accent); background: none; border: none; cursor: pointer; }
  .error { margin: 0.8rem 0 0; padding: 0.5rem 0.7rem; font-size: 0.8rem; color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); border-radius: 8px; }
</style>
```

- [ ] **Step 4: `App.svelte`**: render `<SettingsPanel />` after `<ProfilesPanel />` on the Settings
  page. Put it outside the daemon-connected guard: autostart and backup work without the daemon.
  The settings blurb becomes `"Profiles, start at login, backups"`. Run `npm run check`.
- [ ] **Step 5: Commit** — `UI: start at login, backups, and starting hidden in the tray`

---

### Task 4: Installing the UI, and the desktop entry

**Files:** `data/gglinux.desktop` (new), `scripts/install-local.sh`, `README.md`

- [ ] **Step 1: The desktop entry**

```
[Desktop Entry]
Type=Application
Name=gglinux
GenericName=SteelSeries Control
Comment=Open-source SteelSeries GG for Linux
Exec=gglinux
Icon=gglinux
Terminal=false
Categories=Utility;Settings;HardwareSettings;
StartupWMClass=gglinux
```

Run `desktop-file-validate data/gglinux.desktop`. Expected: no output.

- [ ] **Step 2: `install-local.sh`**

- Usage gains `[--no-ui]`, parsed as `do_ui=0`.
- After the daemon install, also run `install -Dm755 "$repo/target/release/ggctl" "$HOME/.local/bin/ggctl"`.
  `cargo build --bins` already builds it.
- Add:

```bash
if [[ $do_ui -eq 1 ]]; then
  if [[ $do_build -eq 1 ]]; then
    echo "==> building the UI (release)"
    (cd "$repo/ui" && npm ci && npm run tauri build -- --no-bundle)
  fi
  echo "==> installing the UI -> ~/.local/bin/gglinux"
  install -Dm755 "$repo/ui/src-tauri/target/release/gglinux" "$HOME/.local/bin/gglinux"
  sed "s|^Exec=.*|Exec=$HOME/.local/bin/gglinux|" "$repo/data/gglinux.desktop" \
    | install -Dm644 /dev/stdin "$HOME/.local/share/applications/gglinux.desktop"
  install -Dm644 "$repo/ui/src-tauri/icons/128x128.png" "$HOME/.local/share/icons/hicolor/128x128/apps/gglinux.png"
fi
```

- The closing message mentions opening gglinux from the app menu, and Settings → Start at login.

Run `bash -n scripts/install-local.sh`. Expected: no output. **Don't run the installer.**

- [ ] **Step 3: README**: the install section says the script now installs the UI and `ggctl`
  (`--no-ui` to skip), and gives the uninstall list:
  - `~/.local/bin/{gglinuxd,gglinux,ggctl}`;
  - the desktop entry and icon;
  - `~/.config/systemd/user/gglinuxd.service`;
  - `~/.config/autostart/gglinux.desktop`;
  - the udev rules;
  - `~/.config/gglinux`.
- [ ] **Step 4: Commit** — `install-local.sh installs the UI, ggctl and a desktop entry`

---

### Task 5: The Arch package

**Files:** `packaging/arch/PKGBUILD`, `packaging/arch/gglinux.install`, `.gitignore` (packaging build
dirs), `PLAN.md`

- [ ] **Step 1: `PKGBUILD`**

```bash
# Maintainer: ykk
pkgname=gglinux-git
pkgver=r0.0000000
pkgrel=1
pkgdesc="Open-source SteelSeries GG for Linux: lighting, headset, Sonar-style audio, GameSense, clips"
arch=('x86_64')
url="https://github.com/ykk/gglinux"
license=('GPL-3.0-or-later')
depends=('systemd-libs' 'webkit2gtk-4.1' 'gtk3' 'libayatana-appindicator' 'pipewire' 'wireplumber')
makedepends=('git' 'cargo' 'nodejs' 'npm')
optdepends=('noise-suppression-for-voice: noise suppression for the Sonar microphone'
            'swh-plugins: compressor for the Sonar microphone'
            'gpu-screen-recorder: instant-replay clips')
provides=('gglinux')
conflicts=('gglinux')
install=gglinux.install
source=("gglinux::git+https://github.com/ykk/gglinux.git")
sha256sums=('SKIP')

pkgver() {
  cd gglinux
  printf "r%s.%s" "$(git rev-list --count HEAD)" "$(git rev-parse --short=7 HEAD)"
}

build() {
  cd gglinux
  cargo build --release --locked -p gglinuxd --bins
  cd ui
  npm ci
  npm run tauri build -- --no-bundle
}

check() {
  cd gglinux
  cargo test --release --locked --workspace
}

package() {
  cd gglinux
  install -Dm755 target/release/gglinuxd "$pkgdir/usr/bin/gglinuxd"
  install -Dm755 target/release/ggctl "$pkgdir/usr/bin/ggctl"
  install -Dm755 ui/src-tauri/target/release/gglinux "$pkgdir/usr/bin/gglinux"
  sed 's|^ExecStart=.*|ExecStart=/usr/bin/gglinuxd|' data/systemd/gglinuxd.service \
    | install -Dm644 /dev/stdin "$pkgdir/usr/lib/systemd/user/gglinuxd.service"
  install -Dm644 data/udev/60-gglinux.rules "$pkgdir/usr/lib/udev/rules.d/60-gglinux.rules"
  install -Dm644 data/udev/61-gglinux-reactive.rules "$pkgdir/usr/share/gglinux/61-gglinux-reactive.rules"
  sed 's|^Exec=.*|Exec=/usr/bin/gglinux|' data/gglinux.desktop \
    | install -Dm644 /dev/stdin "$pkgdir/usr/share/applications/gglinux.desktop"
  install -Dm644 ui/src-tauri/icons/128x128.png "$pkgdir/usr/share/icons/hicolor/128x128/apps/gglinux.png"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
```

- [ ] **Step 2: `gglinux.install`**

```bash
post_install() {
  cat <<'EOF'
==> gglinux: start the daemon for your user with
      systemctl --user enable --now gglinuxd
    then open gglinux from the app menu (Settings -> Start at login keeps both running).
==> Reactive keyboard lighting needs read access to the Apex's typing interfaces,
    which lets any program you run read what you type on it. Opt in only if you want it:
      sudo install -m644 /usr/share/gglinux/61-gglinux-reactive.rules /etc/udev/rules.d/
      sudo udevadm control --reload   # then replug the keyboard
EOF
}

post_upgrade() {
  post_install
}
```

- [ ] **Step 3: Check it**

Run: `cd packaging/arch && bash -n PKGBUILD && makepkg --printsrcinfo > .SRCINFO && namcap PKGBUILD; echo "exit=$?"`
Expected:
- `.SRCINFO` is produced, with `pkgname = gglinux-git` and the `optdepends`;
- `namcap` prints at most informational notes, and `exit=0`;
- no network access (`--printsrcinfo` doesn't fetch sources).

Ledger what `namcap` printed. Add `packaging/arch/src/`, `packaging/arch/pkg/`,
`packaging/arch/gglinux/` and `packaging/arch/*.pkg.tar.zst` to `.gitignore`. Commit `.SRCINFO`
too.

- [ ] **Step 4: PLAN.md**, M7 section:

```markdown
**M7d done in software** (spec `docs/superpowers/specs/2026-09-29-desktop-integration-design.md`):
`ggctl` installed as a command (clip/profile/GameSense), start at login (user service + tray
autostart), settings backup/restore, `install-local.sh` installs the UI, and an Arch `PKGBUILD`
(`gglinux-git`, for when the repo is published — publishing is the owner's call). Skipped:
PrismSync (one RGB device) and virtual surround (optional).
```

- [ ] **Step 5: Final checks and commit**
  - Checks: the workspace tests, the Tauri crate tests, clippy `0`, `npm run check`.
  - Commit message: `packaging: an Arch PKGBUILD for gglinux-git`.

## Hardware acceptance (the owner)

1. `./scripts/install-local.sh --no-build`, or without it to build the UI too.
2. Open gglinux from the menu; Settings → Start at login; log out and in: the tray icon and the
   daemon are up.
3. Back up settings, change the lighting, restore: it comes back.
4. KDE System Settings → Shortcuts → add `ggctl clip save`.
5. Optionally: `cd packaging/arch && makepkg -si` once the repository is published.
