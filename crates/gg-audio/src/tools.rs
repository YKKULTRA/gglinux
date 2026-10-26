//! PipeWire's own command-line tools, behind a trait so the orchestration can
//! be tested against a fake.

use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};

pub trait PwTools {
    /// Everything PipeWire has, as `pw-dump` prints it.
    fn dump(&self) -> Result<String>;
    /// Run one control command; `argv[0]` is the program.
    fn run(&self, argv: &[String]) -> Result<()>;
    /// Wait. The fake in the tests does not.
    fn pause(&self, duration: Duration) {
        std::thread::sleep(duration)
    }
    /// Whether the mic chain's LADSPA plugins are installed.
    fn mic_plugins_present(&self) -> bool {
        crate::mic::plugins_present()
    }
    /// Whether PipeWire can load Smart Volume's compressor (swh's `sc4m_1916`).
    fn smart_volume_supported(&self) -> bool {
        crate::smart::compressor_present()
    }
}

/// The real tools, run as child processes.
pub struct SystemTools;

impl PwTools for SystemTools {
    fn dump(&self) -> Result<String> {
        let out = Command::new("pw-dump")
            .output()
            .context("running pw-dump — is PipeWire installed?")?;
        if !out.status.success() {
            bail!(
                "pw-dump failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(dump_text(&out.stdout))
    }

    fn run(&self, argv: &[String]) -> Result<()> {
        let (program, args) = argv.split_first().context("empty command")?;
        let out = Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("running {program}"))?;
        if !out.status.success() {
            bail!(
                "`{}` failed: {}",
                argv.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

/// `pw-dump`'s output as text. PipeWire passes the property strings clients
/// give it through byte for byte, so one app named in Latin-1 must not make
/// the whole graph unreadable: its stray bytes become U+FFFD inside the string
/// they're in, and the JSON stays valid.
fn dump_text(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout).into_owned()
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// Set a node's volume on the mixer's 0..=1 scale (what `wpctl` and the desktop show).
pub fn set_volume(id: u32, volume: f32) -> Vec<String> {
    argv(&[
        "wpctl",
        "set-volume",
        &id.to_string(),
        &format!("{:.3}", volume.clamp(0.0, 1.0)),
    ])
}

/// Send a stream or outlet to a sink, by `node.name`.
pub fn set_target(id: u32, sink: &str) -> Vec<String> {
    argv(&["pw-metadata", &id.to_string(), "target.object", sink])
}

/// Make a sink the default output.
pub fn set_default(id: u32) -> Vec<String> {
    argv(&["wpctl", "set-default", &id.to_string()])
}

/// Set filter controls on a node (`pw-cli set-param <id> Props <props>`).
pub fn set_params(id: u32, props: &str) -> Vec<String> {
    argv(&["pw-cli", "set-param", &id.to_string(), "Props", props])
}

/// Restart PipeWire so it re-reads its config: a 1–2 s gap in all audio.
pub fn restart_pipewire() -> Vec<String> {
    argv(&[
        "systemctl",
        "--user",
        "restart",
        "pipewire.service",
        "pipewire-pulse.service",
        "wireplumber.service",
    ])
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_isnt_utf8_leaves_the_dump_readable() {
        let mut stdout =
            br#"[{"id":57,"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"caf"#
                .to_vec();
        stdout.push(0xE9); // é in Latin-1, as an ALSA client may name itself
        stdout.extend_from_slice(
            br#"","media.class":"Stream/Output/Audio"}}},{"id":58,"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"firefox","media.class":"Stream/Output/Audio"}}}]"#,
        );
        let graph = crate::graph::Graph::parse(&dump_text(&stdout)).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert!(
            graph.node("firefox").is_some(),
            "every other node is still read"
        );
    }

    #[test]
    fn commands_are_what_a_person_would_type() {
        assert_eq!(set_volume(57, 0.5), ["wpctl", "set-volume", "57", "0.500"]);
        assert_eq!(
            set_volume(57, 1.7),
            ["wpctl", "set-volume", "57", "1.000"],
            "clamped"
        );
        assert_eq!(
            set_target(99, "gglinux_chat"),
            ["pw-metadata", "99", "target.object", "gglinux_chat"]
        );
        assert_eq!(set_default(20), ["wpctl", "set-default", "20"]);
        assert_eq!(
            set_params(21, "{ params = [ ] }"),
            ["pw-cli", "set-param", "21", "Props", "{ params = [ ] }"]
        );
        assert_eq!(
            restart_pipewire(),
            [
                "systemctl",
                "--user",
                "restart",
                "pipewire.service",
                "pipewire-pulse.service",
                "wireplumber.service"
            ]
        );
    }
}
