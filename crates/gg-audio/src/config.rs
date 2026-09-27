//! The PipeWire drop-in that declares Sonar's channels.
//!
//! PipeWire hosts the channels itself, so they keep working while gglinux is
//! not running. Each channel is a loopback: a virtual sink apps play into, and
//! an outlet stream that plays it into the chosen physical output.

use std::path::PathBuf;

use anyhow::{ensure, Result};
use gg_ipc::Channel;

/// Sorts after the stock drop-ins.
pub const FILE_NAME: &str = "60-gglinux-sonar.conf";

/// Every node gglinux creates is named with this prefix.
const PREFIX: &str = "gglinux_";

/// The virtual sink apps play into.
pub fn sink_name(channel: Channel) -> String {
    format!("{PREFIX}{}", channel.key())
}

/// The stream that carries a channel into the physical output.
pub fn outlet_name(channel: Channel) -> String {
    format!("{PREFIX}{}.out", channel.key())
}

/// Whether a node is one of ours (a channel sink or outlet).
pub fn is_ours(node_name: &str) -> bool {
    node_name.starts_with(PREFIX)
}

fn label(channel: Channel) -> &'static str {
    match channel {
        Channel::Game => "Game",
        Channel::Chat => "Chat",
        Channel::Media => "Media",
        Channel::Aux => "Aux",
    }
}

/// `$XDG_CONFIG_HOME`, falling back to `~/.config`.
pub fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".config")
        })
}

/// Where the drop-in lives.
pub fn dropin_path() -> PathBuf {
    config_home().join("pipewire").join("pipewire.conf.d").join(FILE_NAME)
}

/// The drop-in text for channels that play into `output` (a `node.name`).
///
/// Every outlet sets `node.dont-fallback`: if the output disappears, the
/// outlet waits for it rather than falling back to the default sink — which,
/// with Sonar on, is the Game channel, and would feed audio into itself.
pub fn dropin(output: &str) -> Result<String> {
    ensure!(
        !output.is_empty() && !output.contains(['"', '\\', '\n']),
        "unusable output device name {output:?}"
    );
    let mut text = String::from(
        "# Written by gglinux: Sonar-style output channels.\n\
         # Turn Sonar off in gglinux, or delete this file and restart PipeWire, to remove them.\n\
         context.modules = [\n",
    );
    for channel in Channel::ALL {
        text.push_str(&format!(
            r#"    {{ name = libpipewire-module-loopback
      args = {{
        node.description = "gglinux {label}"
        capture.props = {{
          node.name      = "{sink}"
          media.class    = "Audio/Sink"
          audio.position = [ FL FR ]
        }}
        playback.props = {{
          node.name          = "{outlet}"
          node.description   = "gglinux {label} (output)"
          target.object      = "{output}"
          node.dont-fallback = true
          node.passive       = true
          stream.dont-remix  = true
          audio.position     = [ FL FR ]
        }}
      }}
    }}
"#,
            label = label(channel),
            sink = sink_name(channel),
            outlet = outlet_name(channel),
        ));
    }
    text.push_str("]\n");
    Ok(text)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropin_declares_four_loopbacks_into_the_output() {
        let text = dropin("alsa_output.usb-SteelSeries_Arctis").unwrap();
        assert_eq!(text.matches("libpipewire-module-loopback").count(), 4);
        for c in Channel::ALL {
            assert!(text.contains(&format!("node.name      = \"gglinux_{}\"", c.key())), "{text}");
            assert!(text.contains(&format!("node.name          = \"gglinux_{}.out\"", c.key())), "{text}");
        }
        assert_eq!(
            text.matches("target.object      = \"alsa_output.usb-SteelSeries_Arctis\"").count(),
            4
        );
        assert_eq!(
            text.matches("node.dont-fallback = true").count(),
            4,
            "an outlet must never fall back into a channel"
        );
    }

    #[test]
    fn dropin_refuses_names_that_would_break_the_config() {
        assert!(dropin("").is_err());
        assert!(dropin("evil\" }").is_err());
        assert!(dropin("a\\b").is_err());
        assert!(dropin("a\nb").is_err());
    }

    #[test]
    fn names_identify_our_nodes() {
        assert_eq!(sink_name(Channel::Chat), "gglinux_chat");
        assert_eq!(outlet_name(Channel::Chat), "gglinux_chat.out");
        assert!(is_ours("gglinux_game.out"));
        assert!(!is_ours("alsa_output.pci-0000"));
    }
}
