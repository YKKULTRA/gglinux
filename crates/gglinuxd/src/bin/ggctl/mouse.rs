// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl mouse`: the Rival 3's settings, as the daemon has applied them.

use anyhow::{bail, Context, Result};
use gg_device_mouse::proto::{self, WheelSide};
use gg_device_mouse::ButtonAction;
use gg_ipc::{DpiStage, MouseDpi, MouseRadio, MouseState, Request, ResponseData};

use crate::flag_value;

/// The factory buttons (`proto::default_buttons`), by the names the daemon takes.
const FACTORY_BUTTONS: [&str; 6] = ["button1", "button2", "button3", "button4", "button5", "dpi"];

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
        Some((&"buttons", _)) => {
            bail!("usage: ggctl mouse buttons button1,...,dpi[,up,down] [--save]   (6 buttons, then the wheel's two)")
        }
        // Checked before the daemon is asked anything, so a typo fails the same either way.
        Some((&"wheel", [up, down])) => {
            let wheel = [
                wheel_name(up, WheelSide::Up)?,
                wheel_name(down, WheelSide::Down)?,
            ];
            wheel_request(applied_buttons()?.as_deref(), wheel, save)
        }
        Some((&"wheel", _)) => {
            bail!("usage: ggctl mouse wheel <up> <down> [--save]   (each {WHEEL_TAKES})")
        }
        Some((&"save", [])) => Request::SaveMouse,
        Some((&"save", _)) => bail!("usage: ggctl mouse save"),
        Some((&"sleep", [minutes])) => Request::SetMouseSleep {
            minutes: minutes
                .trim_end_matches("min")
                .parse()
                .context("the sleep timer is a number of minutes, 1 to 20")?,
        },
        Some((&"sleep", _)) => bail!("usage: ggctl mouse sleep 20   (minutes, 1 to 20; saved)"),
        Some((&"radio", [mode])) => Request::SetMouseRadio {
            radio: MouseRadio::ALL
                .into_iter()
                .find(|r| r.key() == *mode)
                .context("the radio mode is one of: normal, stable, efficient")?,
            save,
        },
        Some((&"radio", _)) => bail!("usage: ggctl mouse radio normal|stable|efficient [--save]"),
        Some((other, _)) => {
            bail!(
                "unknown mouse command '{other}' (try: dpi, polling, buttons, wheel, sleep, radio, save)"
            )
        }
    })
}

/// What the wheel takes, in words: turned up what a button takes, as GG offers it; turned
/// down only what a capture shows there.
const WHEEL_TAKES: &str =
    "up: what a button takes (button1 to button6, dpi, scrollup, scrolldown, disable, a key such as f5, or a media key such as volumeup); down: scrolldown or button4";

/// A wheel action as typed, if the daemon takes it for the wheel on `side`.
fn wheel_name(name: &str, side: WheelSide) -> Result<String> {
    match ButtonAction::parse(name) {
        Ok(action) if proto::wheel_takes(side, action) => Ok(name.trim().to_ascii_lowercase()),
        Ok(_) => bail!(
            "“{}” is not known for the wheel's down position yet: turned down it takes scrolldown or button4",
            name.trim()
        ),
        Err(_) => bail!("the wheel doesn't take “{}”: {WHEEL_TAKES}", name.trim()),
    }
}

/// `ggctl mouse wheel`'s request: the six buttons as the daemon applied them (the factory
/// six if it applied none), then the wheel's two. Buttons can't be read from the mouse, so
/// the daemon's record is all there is.
fn wheel_request(applied: Option<&[String]>, wheel: [String; 2], save: bool) -> Request {
    let mut actions: Vec<String> = match applied {
        Some(buttons) if buttons.len() >= FACTORY_BUTTONS.len() => {
            buttons[..FACTORY_BUTTONS.len()].to_vec()
        }
        _ => FACTORY_BUTTONS.map(String::from).to_vec(),
    };
    actions.extend(wheel);
    Request::SetMouseButtons { actions, save }
}

/// The buttons the daemon has applied, if any.
fn applied_buttons() -> Result<Option<Vec<String>>> {
    let ResponseData::Mouse { state } = crate::send(crate::connect()?, Request::GetMouseState)?
    else {
        bail!("the daemon did not send the mouse state");
    };
    Ok(state.applied.and_then(|a| a.buttons))
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
    let reported = reported_lines(state);
    if !reported.is_empty() {
        println!("the mouse reports:");
        for line in reported {
            println!("  {line}");
        }
    } else if state.connected && state.battery.is_none() {
        println!("settings:  not known while it's asleep; it reports them once it wakes");
    }
    match &state.applied {
        Some(s) => {
            if let Some(dpi) = &s.dpi {
                println!("dpi:       {}", dpi_summary(dpi));
            }
            if let Some(buttons) = &s.buttons {
                for line in button_lines(buttons) {
                    println!("{line}");
                }
            }
            if let Some(hz) = s.polling_hz {
                println!("polling:   {hz} Hz");
            }
            if let (Some(m), None) = (s.sleep_minutes, state.sleep_secs) {
                println!("sleep:     {m} min (written)");
            }
            if let (Some(r), None) = (s.radio, state.radio) {
                println!("radio:     {} (written)", r.key());
            }
        }
        // The mouse protocol is write-only, so an unwritten setting is
        // genuinely unknown rather than assumed to be the default.
        None => println!("applied:   nothing yet this session (mouse is on its onboard settings)"),
    }
}

/// The applied buttons, and the wheel's two on their own line once they were written.
fn button_lines(buttons: &[String]) -> Vec<String> {
    let (six, wheel) = buttons.split_at(buttons.len().min(FACTORY_BUTTONS.len()));
    let mut lines = vec![format!("buttons:   {}", six.join(","))];
    if let [up, down] = wheel {
        lines.push(format!("wheel:     up {up}, down {down}"));
    }
    lines
}

/// What the mouse itself reports of its settings, a line each; empty while they aren't
/// read (`mouse_power` off, or no answer yet).
fn reported_lines(state: &MouseState) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(dpi) = &state.reported_dpi {
        lines.push(format!("dpi:     {}", dpi_summary(dpi)));
    }
    if let Some(hz) = state.reported_polling_hz {
        lines.push(format!("polling: {hz} Hz"));
    }
    if let Some(secs) = state.sleep_secs {
        lines.push(format!("sleep:   {}", sleep_text(secs)));
    }
    if let Some(radio) = state.radio {
        lines.push(format!("radio:   {} ({})", radio.key(), radio.label()));
    }
    lines
}

/// A sleep timer in seconds as minutes, or minutes and seconds if it isn't whole.
fn sleep_text(secs: u16) -> String {
    match secs % 60 {
        0 => format!("{} min", secs / 60),
        s => format!("{} min {s} s", secs / 60),
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
    fn sleep_takes_minutes_and_nothing_else() {
        for line in ["mouse sleep 20", "mouse sleep 20min"] {
            assert!(
                matches!(sends(line), Request::SetMouseSleep { minutes: 20 }),
                "{line}"
            );
        }
        for bad in [
            "mouse sleep",
            "mouse sleep twenty",
            "mouse sleep 5 10",
            "mouse sleep -1",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
        assert_eq!(sleep_text(1200), "20 min");
        assert_eq!(sleep_text(90), "1 min 30 s");
    }

    #[test]
    fn radio_takes_one_of_its_three_names() {
        for (name, radio) in [
            ("normal", MouseRadio::Normal),
            ("stable", MouseRadio::Stable),
            ("efficient", MouseRadio::HighEfficiency),
        ] {
            match sends(&format!("mouse radio {name} --save")) {
                Request::SetMouseRadio { radio: r, save } => assert_eq!((r, save), (radio, true)),
                other => panic!("{name}: {other:?}"),
            }
        }
        for bad in [
            "mouse radio",
            "mouse radio fast",
            "mouse radio normal stable",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn what_the_mouse_reports_is_listed_only_where_it_answered() {
        let mut state = MouseState::default();
        assert!(reported_lines(&state).is_empty());
        state.sleep_secs = Some(1200);
        state.radio = Some(MouseRadio::Stable);
        state.reported_polling_hz = Some(125);
        state.reported_dpi = Some(MouseDpi {
            stages: [400, 800].map(|d| DpiStage { x: d, y: d }).to_vec(),
            active: 0,
        });
        assert_eq!(
            reported_lines(&state),
            [
                "dpi:     400,800 (stage 0 active, counting from 0)",
                "polling: 125 Hz",
                "sleep:   20 min",
                "radio:   stable (Wireless Stability)",
            ]
        );
    }

    #[test]
    fn the_wheel_keeps_the_applied_buttons_or_the_factory_six() {
        let wheel = || ["button4".to_string(), "scrolldown".to_string()];
        let Request::SetMouseButtons { actions, save } = wheel_request(None, wheel(), true) else {
            panic!("not a buttons request");
        };
        assert_eq!(
            actions,
            [
                "button1",
                "button2",
                "button3",
                "button4",
                "button5",
                "dpi",
                "button4",
                "scrolldown"
            ]
        );
        assert!(save);
        let applied: Vec<String> = [
            "f13",
            "button2",
            "button3",
            "button4",
            "button5",
            "dpi",
            "scrollup",
            "scrolldown",
        ]
        .map(String::from)
        .to_vec();
        let Request::SetMouseButtons { actions, .. } =
            wheel_request(Some(applied.as_slice()), wheel(), false)
        else {
            panic!("not a buttons request");
        };
        assert_eq!(actions[0], "f13", "the applied buttons are kept");
        assert_eq!(actions[6..], ["button4", "scrolldown"]);
        assert_eq!(
            button_lines(&actions),
            [
                "buttons:   f13,button2,button3,button4,button5,dpi",
                "wheel:     up button4, down scrolldown"
            ]
        );
        assert_eq!(
            button_lines(&applied[..6]).len(),
            1,
            "no wheel line for six"
        );
    }

    /// Turned up, any name a button takes goes on the wheel; turned down only scrolldown
    /// and button4. Anything else is refused before the daemon is asked anything, so no
    /// daemon is needed here.
    #[test]
    fn the_wheel_takes_any_button_name_turned_up() {
        for name in [
            "button1", "Button6", "dpi", "disable", "f5", "a", "volumeup", "mute",
        ] {
            assert_eq!(
                wheel_name(name, WheelSide::Up).unwrap(),
                name.to_ascii_lowercase()
            );
            if !name.eq_ignore_ascii_case("button4") {
                let e = wheel_name(name, WheelSide::Down).unwrap_err().to_string();
                assert!(
                    e.contains("not known for the wheel's down position yet"),
                    "{e}"
                );
            }
        }
        for name in ["scrolldown", "button4"] {
            assert_eq!(wheel_name(name, WheelSide::Down).unwrap(), name);
        }
        let e = parse(&args("mouse wheel scrolldown scrollup"))
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("down position"), "{e}");
        for bad in ["mouse wheel f99 scrolldown", "mouse wheel scrollup button7"] {
            let e = parse(&args(bad)).err().unwrap().to_string();
            assert!(e.contains("the wheel doesn't take"), "{bad}: {e}");
            assert!(e.contains("button1 to button6"), "{e}");
        }
        for bad in ["mouse wheel", "mouse wheel button4", "mouse wheel a b c"] {
            let e = parse(&args(bad)).err().unwrap().to_string();
            assert!(e.starts_with("usage: ggctl mouse wheel"), "{bad}: {e}");
        }
        assert_eq!(wheel_name(" Button4 ", WheelSide::Down).unwrap(), "button4");
    }

    #[test]
    fn select_takes_the_equals_form_too() {
        match sends("mouse dpi 400,800 --select=1") {
            Request::SetMouseDpi { active, .. } => assert_eq!(active, 1),
            other => panic!("{other:?}"),
        }
    }
}
