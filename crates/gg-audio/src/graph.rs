//! What PipeWire has right now, read from `pw-dump`.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::config;

/// One PipeWire node, reduced to what Sonar needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: u32,
    pub name: String,
    pub description: Option<String>,
    pub media_class: String,
    pub app_name: Option<String>,
    pub app_binary: Option<String>,
    /// Loudest channel, on the cubic scale `wpctl` and desktop mixers show.
    pub volume: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    /// `node.name` of the configured default sink.
    pub default_sink: Option<String>,
}

const SINK: &str = "Audio/Sink";
const APP_STREAM: &str = "Stream/Output/Audio";

impl Graph {
    pub fn parse(json: &str) -> Result<Graph> {
        let objects: Vec<Value> = serde_json::from_str(json).context("parsing pw-dump output")?;
        let mut graph = Graph::default();
        for object in &objects {
            match object["type"].as_str() {
                Some("PipeWire:Interface:Node") => graph.nodes.extend(parse_node(object)),
                Some("PipeWire:Interface:Metadata")
                    if object["props"]["metadata.name"] == "default" =>
                {
                    graph.default_sink = default_sink(object);
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
        self.nodes.iter().filter(|n| n.media_class == SINK && !config::is_ours(&n.name))
    }

    /// Apps playing audio. Our outlets are streams too, and are left out.
    pub fn app_streams(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(|n| n.media_class == APP_STREAM && !config::is_ours(&n.name))
    }
}

fn parse_node(object: &Value) -> Option<Node> {
    let props = &object["info"]["props"];
    let text = |key: &str| props[key].as_str().map(str::to_owned);
    Some(Node {
        id: u32::try_from(object["id"].as_u64()?).ok()?,
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
    let channels = props.as_array()?.iter().find_map(|p| p["channelVolumes"].as_array())?;
    let loudest = channels.iter().filter_map(Value::as_f64).reduce(f64::max)?;
    Some(loudest.cbrt() as f32)
}

fn default_sink(object: &Value) -> Option<String> {
    let entries = object["metadata"].as_array()?;
    let lookup = |key: &str| {
        entries
            .iter()
            .find(|e| e["subject"] == 0 && e["key"] == key)
            .and_then(|e| sink_name_of(&e["value"]))
    };
    lookup("default.configured.audio.sink").or_else(|| lookup("default.audio.sink"))
}

/// The value is `{"name": …}`, sometimes serialised as a JSON string.
fn sink_name_of(value: &Value) -> Option<String> {
    match value {
        Value::Object(_) => value["name"].as_str().map(str::to_owned),
        Value::String(s) => serde_json::from_str::<Value>(s).ok()?["name"].as_str().map(str::to_owned),
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
        assert!(g.app_streams().any(|s| s.app_binary.as_deref() == Some("firefox")));
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
    fn default_sink_may_be_a_json_string() {
        let json = r#"[{"id":39,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},
            "metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":"{\"name\":\"alsa_output.y\"}"}]}]"#;
        assert_eq!(Graph::parse(json).unwrap().default_sink.as_deref(), Some("alsa_output.y"));
    }
}
