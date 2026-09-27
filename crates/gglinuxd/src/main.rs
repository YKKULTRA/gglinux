//! gglinuxd — the gglinux background daemon.
//!
//! Current scope: enumerate SteelSeries devices, monitor hotplug, control the
//! Rival 3 Wireless Gen 2, drive the Arctis Nova Pro Wireless headset (settings,
//! and keeping its base-station screen from burning in), run Sonar-style audio
//! channels on PipeWire, and serve all of it over a newline-delimited JSON unix
//! socket. PipeWire audio and the GameSense server land in later milestones.

use std::time::Duration;

use anyhow::{Context, Result};
use gg_ipc::{DeviceInfo, Event, Request, RequestFrame, ResponseData, ServerMessage};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{broadcast, watch};
use tracing::{debug, error, info, warn};

mod clock;
mod headset;
mod mouse;
mod sonar;

use headset::{Command as HeadsetCommand, Handle as HeadsetHandle};
use mouse::{Command as MouseCommand, Handle as MouseHandle};
use sonar::{Command as SonarCommand, Handle as SonarHandle};

/// Capacity of the per-client event broadcast buffer.
const EVENT_CHANNEL_CAP: usize = 256;

/// Longest the daemon waits, on exit, for the base-station screen to be blanked.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// Handles to the device workers, passed to every client task.
#[derive(Clone)]
struct Workers {
    mouse: MouseHandle,
    headset: HeadsetHandle,
    sonar: SonarHandle,
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
    // Clear a stale socket from an unclean shutdown.
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("binding unix socket {}", path.display()))?;
    info!(socket = %path.display(), "gglinuxd listening");

    // Shared device state: `snapshot` holds the current list; `events` fans out diffs.
    let (snapshot_tx, snapshot_rx) = watch::channel::<Vec<DeviceInfo>>(Vec::new());
    let (event_tx, _) = broadcast::channel::<Event>(EVENT_CHANNEL_CAP);

    spawn_hid_monitor(snapshot_tx, event_tx.clone());
    let workers = Workers {
        mouse: mouse::spawn(event_tx.clone()),
        headset: headset::spawn(event_tx.clone()),
        sonar: sonar::spawn(event_tx.clone()),
    };

    let result = accept_loop(&listener, snapshot_rx, event_tx, workers.clone()).await;

    // Leave the base-station screen dark: at logout or shutdown its USB power
    // often stays on, and nobody is there to see it.
    shutdown_headset(&workers.headset).await;

    // Best-effort cleanup so the next start binds cleanly.
    let _ = std::fs::remove_file(&path);
    result
}

/// Bridge the blocking gg-hid monitor thread into the async world via watch/broadcast.
fn spawn_hid_monitor(snapshot_tx: watch::Sender<Vec<DeviceInfo>>, event_tx: broadcast::Sender<Event>) {
    std::thread::Builder::new()
        .name("gg-hid".into())
        .spawn(move || {
            let res = gg_hid::run_monitor(|snapshot, events| {
                // `send` here is synchronous — safe to call from this OS thread.
                let _ = snapshot_tx.send(snapshot);
                for ev in events {
                    let _ = event_tx.send(ev);
                }
            });
            if let Err(e) = res {
                error!(error = %e, "HID monitor stopped");
            }
        })
        .expect("spawn gg-hid thread");
}

/// Accept connections until SIGINT/SIGTERM.
async fn accept_loop(
    listener: &UnixListener,
    snapshot_rx: watch::Receiver<Vec<DeviceInfo>>,
    event_tx: broadcast::Sender<Event>,
    workers: Workers,
) -> Result<()> {
    let mut sigterm = signal(SignalKind::terminate()).context("installing SIGTERM handler")?;

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
            _ = tokio::signal::ctrl_c() => { info!("received SIGINT; shutting down"); break; }
            _ = sigterm.recv() => { info!("received SIGTERM; shutting down"); break; }
        }
    }
    Ok(())
}

/// Serve one client: push the current snapshot, then stream events and answer requests.
async fn handle_client(
    stream: UnixStream,
    snapshot_rx: watch::Receiver<Vec<DeviceInfo>>,
    event_tx: broadcast::Sender<Event>,
    workers: Workers,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    let mut events = event_tx.subscribe();

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
                    Some(line) if !line.trim().is_empty() => {
                        let reply = dispatch(&line, &snapshot_rx, &workers).await;
                        write_message(&mut write_half, &reply).await?;
                    }
                    Some(_) => {} // blank keepalive line
                    None => break, // client closed
                }
            }
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
    let frame = match serde_json::from_str::<RequestFrame>(line) {
        Ok(frame) => frame,
        // No id to correlate against, so reply on id 0 and let the client log it.
        Err(e) => {
            return ServerMessage::response(
                0,
                ResponseData::Error {
                    message: format!("invalid request: {e}"),
                },
            )
        }
    };

    let result = match frame.req {
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
            workers.sonar.send::<()>(|reply| SonarCommand::SetEnabled { enabled, reply }).await,
        ),
        Request::SetSonarOutput { name } => outcome(
            workers.sonar.send::<()>(|reply| SonarCommand::SetOutput { name, reply }).await,
        ),
        Request::SetChannelVolume { channel, volume } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetVolume { channel, volume, reply })
                .await,
        ),
        Request::SetChatMix { balance } => outcome(
            workers.sonar.send::<()>(|reply| SonarCommand::SetChatMix { balance, reply }).await,
        ),
        Request::SetAppChannel { binary, channel } => outcome(
            workers
                .sonar
                .send::<()>(|reply| SonarCommand::SetAppChannel { binary, channel, reply })
                .await,
        ),
    };
    ServerMessage::response(frame.id, result)
}

/// Collapse a command result into `Success` or `Error`.
fn outcome(result: Result<(), String>) -> ResponseData {
    match result {
        Ok(()) => ResponseData::Success,
        Err(message) => ResponseData::Error { message },
    }
}

/// Ask the headset worker to leave the screen dark, waiting at most [`SHUTDOWN_GRACE`].
async fn shutdown_headset(headset: &HeadsetHandle) {
    let request = headset.send::<()>(HeadsetCommand::Shutdown);
    match tokio::time::timeout(SHUTDOWN_GRACE, request).await {
        Ok(Ok(())) => debug!("base-station screen left dark"),
        Ok(Err(e)) => warn!(error = %e, "headset shutdown failed"),
        Err(_) => warn!("headset worker did not confirm shutdown in time"),
    }
}

async fn write_message(write_half: &mut (impl AsyncWriteExt + Unpin), msg: &ServerMessage) -> Result<()> {
    let line = msg.to_line()?;
    write_half.write_all(line.as_bytes()).await?;
    Ok(())
}
