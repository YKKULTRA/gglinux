//! Sonar-style audio for gglinux, on PipeWire.
//!
//! PipeWire hosts the graph — four virtual output channels declared in a
//! drop-in config ([`config`]) — and this crate controls it with PipeWire's own
//! tools (`pw-dump`, `pw-metadata`, `wpctl`) through [`tools::PwTools`]. Every
//! action is a command a person could type, and the logic is tested against a
//! fake.

pub mod config;
pub mod eq;
pub mod mix;
pub mod rules;
pub mod tools;
pub mod graph;
pub mod mic;
pub mod settings;
pub mod smart;
pub mod sonar;

#[cfg(test)]
pub(crate) mod testing {
    use serde_json::{json, Value};

    /// A minimal `pw-dump`: nodes as (id, node.name, media.class, binary), all at
    /// full volume, plus the configured default sink.
    pub fn dump(nodes: &[(u32, &str, &str, Option<&str>)], default_sink: &str) -> String {
        dump_with_source(nodes, default_sink, None)
    }

    /// As [`dump`], plus the configured default source.
    pub fn dump_with_source(
        nodes: &[(u32, &str, &str, Option<&str>)],
        default_sink: &str,
        default_source: Option<&str>,
    ) -> String {
        let mut objects: Vec<Value> = nodes
            .iter()
            .map(|(id, name, class, binary)| {
                let mut props = json!({ "node.name": name, "media.class": class });
                if let Some(b) = binary {
                    props["application.process.binary"] = json!(b);
                    props["application.name"] = json!(b);
                }
                json!({
                    "id": id,
                    "type": "PipeWire:Interface:Node",
                    "info": { "props": props, "params": { "Props": [{ "channelVolumes": [1.0, 1.0] }] } }
                })
            })
            .collect();
        let mut metadata = vec![json!({
            "subject": 0,
            "key": "default.configured.audio.sink",
            "type": "Spa:String:JSON",
            "value": { "name": default_sink }
        })];
        if let Some(source) = default_source {
            metadata.push(json!({
                "subject": 0,
                "key": "default.configured.audio.source",
                "type": "Spa:String:JSON",
                "value": { "name": source }
            }));
        }
        objects.push(json!({
            "id": 39,
            "type": "PipeWire:Interface:Metadata",
            "props": { "metadata.name": "default" },
            "metadata": metadata
        }));
        Value::Array(objects).to_string()
    }

    /// An empty directory unique to one test, removed with everything in it
    /// when this goes out of scope.
    pub struct Scratch(std::path::PathBuf);

    impl std::ops::Deref for Scratch {
        type Target = std::path::Path;

        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir()
            .join("gg-audio-tests")
            .join(format!("{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}
