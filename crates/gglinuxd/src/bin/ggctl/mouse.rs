// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl mouse`: the Rival 3's settings, as the daemon has applied them.

use anyhow::{bail, Context, Result};
use gg_ipc::{DpiStage, MouseDpi, MouseState, Request};

use crate::flag_value;

/// Each mouse command takes exactly one value, so `dpi 800 1600` (a space where
/// the comma belongs) is refused rather than sent as the single stage 800.
pub fn request(rest: &[&str], args: &[String], save: bool) -> Result<Request> {
    Ok(match rest.split_first() {
        None => Request::GetMouseState,
        Some((&"dpi", [spec])) => Request::SetMouseDpi {
            stages: parse_stages(spec)?,
            active: flag_value(args, "--select")
                .map(|v| v.parse::<usize>().context("--select expects a number"))
                .transpose()?
                .unwrap_or(0),
            save,
        },
        Some((&"dpi", _)) => bail!("usage: ggctl mouse dpi 800,1600 [--select N] [--save]"),
        Some((&"polling", [hz])) => Request::SetMousePolling {
            hz: hz.parse().context("polling rate must be a number")?,
            save,
        },
        Some((&"polling", _)) => bail!("usage: ggctl mouse polling 1000 [--save]"),
        Some((&"buttons", [actions])) => Request::SetMouseButtons {
            actions: actions
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            save,
        },
        Some((&"buttons", _)) => bail!("usage: ggctl mouse buttons button1,...,dpi [--save]"),
        Some((&"save", [])) => Request::SaveMouse,
        Some((&"save", _)) => bail!("usage: ggctl mouse save"),
        Some((other, _)) => {
            bail!("unknown mouse command '{other}' (try: dpi, polling, buttons, save)")
        }
    })
}

/// What `ggctl mouse` prints: whether it's there, its battery, what was applied.
pub fn print(state: &MouseState) {
    println!(
        "connected: {}\nbattery:   {}",
        state.connected,
        match &state.battery {
            Some(b) => format!(
                "{}%{}",
                b.level,
                if b.charging { " (charging)" } else { "" }
            ),
            None => "unknown (asleep — move the mouse and retry)".into(),
        }
    );
    match &state.applied {
        Some(s) => {
            if let Some(dpi) = &s.dpi {
                println!("dpi:       {}", dpi_summary(dpi));
            }
            if let Some(buttons) = &s.buttons {
                println!("buttons:   {}", buttons.join(","));
            }
            if let Some(hz) = s.polling_hz {
                println!("polling:   {hz} Hz");
            }
        }
        // The mouse protocol is write-only, so an unwritten setting is
        // genuinely unknown rather than assumed to be the default.
        None => println!("applied:   nothing yet this session (mouse is on its onboard settings)"),
    }
}

/// The applied DPI stages, and which one is active.
///
/// The stage is numbered from 0, as `--select` takes it, so the number shown here
/// can be passed straight back.
fn dpi_summary(dpi: &MouseDpi) -> String {
    let stages: Vec<String> = dpi
        .stages
        .iter()
        .map(|d| {
            if d.x == d.y {
                d.x.to_string()
            } else {
                format!("{}:{}", d.x, d.y)
            }
        })
        .collect();
    format!(
        "{} (stage {} active, counting from 0)",
        stages.join(","),
        dpi.active
    )
}

/// Parse `800,1600` or `400:800,1600:1600` into DPI stages.
fn parse_stages(spec: &str) -> Result<Vec<DpiStage>> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|part| {
            Ok(match part.split_once(':') {
                Some((x, y)) => DpiStage {
                    x: x.trim().parse().context("invalid X DPI")?,
                    y: y.trim().parse().context("invalid Y DPI")?,
                },
                None => {
                    let dpi = part.parse().context("invalid DPI")?;
                    DpiStage { x: dpi, y: dpi }
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{args, parse, sends};

    #[test]
    fn the_select_value_is_not_read_as_the_dpi_list() {
        for line in [
            "mouse dpi --select 1 800,1600 --save",
            "mouse dpi 800,1600 --select 1 --save",
        ] {
            match sends(line) {
                Request::SetMouseDpi {
                    stages,
                    active,
                    save,
                } => {
                    assert_eq!(
                        stages,
                        [DpiStage { x: 800, y: 800 }, DpiStage { x: 1600, y: 1600 }],
                        "{line}"
                    );
                    assert_eq!((active, save), (1, true), "{line}");
                }
                other => panic!("{line}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_active_stage_shows_as_the_number_select_takes() {
        let applied = MouseDpi {
            stages: [400, 800, 1600].map(|d| DpiStage { x: d, y: d }).to_vec(),
            active: 1,
        };
        let shown = dpi_summary(&applied);
        let n = shown
            .split("stage ")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .unwrap();
        // Passing back the number `ggctl mouse` shows keeps that stage active.
        match sends(&format!("mouse dpi 400,800,1600 --select {n}")) {
            Request::SetMouseDpi { active, .. } => assert_eq!(active, applied.active, "{shown}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_mouse_command_with_a_stray_word_or_flag_writes_nothing() {
        for bad in [
            // A space where the comma belongs would otherwise send just the first stage.
            "mouse dpi 800 1600 --save",
            "mouse dpi",
            "mouse dpi 800,1600 --sava",
            "mouse dpi 800,1600 --select",
            "mouse polling 1000 500",
            "mouse buttons button1, button2",
            "mouse save now",
            "--version",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn select_takes_the_equals_form_too() {
        match sends("mouse dpi 400,800 --select=1") {
            Request::SetMouseDpi { active, .. } => assert_eq!(active, 1),
            other => panic!("{other:?}"),
        }
    }
}
