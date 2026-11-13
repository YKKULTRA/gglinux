// SPDX-License-Identifier: GPL-3.0-or-later
//! A stand-in for `gglinuxd` that speaks the real protocol against no hardware.
//!
//! ```text
//! fakemouse                       # three devices, battery drifting down from 87%
//! fakemouse --battery 12          # start low, to see the panel's low-charge state
//! fakemouse --charging
//! fakemouse --fail set_mouse_dpi  # make one method return an error
//! fakemouse --no-mouse            # keyboard + headset only
//! fakemouse --sonar off           # Sonar off; also `unavailable` (no PipeWire) and
//!                                 # `bare` (no plugins, no apps, a missing channel)
//! ```
//!
//! States for the Engine page (any mix; see `Looks`):
//!
//! ```text
//! fakemouse --no-mouse --no-headset --no-keyboard  # no devices at all
//! fakemouse --asleep              # the mouse reports no battery
//! fakemouse --headset off         # or charging / unknown: where the headset is
//! fakemouse --no-spare            # no spare battery in the base station
//! fakemouse --base-closed         # base station listed but not open
//! fakemouse --reported            # base-station readings filled in, screen dark
//! ```
//!
//! Two things this buys that the real daemon cannot:
//!
//! - **It writes nothing.** The Rival 3 protocol has no getters, so applying a
//!   setting destroys the record of what preceded it. Every UI path that ends in a
//!   write can be exercised here instead, as often as you like.
//! - **It logs what the UI actually sent.** Since it deserializes with the same
//!   `gg_ipc` types the daemon uses, a frame this accepts is a frame the daemon
//!   accepts, and a malformed one fails here in exactly the same way.
//!
//! It deliberately does *not* validate values the way the device layer does — the
//! point is to observe what the UI sends, including anything out of range.
//!
//! One module per area, each the place its unit builds its fake: [`mouse`], [`headset`],
//! [`sonar`], [`keyboard`] (with [`keyboard_screen`], [`keyboard_bindings`] and
//! [`keyboard_onboard`]), [`gamesense`], [`profiles`], [`clips`], [`experimental`] and
//! [`actions`]. [`devices`] has the device list and how it looks; [`log`] what's printed.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gg_ipc::{
    BatteryState, ClipInfo, ClipsState, DeviceInfo, Event, Experimental, GameSenseState,
    HeadsetControls, KeyboardState, MouseSettings, Part, Parts, ProfilesState, Request,
    RequestFrame, ResponseData, ScreenSettings, ServerMessage, SonarState,
};

mod actions;
mod clips;
mod devices;
mod experimental;
mod gamesense;
mod headset;
mod keyboard;
mod keyboard_bindings;
mod keyboard_macros;
mod keyboard_onboard;
mod keyboard_screen;
mod log;
mod mouse;
mod profiles;
mod sonar;

use devices::Looks;

/// How often to push a `mouse_battery` notification.
const BATTERY_TICK: Duration = Duration::from_secs(5);

struct Fake {
    battery: Option<BatteryState>,
    applied: Option<MouseSettings>,
    /// Screen settings as last sent, echoed back unvalidated.
    screen: ScreenSettings,
    /// Headset settings as last sent, echoed back as the device would report them.
    controls: HeadsetControls,
    /// Sonar as last changed by the client.
    sonar: SonarState,
    /// The keyboard, with lighting as last set.
    keyboard: KeyboardState,
    /// GameSense as the client last set it.
    gamesense: GameSenseState,
    /// Profiles and rules as the client last set them.
    profiles: ProfilesState,
    /// The replay recorder, pretending gpu-screen-recorder is installed.
    clips: ClipsState,
    devices: Vec<DeviceInfo>,
    /// Method name that should return an error instead of succeeding.
    fail: Option<String>,
    /// Which parts run; set from the client, refused as the daemon refuses, but no
    /// restart: a change applies at once.
    parts: Parts,
    /// The experimental gates as the client last set them.
    experimental: Experimental,
    looks: Looks,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    let level: u8 = value("--battery")
        .map_or(Ok(87), |v| v.parse())
        .context("--battery")?;
    let charging = flag("--charging");
    let with_mouse = !flag("--no-mouse");
    let looks = Looks::from_args(&args)?;

    let path = gg_ipc::socket_path();
    // Never shadow a real daemon: if anything answers on this path, a client that
    // connects next would get whichever of us won the race.
    if UnixStream::connect(&path).is_ok() {
        bail!(
            "something is already listening on {} — stop gglinuxd first",
            path.display()
        );
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(&path);
    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;

    let state = Arc::new(Mutex::new(Fake {
        battery: (!flag("--asleep")).then_some(BatteryState { level, charging }),
        applied: None,
        screen: ScreenSettings::default(),
        controls: looks.controls(),
        sonar: sonar::fake_sonar(),
        keyboard: keyboard::fake_keyboard(),
        gamesense: GameSenseState::default(),
        profiles: ProfilesState::default(),
        clips: ClipsState {
            available: true,
            dir: "/home/you/Videos/gglinux".into(),
            clips: vec![ClipInfo {
                name: "Replay_2026-09-29_20-00-00.mp4".into(),
                bytes: 48_000_000,
                modified: 1_790_700_000,
            }],
            ..ClipsState::default()
        },
        devices: looks.listed(devices::devices(with_mouse)),
        fail: value("--fail"),
        parts: Parts::default(),
        experimental: Experimental::default(),
        looks,
    }));
    if let Some(variant) = value("--sonar") {
        sonar::sonar_variant(&mut state.lock().unwrap().sonar, &variant)?;
    }

    eprintln!("fakemouse listening on {}", path.display());
    eprintln!(
        "  battery {level}%{}",
        if charging { ", charging" } else { "" }
    );
    if let Some(f) = &state.lock().unwrap().fail {
        eprintln!("  {f} will return an error");
    }
    eprintln!("  waiting for a client (start the UI, or run ggctl)\n");

    for stream in listener.incoming() {
        let stream = stream?;
        let state = Arc::clone(&state);
        thread::spawn(move || {
            if let Err(e) = serve(stream, state) {
                eprintln!("client gone: {e:#}");
            }
        });
    }
    Ok(())
}

fn serve(stream: UnixStream, state: Arc<Mutex<Fake>>) -> Result<()> {
    eprintln!("-- client connected");
    // One lock around the write half: the battery ticker and the request handler
    // both write here, and a half-written line would desync the client's parser.
    let writer = Arc::new(Mutex::new(stream.try_clone()?));

    {
        let devices = state.lock().unwrap().devices.clone();
        send(
            &writer,
            ServerMessage::notify(Event::DevicesChanged { devices }),
        )?;
    }

    // Drift the battery so the push path is visible without touching anything.
    let ticker_state = Arc::clone(&state);
    let ticker_writer = Arc::clone(&writer);
    thread::spawn(move || loop {
        thread::sleep(BATTERY_TICK);
        let battery = {
            let mut s = ticker_state.lock().unwrap();
            match &mut s.battery {
                Some(b) if b.charging => b.level = (b.level + 1).min(100),
                Some(b) => b.level = b.level.saturating_sub(1).max(1),
                None => {}
            }
            s.battery
        };
        let msg = ServerMessage::notify(Event::MouseBattery { battery });
        if send(&ticker_writer, msg).is_err() {
            return; // client hung up
        }
    });

    for line in BufReader::new(stream).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let frame: RequestFrame = match serde_json::from_str(&line) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("<< UNPARSEABLE {line}\n   {e}");
                // On the request's own id when it has one, as the daemon answers.
                let id = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|v| v.get("id")?.as_u64())
                    .unwrap_or(0);
                send(
                    &writer,
                    ServerMessage::response(id, error(format!("invalid request: {e}"))),
                )?;
                continue;
            }
        };
        eprintln!("<< {}", log::describe(&frame.req));
        // The daemon publishes Sonar's state whenever it changes.
        let sonar_change = frame.req.part() == Some(gg_ipc::Part::Sonar)
            && !matches!(frame.req, Request::GetSonarState);
        let result = handle(frame.req, &state);
        if let ResponseData::Error { message } = &result {
            eprintln!(">>   error: {message}");
        }
        let changed = sonar_change && matches!(result, ResponseData::Success);
        send(&writer, ServerMessage::response(frame.id, result))?;
        if changed {
            let state = state.lock().unwrap().sonar.clone();
            send(
                &writer,
                ServerMessage::notify(Event::SonarChanged { state }),
            )?;
        }
    }
    eprintln!("-- client disconnected");
    Ok(())
}

fn handle(req: Request, state: &Arc<Mutex<Fake>>) -> ResponseData {
    let mut s = state.lock().unwrap();

    // `fail` matches on the wire name, so it can target a method by the same
    // string the UI sends.
    if s.fail.as_deref() == Some(log::method_name(&req)) {
        return error(format!("simulated failure for {}", log::method_name(&req)));
    }
    if let Some(message) = req.part().and_then(|p| s.parts.refusal(p)) {
        return error(message);
    }

    // Each part's requests go to its module; the rest are the daemon's own.
    match req.part() {
        Some(Part::Mouse) => mouse::handle(req, &mut s),
        Some(Part::Headset) => headset::handle(req, &mut s),
        Some(Part::Sonar) => sonar::handle(req, &mut s),
        Some(Part::Keyboard) => keyboard::handle(req, &mut s),
        Some(Part::Gamesense) => gamesense::handle(req, &mut s),
        Some(Part::Profiles) => profiles::handle(req, &mut s),
        Some(Part::Clips) => clips::handle(req, &mut s),
        None => match req {
            Request::ListDevices => ResponseData::Devices {
                devices: s.devices.clone(),
            },
            Request::Ping => ResponseData::Pong,
            Request::GetParts => ResponseData::Parts { parts: s.parts },
            Request::SetParts { parts } => {
                s.parts = parts;
                ResponseData::Success
            }
            req @ (Request::GetExperimental | Request::SetExperimental { .. }) => {
                experimental::handle(req, &mut s)
            }
            req @ (Request::GetActions | Request::SetActions { .. }) => {
                actions::handle(req, &mut s)
            }
            other => error(format!(
                "{} belongs to no part, yet isn't the daemon's own",
                log::method_name(&other)
            )),
        },
    }
}

fn error(message: String) -> ResponseData {
    ResponseData::Error { message }
}

fn send(writer: &Arc<Mutex<UnixStream>>, msg: ServerMessage) -> Result<()> {
    let line = msg.to_line()?;
    let mut w = writer.lock().unwrap();
    w.write_all(line.as_bytes())?;
    w.flush()?;
    Ok(())
}
