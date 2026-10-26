// SPDX-License-Identifier: GPL-3.0-or-later
//! Which channel an app plays into.

use std::collections::BTreeMap;

use gg_ipc::Channel;
use serde::{Deserialize, Serialize};

/// Channel per `application.process.binary`, matched case-insensitively.
/// Apps without a rule play into Game, as Sonar's default output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rules(BTreeMap<String, Channel>);

impl Default for Rules {
    fn default() -> Self {
        let chat = [
            "discord",
            "vesktop",
            "teamspeak",
            "ts3client_linux_amd64",
            "mumble",
        ];
        let media = [
            "firefox", "chromium", "chrome", "brave", "spotify", "mpv", "vlc",
        ];
        Rules(
            chat.iter()
                .map(|b| (b.to_string(), Channel::Chat))
                .chain(media.iter().map(|b| (b.to_string(), Channel::Media)))
                .collect(),
        )
    }
}

impl Rules {
    pub fn channel_for(&self, binary: Option<&str>) -> Channel {
        binary
            .and_then(|b| self.0.get(&b.to_lowercase()))
            .copied()
            .unwrap_or(Channel::Game)
    }

    pub fn set(&mut self, binary: &str, channel: Channel) {
        self.0.insert(binary.to_lowercase(), channel);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_send_chat_apps_to_chat_and_players_to_media() {
        let rules = Rules::default();
        assert_eq!(rules.channel_for(Some("discord")), Channel::Chat);
        assert_eq!(
            rules.channel_for(Some("Discord")),
            Channel::Chat,
            "case-insensitive"
        );
        assert_eq!(rules.channel_for(Some("firefox")), Channel::Media);
        assert_eq!(rules.channel_for(Some("some-game.x86_64")), Channel::Game);
        assert_eq!(rules.channel_for(None), Channel::Game);
    }

    #[test]
    fn a_set_rule_overrides_and_survives_serialisation() {
        let mut rules = Rules::default();
        rules.set("Firefox", Channel::Aux);
        let json = serde_json::to_string(&rules).unwrap();
        assert!(json.contains(r#""firefox":"aux""#), "{json}");
        let back: Rules = serde_json::from_str(&json).unwrap();
        assert_eq!(back.channel_for(Some("firefox")), Channel::Aux);
    }
}
