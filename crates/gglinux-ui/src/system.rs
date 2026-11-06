// SPDX-License-Identifier: GPL-3.0-or-later
//! Desktop chores for the app: starting at login, backing settings up, and
//! asking the desktop for a file. Paths and the command runner are passed in,
//! so all of it is testable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const BACKUP_FILES: [&str; 9] = [
    "lighting.json",
    "switches.json",
    "screen.json",
    "screen-care.json",
    "gamesense.json",
    "profiles.json",
    "clips.json",
    "sonar.json",
    "parts.json",
];
const ENTRY: &str = "gglinux.desktop";
const SERVICE: &str = "gglinuxd.service";

/// Where the program is installed. Once its file is replaced on disk (an
/// upgrade while it sat in the tray), Linux reports the running copy as
/// `<path> (deleted)`; the new one is at `<path>`.
pub fn installed_exe(exe: &Path) -> PathBuf {
    exe.to_str()
        .and_then(|s| s.strip_suffix(" (deleted)"))
        .map(PathBuf::from)
        .unwrap_or_else(|| exe.to_path_buf())
}

/// The command that starts the UI hidden in the tray, quoted for a shell: what a
/// desktop that doesn't read autostart entries (Hyprland) runs at login instead.
pub fn tray_command(exe: &Path) -> String {
    let path = exe.to_string_lossy();
    let plain = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c));
    if plain {
        format!("{path} --hidden")
    } else {
        format!("'{}' --hidden", path.replace('\'', r"'\''"))
    }
}

/// Whether this desktop runs autostart entries itself. Hyprland doesn't, unless
/// a session manager such as uwsm does it for it.
pub fn reads_autostart(desktop: &str) -> bool {
    !desktop.to_ascii_lowercase().contains("hyprland")
}

/// The autostart entry: the UI, hidden in the tray.
pub fn autostart_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=gglinux\n\
         Comment=SteelSeries control, in the tray\n\
         Exec={} --hidden\n\
         Icon=gglinux\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n",
        exec_arg(&exe.to_string_lossy())
    )
}

/// One `Exec=` argument, quoted per the Desktop Entry spec. Inside the quotes,
/// `"`, `` ` ``, `$` and `\` take a backslash and `%` is doubled; then the key's
/// own string escaping doubles every backslash again, so a literal `\` ends up
/// as four of them.
fn exec_arg(arg: &str) -> String {
    let mut out = String::from('"');
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str(r"\\");
                out.push(c);
            }
            '\\' => out.push_str(r"\\\\"),
            '%' => out.push_str("%%"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn autostart_on(dir: &Path) -> bool {
    dir.join(ENTRY).is_file()
}

/// Start gglinux at login, or stop doing so: the UI's autostart entry, and the
/// daemon's user service. The entry is handled even if `systemctl` fails.
pub fn set_autostart(
    dir: &Path,
    exe: &Path,
    on: bool,
    run: &dyn Fn(&[&str]) -> Result<(), String>,
) -> Result<(), String> {
    let path = dir.join(ENTRY);
    if on {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        std::fs::write(&path, autostart_entry(exe))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    } else if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    }
    let verb = if on { "enable" } else { "disable" };
    run(&["systemctl", "--user", verb, SERVICE])
        .map_err(|e| format!("the tray part is done, but the daemon's service: {e}"))
}

/// Restart the daemon's service so it loads restored settings; true when it did.
/// `try-restart` also succeeds on a stopped service, leaving it stopped (and a
/// daemon started by hand running on its old settings), so the answer is
/// whether the service is running once it's done.
pub fn restart_daemon(run: &dyn Fn(&[&str]) -> Result<(), String>) -> bool {
    run(&["systemctl", "--user", "try-restart", SERVICE]).is_ok()
        && run(&["systemctl", "--user", "is-active", SERVICE]).is_ok()
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

/// A backup's files, once the whole bundle checks out.
fn checked(text: &str) -> Result<(serde_json::Map<String, Value>, Option<u64>), String> {
    let mut bundle: Value =
        serde_json::from_str(text).map_err(|e| format!("not a gglinux backup: {e}"))?;
    if bundle["gglinux_backup"] != 1 {
        return Err("not a gglinux backup this version understands".into());
    }
    let created = bundle["created"].as_u64().filter(|t| *t > 0);
    let Some(Value::Object(files)) = bundle.get_mut("files").map(Value::take) else {
        return Err("the backup has no files".into());
    };
    if let Some(bad) = files.keys().find(|k| !BACKUP_FILES.contains(&k.as_str())) {
        return Err(format!("the backup names an unknown file {bad:?}"));
    }
    Ok((files, created))
}

/// What a backup holds, checked as a restore checks it; nothing is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupInfo {
    pub files: Vec<String>,
    /// When it was made, in Unix seconds, if it says.
    pub created: Option<u64>,
}

pub fn inspect(text: &str) -> Result<BackupInfo, String> {
    let (files, created) = checked(text)?;
    Ok(BackupInfo {
        files: files.keys().cloned().collect(),
        created,
    })
}

/// Write a backup's files into `config`. Nothing is written unless the whole
/// bundle checks out. Returns the names written.
pub fn import(config: &Path, text: &str) -> Result<Vec<String>, String> {
    let (files, _) = checked(text)?;
    std::fs::create_dir_all(config)
        .map_err(|e| format!("cannot create {}: {e}", config.display()))?;
    let mut wrote = Vec::new();
    for (name, value) in files {
        let path = config.join(&name);
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| format!("cannot write {name}: {e}"))?;
        wrote.push(name);
    }
    Ok(wrote)
}

/// Far more than any backup holds: a file this big was picked by mistake, and
/// isn't read into memory to find that out.
const BACKUP_MAX_BYTES: u64 = 64 << 20;

/// A picked backup's text.
fn read_backup_text(path: &Path) -> Result<String, String> {
    let size = std::fs::metadata(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?
        .len();
    if size > BACKUP_MAX_BYTES {
        return Err(format!(
            "{} is too big to be a gglinux backup",
            path.display()
        ));
    }
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// What the backup at `path` holds, before restoring it.
pub fn read_backup(path: &Path) -> Result<BackupInfo, String> {
    inspect(&read_backup_text(path)?)
}

/// A path as typed: `~/` is home, a pasted `file://` URI is its path, quotes a
/// file manager added are dropped. Only whole paths: a relative one would be
/// relative to wherever the app was started.
pub fn typed_path(typed: &str, home: &Path) -> Result<PathBuf, String> {
    let t = typed.trim();
    let t = ['"', '\'']
        .iter()
        .find_map(|q| t.strip_prefix(*q).and_then(|s| s.strip_suffix(*q)))
        .unwrap_or(t);
    let path = if t.starts_with("file://") {
        file_path(t).ok_or_else(|| format!("{t} isn't a file on this computer"))?
    } else if t == "~" {
        home.to_path_buf()
    } else if let Some(rest) = t.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(t)
    };
    if !path.is_absolute() {
        return Err(format!(
            "type the whole path, starting with / or ~/, not {t:?}"
        ));
    }
    Ok(path)
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
    format!(
        "{y:04}-{m:02}-{d:02}_{:02}-{:02}-{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// Where an export goes: `~/Documents`, or home when there's none.
pub fn export_dir(home: &Path) -> PathBuf {
    let docs = home.join("Documents");
    if docs.is_dir() {
        docs
    } else {
        home.to_path_buf()
    }
}

/// Open a folder in the file manager; only existing directories, given as
/// absolute paths so `xdg-open` can't mistake one for an option.
pub fn open_folder(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || !path.is_dir() {
        return Err(format!("{} is not a folder", path.display()));
    }
    let mut child = std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map_err(|e| e.to_string())?;
    // Reap it when it exits, so each click doesn't leave a zombie behind.
    std::thread::spawn(move || child.wait());
    Ok(())
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// `$XDG_CONFIG_HOME/autostart` (so usually `~/.config/autostart`): the folder
/// beside gglinux's own config folder.
pub fn autostart_dir() -> PathBuf {
    gg_ipc::config_dir().with_file_name("autostart")
}

/// Run a program to the end; when it fails, its stderr is the error.
pub fn run_quietly(argv: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new(argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if err.is_empty() {
        format!("{} {}", argv[0], out.status)
    } else {
        err
    })
}

/// This program, where it's installed.
pub fn this_exe() -> PathBuf {
    std::env::current_exe()
        .map(|e| installed_exe(&e))
        .unwrap_or_else(|_| "gglinux".into())
}

/// Write a backup of every setting in `config` into `dir`; returns its path.
pub fn export_to(config: &Path, dir: &Path, now: u64) -> Result<PathBuf, String> {
    let bundle = export(config, now);
    let path = dir.join(format!("gglinux-backup-{}.json", stamp(now)));
    let text = serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

/// Write a backup of every setting into `~/Documents` (or home); returns its path.
pub fn export_settings() -> Result<PathBuf, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    export_to(&gg_ipc::config_dir(), &export_dir(&home()), now)
}

/// Restore the backup at `path` into `config` and restart the daemon so it
/// loads it; says what happened. Blocks: the restart waits for the daemon to
/// stop and start again.
pub fn import_file(
    config: &Path,
    path: &Path,
    run: &dyn Fn(&[&str]) -> Result<(), String>,
) -> Result<String, String> {
    let wrote = import(config, &read_backup_text(path)?)?;
    if wrote.is_empty() {
        return Ok("The backup holds no settings, so nothing changed.".into());
    }
    let restarted = restart_daemon(run);
    Ok(format!(
        "Restored {}. {}",
        wrote.join(", "),
        if restarted {
            "gglinuxd restarted to load them."
        } else {
            "Restart gglinuxd to load them."
        }
    ))
}

/// [`import_file`] into gglinux's own settings.
pub fn import_settings(path: &Path) -> Result<String, String> {
    import_file(&gg_ipc::config_dir(), path, &run_quietly)
}

/// Ask the desktop for a file through its portal (the same dialog other apps
/// get); `None` when the user cancels. The filter is a label and its MIME types,
/// or globs (`*.json`) for files whose type the desktop may not know.
pub async fn pick_file(title: &str, filter: (&str, &[&str])) -> Result<Option<PathBuf>, String> {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
    let mut f = FileFilter::new(filter.0);
    for pattern in filter.1 {
        f = if pattern.contains('*') {
            f.glob(pattern)
        } else {
            f.mimetype(pattern)
        };
    }
    let request = SelectedFiles::open_file()
        .title(title)
        .modal(true)
        .multiple(false)
        .filter(f)
        .send()
        .await
        .map_err(|e| format!("the desktop's file chooser didn't open: {e}"))?;
    match request.response() {
        Ok(files) => Ok(files.uris().first().and_then(|u| file_path(u.as_str()))),
        Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => Ok(None),
        Err(e) => Err(format!("the file chooser failed: {e}")),
    }
}

/// A `file://` URI as a path: the portal answers with those.
pub fn file_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // An authority (host) before the path: only the local one is a file here.
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| (b as char).to_digit(16);
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            // Two hex digits; anything else is a literal `%`.
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

pub async fn pick_picture() -> Result<Option<PathBuf>, String> {
    pick_file(
        "A picture or animation for the keyboard's screen",
        (
            "Pictures",
            &[
                "image/png",
                "image/apng",
                "image/jpeg",
                "image/gif",
                "image/webp",
                "image/bmp",
            ],
        ),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::ffi::OsStringExt;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gglinux-ui-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_tray_command_is_quoted_for_a_shell_only_when_it_must_be() {
        assert_eq!(
            tray_command(Path::new("/home/u/.local/bin/gglinux")),
            "/home/u/.local/bin/gglinux --hidden"
        );
        assert_eq!(
            tray_command(Path::new("/opt/my apps/it's/gglinux")),
            r"'/opt/my apps/it'\''s/gglinux' --hidden"
        );
    }

    #[test]
    fn hyprland_is_the_desktop_that_skips_autostart_entries() {
        assert!(!reads_autostart("Hyprland"));
        assert!(reads_autostart("KDE"));
        assert!(reads_autostart("GNOME"));
    }

    #[test]
    fn the_autostart_entry_quotes_the_program() {
        let e = autostart_entry(Path::new("/opt/my apps/gglinux"));
        assert!(e.starts_with("[Desktop Entry]\n"));
        assert!(
            e.contains("\nExec=\"/opt/my apps/gglinux\" --hidden\n"),
            "{e}"
        );
        assert!(e.contains("\nType=Application\n") && e.contains("\nName=gglinux\n"));
        // `$` and `\` are escaped for the quotes, then backslashes again for the key.
        let e = autostart_entry(Path::new(r"/opt/a$b\c"));
        assert!(e.contains(r#"Exec="/opt/a\\$b\\\\c" --hidden"#), "{e}");
    }

    #[test]
    fn a_replaced_program_autostarts_from_where_it_is_installed() {
        assert_eq!(
            installed_exe(Path::new("/usr/bin/gglinux (deleted)")),
            Path::new("/usr/bin/gglinux")
        );
        assert_eq!(
            installed_exe(Path::new("/usr/bin/gglinux")),
            Path::new("/usr/bin/gglinux")
        );
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
        assert_eq!(
            *calls.borrow(),
            [
                "systemctl --user enable gglinuxd.service",
                "systemctl --user disable gglinuxd.service",
            ]
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_failing_service_step_is_reported_not_fatal() {
        let d = scratch("autostart-fail");
        let err = set_autostart(&d, Path::new("/usr/bin/gglinux"), true, &|_: &[&str]| {
            Err("Unit not found".into())
        })
        .unwrap_err();
        assert!(err.contains("Unit not found"));
        assert!(autostart_on(&d), "the tray part still happened");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_restart_counts_only_when_the_service_is_running_after_it() {
        // `try-restart` succeeds on a stopped service without starting it, which
        // is also how it looks when gglinuxd was started by hand.
        let calls = RefCell::new(Vec::new());
        let stopped = |argv: &[&str]| {
            calls.borrow_mut().push(argv.join(" "));
            match argv[2] {
                "is-active" => Err("systemctl exit status: 3".into()),
                _ => Ok(()),
            }
        };
        assert!(!restart_daemon(&stopped));
        assert_eq!(
            *calls.borrow(),
            [
                "systemctl --user try-restart gglinuxd.service",
                "systemctl --user is-active gglinuxd.service",
            ]
        );
        assert!(restart_daemon(&|_: &[&str]| Ok(())));
        assert!(!restart_daemon(&|_: &[&str]| {
            Err("Unit gglinuxd.service not found.".into())
        }));
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
        assert_eq!(
            names,
            ["lighting.json", "profiles.json"],
            "unknown and unreadable files are left out"
        );
        let to = scratch("backup-to");
        let wrote = import(&to, &bundle.to_string()).unwrap();
        assert_eq!(wrote, ["lighting.json", "profiles.json"]);
        let back: Value =
            serde_json::from_str(&std::fs::read_to_string(to.join("lighting.json")).unwrap())
                .unwrap();
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
        assert_eq!(
            std::fs::read_dir(&to).unwrap().count(),
            0,
            "nothing written"
        );
        std::fs::remove_dir_all(to).unwrap();
    }

    #[test]
    fn a_backup_is_written_where_asked_and_restored_from_its_file() {
        let config = scratch("roundtrip-config");
        std::fs::write(config.join("parts.json"), r#"{"sonar":false}"#).unwrap();
        std::fs::write(config.join("sonar.json"), r#"{"enabled":true}"#).unwrap();
        let docs = scratch("roundtrip-docs");
        let path = export_to(&config, &docs, 1_790_606_404).unwrap();
        assert_eq!(path, docs.join("gglinux-backup-2026-09-28_14-40-04.json"));

        let info = read_backup(&path).unwrap();
        assert_eq!(info.files, ["parts.json", "sonar.json"]);
        assert_eq!(info.created, Some(1_790_606_404));

        let to = scratch("roundtrip-to");
        let calls = RefCell::new(Vec::new());
        let run = |argv: &[&str]| {
            calls.borrow_mut().push(argv.join(" "));
            Ok(())
        };
        let note = import_file(&to, &path, &run).unwrap();
        assert_eq!(
            note,
            "Restored parts.json, sonar.json. gglinuxd restarted to load them."
        );
        assert_eq!(calls.borrow().len(), 2, "try-restart, then is-active");
        let back: Value =
            serde_json::from_str(&std::fs::read_to_string(to.join("parts.json")).unwrap()).unwrap();
        assert_eq!(back["sonar"], false);

        // A daemon not run by systemd keeps its old settings until restarted.
        let note = import_file(&to, &path, &|_: &[&str]| Err("not loaded".into())).unwrap();
        assert!(note.ends_with("Restart gglinuxd to load them."), "{note}");
        for d in [config, docs, to] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    #[test]
    fn looking_at_a_backup_writes_nothing() {
        let to = scratch("inspect");
        let info =
            inspect(r#"{"gglinux_backup":1,"created":0,"files":{"lighting.json":{}}}"#).unwrap();
        assert_eq!(info.files, ["lighting.json"]);
        assert_eq!(info.created, None, "a zero stamp says nothing");
        assert!(inspect(r#"{"gglinux_backup":1,"files":{"../x":{}}}"#).is_err());
        assert!(inspect(r#"{"gglinux_backup":1,"files":[]}"#).is_err());
        assert_eq!(std::fs::read_dir(&to).unwrap().count(), 0);
        std::fs::remove_dir_all(to).unwrap();
    }

    #[test]
    fn an_empty_backup_changes_nothing_and_restarts_nothing() {
        let d = scratch("empty-backup");
        let file = d.join("b.json");
        std::fs::write(&file, r#"{"gglinux_backup":1,"created":5,"files":{}}"#).unwrap();
        let config = d.join("config");
        let note = import_file(&config, &file, &|_: &[&str]| -> Result<(), String> {
            panic!("restarted for nothing")
        })
        .unwrap();
        assert_eq!(note, "The backup holds no settings, so nothing changed.");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_missing_or_huge_file_is_refused_before_it_is_read() {
        let d = scratch("huge");
        assert!(read_backup(&d.join("nope.json"))
            .unwrap_err()
            .contains("cannot read"));
        let big = d.join("video.mp4");
        // Sparse: no 64 MB written to disk.
        std::fs::File::create(&big)
            .unwrap()
            .set_len(BACKUP_MAX_BYTES + 1)
            .unwrap();
        assert!(read_backup(&big).unwrap_err().contains("too big"));
        let config = d.join("config");
        assert!(import_file(&config, &big, &|_: &[&str]| Ok(())).is_err());
        assert!(!config.exists(), "nothing written");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn typed_paths_are_whole_paths() {
        let home = Path::new("/home/u");
        let ok = |t: &str| typed_path(t, home).unwrap();
        assert_eq!(
            ok("~/Documents/b.json"),
            Path::new("/home/u/Documents/b.json")
        );
        assert_eq!(ok("  /tmp/b.json "), Path::new("/tmp/b.json"));
        assert_eq!(ok("'/tmp/my b.json'"), Path::new("/tmp/my b.json"));
        assert_eq!(ok("\"/tmp/b.json\""), Path::new("/tmp/b.json"));
        assert_eq!(ok("file:///tmp/my%20b.json"), Path::new("/tmp/my b.json"));
        assert_eq!(ok("~"), home);
        for bad in ["b.json", "Documents/b.json", "", "file://elsewhere/b.json"] {
            assert!(typed_path(bad, home).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(
            file_path("file:///home/u/a%20b.png"),
            Some(PathBuf::from("/home/u/a b.png"))
        );
        assert_eq!(
            file_path("file://localhost/tmp/x"),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(
            file_path("file:///tmp/100%25.png"),
            Some(PathBuf::from("/tmp/100%.png"))
        );
        assert_eq!(
            file_path("file:///tmp/end%2"),
            Some(PathBuf::from("/tmp/end%2"))
        );
        assert_eq!(
            file_path("file:///tmp/a%zz"),
            Some(PathBuf::from("/tmp/a%zz")),
            "not hex: a literal %"
        );
        assert_eq!(
            file_path("file:///tmp/%E9t%C3%A9"),
            Some(PathBuf::from(std::ffi::OsString::from_vec(
                b"/tmp/\xe9t\xc3\xa9".to_vec()
            ))),
            "bytes, UTF-8 or not"
        );
        assert_eq!(file_path("https://example.com/a.png"), None);
        assert_eq!(file_path("file://otherhost/a.png"), None);
    }

    #[test]
    fn stamps_are_utc_date_and_time() {
        assert_eq!(stamp(0), "1970-01-01_00-00-00");
        assert_eq!(stamp(1_790_606_404), "2026-09-28_14-40-04");
        assert_eq!(stamp(951_782_400), "2000-02-29_00-00-00", "a leap day");
    }
}
