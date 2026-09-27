//! Sonar's saved state: `$XDG_CONFIG_HOME/gglinux/sonar.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gg_ipc::{Channel, EqBand, HeadphoneEq, MicSettings};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{config, eq, rules::Rules};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// `node.name` of the output the channels play into.
    pub output: Option<String>,
    /// The default sink before Sonar took over, restored when it is turned off.
    pub previous_default: Option<String>,
    /// -1.0 all Game … 1.0 all Chat.
    pub chatmix: f32,
    /// Per-channel volume on the mixer's 0..=1 scale.
    pub volumes: BTreeMap<Channel, f32>,
    pub rules: Rules,
    /// Channel EQ bands; a channel without an entry is flat.
    pub eq: BTreeMap<Channel, Vec<EqBand>>,
    /// Headphone correction shared by every channel.
    pub headphone: HeadphoneEq,
    /// `node.name` of the physical mic the virtual mic reads.
    pub mic_input: Option<String>,
    /// The default input before Sonar took over, restored when it is turned off.
    pub previous_default_source: Option<String>,
    pub mic: MicSettings,
    /// Virtual mic volume on the mixer's 0..=1 scale.
    pub mic_volume: Option<f32>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            output: None,
            previous_default: None,
            chatmix: 0.0,
            volumes: BTreeMap::new(),
            rules: Rules::default(),
            eq: BTreeMap::new(),
            headphone: eq::flat_headphone(),
            mic_input: None,
            previous_default_source: None,
            mic: MicSettings::default(),
            mic_volume: None,
        }
    }
}

impl Settings {
    /// A channel's bands, flat if never set.
    pub fn channel_eq(&self, channel: Channel) -> Vec<EqBand> {
        self.eq.get(&channel).cloned().unwrap_or_else(eq::flat_channel)
    }
}

pub fn path() -> PathBuf {
    config::config_home().join("gglinux").join("sonar.json")
}

/// Read settings. A missing file means the defaults; an unreadable one is
/// logged and ignored, never fatal.
pub fn load(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            warn!(path = %path.display(), error = %e, "ignoring unreadable Sonar settings");
            Settings::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "could not read Sonar settings");
            Settings::default()
        }
    }
}

/// Write settings atomically: a sibling temp file renamed over the old one.
pub fn save(path: &Path, settings: &Settings) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(settings)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scratch;

    #[test]
    fn settings_round_trip() {
        let path = scratch("settings-rt").join("gglinux").join("sonar.json");
        let mut rules = Rules::default();
        rules.set("firefox", Channel::Aux);
        let s = Settings {
            enabled: true,
            output: Some("alsa_output.x".into()),
            chatmix: -0.5,
            volumes: BTreeMap::from([(Channel::Chat, 0.4)]),
            rules,
            ..Settings::default()
        };
        save(&path, &s).unwrap();
        assert_eq!(load(&path), s);
    }

    #[test]
    fn missing_settings_are_the_defaults() {
        assert_eq!(load(&scratch("settings-missing").join("sonar.json")), Settings::default());
    }

    #[test]
    fn corrupt_settings_fall_back_to_defaults() {
        let path = scratch("settings-corrupt").join("sonar.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(&path), Settings::default());
    }
}
