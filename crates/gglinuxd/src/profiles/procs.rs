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
    if let Some(arg0) = cmdline.split(|&b| b == 0).next().filter(|a| !a.is_empty()) {
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
