//! The PipeWire drop-in that declares Sonar's channels.
//!
//! PipeWire hosts the channels itself, so they keep working while gglinux is
//! not running. Each channel is a filter chain: a virtual sink apps play into,
//! the EQ stages, and an outlet stream that plays the result into the chosen
//! physical output.

use std::path::PathBuf;

use anyhow::{ensure, Result};
use gg_ipc::{BandKind, Channel};

use crate::eq;

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

/// A `node.name` we can safely quote into the drop-in.
pub(crate) fn check_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.contains(['"', '\\', '\n']),
        "unusable device name {name:?}"
    );
    Ok(())
}

/// The drop-in text for channels that play into `output` (a `node.name`), plus
/// the mic chain reading `mic_input` when one is given.
///
/// Every outlet sets `node.dont-fallback`: if the output disappears, the
/// outlet waits for it rather than falling back to the default sink — which,
/// with Sonar on, is the Game channel, and would feed audio into itself.
pub fn dropin(output: &str, mic_input: Option<&str>) -> Result<String> {
    check_name(output)?;
    let mut text = String::from(
        "# Written by gglinux: Sonar-style output channels.\n\
         # Turn Sonar off in gglinux, or delete this file and restart PipeWire, to remove them.\n\
         context.modules = [\n",
    );
    for channel in Channel::ALL {
        text.push_str(&format!(
            r#"    {{ name = libpipewire-module-filter-chain
      args = {{
        node.description = "gglinux {label}"
        audio.channels   = 2
        audio.position   = [ FL FR ]
        filter.graph = {{
{graph}
        }}
        capture.props = {{
          node.name      = "{sink}"
          media.class    = "Audio/Sink"
        }}
        playback.props = {{
          node.name          = "{outlet}"
          node.description   = "gglinux {label} (output)"
          target.object      = "{output}"
          node.dont-fallback = true
          node.passive       = true
          stream.dont-remix  = true
        }}
      }}
    }}
"#,
            label = label(channel),
            sink = sink_name(channel),
            outlet = outlet_name(channel),
            graph = filter_graph(),
        ));
    }
    if let Some(input) = mic_input {
        text.push_str(&crate::mic::module(input)?);
    }
    text.push_str("]\n");
    Ok(text)
}
/// The chain every channel runs: preamp, headphone correction, channel EQ.
/// Everything starts flat; gglinux sets the controls live.
fn filter_graph() -> String {
    let headphone = eq::flat_headphone().bands;
    let channel = eq::flat_channel();
    let stages: Vec<(String, gg_ipc::EqBand)> = headphone
        .iter()
        .enumerate()
        .map(|(i, b)| (format!("hp_{}", i + 1), *b))
        .chain(channel.iter().enumerate().map(|(i, b)| (format!("eq_{}", i + 1), *b)))
        .collect();

    let mut nodes =
        vec![r#"            { type = builtin name = pre label = linear control = { "Mult" = 1.0 } }"#.to_string()];
    for (name, band) in &stages {
        nodes.push(format!(
            r#"            {{ type = builtin name = {name} label = {label} control = {{ "Freq" = {freq:.1} "Q" = {q:.3} "Gain" = 0.0 }} }}"#,
            label = filter_label(band.kind),
            freq = band.freq,
            q = band.q,
        ));
    }
    let names: Vec<&str> =
        std::iter::once("pre").chain(stages.iter().map(|(n, _)| n.as_str())).collect();
    let links: Vec<String> = names
        .windows(2)
        .map(|w| format!(r#"            {{ output = "{}:Out" input = "{}:In" }}"#, w[0], w[1]))
        .collect();
    format!(
        "          nodes = [\n{}\n          ]\n          links = [\n{}\n          ]",
        nodes.join("\n"),
        links.join("\n")
    )
}

fn filter_label(kind: BandKind) -> &'static str {
    match kind {
        BandKind::LowShelf => "bq_lowshelf",
        BandKind::Peaking => "bq_peaking",
        BandKind::HighShelf => "bq_highshelf",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropin_declares_four_filter_chains_into_the_output() {
        let text = dropin("alsa_output.usb-SteelSeries_Arctis", None).unwrap();
        assert_eq!(text.matches("libpipewire-module-filter-chain").count(), 4);
        assert!(!text.contains("libpipewire-module-loopback"));
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
    fn every_channel_runs_the_same_chain() {
        let text = dropin("alsa_output.x", None).unwrap();
        assert_eq!(text.matches("name = pre label = linear").count(), 4);
        assert_eq!(text.matches("name = hp_1 label = bq_lowshelf").count(), 4);
        assert_eq!(text.matches("name = hp_12 label = bq_highshelf").count(), 4);
        assert_eq!(text.matches("name = eq_10 label = bq_highshelf").count(), 4);
        assert_eq!(text.matches(r#"{ output = "hp_12:Out" input = "eq_1:In" }"#).count(), 4);
        assert_eq!(text.matches("input = ").count(), 4 * 22, "23 stages, 22 links each");
    }

    #[test]
    fn dropin_refuses_names_that_would_break_the_config() {
        assert!(dropin("", None).is_err());
        assert!(dropin("evil\" }", None).is_err());
        assert!(dropin("a\\b", None).is_err());
        assert!(dropin("a\nb", None).is_err());
    }

    #[test]
    fn the_mic_module_is_added_only_when_asked() {
        assert!(!dropin("alsa_output.x", None).unwrap().contains("gglinux_mic"));
        let with = dropin("alsa_output.x", Some("alsa_input.y")).unwrap();
        assert_eq!(with.matches("libpipewire-module-filter-chain").count(), 5);
        assert!(with.contains(r#"target.object      = "alsa_input.y""#));
        assert!(dropin("alsa_output.x", Some("bad\"")).is_err());
    }

    #[test]
    fn names_identify_our_nodes() {
        assert_eq!(sink_name(Channel::Chat), "gglinux_chat");
        assert_eq!(outlet_name(Channel::Chat), "gglinux_chat.out");
        assert!(is_ours("gglinux_game.out"));
        assert!(!is_ours("alsa_output.pci-0000"));
    }
}
