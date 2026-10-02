//! CLI client for gglinuxd. Installed as `ggctl`; from a checkout:
//! `cargo run -p gglinuxd --bin ggctl`.
//!
//! ```text
//! ggctl                                   # watch: device list, then live events
//! ggctl mouse                             # battery + settings we have applied
//! ggctl mouse dpi 800,1600 [--select N] [--save]   # N counts from 0
//! ggctl mouse dpi 400:800,1600:1600       # per-axis X:Y
//! ggctl mouse polling 1000 [--save]
//! ggctl mouse buttons button1,button2,button3,button4,button5,dpi [--save]
//! ggctl mouse save                        # commit whatever is applied to onboard memory
//! ggctl clip save                         # save the instant replay (bind this to a key)
//! ggctl profile list                      # saved lighting profiles
//! ggctl profile apply <name>
//! ggctl gamesense test|on|off
//! ```
//!
//! Prefer this over `gg-device-mouse`'s `mousectl` whenever the daemon is running:
//! the daemon holds the device, and two writers on one hidraw node interleave badly.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gg_ipc::{DpiStage, MouseSettings, Request, RequestFrame, ResponseData, ServerMessage};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Before connecting, so a typo fails the same whether or not the daemon runs.
    let action = parse(&args)?;

    let path = gg_ipc::socket_path();
    let stream = UnixStream::connect(&path)
        .with_context(|| format!("connecting to {} — is gglinuxd running?", path.display()))?;

    let request = match action {
        Action::Watch => return watch(stream),
        Action::Send(request) => request,
    };

    match send(stream, request)? {
        ResponseData::Mouse { state } => {
            println!(
                "connected: {}\nbattery:   {}",
                state.connected,
                match state.battery {
                    Some(b) => format!("{}%{}", b.level, if b.charging { " (charging)" } else { "" }),
                    None => "unknown (asleep — move the mouse and retry)".into(),
                }
            );
            match state.applied {
                Some(s) => {
                    if !s.dpi_stages.is_empty() {
                        println!("dpi:       {}", dpi_summary(&s));
                    }
                    if !s.buttons.is_empty() {
                        println!("buttons:   {}", s.buttons.join(","));
                    }
                    println!("polling:   {} Hz", s.polling_hz);
                }
                // The mouse protocol is write-only, so an unwritten setting is
                // genuinely unknown rather than assumed to be the default.
                None => println!("applied:   nothing yet this session (mouse is on its onboard settings)"),
            }
        }
        ResponseData::Profiles { state } => {
            for name in state.profiles {
                println!("{name}");
            }
        }
        ResponseData::Success => {
            println!("ok");
            // Only the mouse forgets what isn't saved; `mouse save` is the saving itself.
            let save = args.iter().any(|a| a == "--save");
            if !save && matches!(positional(&args).as_slice(), ["mouse", command, ..] if *command != "save") {
                println!("note: not saved — pass --save to keep this across a power cycle");
            }
        }
        other => println!("{other:?}"),
    }
    Ok(())
}

/// What the command line asks for.
enum Action {
    /// Stream everything the daemon sends until it goes quiet.
    Watch,
    Send(Request),
}

fn parse(args: &[String]) -> Result<Action> {
    let save = args.iter().any(|a| a == "--save");
    Ok(Action::Send(match positional(args).split_first() {
        None => return Ok(Action::Watch),
        Some((&"mouse", rest)) => mouse_request(rest, args, save)?,
        Some((&"clip", ["save"])) => Request::SaveClip,
        Some((&"clip", _)) => bail!("usage: ggctl clip save"),
        Some((&"profile", ["list"])) => Request::GetProfiles,
        // Names may have spaces, so `profile apply Late Night` needs no quotes.
        Some((&"profile", ["apply", name @ ..])) if !name.is_empty() => {
            Request::ApplyProfile { name: name.join(" ") }
        }
        Some((&"profile", _)) => bail!("usage: ggctl profile list | ggctl profile apply <name>"),
        Some((&"gamesense", ["test"])) => Request::GamesenseTest,
        Some((&"gamesense", ["on"])) => Request::SetGamesenseEnabled { enabled: true },
        Some((&"gamesense", ["off"])) => Request::SetGamesenseEnabled { enabled: false },
        Some((&"gamesense", _)) => bail!("usage: ggctl gamesense test|on|off"),
        Some((other, _)) => {
            bail!("unknown command '{other}' (try: mouse, clip, profile, gamesense, or nothing to watch)")
        }
    }))
}

/// The arguments that aren't `--flags`, or the value that follows `--select`.
fn positional(args: &[String]) -> Vec<&str> {
    let mut words = Vec::new();
    let mut args = args.iter().map(String::as_str);
    while let Some(a) = args.next() {
        if a == "--select" {
            // Its number, which would otherwise be taken for the DPI list.
            args.next();
        } else if !a.starts_with("--") {
            words.push(a);
        }
    }
    words
}

fn mouse_request(rest: &[&str], args: &[String], save: bool) -> Result<Request> {
    Ok(match rest.split_first() {
        None => Request::GetMouseState,
        Some((&"dpi", tail)) => Request::SetMouseDpi {
            stages: parse_stages(tail.first().context("usage: ggctl mouse dpi 800,1600")?)?,
            active: flag_value(args, "--select")
                .map(|v| v.parse::<usize>().context("--select expects a number"))
                .transpose()?
                .unwrap_or(0),
            save,
        },
        Some((&"polling", tail)) => Request::SetMousePolling {
            hz: tail
                .first()
                .context("usage: ggctl mouse polling 1000")?
                .parse()
                .context("polling rate must be a number")?,
            save,
        },
        Some((&"buttons", tail)) => Request::SetMouseButtons {
            actions: tail
                .first()
                .context("usage: ggctl mouse buttons button1,...,dpi")?
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            save,
        },
        Some((&"save", _)) => Request::SaveMouse,
        Some((other, _)) => bail!("unknown mouse command '{other}' (try: dpi, polling, buttons, save)"),
    })
}

/// Send one request and return its response, ignoring events that arrive first.
fn send(mut stream: UnixStream, req: Request) -> Result<ResponseData> {
    const ID: u64 = 1;
    let frame = serde_json::to_string(&RequestFrame { id: ID, req })?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    writeln!(stream, "{frame}")?;

    for line in BufReader::new(stream).lines() {
        let line = line.context("daemon closed the connection or timed out")?;
        match serde_json::from_str::<ServerMessage>(&line) {
            // The daemon pushes a snapshot on connect; that is not our answer.
            Ok(ServerMessage::Notification { .. }) => continue,
            Ok(ServerMessage::Response { result, .. }) => {
                if let ResponseData::Error { message } = result {
                    bail!("{message}");
                }
                return Ok(result);
            }
            Err(e) => bail!("could not parse daemon reply: {e}\n  {line}"),
        }
    }
    bail!("no response from daemon")
}

/// Print every message until the daemon goes quiet for two seconds.
fn watch(stream: UnixStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut stream = stream;
    writeln!(stream, r#"{{"id":1,"req":{{"method":"list_devices"}}}}"#)?;

    for line in BufReader::new(stream).lines() {
        match line {
            Ok(l) => println!("{l}"),
            Err(_) => break, // read timeout — nothing more to read
        }
    }
    Ok(())
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).map(String::as_str)
}

/// The applied DPI stages, and which one is active.
///
/// The stage is numbered from 0, as `--select` takes it, so the number shown here
/// can be passed straight back.
fn dpi_summary(s: &MouseSettings) -> String {
    let stages: Vec<String> = s
        .dpi_stages
        .iter()
        .map(|d| if d.x == d.y { d.x.to_string() } else { format!("{}:{}", d.x, d.y) })
        .collect();
    format!("{} (stage {} active, counting from 0)", stages.join(","), s.active_stage)
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

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn sends(line: &str) -> Request {
        match parse(&args(line)).unwrap() {
            Action::Send(r) => r,
            Action::Watch => panic!("{line} parsed as watch"),
        }
    }

    #[test]
    fn no_arguments_watches() {
        assert!(matches!(parse(&[]).unwrap(), Action::Watch));
    }

    #[test]
    fn the_new_commands_map_to_their_requests() {
        assert!(matches!(sends("clip save"), Request::SaveClip));
        assert!(matches!(sends("profile list"), Request::GetProfiles));
        assert!(matches!(sends("gamesense test"), Request::GamesenseTest));
        assert!(matches!(sends("gamesense on"), Request::SetGamesenseEnabled { enabled: true }));
        assert!(matches!(sends("gamesense off"), Request::SetGamesenseEnabled { enabled: false }));
        assert!(matches!(sends("mouse"), Request::GetMouseState));
        assert!(matches!(sends("mouse polling 1000"), Request::SetMousePolling { hz: 1000, save: false }));
    }

    #[test]
    fn profile_names_may_contain_spaces() {
        match sends("profile apply Late Night") {
            Request::ApplyProfile { name } => assert_eq!(name, "Late Night"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_select_value_is_not_read_as_the_dpi_list() {
        for line in ["mouse dpi --select 1 800,1600 --save", "mouse dpi 800,1600 --select 1 --save"] {
            match sends(line) {
                Request::SetMouseDpi { stages, active, save } => {
                    assert_eq!(stages, [DpiStage { x: 800, y: 800 }, DpiStage { x: 1600, y: 1600 }], "{line}");
                    assert_eq!((active, save), (1, true), "{line}");
                }
                other => panic!("{line}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_active_stage_shows_as_the_number_select_takes() {
        let applied = MouseSettings {
            dpi_stages: [400, 800, 1600].map(|d| DpiStage { x: d, y: d }).to_vec(),
            active_stage: 1,
            polling_hz: 1000,
            buttons: vec![],
        };
        let shown = dpi_summary(&applied);
        let n = shown.split("stage ").nth(1).and_then(|s| s.split(' ').next()).unwrap();
        // Passing back the number `ggctl mouse` shows keeps that stage active.
        match sends(&format!("mouse dpi 400,800,1600 --select {n}")) {
            Request::SetMouseDpi { active, .. } => assert_eq!(active, applied.active_stage, "{shown}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mistakes_are_errors() {
        for bad in ["clip", "clip delete", "profile", "profile apply", "gamesense maybe", "frobnicate"] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }
}
