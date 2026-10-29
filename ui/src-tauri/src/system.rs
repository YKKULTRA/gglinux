//! Desktop chores for the UI: starting at login, and backing settings up.
//! Paths and the command runner are passed in, so all of it is testable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const BACKUP_FILES: [&str; 5] = [
    "lighting.json",
    "gamesense.json",
    "profiles.json",
    "clips.json",
    "sonar.json",
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

/// Write a backup's files into `config`. Nothing is written unless the whole
/// bundle checks out. Returns the names written.
pub fn import(config: &Path, text: &str) -> Result<Vec<String>, String> {
    let bundle: Value =
        serde_json::from_str(text).map_err(|e| format!("not a gglinux backup: {e}"))?;
    if bundle["gglinux_backup"] != 1 {
        return Err("not a gglinux backup this version understands".into());
    }
    let files = bundle["files"]
        .as_object()
        .ok_or("the backup has no files")?;
    if let Some(bad) = files.keys().find(|k| !BACKUP_FILES.contains(&k.as_str())) {
        return Err(format!("the backup names an unknown file {bad:?}"));
    }
    std::fs::create_dir_all(config)
        .map_err(|e| format!("cannot create {}: {e}", config.display()))?;
    let mut wrote = Vec::new();
    for (name, value) in files {
        let path = config.join(name);
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| format!("cannot write {name}: {e}"))?;
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
    fn stamps_are_utc_date_and_time() {
        assert_eq!(stamp(0), "1970-01-01_00-00-00");
        assert_eq!(stamp(1_790_606_404), "2026-09-28_14-40-04");
        assert_eq!(stamp(951_782_400), "2000-02-29_00-00-00", "a leap day");
    }
}
