//! Which programs are running, by the names a person would recognise.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// The most names `running` returns.
const MAX_NAMES: usize = 1000;

/// A process's names: its `comm`, and the file name of `argv[0]` (Proton gives
/// Windows paths, so both `/` and `\` split).
pub fn names(comm: &str, cmdline: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let comm = comm.trim();
    if !comm.is_empty() {
        out.push(comm.to_string());
    }
    let mut args = cmdline.split(|&b| b == 0);
    if let Some(arg0) = args.next().filter(|a| !a.is_empty()) {
        // Chromium, Electron and node rewrite their title as the whole command
        // line in one string; the program is what comes before its first option.
        // A dash word with a path separator before any `=` carries on the path
        // ("Odd -Name\odd.exe"), so it isn't an option.
        let rewritten = args.all(|a| a.is_empty());
        let is_option = |word: &[u8]| {
            word.iter().take_while(|&&b| b != b' ' && b != b'=').all(|&b| b != b'/' && b != b'\\')
        };
        let option = (0..arg0.len().saturating_sub(2))
            .find(|&i| arg0[i] == b' ' && arg0[i + 1] == b'-' && arg0[i + 2] != b' ' && is_option(&arg0[i + 1..]));
        let arg0 = match option {
            Some(i) if rewritten => &arg0[..i],
            _ => arg0,
        };
        let arg0 = String::from_utf8_lossy(arg0);
        let base = arg0.rsplit(['/', '\\']).next().unwrap_or(&arg0).trim().to_string();
        if !base.is_empty() && !out.iter().any(|n| n.eq_ignore_ascii_case(&base)) {
            out.push(base);
        }
    }
    out
}

/// Every name of every process `uid` owns, once each (ignoring case), sorted.
pub fn running(proc_root: &Path, uid: u32) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(proc_root) else { return Vec::new() };
    // Oldest process first, so which spelling of a name wins doesn't depend on
    // directory order.
    let mut pids: Vec<u32> = entries.flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect();
    pids.sort_unstable();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for pid in pids {
        let dir = proc_root.join(pid.to_string());
        if !std::fs::metadata(&dir).is_ok_and(|m| m.uid() == uid) {
            continue;
        }
        let comm = std::fs::read_to_string(dir.join("comm")).unwrap_or_default();
        let cmdline = std::fs::read(dir.join("cmdline")).unwrap_or_default();
        for n in names(&comm, &cmdline) {
            seen.entry(n.to_lowercase()).or_insert(n);
        }
    }
    seen.into_values().take(MAX_NAMES).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_comm_and_the_program_path() {
        assert_eq!(names("kate\n", b"/usr/bin/kate\0--new\0"), vec!["kate".to_string()], "same name once");
        assert_eq!(names("RDR2.exe\n", b"Z:\\games\\RDR2\\RDR2.exe\0"), vec!["RDR2.exe".to_string()]);
        assert_eq!(names("wine64-preload\n", b"C:\\Games\\Portal\\portal.exe\0"),
            vec!["wine64-preload".to_string(), "portal.exe".to_string()]);
        assert_eq!(names("kworker/0:1\n", b""), vec!["kworker/0:1".to_string()]);
        assert!(names("", b"").is_empty());
    }

    #[test]
    fn a_rewritten_title_gives_its_program_not_its_options() {
        let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(names("chrome\n", b"/opt/google/chrome/chrome --type=renderer --renderer-client-id=7\0\0\0"), owned(&["chrome"]));
        assert_eq!(names("steamwebhelper\n", b"/home/u/.steam/ubuntu12_64/steamwebhelper -lang=en_US -cachedir=/tmp/x\0"),
            owned(&["steamwebhelper"]));
        assert_eq!(names("npm exec @model\n", b"npm exec @modelcontextprotocol/server-pdf --stdio\0\0"),
            owned(&["npm exec @model", "server-pdf"]));
        // A path with spaces, dashes or real arguments is the program as it is.
        assert_eq!(names("wine64-preload\n", b"C:\\Games\\Star Wars - KOTOR\\swkotor.exe\0"), owned(&["wine64-preload", "swkotor.exe"]));
        assert_eq!(names("wine64-preload\n", b"C:\\Games\\Odd -Name\\odd.exe\0-windowed\0"), owned(&["wine64-preload", "odd.exe"]));
    }

    #[test]
    fn a_dash_that_carries_on_the_path_is_not_an_option() {
        let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(names("wine64-preload\n", b"C:\\Games\\Odd -Name\\odd.exe\0"), owned(&["wine64-preload", "odd.exe"]));
        assert_eq!(names("GameThread\n", b"/home/u/Games/My -Game/bin/game\0"), owned(&["GameThread", "game"]));
        assert_eq!(names("game\n", b"/opt/My -Game/game --fullscreen -cfg=/tmp/x.cfg\0\0"), owned(&["game"]));
    }

    #[test]
    fn only_numeric_entries_owned_by_the_user_count() {
        let root = std::env::temp_dir().join(format!("gglinux-proc-{}", std::process::id()));
        for (pid, comm, cmdline) in [("12", "kate\n", &b"/usr/bin/kate\0"[..]), ("34", "RDR2.exe\n", &b"Z:\\x\\RDR2.exe\0"[..]), ("56", "KATE\n", &b""[..])] {
            std::fs::create_dir_all(root.join(pid)).unwrap();
            std::fs::write(root.join(pid).join("comm"), comm).unwrap();
            std::fs::write(root.join(pid).join("cmdline"), cmdline).unwrap();
        }
        std::fs::create_dir_all(root.join("self")).unwrap();
        std::fs::write(root.join("self/comm"), "ignored\n").unwrap();
        let me = unsafe { libc::getuid() };
        assert_eq!(running(&root, me), vec!["kate".to_string(), "RDR2.exe".to_string()], "KATE folds into kate; self is skipped");
        assert!(running(&root, me.wrapping_add(1)).is_empty(), "other users' processes don't count");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
