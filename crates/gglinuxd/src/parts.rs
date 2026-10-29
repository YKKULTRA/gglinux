// SPDX-License-Identifier: GPL-3.0-or-later
//! Which parts of gglinux run, saved in `~/.config/gglinux/parts.json`. Read once at
//! start: a change is saved and the daemon starts again (see `main`).

use std::path::{Path, PathBuf};

use gg_ipc::Parts;
use tracing::warn;

pub fn path() -> PathBuf {
    gg_ipc::config_dir().join("parts.json")
}

/// Saved parts; a missing file means all of them. An unreadable one is moved aside.
pub fn load(path: &Path) -> Parts {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            let moved_to = gg_ipc::set_aside(path);
            warn!(path = %path.display(), error = %e, %moved_to, "ignoring unreadable parts settings");
            Parts::default()
        }),
        Err(_) => Parts::default(),
    }
}

/// Save atomically.
pub fn save(path: &Path, parts: &Parts) -> Result<(), String> {
    let write = || -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(parts)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    write().map_err(|e| format!("could not save which parts run: {e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_parts_load_back_and_a_missing_file_means_all() {
        let dir = std::env::temp_dir().join(format!("gglinux-parts-{}", std::process::id()));
        let path = dir.join("parts.json");
        assert_eq!(load(&path), Parts::default());
        let parts = Parts {
            sonar: false,
            clips: false,
            ..Parts::default()
        };
        save(&path, &parts).unwrap();
        assert_eq!(load(&path), parts);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
