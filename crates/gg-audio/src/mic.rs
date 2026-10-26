//! The virtual mic: RNNoise noise suppression → noise gate → compressor on a
//! physical mic, as a source apps record from.
//!
//! Like the channels, the chain is fixed once PipeWire builds it, so every stage
//! stays in the graph and is switched off through its own controls: RNNoise's
//! `Dry Mix` at 1, the gate's thresholds at 0 (always open), the compressor at 1:1.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{ensure, Result};
use gg_ipc::MicSettings;

/// The virtual source apps record from.
pub const SOURCE: &str = "gglinux_mic";
/// The stream that reads the physical mic; the filter's controls live on it.
pub const CAPTURE: &str = "gglinux_mic.in";

/// LADSPA plugins; PipeWire appends `.so` when it loads them.
const RNNOISE: &str = "librnnoise_ladspa";
/// swh's SC4 compressor, which Smart Volume uses too.
pub(crate) const SC4M: &str = "sc4m_1916";

/// Where PipeWire 1.6 looks for LADSPA plugins when `LADSPA_PATH` isn't set
/// (`/usr/lib` is its library directory on Arch).
const DEFAULT_LADSPA_DIRS: [&str; 3] = ["/usr/lib64/ladspa", "/usr/lib/ladspa", "/usr/lib"];

/// Compressor settings the UI does not expose.
const COMP_THRESHOLD_DB: f32 = -18.0;
const COMP_RATIO: f32 = 4.0;

/// Whether both plugins are installed where PipeWire looks.
pub fn plugins_present() -> bool {
    plugins_present_in(&ladspa_dirs(std::env::var_os("LADSPA_PATH")))
}

/// Whether both plugins exist as files in one of `dirs`.
pub fn plugins_present_in(dirs: &[PathBuf]) -> bool {
    ladspa_found(dirs, RNNOISE) && ladspa_found(dirs, SC4M)
}

/// Where PipeWire looks for LADSPA plugins, given `LADSPA_PATH`: its entries
/// replace the defaults rather than come before them.
pub(crate) fn ladspa_dirs(ladspa_path: Option<OsString>) -> Vec<PathBuf> {
    match ladspa_path {
        Some(path) => std::env::split_paths(&path)
            .filter(|d| !d.as_os_str().is_empty())
            .collect(),
        None => DEFAULT_LADSPA_DIRS.iter().map(PathBuf::from).collect(),
    }
}

/// Whether a plugin, named as PipeWire loads it (without `.so`), is a file in one of `dirs`.
pub(crate) fn ladspa_found(dirs: &[PathBuf], plugin: &str) -> bool {
    dirs.iter()
        .any(|d| d.join(format!("{plugin}.so")).is_file())
}

pub fn validate(s: &MicSettings) -> Result<()> {
    ensure!(
        s.denoise_strength.is_finite() && (0.0..=95.0).contains(&s.denoise_strength),
        "noise suppression strength {} out of range (0..=95)",
        s.denoise_strength
    );
    ensure!(
        s.gate_threshold_db.is_finite() && (-80.0..=-10.0).contains(&s.gate_threshold_db),
        "gate threshold {} dB out of range (-80..=-10)",
        s.gate_threshold_db
    );
    ensure!(
        s.makeup_db.is_finite() && (0.0..=18.0).contains(&s.makeup_db),
        "makeup gain {} dB out of range (0..=18)",
        s.makeup_db
    );
    Ok(())
}

/// The drop-in module for a mic chain reading `input` (a source `node.name`).
///
/// `nofail`: if a plugin can't be loaded after all, PipeWire skips this module
/// and still loads the channels. `node.dont-fallback`: if the physical mic
/// disappears, the capture waits rather than falling back to the default input —
/// which, with Sonar on, is this very mic.
pub fn module(input: &str) -> Result<String> {
    crate::config::check_name(input)?;
    Ok(format!(
        r#"    {{ name = libpipewire-module-filter-chain
      flags = [ nofail ]
      args = {{
        node.description = "gglinux Mic"
        audio.channels   = 1
        audio.position   = [ MONO ]
        filter.graph = {{
          nodes = [
            {{ type = ladspa name = rnnoise plugin = "{RNNOISE}" label = noise_suppressor_mono control = {{ "VAD Threshold (%)" = 50.0 "VAD Grace Period (ms)" = 200.0 "Retroactive VAD Grace (ms)" = 0.0 "Dry Mix" = 0.0 }} }}
            {{ type = builtin name = gate label = noisegate control = {{ "Open Threshold" = 0.0 "Close Threshold" = 0.0 "Attack (s)" = 0.005 "Hold (s)" = 0.2 "Release (s)" = 0.1 }} }}
            {{ type = ladspa name = comp plugin = "{SC4M}" label = sc4m control = {{ "RMS/peak" = 0.5 "Attack time (ms)" = 5.0 "Release time (ms)" = 100.0 "Threshold level (dB)" = {COMP_THRESHOLD_DB:.1} "Ratio (1:n)" = 1.0 "Knee radius (dB)" = 3.0 "Makeup gain (dB)" = 0.0 }} }}
          ]
          links = [
            {{ output = "rnnoise:Output" input = "gate:In" }}
            {{ output = "gate:Out" input = "comp:Input" }}
          ]
        }}
        capture.props = {{
          node.name          = "{CAPTURE}"
          node.passive       = true
          target.object      = "{input}"
          node.dont-fallback = true
        }}
        playback.props = {{
          node.name   = "{SOURCE}"
          media.class = "Audio/Source"
        }}
      }}
    }}
"#
    ))
}

/// The `pw-cli set-param` Props for the chain's live controls.
pub fn props(s: &MicSettings) -> String {
    let dry = if s.denoise { 0.0 } else { 1.0 };
    let open = if s.gate {
        10f32.powf(s.gate_threshold_db / 20.0)
    } else {
        0.0
    };
    // Close a little below the open level, so the gate doesn't chatter.
    let close = open * 0.7;
    let (ratio, makeup) = if s.compressor {
        (COMP_RATIO, s.makeup_db)
    } else {
        (1.0, 0.0)
    };
    format!(
        "{{ params = [ \"rnnoise:VAD Threshold (%)\" {strength:.1} \"rnnoise:Dry Mix\" {dry:.1} \
         \"gate:Open Threshold\" {open:.6} \"gate:Close Threshold\" {close:.6} \
         \"comp:Ratio (1:n)\" {ratio:.1} \"comp:Makeup gain (dB)\" {makeup:.1} ] }}",
        strength = s.denoise_strength
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_is_a_mono_nofail_chain_into_a_virtual_source() {
        let text = module("alsa_input.usb-arctis").unwrap();
        assert!(
            text.contains("flags = [ nofail ]"),
            "a broken plugin must not take the channels down"
        );
        assert!(text.contains("audio.channels   = 1"));
        for label in [
            "label = noise_suppressor_mono",
            "label = noisegate",
            "label = sc4m",
        ] {
            assert!(text.contains(label), "{label}");
        }
        assert!(text.contains(r#"target.object      = "alsa_input.usb-arctis""#));
        assert!(
            text.contains("node.dont-fallback = true"),
            "never falls back to the default input: itself"
        );
        assert!(text.contains(r#"node.name   = "gglinux_mic""#));
        assert!(text.contains(r#"media.class = "Audio/Source""#));
        assert_eq!(text.matches("input = ").count(), 2);
        assert!(module("bad\"name").is_err());
    }

    #[test]
    fn every_stage_switches_off_with_its_own_controls() {
        let off = MicSettings {
            denoise: false,
            gate: false,
            compressor: false,
            ..MicSettings::default()
        };
        let p = props(&off);
        assert!(p.contains("\"rnnoise:Dry Mix\" 1.0"), "{p}");
        assert!(p.contains("\"gate:Open Threshold\" 0.000000"), "{p}");
        assert!(p.contains("\"gate:Close Threshold\" 0.000000"), "{p}");
        assert!(p.contains("\"comp:Ratio (1:n)\" 1.0"), "{p}");
        assert!(p.contains("\"comp:Makeup gain (dB)\" 0.0"), "{p}");
    }

    #[test]
    fn stages_on_use_their_settings() {
        let on = MicSettings {
            denoise: true,
            denoise_strength: 70.0,
            gate: true,
            gate_threshold_db: -40.0,
            compressor: true,
            makeup_db: 9.0,
        };
        let p = props(&on);
        assert!(p.contains("\"rnnoise:VAD Threshold (%)\" 70.0"), "{p}");
        assert!(p.contains("\"rnnoise:Dry Mix\" 0.0"), "{p}");
        assert!(p.contains("\"gate:Open Threshold\" 0.010000"), "{p}");
        assert!(p.contains("\"gate:Close Threshold\" 0.007000"), "{p}");
        assert!(p.contains("\"comp:Ratio (1:n)\" 4.0"), "{p}");
        assert!(p.contains("\"comp:Makeup gain (dB)\" 9.0"), "{p}");
    }

    #[test]
    fn settings_are_range_checked() {
        assert!(validate(&MicSettings::default()).is_ok());
        for bad in [
            MicSettings {
                denoise_strength: 99.0,
                ..MicSettings::default()
            },
            MicSettings {
                gate_threshold_db: 0.0,
                ..MicSettings::default()
            },
            MicSettings {
                makeup_db: 30.0,
                ..MicSettings::default()
            },
            MicSettings {
                makeup_db: f32::NAN,
                ..MicSettings::default()
            },
        ] {
            assert!(validate(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn plugins_are_found_by_file() {
        let dir = crate::testing::scratch("ladspa");
        let dirs = [dir.to_path_buf()];
        assert!(!plugins_present_in(&dirs));
        std::fs::write(dir.join("librnnoise_ladspa.so"), b"").unwrap();
        assert!(!plugins_present_in(&dirs), "both are needed");
        std::fs::write(dir.join("sc4m_1916.so"), b"").unwrap();
        assert!(plugins_present_in(&dirs));
    }

    #[test]
    fn plugins_are_looked_for_where_pipewire_looks() {
        let defaults: Vec<PathBuf> = ["/usr/lib64/ladspa", "/usr/lib/ladspa", "/usr/lib"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(ladspa_dirs(None), defaults);
        assert_eq!(
            ladspa_dirs(Some("/opt/a::/opt/b".into())),
            [PathBuf::from("/opt/a"), PathBuf::from("/opt/b")],
            "LADSPA_PATH replaces the defaults, and an empty entry is skipped"
        );
    }
}
