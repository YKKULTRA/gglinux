// SPDX-License-Identifier: GPL-3.0-or-later
//! What PipeWire has right now, read from `pw-dump`.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::config;

/// One PipeWire node, reduced to what Sonar needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: u32,
    /// `object.serial`: unlike the id, which PipeWire hands out again as soon
    /// as a node goes, never reused.
    pub serial: Option<u64>,
    pub name: String,
    pub description: Option<String>,
    pub media_class: String,
    pub app_name: Option<String>,
    pub app_binary: Option<String>,
    /// Loudest channel, on the cubic scale `wpctl` and desktop mixers show.
    pub volume: Option<f32>,
}

/// Prefix PipeWire's ALSA plugin gives the streams of plain ALSA apps.
const ALSA_PLAYBACK: &str = "alsa_playback.";

impl Node {
    /// What routing rules match an app by: its binary, or — for apps playing
    /// through PipeWire's ALSA plugin, which report no binary — the app name in
    /// `alsa_playback.<app>`.
    pub fn route_key(&self) -> Option<String> {
        self.app_binary
            .clone()
            .or_else(|| self.name.strip_prefix(ALSA_PLAYBACK).map(str::to_owned))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    /// `node.name` of the configured default sink.
    pub default_sink: Option<String>,
    /// `node.name` of the configured default source.
    pub default_source: Option<String>,
    /// `node.name` of the sink WirePlumber uses as the default now: the
    /// configured one, or a stand-in while that one is away.
    pub current_sink: Option<String>,
    /// The same for the default source.
    pub current_source: Option<String>,
    /// The PipeWire daemon's `info.cookie`, random each time it starts: a new
    /// one means PipeWire restarted, however quickly.
    pub cookie: Option<u64>,
}

const SINK: &str = "Audio/Sink";
const APP_STREAM: &str = "Stream/Output/Audio";
const SOURCE: &str = "Audio/Source";

impl Graph {
    pub fn parse(json: &str) -> Result<Graph> {
        let objects: Vec<Value> = serde_json::from_str(json).context("parsing pw-dump output")?;
        let mut graph = Graph::default();
        for object in &objects {
            match object["type"].as_str() {
                Some("PipeWire:Interface:Core") => graph.cookie = object["info"]["cookie"].as_u64(),
                Some("PipeWire:Interface:Node") => graph.nodes.extend(parse_node(object)),
                Some("PipeWire:Interface:Metadata")
                    if object["props"]["metadata.name"] == "default" =>
                {
                    graph.default_sink = default_node(object, "sink");
                    graph.default_source = default_node(object, "source");
                    graph.current_sink = metadata_node(object, "default.audio.sink");
                    graph.current_source = metadata_node(object, "default.audio.source");
                }
                _ => {}
            }
        }
        Ok(graph)
    }

    pub fn node(&self, name: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.name == name)
    }

    /// Real output devices: sinks that are not ours.
    pub fn physical_sinks(&self) -> impl Iterator<Item = &Node> {
        self.nodes
            .iter()
            .filter(|n| n.media_class == SINK && !config::is_ours(&n.name))
    }

    /// Real inputs: sources that are not ours.
    pub fn physical_sources(&self) -> impl Iterator<Item = &Node> {
        self.nodes
            .iter()
            .filter(|n| n.media_class == SOURCE && !config::is_ours(&n.name))
    }

    /// Apps playing audio. Our outlets are streams too, and are left out.
    pub fn app_streams(&self) -> impl Iterator<Item = &Node> {
        self.nodes
            .iter()
            .filter(|n| n.media_class == APP_STREAM && !config::is_ours(&n.name))
    }
}

fn parse_node(object: &Value) -> Option<Node> {
    let props = &object["info"]["props"];
    let text = |key: &str| props[key].as_str().map(str::to_owned);
    Some(Node {
        id: u32::try_from(object["id"].as_u64()?).ok()?,
        serial: props["object.serial"].as_u64(),
        name: text("node.name")?,
        description: text("node.description"),
        media_class: text("media.class").unwrap_or_default(),
        app_name: text("application.name"),
        app_binary: text("application.process.binary"),
        volume: volume(&object["info"]["params"]["Props"]),
    })
}

/// The loudest channel's volume, converted from PipeWire's linear amplitude to
/// the cubic scale the mixer shows.
fn volume(props: &Value) -> Option<f32> {
    let channels = props
        .as_array()?
        .iter()
        .find_map(|p| p["channelVolumes"].as_array())?;
    let loudest = channels.iter().filter_map(Value::as_f64).reduce(f64::max)?;
    Some(loudest.cbrt() as f32)
}

/// The configured default `kind` ("sink" or "source"), else the current one.
fn default_node(object: &Value, kind: &str) -> Option<String> {
    metadata_node(object, &format!("default.configured.audio.{kind}"))
        .or_else(|| metadata_node(object, &format!("default.audio.{kind}")))
}

/// The node a `default` metadata key names.
fn metadata_node(object: &Value, key: &str) -> Option<String> {
    object["metadata"]
        .as_array()?
        .iter()
        .find(|e| e["subject"] == 0 && e["key"] == key)
        .and_then(|e| node_name_of(&e["value"]))
}

/// The value is `{"name": …}`, sometimes serialised as a JSON string.
fn node_name_of(value: &Value) -> Option<String> {
    match value {
        Value::Object(_) => value["name"].as_str().map(str::to_owned),
        Value::String(s) => serde_json::from_str::<Value>(s).ok()?["name"]
            .as_str()
            .map(str::to_owned),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::dump;

    const CAPTURE: &str = include_str!("../fixtures/pw-dump-2026-09-27.json");

    #[test]
    fn parses_a_real_capture() {
        let g = Graph::parse(CAPTURE).unwrap();
        let arctis = g
            .node("alsa_output.usb-SteelSeries_Arctis_Nova_Pro_Wireless-00.analog-stereo")
            .expect("the Arctis sink is in the capture");
        assert_eq!(arctis.media_class, "Audio/Sink");
        assert!(arctis.volume.is_some());
        assert!(g.physical_sinks().count() >= 4);
        assert!(g
            .app_streams()
            .any(|s| s.app_binary.as_deref() == Some("firefox")));
        assert!(g.default_sink.is_some(), "the capture names a default sink");
    }

    #[test]
    fn volumes_use_the_mixer_scale() {
        // Linear 0.125 is 0.5 on the cubic scale wpctl and the desktop show.
        let json = r#"[{"id":7,"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"x","media.class":"Audio/Sink"},"params":{"Props":[{"channelVolumes":[0.125,0.064]}]}}}]"#;
        let v = Graph::parse(json).unwrap().nodes[0].volume.unwrap();
        assert!((v - 0.5).abs() < 1e-6, "{v}");
    }

    #[test]
    fn our_nodes_are_neither_outputs_nor_apps() {
        let g = Graph::parse(&dump(
            &[
                (1, "alsa_output.x", "Audio/Sink", None),
                (2, "gglinux_game", "Audio/Sink", None),
                (3, "gglinux_game.out", "Stream/Output/Audio", None),
                (4, "Firefox", "Stream/Output/Audio", Some("firefox")),
            ],
            "alsa_output.x",
        ))
        .unwrap();
        assert_eq!(g.physical_sinks().map(|n| n.id).collect::<Vec<_>>(), [1]);
        assert_eq!(g.app_streams().map(|n| n.id).collect::<Vec<_>>(), [4]);
        assert_eq!(g.default_sink.as_deref(), Some("alsa_output.x"));
    }

    #[test]
    fn apps_without_a_binary_are_keyed_by_their_alsa_name() {
        let g = Graph::parse(CAPTURE).unwrap();
        let elisa = g
            .node("alsa_playback.elisa")
            .expect("the capture has Elisa via PipeWire ALSA");
        assert_eq!(elisa.app_binary, None);
        assert_eq!(elisa.route_key().as_deref(), Some("elisa"));
        let firefox = g
            .app_streams()
            .find(|s| s.app_binary.as_deref() == Some("firefox"))
            .unwrap();
        assert_eq!(firefox.route_key().as_deref(), Some("firefox"));
    }

    #[test]
    fn the_cookie_tells_pipewire_starts_apart() {
        let json = r#"[{"id":0,"type":"PipeWire:Interface:Core","info":{"cookie":3377742783,"name":"pipewire-0"}}]"#;
        assert_eq!(Graph::parse(json).unwrap().cookie, Some(3377742783));
        assert_eq!(
            Graph::parse(CAPTURE).unwrap().cookie,
            None,
            "a dump without the core has none"
        );
    }

    #[test]
    fn the_default_in_use_is_read_beside_the_configured_one() {
        let json = r#"[{"id":39,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},
            "metadata":[{"subject":0,"key":"default.configured.audio.sink","value":{"name":"bluez_output.away"}},
                        {"subject":0,"key":"default.audio.sink","value":{"name":"alsa_output.y"}},
                        {"subject":0,"key":"default.audio.source","value":{"name":"alsa_input.y"}}]}]"#;
        let g = Graph::parse(json).unwrap();
        assert_eq!(
            g.default_sink.as_deref(),
            Some("bluez_output.away"),
            "the user's choice"
        );
        assert_eq!(
            g.current_sink.as_deref(),
            Some("alsa_output.y"),
            "what plays meanwhile"
        );
        assert_eq!(
            g.default_source.as_deref(),
            Some("alsa_input.y"),
            "nothing configured: the one in use"
        );
        assert_eq!(g.current_source.as_deref(), Some("alsa_input.y"));
    }

    #[test]
    fn default_sink_may_be_a_json_string() {
        let json = r#"[{"id":39,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},
            "metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":"{\"name\":\"alsa_output.y\"}"}]}]"#;
        assert_eq!(
            Graph::parse(json).unwrap().default_sink.as_deref(),
            Some("alsa_output.y")
        );
    }

    #[test]
    fn inputs_and_the_default_input_are_read() {
        let g = Graph::parse(CAPTURE).unwrap();
        let arctis = "alsa_input.usb-SteelSeries_Arctis_Nova_Pro_Wireless-00.mono-fallback";
        assert!(g.physical_sources().any(|n| n.name == arctis));
        assert_eq!(g.default_source.as_deref(), Some(arctis));
        let ours = Graph::parse(&crate::testing::dump_with_source(
            &[
                (1, "alsa_input.x", "Audio/Source", None),
                (2, "gglinux_mic", "Audio/Source", None),
            ],
            "alsa_output.x",
            Some("alsa_input.x"),
        ))
        .unwrap();
        assert_eq!(
            ours.physical_sources().map(|n| n.id).collect::<Vec<_>>(),
            [1],
            "the virtual mic is not an input"
        );
        assert_eq!(ours.default_source.as_deref(), Some("alsa_input.x"));
    }
}
