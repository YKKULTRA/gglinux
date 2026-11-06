// SPDX-License-Identifier: GPL-3.0-or-later
//! The actions as saved in `~/.config/gglinux/actions.json`, with the shortcut ids the
//! desktop bound for them the last time it was asked. At start the daemon binds those
//! again, which the desktop takes without asking; it asks only after a change.

use std::path::{Path, PathBuf};

use gg_ipc::ActionBinding;
use serde::{Deserialize, Serialize};
use tracing::warn;

pub fn path() -> PathBuf {
    gg_ipc::config_dir().join("actions.json")
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    pub bindings: Vec<ActionBinding>,
    /// The shortcut ids (`action-f13` …) the desktop bound, as it last said.
    pub bound: Vec<String>,
}

/// What's saved; nothing for a missing file. A file that can't be read, or that holds
/// a list the daemon would refuse, is moved aside.
pub fn load(path: &Path) -> Saved {
    let Ok(bytes) = std::fs::read(path) else {
        return Saved::default();
    };
    let checked = serde_json::from_slice::<Saved>(&bytes)
        .map_err(|e| e.to_string())
        .and_then(|s| gg_ipc::validate_actions(&s.bindings).map(|()| s));
    checked.unwrap_or_else(|e| {
        let moved_to = gg_ipc::set_aside(path);
        warn!(path = %path.display(), error = %e, %moved_to, "ignoring unreadable actions");
        Saved::default()
    })
}

/// Save atomically.
pub fn save(path: &Path, saved: &Saved) -> Result<(), String> {
    let write = || -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(saved)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    write().map_err(|e| format!("could not save the actions: {e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use gg_ipc::Action;

    #[test]
    fn a_saved_list_loads_back_and_a_missing_one_is_empty() {
        let d = Scratch::new("actions-store");
        let path = d.0.join("actions.json");
        assert_eq!(load(&path), Saved::default());
        let saved = Saved {
            bindings: vec![ActionBinding {
                key: 0x68,
                label: "Terminal".into(),
                action: Action::Run {
                    argv: vec!["kitty".into()],
                },
            }],
            bound: vec!["action-f13".into()],
        };
        save(&path, &saved).unwrap();
        assert_eq!(load(&path), saved);
        assert!(!d.0.join("actions.json.tmp").exists(), "written atomically");
    }

    #[test]
    fn a_file_with_a_bad_list_is_set_aside() {
        let d = Scratch::new("actions-store-bad");
        let path = d.0.join("actions.json");
        // F12 is not a spare key.
        std::fs::write(
            &path,
            r#"{"bindings":[{"key":69,"label":"x","action":{"kind":"clip_save"}}]}"#,
        )
        .unwrap();
        assert_eq!(load(&path), Saved::default());
        assert!(!path.exists(), "moved out of the way");
        std::fs::write(&path, b"{\"bindings\": [").unwrap();
        assert_eq!(load(&path), Saved::default());
        assert!(!path.exists());
    }
}
