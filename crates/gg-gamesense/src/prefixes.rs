//! The Proton bridge: `coreProps.json` in every Proton prefix, so Windows games
//! under Proton find the server — Wine maps `C:\ProgramData` to the prefix's
//! `drive_c/ProgramData`, and pressure-vessel shares the host network, so the
//! loopback address works from inside. Only files gglinux wrote are ever
//! changed or removed.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Where GameSense clients look, under `C:\ProgramData`.
const CORE_PROPS: &str = "SteelSeries/SteelSeries Engine 3/coreProps.json";

/// Native Steam's install locations under `home` that exist, deduplicated
/// (`~/.steam/steam` is usually a link to `~/.local/share/Steam`).
pub fn steam_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for p in [home.join(".local/share/Steam"), home.join(".steam/steam")] {
        if let Ok(c) = p.canonicalize() {
            if !roots.contains(&c) {
                roots.push(c);
            }
        }
    }
    roots
}

/// The library folders Steam lists in `steamapps/libraryfolders.vdf` under each root.
pub fn libraries(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut libs = Vec::new();
    for root in roots {
        let Ok(text) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) else { continue };
        for path in text.lines().filter_map(vdf_path) {
            let p = PathBuf::from(path);
            let p = p.canonicalize().unwrap_or(p);
            if !libs.contains(&p) {
                libs.push(p);
            }
        }
    }
    libs
}

/// `"path"  "/some/where"` (tab-separated in the file) → `/some/where`.
fn vdf_path(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("\"path\"")?.trim();
    let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.replace("\\\\", "\\"))
}

/// `drive_c/ProgramData` of every Proton prefix in these libraries, sorted.
pub fn program_data_dirs(libraries: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for lib in libraries {
        let Ok(entries) = std::fs::read_dir(lib.join("steamapps/compatdata")) else { continue };
        for entry in entries.flatten() {
            let pd = entry.path().join("pfx/drive_c/ProgramData");
            if pd.is_dir() {
                dirs.push(pd);
            }
        }
    }
    dirs.sort();
    dirs
}

/// The file gglinux writes.
pub fn contents(port: u16) -> String {
    format!("{{\"address\":\"127.0.0.1:{port}\"}}")
}

/// Whether a coreProps.json is one gglinux wrote: only an `address`, on loopback.
pub fn is_ours(text: &str) -> bool {
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(text) else { return false };
    o.len() == 1 && o.get("address").and_then(Value::as_str).is_some_and(|a| a.starts_with("127.0.0.1:"))
}

pub enum Written {
    Wrote,
    Unchanged,
    /// Someone else's file (a real GG in the prefix); left alone.
    Foreign,
}

/// Put gglinux's coreProps.json in one prefix's `ProgramData`.
pub fn write(program_data: &Path, port: u16) -> io::Result<Written> {
    let file = program_data.join(CORE_PROPS);
    let want = contents(port);
    match std::fs::read_to_string(&file) {
        Ok(have) if have == want => return Ok(Written::Unchanged),
        Ok(have) if !is_ours(&have) => return Ok(Written::Foreign),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let dir = file.parent().expect("CORE_PROPS has a parent");
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("coreProps.json.tmp");
    std::fs::write(&tmp, &want)?;
    std::fs::rename(&tmp, &file)?;
    Ok(Written::Wrote)
}

/// Remove gglinux's coreProps.json from one prefix, and its two directories
/// if that leaves them empty. `true` if a file was removed.
pub fn remove(program_data: &Path) -> io::Result<bool> {
    let file = program_data.join(CORE_PROPS);
    match std::fs::read_to_string(&file) {
        Ok(text) if is_ours(&text) => {
            std::fs::remove_file(&file)?;
            let engine = file.parent().expect("CORE_PROPS has a parent");
            // `remove_dir` refuses a directory that isn't empty: exactly what we want.
            let _ = std::fs::remove_dir(engine);
            let _ = std::fs::remove_dir(engine.parent().expect("two levels deep"));
            Ok(true)
        }
        Ok(_) => Ok(false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether this prefix holds gglinux's file right now.
pub fn holds_ours(program_data: &Path) -> bool {
    std::fs::read_to_string(program_data.join(CORE_PROPS)).is_ok_and(|t| is_ours(&t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A scratch directory, removed when dropped.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Tmp {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir()
                .join(format!("gglinux-prefixes-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A Steam root with a vdf listing itself and `extra`, and prefixes 10 (with ProgramData) and 20 (without).
    fn steam(tmp: &Tmp, extra: &Path) -> PathBuf {
        let root = tmp.0.join("Steam");
        std::fs::create_dir_all(root.join("steamapps/compatdata/10/pfx/drive_c/ProgramData")).unwrap();
        std::fs::create_dir_all(root.join("steamapps/compatdata/20")).unwrap();
        std::fs::write(root.join("steamapps/libraryfolders.vdf"), format!(
            "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"label\"\t\t\"\"\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
            root.display(), extra.display())).unwrap();
        root
    }

    #[test]
    fn libraries_come_from_the_vdf_deduplicated() {
        let tmp = Tmp::new();
        let other = tmp.0.join("Games");
        std::fs::create_dir_all(other.join("steamapps/compatdata/30/pfx/drive_c/ProgramData")).unwrap();
        let root = steam(&tmp, &other);
        let libs = libraries(&[root.clone(), root.clone()]);
        assert_eq!(libs, vec![root.canonicalize().unwrap(), other.canonicalize().unwrap()]);
        let dirs = program_data_dirs(&libs);
        assert_eq!(dirs.len(), 2, "prefix 20 has no ProgramData yet");
        assert!(dirs.iter().all(|d| d.ends_with("pfx/drive_c/ProgramData")));
    }

    #[test]
    fn no_steam_means_no_libraries_and_no_error() {
        let tmp = Tmp::new();
        assert!(libraries(&[tmp.0.join("nope")]).is_empty());
        assert!(program_data_dirs(&[tmp.0.join("nope")]).is_empty());
        assert!(steam_roots(&tmp.0).is_empty());
    }

    #[test]
    fn writing_is_idempotent_and_follows_the_port() {
        let tmp = Tmp::new();
        let pd = tmp.0.join("ProgramData");
        std::fs::create_dir_all(&pd).unwrap();
        assert!(matches!(write(&pd, 5000).unwrap(), Written::Wrote));
        assert!(matches!(write(&pd, 5000).unwrap(), Written::Unchanged));
        let file = pd.join("SteelSeries/SteelSeries Engine 3/coreProps.json");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), r#"{"address":"127.0.0.1:5000"}"#);
        assert!(matches!(write(&pd, 6000).unwrap(), Written::Wrote), "our own file follows the port");
        assert!(holds_ours(&pd));
    }

    #[test]
    fn a_file_that_is_not_ours_is_never_touched() {
        let tmp = Tmp::new();
        let pd = tmp.0.join("ProgramData");
        let dir = pd.join("SteelSeries/SteelSeries Engine 3");
        std::fs::create_dir_all(&dir).unwrap();
        let gg = r#"{"address":"127.0.0.1:51234","encryptedAddress":"x"}"#;
        std::fs::write(dir.join("coreProps.json"), gg).unwrap();
        assert!(matches!(write(&pd, 5000).unwrap(), Written::Foreign));
        assert!(!remove(&pd).unwrap());
        assert_eq!(std::fs::read_to_string(dir.join("coreProps.json")).unwrap(), gg);
        assert!(!holds_ours(&pd));
    }

    #[test]
    fn removing_takes_our_file_and_empty_directories_only() {
        let tmp = Tmp::new();
        let pd = tmp.0.join("ProgramData");
        std::fs::create_dir_all(&pd).unwrap();
        write(&pd, 5000).unwrap();
        assert!(remove(&pd).unwrap());
        assert!(!pd.join("SteelSeries").exists(), "both directories were ours and empty");
        assert!(!remove(&pd).unwrap(), "nothing left to remove");
        write(&pd, 5000).unwrap();
        std::fs::write(pd.join("SteelSeries/other.txt"), "keep").unwrap();
        assert!(remove(&pd).unwrap());
        assert!(pd.join("SteelSeries/other.txt").exists(), "a directory with other files stays");
    }

    #[test]
    fn ours_means_only_a_loopback_address() {
        assert!(is_ours(&contents(1234)));
        assert!(!is_ours(r#"{"address":"10.0.0.2:1234"}"#));
        assert!(!is_ours(r#"{"address":"127.0.0.1:1","more":1}"#));
        assert!(!is_ours("not json"));
    }
}
