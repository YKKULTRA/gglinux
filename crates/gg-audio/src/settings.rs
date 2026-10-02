//! Sonar's saved state: `$XDG_CONFIG_HOME/gglinux/sonar.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gg_ipc::{Channel, EqBand, HeadphoneEq, MicSettings, SmartVolume};
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
    /// Streamer mode: a second mix of the channels for OBS.
    pub streamer: bool,
    /// Per-channel stream level, 0..=1; 1.0 when unset.
    pub stream_volumes: BTreeMap<Channel, f32>,
    /// The stream mix's master level, 0..=1.
    pub stream_master: f32,
    /// Smart Volume per channel; off when unset.
    pub smart_volume: BTreeMap<Channel, SmartVolume>,
    /// PipeWire wouldn't make the channels with Smart Volume's stage, so they
    /// run without it. Only ever set while Sonar is on: turning Sonar off
    /// forgets it, so turning it on again tries the stage again.
    pub smart_volume_refused: bool,
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
            streamer: false,
            stream_volumes: BTreeMap::new(),
            stream_master: 1.0,
            smart_volume: BTreeMap::new(),
            smart_volume_refused: false,
        }
    }
}

impl Settings {
    /// A channel's bands, flat if never set.
    pub fn channel_eq(&self, channel: Channel) -> Vec<EqBand> {
        self.eq.get(&channel).cloned().unwrap_or_else(eq::flat_channel)
    }

    /// A channel's stream level, full if never set.
    pub fn stream_volume(&self, channel: Channel) -> f32 {
        self.stream_volumes.get(&channel).copied().unwrap_or(1.0)
    }

    /// A channel's Smart Volume, off if never set.
    pub fn smart_volume(&self, channel: Channel) -> SmartVolume {
        self.smart_volume.get(&channel).copied().unwrap_or_default()
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
        let dir = scratch("settings-rt");
        let path = dir.join("gglinux").join("sonar.json");
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
        let dir = scratch("settings-corrupt");
        let path = dir.join("sonar.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(&path), Settings::default());
    }

    #[test]
    fn settings_from_before_smart_volume_still_load() {
        let dir = scratch("settings-old");
        let path = dir.join("sonar.json");
        std::fs::write(&path, r#"{"enabled":true,"output":"alsa_output.x","chatmix":0.5}"#).unwrap();
        let s = load(&path);
        assert!(s.enabled && s.smart_volume.is_empty());
        assert_eq!(s.smart_volume(Channel::Game), gg_ipc::SmartVolume::default());
        assert!(!s.smart_volume_refused);
    }
}
