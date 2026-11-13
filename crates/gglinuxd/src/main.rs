// SPDX-License-Identifier: GPL-3.0-or-later
//! gglinuxd — the gglinux background daemon.
//!
//! Current scope: enumerate SteelSeries devices, monitor hotplug, control the
//! Rival 3 Wireless Gen 2, drive the Arctis Nova Pro Wireless headset (settings,
//! and keeping its base-station screen from burning in), run the Apex Pro TKL
//! Gen 3's lighting, screen and switches, run Sonar-style audio channels on
//! PipeWire, the GameSense server, profiles and clips, and serve all of it over a
//! newline-delimited JSON unix socket. Each of those parts can be turned off
//! (`parts.json`); a part that's off isn't started.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::stream::{FuturesUnordered, StreamExt};
use gg_ipc::{
    DeviceInfo, Event, Feature, Part, Parts, Request, RequestFrame, ResponseData, ServerMessage,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{broadcast, watch, Notify};
use tracing::{debug, error, info, warn};

mod actions;
mod clips;
mod clock;
mod experimental;
mod gamesense;
mod headset;
mod keyboard;
mod mouse;
mod parts;
mod presence;
mod profiles;
mod sonar;

use clips::{Command as ClipsCommand, Handle as ClipsHandle};
use gamesense::{Command as GamesenseCommand, Handle as GamesenseHandle};
use headset::{Command as HeadsetCommand, Handle as HeadsetHandle};
use keyboard::{Command as KeyboardCommand, Handle as KeyboardHandle};
use mouse::{Command as MouseCommand, Handle as MouseHandle};
use profiles::{Command as ProfilesCommand, Handle as ProfilesHandle};
use sonar::{Command as SonarCommand, Handle as SonarHandle};

/// Capacity of the per-client event broadcast buffer.
const EVENT_CHANNEL_CAP: usize = 256;

/// Longest the daemon waits, on exit, for the devices and the clip recorder
/// (which may take its own 3 s to stop cleanly).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(4);

/// Handles to the device workers, passed to every client task.
#[derive(Clone)]
struct Workers {
    mouse: MouseHandle,
    headset: HeadsetHandle,
    keyboard: KeyboardHandle,
    sonar: SonarHandle,
    gamesense: GamesenseHandle,
    profiles: ProfilesHandle,
    clips: ClipsHandle,
    /// The experimental gates: features not tried on the owner's hardware yet.
    experimental: Arc<experimental::Gates>,
    /// The actions on the spare keys F13–F24.
    actions: Arc<actions::Service>,
    /// The parts as saved when this daemon started; the handles of those not running
    /// lead nowhere.
    parts: Parts,
    /// Told when a change of parts wants the daemon started again.
    restart: Arc<Notify>,
}

/// Why the daemon stops.
#[derive(Debug, PartialEq, Eq)]
enum Stop {
    /// SIGINT or SIGTERM.
    Signal,
    /// The parts changed: start again in place.
    Restart,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let path = gg_ipc::socket_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating socket dir {}", dir.display()))?;
    }
    let listener = claim_socket(&path)?;
    info!(socket = %path.display(), "gglinuxd listening");

    // Shared device state: `snapshot` holds the current list; `events` fans out diffs.
    let (snapshot_tx, snapshot_rx) = watch::channel::<Vec<DeviceInfo>>(Vec::new());
    let (event_tx, _) = broadcast::channel::<Event>(EVENT_CHANNEL_CAP);

    spawn_hid_monitor(snapshot_tx, event_tx.clone());
    let parts = parts::load(&parts::path());
    let run = parts.running();
    for part in Part::ALL.into_iter().filter(|&p| !run.on(p)) {
        info!(part = part.label(), "turned off; not started");
    }
    // The experimental gates, before the keyboard worker: it starts with them.
    let experimental_path = experimental::path();
    let gates_now = experimental::load(&experimental_path);
    // GameSense's registry exists before the keyboard worker, which draws it.
    let engine = run.gamesense.then(gamesense::Engine::load);
    let keyboard = if run.keyboard {
        let overlays = engine.iter().map(|e| e.overlay()).collect();
        let keyboard = keyboard::spawn(event_tx.clone(), overlays, gates_now);
        presence::spawn(keyboard.clone());
        keyboard
    } else {
        KeyboardHandle::off()
    };
    let gates = Arc::new(experimental::Gates::new(
        experimental_path,
        gates_now,
        event_tx.clone(),
        keyboard.clone(),
    ));
    // The game from the profiles' rules that's running now; games-only clips follow it.
    let (game_tx, game_rx) = watch::channel::<Option<String>>(None);
    // Where everything playing mixes, from Sonar; clips' desktop audio follows it.
    let (mixed_tx, mixed_rx) = watch::channel::<Option<String>>(None);
    // Kept here while the part that sends on them is off, so clips just hear nothing.
    let (mut game_tx, mut mixed_tx) = (Some(game_tx), Some(mixed_tx));
    let profiles = match (run.profiles, game_tx.take()) {
        (true, Some(tx)) => profiles::spawn(event_tx.clone(), keyboard.clone(), tx),
        (_, tx) => {
            game_tx = tx;
            ProfilesHandle::off()
        }
    };
    let clips = if run.clips {
        clips::spawn(event_tx.clone(), game_rx, mixed_rx)
    } else {
        ClipsHandle::off()
    };
    // Actions run whichever parts are on; a profile or clip action says so while its
    // part is off.
    let actions = actions::spawn(event_tx.clone(), profiles.clone(), clips.clone());
    let workers = Workers {
        mouse: if run.mouse {
            mouse::spawn(event_tx.clone(), gates.subscribe())
        } else {
            MouseHandle::off()
        },
        headset: if run.headset {
            headset::spawn(event_tx.clone())
        } else {
            HeadsetHandle::off()
        },
        gamesense: match engine {
            Some(engine) => gamesense::spawn(engine, event_tx.clone(), keyboard.clone()),
            None => {
                tokio::task::spawn_blocking(gamesense::clear_files);
                GamesenseHandle::off()
            }
        },
        profiles,
        clips,
        // Sonar tells the keyboard's visualizer and clips where everything playing mixes.
        sonar: match (run.sonar, mixed_tx.take()) {
            (true, Some(tx)) => sonar::spawn(event_tx.clone(), keyboard.clone(), tx),
            (_, tx) => {
                mixed_tx = tx;
                std::thread::spawn(sonar::turn_off);
                SonarHandle::off()
            }
        },
        keyboard,
        experimental: gates,
        actions,
        parts,
        restart: Arc::new(Notify::new()),
    };
    // Profiles apply their sections through `dispatch`: they get the workers at start, so
    // automatic switching works before the first profiles request.
    profiles::connect(&workers);

    let result = accept_loop(&listener, snapshot_rx, event_tx, workers.clone()).await;

    // Leave the base-station screen dark — at logout or shutdown its USB power
    // often stays on, and nobody is there to see it — and give the keyboard its
    // own lighting back.
    shutdown_devices(&workers).await;

    // Best-effort cleanup so the next start binds cleanly.
    let _ = std::fs::remove_file(&path);
    drop((game_tx, mixed_tx));
    match result? {
        Stop::Signal => Ok(()),
        Stop::Restart => Err(exec_self()),
    }
}

/// Start this program again in place, with the same arguments: the same process, so
/// systemd keeps tracking it. Returns only if that failed.
fn exec_self() -> anyhow::Error {
    use std::os::unix::process::CommandExt;
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return anyhow::Error::new(e).context("finding gglinuxd to restart it"),
    };
    // Reinstalled while running, the old file is gone: start the new one.
    let exe = exe
        .to_str()
        .and_then(|s| s.strip_suffix(" (deleted)"))
        .map_or(exe.clone(), PathBuf::from);
    info!(exe = %exe.display(), "starting again with the new parts");
    let error = std::process::Command::new(&exe)
        .args(std::env::args_os().skip(1))
        .exec();
    anyhow::Error::new(error).context(format!("restarting {}", exe.display()))
}

/// Bind the daemon's socket at `path`, unless another gglinuxd answers there:
/// two would both drive the devices, and the first to exit would take the
/// other's socket with it.
fn claim_socket(path: &Path) -> Result<UnixListener> {
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => bail!("gglinuxd is already running on {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // Nobody listens: a stale socket from an unclean shutdown.
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            let _ = std::fs::remove_file(path);
        }
        Err(e) => {
            return Err(e)
                .with_context(|| format!("checking {} for a running gglinuxd", path.display()))
        }
    }
    UnixListener::bind(path).with_context(|| format!("binding unix socket {}", path.display()))
}

/// Bridge the blocking gg-hid monitor thread into the async world via watch/broadcast.
fn spawn_hid_monitor(
    snapshot_tx: watch::Sender<Vec<DeviceInfo>>,
    event_tx: broadcast::Sender<Event>,
) {
    std::thread::Builder::new()
        .name("gg-hid".into())
        .spawn(move || {
            let res = gg_hid::run_monitor(|snapshot, events| {
                publish(&snapshot_tx, &event_tx, snapshot, events)
            });
            if let Err(e) = res {
                error!(error = %e, "HID monitor stopped");
            }
        })
        .expect("spawn gg-hid thread");
}

/// Pass on what the HID monitor saw: the device list, for clients that connect
/// from now on, and what changed, for those already connected.
fn publish(
    snapshot_tx: &watch::Sender<Vec<DeviceInfo>>,
    event_tx: &broadcast::Sender<Event>,
    snapshot: Vec<DeviceInfo>,
    events: Vec<Event>,
) {
    // A change with no add or remove to tell it by goes out as the whole list:
    // above all the first scan, which a client that connected before it was
    // sent as empty.
    let whole = events.is_empty().then(|| Event::DevicesChanged {
        devices: snapshot.clone(),
    });
    // `send` here is synchronous — safe to call from this OS thread. The list
    // is updated first: a client subscribes before it reads the list, so one
    // connecting meanwhile gets the new list or the event, never neither.
    let _ = snapshot_tx.send(snapshot);
    for ev in events.into_iter().chain(whole) {
        let _ = event_tx.send(ev);
    }
}

/// Accept connections until SIGINT/SIGTERM, or a change of parts.
async fn accept_loop(
    listener: &UnixListener,
    snapshot_rx: watch::Receiver<Vec<DeviceInfo>>,
    event_tx: broadcast::Sender<Event>,
    workers: Workers,
) -> Result<Stop> {
    let mut sigterm = signal(SignalKind::terminate()).context("installing SIGTERM handler")?;
    let restart = Arc::clone(&workers.restart);

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted.context("accept")?;
                let snapshot_rx = snapshot_rx.clone();
                let event_tx = event_tx.clone();
                let workers = workers.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, snapshot_rx, event_tx, workers).await {
                        debug!(error = %e, "client connection ended");
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => { info!("received SIGINT; shutting down"); return Ok(Stop::Signal); }
            _ = sigterm.recv() => { info!("received SIGTERM; shutting down"); return Ok(Stop::Signal); }
            _ = restart.notified() => { info!("the parts changed; restarting"); return Ok(Stop::Restart); }
        }
    }
}

/// Serve one client: push the current snapshot, then stream events and answer requests.
async fn handle_client(
    stream: UnixStream,
    snapshot_rx: watch::Receiver<Vec<DeviceInfo>>,
    event_tx: broadcast::Sender<Event>,
    workers: Workers,
) -> Result<()> {
    let (snapshot, workers) = (&snapshot_rx, &workers);
    serve(stream, snapshot, &event_tx, move |line| async move {
        dispatch(&line, snapshot, workers).await
    })
    .await
}

/// [`handle_client`]'s connection, with each request line answered by `answer`.
///
/// Replies go out as they are ready, not in the order asked: the UI sends every
/// panel's requests down one connection, each on its own timer, and a slow one
/// (Sonar waiting out a PipeWire restart, the recorder stopping) must not run
/// out the others'. Each request still starts, and so reaches its worker, in
/// the order it was sent: `FuturesUnordered` first polls what it holds in the
/// order it was pushed.
async fn serve<Fut: Future<Output = ServerMessage>>(
    stream: UnixStream,
    snapshot_rx: &watch::Receiver<Vec<DeviceInfo>>,
    event_tx: &broadcast::Sender<Event>,
    answer: impl Fn(String) -> Fut,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    let mut events = event_tx.subscribe();
    let mut pending = FuturesUnordered::new();

    // Send the current state immediately so a fresh UI populates without asking.
    let initial = snapshot_rx.borrow().clone();
    write_message(
        &mut write_half,
        &ServerMessage::notify(Event::DevicesChanged { devices: initial }),
    )
    .await?;

    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line? {
                    Some(line) if !line.trim().is_empty() => pending.push(answer(line)),
                    Some(_) => {} // blank keepalive line
                    None => break, // client closed
                }
            }
            Some(reply) = pending.next() => write_message(&mut write_half, &reply).await?,
            ev = events.recv() => {
                match ev {
                    Ok(event) => write_message(&mut write_half, &ServerMessage::notify(event)).await?,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!(dropped = n, "client lagged; resyncing");
                        let devices = snapshot_rx.borrow().clone();
                        write_message(
                            &mut write_half,
                            &ServerMessage::notify(Event::DevicesChanged { devices }),
                        )
                        .await?;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    // A request sent just before the client hung up is still answered.
    while let Some(reply) = pending.next().await {
        write_message(&mut write_half, &reply).await?;
    }
    Ok(())
}

/// Parse and answer a single request line.
///
/// Device commands are answered by the worker threads, so this awaits; a
/// client issuing a slow command does not block other clients.
async fn dispatch(
    line: &str,
    snapshot_rx: &watch::Receiver<Vec<DeviceInfo>>,
    workers: &Workers,
) -> ServerMessage {
    let frame = match parse_request(line) {
        Ok(frame) => frame,
        Err((id, e)) => {
            return ServerMessage::response(
                id,
                ResponseData::Error {
                    message: format!("invalid request: {e}"),
                },
            )
        }
    };

    if let Some(message) = frame.req.part().and_then(|p| workers.parts.refusal(p)) {
        return ServerMessage::response(frame.id, ResponseData::Error { message });
    }

    let result = match frame.req {
        Request::GetParts => ResponseData::Parts {
            parts: workers.parts,
        },
        Request::SetParts { parts } => set_parts(&parts::path(), parts, workers),
        Request::ListDevices => {
            // Bound the borrow to this block: the watch guard is not Send and this
            // future gets spawned.
            let devices = snapshot_rx.borrow().clone();
            ResponseData::Devices { devices }
        }
        Request::Ping => ResponseData::Pong,

        Request::GetMouseState => match workers.mouse.send(MouseCommand::GetState).await {
            Ok(state) => ResponseData::Mouse { state },
            Err(message) => ResponseData::Error { message },
        },
        Request::SetMouseDpi {
            stages,
            active,
            save,
        } => outcome(
            workers
                .mouse
                .send::<()>(|reply| MouseCommand::SetDpi {
                    stages,
                    active,
                    save,
                    reply,
                })
                .await,
        ),
        Request::SetMousePolling { hz, save } => outcome(
            workers
                .mouse
                .send::<()>(|reply| MouseCommand::SetPolling { hz, save, reply })
                .await,
        ),
        Request::SetMouseButtons { actions, save } => outcome(
            workers
                .mouse
                .send::<()>(|reply| MouseCommand::SetButtons {
                    actions,
                    save,
                    reply,
                })
                .await,
        ),
        Request::SaveMouse => outcome(workers.mouse.send(MouseCommand::Save).await),
        Request::SetMouseRadio { radio, save } => {
            match workers.experimental.current().refusal(Feature::MousePower) {
                Some(message) => ResponseData::Error { message },
                None => outcome(
                    workers
                        .mouse
                        .send::<()>(|reply| MouseCommand::SetRadio { radio, save, reply })
                        .await,
                ),
            }
        }
        // Refused here too, so the gate holds before the worker is even asked.
        Request::SetMouseSleep { minutes } => {
            match workers.experimental.current().refusal(Feature::MousePower) {
                Some(message) => ResponseData::Error { message },
                None => outcome(
                    workers
                        .mouse
                        .send::<()>(|reply| MouseCommand::SetSleep { minutes, reply })
                        .await,
                ),
            }
        }
        Request::GetHeadsetState => match workers.headset.send(HeadsetCommand::GetState).await {
            Ok(state) => ResponseData::Headset { state },
            Err(message) => ResponseData::Error { message },
        },
        Request::SetHeadsetScreen { settings } => outcome(
            workers
                .headset
                .send::<()>(|reply| HeadsetCommand::SetScreen { settings, reply })
                .await,
        ),
        Request::SetHeadset { change } => outcome(
            workers
                .headset
                .send::<()>(|reply| HeadsetCommand::SetControl { change, reply })
                .await,
        ),
        Request::GetSonarState => match workers.sonar.send(SonarCommand::GetState).await {
            Ok(state) => ResponseData::Sonar { state },
            Err(message) => ResponseData::Error { message },
        },
        Request::SetSonarEnabled { enabled } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetEnabled { enabled, reply })
                .await,
        ),
        Request::SetSonarOutput { name } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetOutput { name, reply })
                .await,
        ),
        Request::SetChannelVolume { channel, volume } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetVolume {
                    channel,
                    volume,
                    reply,
                })
                .await,
        ),
        Request::SetChatMix { balance } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetChatMix { balance, reply })
                .await,
        ),
        Request::SetAppChannel { binary, channel } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetAppChannel {
                    binary,
                    channel,
                    reply,
                })
                .await,
        ),
        Request::SetChannelEq { channel, bands } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetChannelEq {
                    channel,
                    bands,
                    reply,
                })
                .await,
        ),
        Request::SetHeadphoneEq { eq } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetHeadphoneEq { eq, reply })
                .await,
        ),
        Request::ImportAutoEq { text, name } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::ImportAutoEq { text, name, reply })
                .await,
        ),
        Request::SetMicInput { name } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetMicInput { name, reply })
                .await,
        ),
        Request::SetMicSettings { settings } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetMicSettings { settings, reply })
                .await,
        ),
        Request::SetMicVolume { volume } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetMicVolume { volume, reply })
                .await,
        ),
        Request::SetStreamer { enabled } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetStreamer { enabled, reply })
                .await,
        ),
        Request::SetStreamVolume { channel, volume } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetStreamVolume {
                    channel,
                    volume,
                    reply,
                })
                .await,
        ),
        Request::SetStreamMaster { volume } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetStreamMaster { volume, reply })
                .await,
        ),
        Request::SetSmartVolume { channel, settings } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetSmartVolume {
                    channel,
                    settings,
                    reply,
                })
                .await,
        ),
        Request::GetKeyboardState => match workers.keyboard.send(KeyboardCommand::GetState).await {
            Ok(state) => ResponseData::Keyboard { state },
            Err(message) => ResponseData::Error { message },
        },
        Request::SetLighting { lighting } => outcome(
            workers
                .keyboard
                .send::<()>(|reply| KeyboardCommand::SetLighting { lighting, reply })
                .await,
        ),
        Request::SetSwitches { switches } => {
            // Protection Mode is refused while its gate is off, before anything is saved.
            let gates = workers.experimental.current();
            match gates
                .refusal(Feature::KeyboardProtection)
                .filter(|_| switches.protection.enabled)
            {
                Some(message) => ResponseData::Error { message },
                None => outcome(
                    workers
                        .keyboard
                        .send::<()>(|reply| KeyboardCommand::SetSwitches { switches, reply })
                        .await,
                ),
            }
        }
        Request::SetScreen { screen } => outcome(
            workers
                .keyboard
                .send::<()>(|reply| KeyboardCommand::SetScreen { screen, reply })
                .await,
        ),
        Request::SetScreenCare { care } => outcome(
            workers
                .keyboard
                .send::<()>(|reply| KeyboardCommand::SetScreenCare { care, reply })
                .await,
        ),
        Request::GetGamesenseState => {
            match workers.gamesense.send(GamesenseCommand::GetState).await {
                Ok(state) => ResponseData::Gamesense { state },
                Err(message) => ResponseData::Error { message },
            }
        }
        Request::SetGamesenseEnabled { enabled } => outcome(
            workers
                .gamesense
                .send::<()>(|reply| GamesenseCommand::SetEnabled { enabled, reply })
                .await,
        ),
        Request::SetGameEnabled { game, enabled } => outcome(
            workers
                .gamesense
                .send::<()>(|reply| GamesenseCommand::SetGameEnabled {
                    game,
                    enabled,
                    reply,
                })
                .await,
        ),
        Request::RemoveGame { game } => outcome(
            workers
                .gamesense
                .send::<()>(|reply| GamesenseCommand::RemoveGame { game, reply })
                .await,
        ),
        Request::GamesenseTest => outcome(workers.gamesense.send(GamesenseCommand::RunTest).await),
        Request::GamesenseTestScreen => outcome(
            workers
                .gamesense
                .send(GamesenseCommand::RunTestScreen)
                .await,
        ),
        // Profiles apply their sections through `dispatch` too: each request hands them the
        // workers (only the first one sends them).
        Request::GetProfiles => {
            profiles::connect(workers);
            match workers.profiles.send(ProfilesCommand::GetState).await {
                Ok(state) => ResponseData::Profiles { state },
                Err(message) => ResponseData::Error { message },
            }
        }
        req @ (Request::SaveProfile { .. }
        | Request::ApplyProfile { .. }
        | Request::DeleteProfile { .. }
        | Request::SetGameRules { .. }
        | Request::GetProfile { .. }
        | Request::ImportProfile { .. }
        | Request::CopyProfile { .. }) => profiles::serve(req, workers).await,
        Request::GetClipsState => match workers.clips.send(ClipsCommand::GetState).await {
            Ok(state) => ResponseData::Clips { state },
            Err(message) => ResponseData::Error { message },
        },
        Request::SetClipsSettings { settings } => outcome(
            workers
                .clips
                .send::<()>(|reply| ClipsCommand::SetSettings { settings, reply })
                .await,
        ),
        Request::SaveClip => outcome(workers.clips.send(ClipsCommand::Save).await),
        Request::DeleteClip { name } => outcome(
            workers
                .clips
                .send::<()>(|reply| ClipsCommand::Delete { name, reply })
                .await,
        ),
        req @ (Request::GetExperimental | Request::SetExperimental { .. }) => {
            experimental::handle(req, &workers.experimental)
        }
        // The keyboard's own modules answer these; while the keyboard part is off they
        // were refused above.
        req @ (Request::GetBindings | Request::ReadBindings | Request::SetBindings { .. }) => {
            keyboard::bindings::handle(req, &workers.keyboard).await
        }
        req @ (Request::GetOnboard
        | Request::ReadOnboard { .. }
        | Request::LoadOnboard { .. }
        | Request::WriteOnboard { .. }) => keyboard::onboard::serve(req, &workers.keyboard).await,
        req @ (Request::GetMacros | Request::SetMacros { .. } | Request::WriteMacros { .. }) => {
            keyboard::macros::handle(req, &workers.keyboard).await
        }
        req @ (Request::GetActions | Request::SetActions { .. }) => {
            actions::serve(req, &workers.actions).await
        }
    };
    ServerMessage::response(frame.id, result)
}

/// Save which parts run; if that changes anything, the daemon starts again a moment
/// after answering, with the new set.
fn set_parts(path: &Path, parts: Parts, workers: &Workers) -> ResponseData {
    if let Err(message) = parts::save(path, &parts) {
        return ResponseData::Error { message };
    }
    if parts != workers.parts {
        let restart = Arc::clone(&workers.restart);
        tokio::spawn(async move {
            // Time for the answer to go out first.
            tokio::time::sleep(Duration::from_millis(200)).await;
            restart.notify_one();
        });
    }
    ResponseData::Success
}

/// A request line as a frame, or why not and the id to answer that on.
fn parse_request(line: &str) -> Result<RequestFrame, (u64, serde_json::Error)> {
    serde_json::from_str::<RequestFrame>(line).map_err(|e| {
        // The request's own id when it has one, so the client sees why instead
        // of waiting out its timer (a method this daemon is too old for, say);
        // else id 0, for the client to log.
        let id = serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|v| v.get("id")?.as_u64())
            .unwrap_or(0);
        (id, e)
    })
}

/// Collapse a command result into `Success` or `Error`.
fn outcome(result: Result<(), String>) -> ResponseData {
    match result {
        Ok(()) => ResponseData::Success,
        Err(message) => ResponseData::Error { message },
    }
}

/// Wait, at most `longest`, for the keyboard slot writes asked for to end: stopping in the
/// middle of one can leave the slot erased or half written. Says so in the log. Whether
/// they ended.
async fn wait_for_slot_write(keyboard: &KeyboardHandle, longest: Duration) -> bool {
    let writes = keyboard.slot_writes();
    if !writes.running() {
        return true;
    }
    info!(
        "waiting for the keyboard slot write to finish (at most {} s)",
        longest.as_secs()
    );
    let deadline = tokio::time::Instant::now() + longest;
    while writes.running() {
        if tokio::time::Instant::now() >= deadline {
            warn!(
                "the keyboard slot write didn't finish in {} s; stopping anyway",
                longest.as_secs()
            );
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    info!("the keyboard slot write finished");
    true
}

/// Leave the devices as their owner expects them — base-station screen dark,
/// keyboard lighting back on its own profile — waiting at most [`SHUTDOWN_GRACE`], after
/// any keyboard slot write has ended (at most [`keyboard::onboard::longest_write`]). A
/// restart for a change of parts comes through here too.
async fn shutdown_devices(workers: &Workers) {
    let run = workers.parts.running();
    if run.keyboard {
        wait_for_slot_write(&workers.keyboard, keyboard::onboard::longest_write()).await;
    }
    let all = async {
        tokio::join!(
            async {
                match run.headset {
                    true => workers.headset.send::<()>(HeadsetCommand::Shutdown).await,
                    false => Ok(()),
                }
            },
            async {
                match run.keyboard {
                    true => workers.keyboard.send::<()>(KeyboardCommand::Shutdown).await,
                    false => Ok(()),
                }
            },
            async {
                match run.clips {
                    true => workers.clips.send::<()>(ClipsCommand::Shutdown).await,
                    false => Ok(()),
                }
            },
        )
    };
    match tokio::time::timeout(SHUTDOWN_GRACE, all).await {
        Ok((headset, keyboard, clips)) => {
            if let Err(e) = headset {
                warn!(error = %e, "headset shutdown failed");
            }
            if let Err(e) = keyboard {
                warn!(error = %e, "keyboard shutdown failed");
            }
            if let Err(e) = clips {
                warn!(error = %e, "clip recorder shutdown failed");
            }
            debug!("devices left as found");
        }
        Err(_) => warn!("device workers did not confirm shutdown in time"),
    }
}

async fn write_message(
    write_half: &mut (impl AsyncWriteExt + Unpin),
    msg: &ServerMessage,
) -> Result<()> {
    let line = msg.to_line()?;
    write_half.write_all(line.as_bytes()).await?;
    Ok(())
}

/// Private folders for tests.
#[cfg(test)]
mod scratch {
    use std::path::PathBuf;

    /// A new private folder for one test, removed when the test ends.
    pub struct Scratch(pub PathBuf);

    impl Scratch {
        pub fn new(tag: &str) -> Self {
            use std::hash::BuildHasher;
            use std::os::unix::fs::DirBuilderExt;
            let random = std::collections::hash_map::RandomState::new().hash_one(tag);
            let d = std::env::temp_dir().join(format!(
                "gglinuxd-{}-{tag}-{random:016x}",
                std::process::id()
            ));
            std::fs::DirBuilder::new().mode(0o700).create(&d).unwrap();
            Scratch(d)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use gg_device_keyboard::testing::FakeTransport;
    use gg_ipc::Experimental;

    #[tokio::test]
    async fn a_second_daemon_leaves_the_running_ones_socket_alone() {
        let d = Scratch::new("second");
        let path = d.0.join("gglinuxd.sock");
        let running = claim_socket(&path).unwrap();

        let second = claim_socket(&path);
        assert!(
            second.is_err(),
            "a second daemon must not start while one listens"
        );
        assert!(format!("{:#}", second.err().unwrap()).contains("already running"));

        // Clients still reach the first one.
        let client = UnixStream::connect(&path)
            .await
            .expect("the running daemon's socket is still there");
        tokio::time::timeout(Duration::from_secs(1), running.accept())
            .await
            .expect("the running daemon hears the client")
            .unwrap();
        drop(client);
    }

    #[tokio::test]
    async fn a_stale_socket_from_an_unclean_shutdown_is_cleared() {
        let d = Scratch::new("stale");
        let path = d.0.join("gglinuxd.sock");
        // Bound and then gone without removing its file, as after a crash.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists());

        let listener = claim_socket(&path).expect("nobody listens there, so it can go");
        let _client = UnixStream::connect(&path).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
    }

    /// Run [`serve`] with `answer` while `client` talks to it over a socket
    /// pair. The connection has no devices and no events.
    async fn talk<Fut, C>(answer: impl Fn(String) -> Fut, client: impl FnOnce(UnixStream) -> C)
    where
        Fut: Future<Output = ServerMessage>,
        C: Future<Output = ()>,
    {
        let (daemon_end, client_end) = UnixStream::pair().unwrap();
        let (_snapshot_tx, snapshot_rx) = watch::channel(Vec::new());
        let (event_tx, _) = broadcast::channel(8);
        let both = async {
            tokio::join!(
                serve(daemon_end, &snapshot_rx, &event_tx, answer),
                client(client_end)
            )
        };
        let (served, ()) = tokio::time::timeout(Duration::from_secs(5), both)
            .await
            .expect("the connection finishes");
        served.unwrap();
    }

    /// The id of the next response, past any notifications.
    async fn next_response(
        lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    ) -> u64 {
        loop {
            let line = lines
                .next_line()
                .await
                .unwrap()
                .expect("the daemon hung up");
            if let ServerMessage::Response { id, .. } = serde_json::from_str(&line).unwrap() {
                return id;
            }
        }
    }

    #[tokio::test]
    async fn a_slow_request_holds_up_no_other_on_the_same_connection() {
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        // "slow" waits until the client has had its other answer, as a Sonar
        // restart keeps its reply; anything else is answered at once.
        let answer = |line: String| {
            let release = std::sync::Arc::clone(&release);
            async move {
                let id = if line == "slow" {
                    release.notified().await;
                    1
                } else {
                    2
                };
                ServerMessage::response(id, ResponseData::Pong)
            }
        };
        let notify = std::sync::Arc::clone(&release);
        talk(answer, |stream| async move {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            write.write_all(b"slow\nfast\n").await.unwrap();
            let first =
                tokio::time::timeout(Duration::from_secs(1), next_response(&mut lines)).await;
            assert_eq!(
                first.ok(),
                Some(2),
                "the fast request is answered while the slow one waits"
            );
            notify.notify_one();
            assert_eq!(next_response(&mut lines).await, 1);
        })
        .await;
    }

    /// A request is handed to its worker on its first poll, so that is the
    /// order a worker sees them in: a slider's last value must arrive last.
    #[tokio::test(flavor = "multi_thread")]
    async fn requests_start_in_the_order_they_were_sent() {
        let started = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let answer = |line: String| {
            let started = std::sync::Arc::clone(&started);
            async move {
                let n: u64 = line.parse().unwrap();
                started.lock().unwrap().push(n);
                // Some take a while, as a worker's reply can.
                if n.is_multiple_of(3) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                ServerMessage::response(n, ResponseData::Pong)
            }
        };
        talk(answer, |stream| async move {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let sent: String = (1..=50).map(|n| format!("{n}\n")).collect();
            write.write_all(sent.as_bytes()).await.unwrap();
            for _ in 1..=50 {
                next_response(&mut lines).await;
            }
        })
        .await;
        assert_eq!(*started.lock().unwrap(), (1..=50).collect::<Vec<u64>>());
    }

    fn mouse() -> DeviceInfo {
        DeviceInfo {
            id: "1038:1872".into(),
            kind: gg_ipc::DeviceKind::Mouse,
            vendor_id: 0x1038,
            product_id: 0x1872,
            name: "Rival 3 Wireless Gen 2".into(),
            serial: None,
            connected: true,
            interfaces: Vec::new(),
        }
    }

    #[test]
    fn a_client_connected_before_the_first_scan_hears_the_devices() {
        let (snapshot_tx, _snapshot_rx) = watch::channel(Vec::new());
        let (event_tx, _) = broadcast::channel(8);
        // It connected while the list was still empty, so it was sent [] and
        // is waiting on events.
        let mut client = event_tx.subscribe();
        // The first scan comes with no events, only the list.
        publish(&snapshot_tx, &event_tx, vec![mouse()], Vec::new());
        match client.try_recv() {
            Ok(Event::DevicesChanged { devices }) => assert_eq!(devices, vec![mouse()]),
            other => panic!("the client heard {other:?}"),
        }

        // An unplug after that goes out as itself, not the whole list again.
        publish(
            &snapshot_tx,
            &event_tx,
            Vec::new(),
            vec![Event::DeviceRemoved { id: mouse().id }],
        );
        assert!(matches!(client.try_recv(), Ok(Event::DeviceRemoved { .. })));
        assert!(client.try_recv().is_err(), "nothing more");
    }

    #[test]
    fn a_request_the_daemon_cannot_read_is_refused_on_its_own_id() {
        // A method from a newer UI than this daemon, which the UI is waiting on.
        match parse_request(r#"{"id":7,"req":{"method":"set_something_new","params":{}}}"#) {
            Err((id, e)) => {
                assert_eq!(id, 7, "on the request's own id, so the client sees why");
                assert!(e.to_string().contains("set_something_new"), "{e}");
            }
            Ok(frame) => panic!("parsed as {frame:?}"),
        }
        // With no id to be found there is nothing else to answer on.
        for line in [
            "not json",
            r#"{"req":{"method":"ping"}}"#,
            r#"{"id":"7","req":{"method":"nope"}}"#,
        ] {
            match parse_request(line) {
                Err((0, _)) => {}
                other => panic!("{line} answered as {other:?}"),
            }
        }
    }

    fn workers_with(parts: Parts) -> Workers {
        let (event_tx, _) = broadcast::channel(8);
        Workers {
            // Never saved by the tests that use these workers; fakes, so nothing runs.
            actions: Arc::new(actions::testing::Fakes::default().service(
                std::env::temp_dir().join("gglinuxd-tests-never-written/actions.json"),
                Err("no typing in tests".into()),
                event_tx.clone(),
            )),
            mouse: MouseHandle::off(),
            headset: HeadsetHandle::off(),
            keyboard: KeyboardHandle::off(),
            sonar: SonarHandle::off(),
            gamesense: GamesenseHandle::off(),
            profiles: ProfilesHandle::off(),
            clips: ClipsHandle::off(),
            // Never saved by the tests that use these workers.
            experimental: Arc::new(experimental::Gates::new(
                std::env::temp_dir().join("gglinuxd-tests-never-written/experimental.json"),
                Experimental::default(),
                event_tx,
                KeyboardHandle::off(),
            )),
            parts,
            restart: Arc::new(Notify::new()),
        }
    }

    #[tokio::test]
    async fn a_request_for_a_part_thats_off_is_refused_with_where_to_turn_it_on() {
        let workers = workers_with(Parts {
            sonar: false,
            keyboard: false,
            ..Parts::default()
        });
        let (_tx, snapshot) = watch::channel(Vec::new());
        let ask = |line: &'static str| dispatch(line, &snapshot, &workers);
        match ask(r#"{"id":3,"req":{"method":"get_sonar_state"}}"#).await {
            ServerMessage::Response {
                id: 3,
                result: ResponseData::Error { message },
            } => assert!(message.contains("Sonar is turned off"), "{message}"),
            other => panic!("{other:?}"),
        }
        match ask(r#"{"id":4,"req":{"method":"get_gamesense_state"}}"#).await {
            ServerMessage::Response {
                result: ResponseData::Error { message },
                ..
            } => assert!(message.contains("draws on the keyboard"), "{message}"),
            other => panic!("{other:?}"),
        }
        // The daemon's own requests still work, or a part could never come back.
        match ask(r#"{"id":5,"req":{"method":"get_parts"}}"#).await {
            ServerMessage::Response {
                id: 5,
                result: ResponseData::Parts { parts },
            } => assert!(!parts.sonar && parts.clips),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn changing_the_parts_saves_them_and_restarts_only_if_something_changed() {
        let d = Scratch::new("parts");
        let path = d.0.join("parts.json");
        let workers = workers_with(Parts::default());
        assert!(matches!(
            set_parts(&path, Parts::default(), &workers),
            ResponseData::Success
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(400), workers.restart.notified())
                .await
                .is_err(),
            "nothing changed, so no restart"
        );
        let off = Parts {
            clips: false,
            ..Parts::default()
        };
        assert!(matches!(
            set_parts(&path, off, &workers),
            ResponseData::Success
        ));
        assert_eq!(parts::load(&path), off);
        tokio::time::timeout(Duration::from_secs(2), workers.restart.notified())
            .await
            .expect("a restart is asked for once the answer is out");
    }

    #[tokio::test]
    async fn stopping_waits_for_a_keyboard_slot_write_but_not_forever() {
        let workers = workers_with(Parts::default());
        // None running: no wait.
        let start = std::time::Instant::now();
        assert!(wait_for_slot_write(&workers.keyboard, Duration::from_secs(5)).await);
        assert!(start.elapsed() < Duration::from_millis(100));
        // One running: until it ends.
        let write = workers.keyboard.slot_writes().start();
        let ends = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            drop(write);
        });
        let start = std::time::Instant::now();
        assert!(wait_for_slot_write(&workers.keyboard, Duration::from_secs(5)).await);
        assert!(start.elapsed() >= Duration::from_millis(300));
        ends.await.unwrap();
        // One that never ends is given up on.
        let _stuck = workers.keyboard.slot_writes().start();
        let start = std::time::Instant::now();
        assert!(!wait_for_slot_write(&workers.keyboard, Duration::from_millis(200)).await);
        let waited = start.elapsed();
        assert!(waited >= Duration::from_millis(200) && waited < Duration::from_secs(2));
    }

    /// The answer to one request line.
    async fn answer(line: &str, workers: &Workers) -> ResponseData {
        let (_tx, snapshot) = watch::channel(Vec::new());
        match dispatch(line, &snapshot, workers).await {
            ServerMessage::Response { result, .. } => result,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn the_m11_requests_reach_their_modules_and_the_keyboards_wait_for_its_part() {
        let d = Scratch::new("m11-routing");
        let (event_tx, _) = broadcast::channel(8);
        let fake = FakeTransport::new();
        let kb = keyboard::spawn_fake_unasked(
            fake.keyboard(),
            d.0.join("keyboard"),
            event_tx.clone(),
            Experimental::default(),
        );
        let workers = Workers {
            experimental: Arc::new(experimental::Gates::new(
                d.0.join("experimental.json"),
                Experimental::default(),
                event_tx,
                kb.clone(),
            )),
            keyboard: kb.clone(),
            ..workers_with(Parts::default())
        };
        let ask = |method: &str, params: &str| {
            let line = match params {
                "" => format!(r#"{{"id":1,"req":{{"method":"{method}"}}}}"#),
                p => format!(r#"{{"id":1,"req":{{"method":"{method}","params":{p}}}}}"#),
            };
            let workers = &workers;
            async move { answer(&line, workers).await }
        };

        assert!(matches!(
            ask("get_bindings", "").await,
            ResponseData::Bindings { state } if state == gg_ipc::BindingsState::default()
        ));
        assert!(matches!(
            ask("get_onboard", "").await,
            ResponseData::Onboard { state } if state == gg_ipc::OnboardState::default()
        ));
        assert!(matches!(
            ask("get_actions", "").await,
            ResponseData::Actions { state } if state == gg_ipc::ActionsState::default()
        ));
        assert!(matches!(
            ask("get_experimental", "").await,
            ResponseData::Experimental { experimental } if experimental == Experimental::default()
        ));
        assert!(matches!(
            ask(
                "set_experimental",
                r#"{"experimental":{"keyboard_onboard":true}}"#
            )
            .await,
            ResponseData::Success
        ));
        assert!(matches!(
            ask("get_experimental", "").await,
            ResponseData::Experimental { experimental } if experimental.keyboard_onboard
        ));
        // Bindings are built: with only keyboard_onboard on, they're refused by their own gate.
        for (method, params) in [
            ("read_bindings", ""),
            ("set_bindings", r#"{"bindings":{},"enabled":true}"#),
        ] {
            match ask(method, params).await {
                ResponseData::Error { message } => assert!(
                    message.contains("hasn't been tried on your hardware yet"),
                    "{method}: {message}"
                ),
                other => panic!("{method}: {other:?}"),
            }
        }
        assert!(fake.sent().is_empty(), "none of it reached the keyboard");
        // The mouse's sleep timer has its own gate, off here: refused before the worker.
        match ask("set_mouse_sleep", r#"{"minutes":20}"#).await {
            ResponseData::Error { message } => assert!(
                message.contains("Mouse: sleep timer") && message.contains("Experimental"),
                "{message}"
            ),
            other => panic!("set_mouse_sleep: {other:?}"),
        }
        // Protection Mode has its own gate too: switch settings that turn it on are refused
        // whole, and the same settings without it go through to the keyboard worker.
        match ask(
            "set_switches",
            r#"{"switches":{"enabled":false,"actuation":15,"protection":{"enabled":true}}}"#,
        )
        .await
        {
            ResponseData::Error { message } => assert!(
                message.contains("Keyboard: Protection Mode") && message.contains("Experimental"),
                "{message}"
            ),
            other => panic!("set_switches: {other:?}"),
        }
        assert!(matches!(
            ask(
                "set_switches",
                r#"{"switches":{"enabled":false,"actuation":15}}"#
            )
            .await,
            ResponseData::Success
        ));
        match ask("set_mouse_radio", r#"{"radio":"stable"}"#).await {
            ResponseData::Error { message } => {
                assert!(message.contains("Settings → Experimental"), "{message}")
            }
            other => panic!("set_mouse_radio: {other:?}"),
        }
        // The onboard slots are built: with the gate on (above), a read reaches the keyboard,
        // whose unscripted answers aren't a slot image, and says so.
        match ask("read_onboard", r#"{"slot":0}"#).await {
            ResponseData::Error { message } => assert!(message.contains("slot 1"), "{message}"),
            other => panic!("read_onboard: {other:?}"),
        }
        let sent = fake.sent();
        assert_eq!(sent.len(), 25);
        assert!(sent.iter().all(|s| s.report[1] == 0x83), "chunk reads only");
        // The actions service answers its own: an empty list is no change, F12 isn't a spare key.
        assert!(matches!(
            ask("set_actions", r#"{"bindings":[]}"#).await,
            ResponseData::Success
        ));
        match ask(
            "set_actions",
            r#"{"bindings":[{"key":69,"label":"x","action":{"kind":"clip_save"}}]}"#,
        )
        .await
        {
            ResponseData::Error { message } => assert!(message.contains("F13–F24"), "{message}"),
            other => panic!("{other:?}"),
        }
        kb.send::<()>(KeyboardCommand::Shutdown).await.unwrap();

        // With the keyboard part off its requests are refused first; the others aren't.
        let off = workers_with(Parts {
            keyboard: false,
            ..Parts::default()
        });
        for method in ["get_bindings", "get_onboard"] {
            match answer(
                &format!(r#"{{"id":2,"req":{{"method":"{method}"}}}}"#),
                &off,
            )
            .await
            {
                ResponseData::Error { message } => {
                    assert!(message.contains("turned off in Settings"), "{message}")
                }
                other => panic!("{method}: {other:?}"),
            }
        }
        assert!(matches!(
            answer(r#"{"id":3,"req":{"method":"get_actions"}}"#, &off).await,
            ResponseData::Actions { .. }
        ));
        assert!(matches!(
            answer(r#"{"id":4,"req":{"method":"get_experimental"}}"#, &off).await,
            ResponseData::Experimental { .. }
        ));
    }

    #[tokio::test]
    async fn a_request_sent_just_before_hanging_up_is_still_answered() {
        let answer = |_line: String| async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            ServerMessage::response(1, ResponseData::Pong)
        };
        talk(answer, |stream| async move {
            let (read, mut write) = stream.into_split();
            write.write_all(b"ping\n").await.unwrap();
            write.shutdown().await.unwrap();
            let mut lines = BufReader::new(read).lines();
            assert_eq!(next_response(&mut lines).await, 1);
        })
        .await;
    }
}
