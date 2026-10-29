//! CLI client for gglinuxd. Run the daemon, then `cargo run -p gglinuxd --example ggctl`.
//!
//! ```text
//! ggctl                                   # watch: device list, then live events
//! ggctl mouse                             # battery + settings we have applied
//! ggctl mouse dpi 800,1600 [--select N] [--save]
//! ggctl mouse dpi 400:800,1600:1600       # per-axis X:Y
//! ggctl mouse polling 1000 [--save]
//! ggctl mouse buttons button1,button2,button3,button4,button5,dpi [--save]
//! ggctl mouse save                        # commit whatever is applied to onboard memory
//! ggctl clip save                         # save the instant replay
//! ```
//!
//! Prefer this over `gg-device-mouse`'s `mousectl` whenever the daemon is running:
//! the daemon holds the device, and two writers on one hidraw node interleave badly.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gg_ipc::{DpiStage, Request, RequestFrame, ResponseData, ServerMessage};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let save = args.iter().any(|a| a == "--save");
    let positional: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with("--"))
        .collect();

    let path = gg_ipc::socket_path();
    let stream = UnixStream::connect(&path)
        .with_context(|| format!("connecting to {} — is gglinuxd running?", path.display()))?;

    let request = match positional.split_first() {
        // No arguments: stream everything the daemon sends until it goes quiet.
        None => return watch(stream),
        Some((&"mouse", rest)) => mouse_request(rest, &args, save)?,
        Some((&"clip", ["save"])) => Request::SaveClip,
        Some((&"clip", _)) => bail!("usage: ggctl clip save"),
        Some((other, _)) => bail!("unknown command '{other}' (try: mouse, clip, or no argument to watch)"),
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
                        let stages: Vec<String> = s
                            .dpi_stages
                            .iter()
                            .map(|d| if d.x == d.y { d.x.to_string() } else { format!("{}:{}", d.x, d.y) })
                            .collect();
                        println!("dpi:       {} (stage {} active)", stages.join(","), s.active_stage + 1);
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
        ResponseData::Success => {
            println!("ok");
            if !save && positional.get(1).is_some_and(|c| *c != "save") {
                println!("note: not saved — pass --save to keep this across a power cycle");
            }
        }
        other => println!("{other:?}"),
    }
    Ok(())
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
