//! gglinuxd — the gglinux background daemon.
//!
//! M0 scope: enumerate SteelSeries devices, monitor hotplug, and serve the device
//! list + live add/remove events over a newline-delimited JSON unix socket. Device
//! control, PipeWire audio, and the GameSense server land in later milestones.

use anyhow::{Context, Result};
use gg_ipc::{DeviceInfo, Event, Request, RequestFrame, ResponseData, ServerMessage};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{broadcast, watch};
use tracing::{debug, error, info};

/// Capacity of the per-client event broadcast buffer.
const EVENT_CHANNEL_CAP: usize = 256;

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

    let result = accept_loop(&listener, snapshot_rx, event_tx).await;

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
) -> Result<()> {
    let mut sigterm = signal(SignalKind::terminate()).context("installing SIGTERM handler")?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted.context("accept")?;
                let snapshot_rx = snapshot_rx.clone();
                let event_tx = event_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, snapshot_rx, event_tx).await {
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
                        let reply = dispatch(&line, &snapshot_rx);
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
fn dispatch(line: &str, snapshot_rx: &watch::Receiver<Vec<DeviceInfo>>) -> ServerMessage {
    match serde_json::from_str::<RequestFrame>(line) {
        Ok(frame) => {
            let result = match frame.req {
                Request::ListDevices => ResponseData::Devices {
                    devices: snapshot_rx.borrow().clone(),
                },
                Request::Ping => ResponseData::Pong,
            };
            ServerMessage::response(frame.id, result)
        }
        Err(e) => ServerMessage::response(0, ResponseData::Error {
            message: format!("invalid request: {e}"),
        }),
    }
}

async fn write_message(write_half: &mut (impl AsyncWriteExt + Unpin), msg: &ServerMessage) -> Result<()> {
    let line = msg.to_line()?;
    write_half.write_all(line.as_bytes()).await?;
    Ok(())
}
