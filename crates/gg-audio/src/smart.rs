//! Smart Volume: evens out a channel's loudness, like Sonar's.
//!
//! It is swh's SC4 compressor (`sc4m`, from the optional `swh-plugins`) at the
//! end of every channel's chain, one per side, written only when PipeWire can
//! load it (see [`compressor_present`]):
//!
//! ```text
//!         ┌→ sv_comp ───→ In 2 ┐
//! eq_10 ──┤                    sv_mix → vol
//!         └───────────→ In 1 ──┘          ↑ the sink's volume
//! ```
//!
//! Quiet sounds come up by the level's makeup gain, and sounds above its
//! threshold are compressed 4:1. `sv_mix` hears the compressor (on) or the
//! channel EQ (off: an exact passthrough), so switching never needs a PipeWire
//! restart, and the compressor has already settled when it's switched in.

use std::path::PathBuf;

use gg_ipc::{SmartLevel, SmartVolume};

use crate::mic;

/// Where the channel EQ's output goes: into the compressor, and straight
/// into the mix for when Smart Volume is off.
pub const INPUTS: [&str; 2] = ["sv_comp:Input", "sv_mix:In 1"];

/// The stage's nodes, in chain order. `vol` must stay last: a graph without
/// explicit outputs plays its last node's output.
pub const NODES: [&str; 3] = [
    // RMS detection with an instant attack (under 2 ms): the plugin's 5 ms
    // RMS window does the smoothing. 800 ms is its slowest release. The level
    // sets the threshold and makeup; this starts at Balanced's.
    r#"{ type = ladspa name = sv_comp plugin = "sc4m_1916" label = sc4m control = { "RMS/peak" = 0.0 "Attack time (ms)" = 1.5 "Release time (ms)" = 800.0 "Threshold level (dB)" = -24.0 "Ratio (1:n)" = 4.0 "Knee radius (dB)" = 6.0 "Makeup gain (dB)" = 6.0 } }"#,
    // Starts off: all of the channel EQ (In 1), none of the compressor (In 2).
    r#"{ type = builtin name = sv_mix label = mixer control = { "Gain 1" = 1.0 "Gain 2" = 0.0 } }"#,
    r#"{ type = builtin name = vol label = linear control = { "Mult" = 1.0 } }"#,
];

/// The stage's links (output, input), after the channel EQ's into [`INPUTS`].
pub const LINKS: [(&str, &str); 2] = [("sv_comp:Output", "sv_mix:In 2"), ("sv_mix:Out", "vol:In")];

/// The control that carries the sink's volume, after the compressor, so the
/// compressor can't squash the volume slider.
pub const VOLUME_CONTROL: &str = "vol:Mult";

/// Whether PipeWire can load the compressor: `sc4m_1916.so` where it looks.
pub fn compressor_present() -> bool {
    compressor_present_in(&mic::ladspa_dirs(std::env::var_os("LADSPA_PATH")))
}

/// Whether the compressor exists as a file in one of `dirs`.
pub fn compressor_present_in(dirs: &[PathBuf]) -> bool {
    mic::ladspa_found(dirs, mic::SC4M)
}

/// A level's compressor threshold and makeup gain, in dB.
pub fn curve(level: SmartLevel) -> (f32, f32) {
    match level {
        SmartLevel::Quiet => (-30.0, 3.0),
        SmartLevel::Balanced => (-24.0, 6.0),
        SmartLevel::Loud => (-22.0, 9.0),
    }
}

/// The `pw-cli set-param` Props for one channel's stage. Off keeps the
/// level's settings on the compressor, so switching on is seamless.
pub fn props(s: &SmartVolume) -> String {
    let (threshold, makeup) = curve(s.level);
    let (dry, wet) = if s.enabled { (0.0, 1.0) } else { (1.0, 0.0) };
    format!(
        "{{ params = [ \"sv_comp:Threshold level (dB)\" {threshold:.1} \"sv_comp:Makeup gain (dB)\" {makeup:.1} \
         \"sv_mix:Gain 1\" {dry:.1} \"sv_mix:Gain 2\" {wet:.1} ] }}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_is_an_exact_passthrough() {
        for level in [SmartLevel::Quiet, SmartLevel::Balanced, SmartLevel::Loud] {
            let p = props(&SmartVolume {
                enabled: false,
                level,
            });
            // PipeWire's mixer skips an input at gain 0 and copies a lone input at gain 1.
            assert!(
                p.contains("\"sv_mix:Gain 1\" 1.0 \"sv_mix:Gain 2\" 0.0"),
                "{p}"
            );
        }
        assert!(
            NODES[1].contains(r#""Gain 1" = 1.0 "Gain 2" = 0.0"#),
            "the stage starts off"
        );
    }

    #[test]
    fn each_level_sets_its_threshold_and_makeup() {
        let p = props(&SmartVolume {
            enabled: true,
            level: SmartLevel::Balanced,
        });
        assert!(p.starts_with("{ params = [ ") && p.ends_with(" ] }"), "{p}");
        assert!(
            p.contains("\"sv_comp:Threshold level (dB)\" -24.0 \"sv_comp:Makeup gain (dB)\" 6.0"),
            "{p}"
        );
        assert!(
            p.contains("\"sv_mix:Gain 1\" 0.0 \"sv_mix:Gain 2\" 1.0"),
            "on hears the compressor: {p}"
        );
        let quiet = props(&SmartVolume {
            enabled: true,
            level: SmartLevel::Quiet,
        });
        assert!(
            quiet.contains("(dB)\" -30.0 ") && quiet.contains("(dB)\" 3.0 "),
            "{quiet}"
        );
        let loud = props(&SmartVolume {
            enabled: false,
            level: SmartLevel::Loud,
        });
        assert!(
            loud.contains("(dB)\" -22.0 ") && loud.contains("(dB)\" 9.0 "),
            "off keeps the level: {loud}"
        );
        for level in [SmartLevel::Quiet, SmartLevel::Balanced, SmartLevel::Loud] {
            let (threshold, makeup) = curve(level);
            // PipeWire would clamp anything outside the plugin's ranges.
            assert!(
                (-30.0..=0.0).contains(&threshold) && (0.0..=24.0).contains(&makeup),
                "{level:?}"
            );
        }
    }

    #[test]
    fn the_stage_ends_with_the_volume_and_hears_the_eq_or_the_compressor() {
        assert!(
            NODES.last().unwrap().contains("name = vol label = linear"),
            "vol is the graph's output"
        );
        assert!(NODES[0].contains(&format!(
            "name = sv_comp plugin = \"{}\" label = sc4m",
            mic::SC4M
        )));
        assert!(
            NODES[0].contains(
                r#""RMS/peak" = 0.0 "Attack time (ms)" = 1.5 "Release time (ms)" = 800.0"#
            ),
            "{}",
            NODES[0]
        );
        assert_eq!(
            INPUTS,
            ["sv_comp:Input", "sv_mix:In 1"],
            "the channel EQ feeds both"
        );
        assert!(LINKS.contains(&("sv_comp:Output", "sv_mix:In 2")));
        assert_eq!(LINKS.last(), Some(&("sv_mix:Out", "vol:In")));
        assert_eq!(VOLUME_CONTROL, "vol:Mult");
        let from_eq = INPUTS.iter().map(|&input| ("eq_10:Out", input));
        for (output, input) in LINKS.iter().copied().chain(from_eq) {
            for port in [output, input] {
                let node = port.split(':').next().unwrap();
                assert!(
                    node == "eq_10" || NODES.iter().any(|n| n.contains(&format!("name = {node} "))),
                    "{port}"
                );
            }
        }
    }

    #[test]
    fn the_compressor_is_found_by_file() {
        let dir = crate::testing::scratch("smart-ladspa");
        let dirs = [dir.to_path_buf()];
        assert!(!compressor_present_in(&dirs));
        std::fs::write(dir.join("sc4m_1916.so"), b"").unwrap();
        assert!(compressor_present_in(&dirs));
    }
}
