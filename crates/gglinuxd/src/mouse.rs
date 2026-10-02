//! Mouse control worker.
//!
//! `hidapi` handles are neither `Send` nor `Sync` and every call blocks, so all
//! mouse I/O happens on one dedicated OS thread. Async tasks talk to it by sending
//! a [`Command`] carrying a oneshot channel for the reply — the same shape as the
//! `gg-hid` monitor bridge, and it keeps blocking device I/O off the tokio runtime.
//!
//! The same thread also polls the battery, so a slow or asleep mouse can never
//! block a client request and a request can never race a poll on the HID handle.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use gg_device_mouse::{ButtonAction, Mouse, PollingRate, BUTTON_COUNT};
use gg_ipc::{BatteryState, DpiStage, Event, MouseDpi, MouseSettings, MouseState};
use hidapi::HidApi;
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, error, warn};

use crate::clock::is_older_than;

/// How often to re-read the battery on our own. The mouse sleeps aggressively and
/// the reading only moves by whole percent, so there is nothing to gain from
/// polling faster.
const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Floor on how often a client request may trigger an extra battery read.
///
/// Each read is a device open plus a ~80 ms round trip, so without this a UI
/// polling `get_mouse_state` in a loop would keep the HID interface permanently
/// busy. The battery cannot meaningfully change inside this window anyway.
const MIN_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Reply channel for a command. `Err` carries a message meant for the user.
type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum Command {
    GetState(Reply<MouseState>),
    SetDpi {
        stages: Vec<DpiStage>,
        active: usize,
        save: bool,
        reply: Reply<()>,
    },
    SetPolling {
        hz: u16,
        save: bool,
        reply: Reply<()>,
    },
    SetButtons {
        actions: Vec<String>,
        save: bool,
        reply: Reply<()>,
    },
    Save(Reply<()>),
}

/// Cloneable handle used by client tasks to reach the worker.
#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    /// Send a command and await its reply.
    ///
    /// A worker that has gone away (its thread died) reports as an error rather
    /// than hanging the client.
    pub async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| "mouse worker is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "mouse worker dropped the request".to_string())?
    }
}

/// Start the worker thread and return a handle to it.
pub fn spawn(event_tx: broadcast::Sender<Event>) -> Handle {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("gg-mouse".into())
        .spawn(move || worker(rx, event_tx))
        .expect("spawn gg-mouse thread");
    Handle { tx }
}

/// Everything the worker remembers between commands.
struct State {
    api: HidApi,
    applied: Option<MouseSettings>,
    battery: Option<BatteryState>,
    /// When the battery was last read, however that read was triggered.
    last_poll: Option<Instant>,
}

fn worker(rx: Receiver<Command>, event_tx: broadcast::Sender<Event>) {
    let hid = gg_hid::lock();
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            error!(error = %e, "mouse worker could not initialise hidapi");
            return;
        }
    };
    drop(hid);
    let mut state = State {
        api,
        applied: None,
        battery: None,
        last_poll: None,
    };

    // Read once at startup so a UI connecting immediately has a value.
    poll_battery(&mut state, &event_tx);

    loop {
        let timeout = state
            .last_poll
            .map_or(Duration::ZERO, |t| POLL_INTERVAL.saturating_sub(t.elapsed()));
        match rx.recv_timeout(timeout) {
            Ok(command) => handle(command, &mut state, &event_tx),
            Err(RecvTimeoutError::Timeout) => {}
            // Every handle dropped: the daemon is shutting down.
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Checked against elapsed time rather than the recv result, so a steady
        // stream of commands cannot starve the poll by resetting the timeout.
        if is_older_than(state.last_poll, POLL_INTERVAL) {
            poll_battery(&mut state, &event_tx);
        }
    }
    debug!("mouse worker stopped");
}

fn handle(command: Command, state: &mut State, event_tx: &broadcast::Sender<Event>) {
    match command {
        Command::GetState(reply) => {
            // Refresh before answering: a client asking for the state directly
            // should not be told a reading that is up to POLL_INTERVAL old.
            poll_if_stale(state, event_tx);
            let _ = reply.send(Ok(MouseState {
                connected: gg_device_mouse::is_connected(&state.api),
                battery: state.battery,
                applied: state.applied.clone(),
            }));
            return;
        }

        Command::SetDpi {
            stages,
            active,
            save,
            reply,
        } => {
            let result = with_mouse(state, |mouse| {
                let hw: Vec<gg_device_mouse::DpiStage> = stages
                    .iter()
                    .map(|s| gg_device_mouse::DpiStage { x: s.x, y: s.y })
                    .collect();
                mouse.set_dpi(&hw, active)?;
                if save {
                    mouse.save()?;
                }
                Ok(())
            });
            if result.is_ok() {
                written(&mut state.applied).dpi = Some(MouseDpi { stages, active });
            }
            let _ = reply.send(result);
        }

        Command::SetPolling { hz, save, reply } => {
            let result = PollingRate::from_hz(hz)
                .map_err(|e| e.to_string())
                .and_then(|rate| {
                    with_mouse(state, |mouse| {
                        mouse.set_polling_rate(rate)?;
                        if save {
                            mouse.save()?;
                        }
                        Ok(())
                    })
                });
            if result.is_ok() {
                written(&mut state.applied).polling_hz = Some(hz);
            }
            let _ = reply.send(result);
        }

        Command::SetButtons {
            actions,
            save,
            reply,
        } => {
            let result = parse_actions(&actions).and_then(|parsed| {
                with_mouse(state, |mouse| {
                    mouse.set_buttons(&parsed)?;
                    if save {
                        mouse.save()?;
                    }
                    Ok(())
                })
            });
            if result.is_ok() {
                written(&mut state.applied).buttons = Some(actions);
            }
            let _ = reply.send(result);
        }

        Command::Save(reply) => {
            let _ = reply.send(with_mouse(state, |mouse| mouse.save()));
            // Nothing changed that a client would want re-read.
            return;
        }
    }

    // A setting was just applied, which means the mouse is awake and talking —
    // a cheap moment for a fresh reading, pushed to clients as an event.
    poll_if_stale(state, event_tx);
}

/// Open the mouse, run `f`, and stringify any failure for the client.
fn with_mouse<T>(
    state: &mut State,
    f: impl FnOnce(&Mouse) -> anyhow::Result<T>,
) -> Result<T, String> {
    // Re-enumerate so a mouse plugged in after startup is found.
    let hid = gg_hid::lock();
    if let Err(e) = state.api.refresh_devices() {
        return Err(format!("could not refresh HID devices: {e}"));
    }
    let mouse = Mouse::open(&state.api).map_err(|e| format!("{e:#}"))?;
    drop(hid);
    f(&mouse).map_err(|e| format!("{e:#}"))
}

/// Read the battery, unless one was read very recently. See [`MIN_POLL_INTERVAL`].
fn poll_if_stale(state: &mut State, event_tx: &broadcast::Sender<Event>) {
    if is_older_than(state.last_poll, MIN_POLL_INTERVAL) {
        poll_battery(state, event_tx);
    }
}

/// Read the battery and broadcast if it changed.
fn poll_battery(state: &mut State, event_tx: &broadcast::Sender<Event>) {
    // Stamped even if the read below fails, so a disconnected mouse is retried on
    // the normal schedule rather than on every request.
    state.last_poll = Some(Instant::now());
    let hid = gg_hid::lock();
    if state.api.refresh_devices().is_err() {
        return;
    }
    let opened = Mouse::open(&state.api);
    drop(hid);
    let reading = match opened {
        Ok(mouse) => match mouse.battery() {
            Ok(battery) => battery,
            Err(e) => {
                warn!(error = %format!("{e:#}"), "battery read failed");
                None
            }
        },
        // Not connected at all — report unknown rather than a stale value.
        Err(_) => None,
    };
    let reading = reading.map(|b| BatteryState {
        level: b.level,
        charging: b.charging,
    });

    if reading != state.battery {
        debug!(?reading, "mouse battery changed");
        state.battery = reading;
        let _ = event_tx.send(Event::MouseBattery { battery: reading });
    }
}

fn parse_actions(actions: &[String]) -> Result<[ButtonAction; BUTTON_COUNT], String> {
    if actions.len() != BUTTON_COUNT {
        return Err(format!(
            "expected {BUTTON_COUNT} button actions, got {}",
            actions.len()
        ));
    }
    let mut parsed = gg_device_mouse::proto::default_buttons();
    for (slot, name) in parsed.iter_mut().zip(actions) {
        *slot = ButtonAction::parse(name).map_err(|e| e.to_string())?;
    }
    Ok(parsed)
}

/// The record of what has been written, started empty by the first write.
///
/// Each setter fills in only its own part. The rest stay `None` until their own
/// setter runs, since the mouse cannot say what they are — see [`MouseState::applied`].
fn written(applied: &mut Option<MouseSettings>) -> &mut MouseSettings {
    applied.get_or_insert_with(MouseSettings::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_write_reports_only_the_part_it_wrote() {
        // As SetPolling records it on the first write of a session.
        let mut applied = None;
        written(&mut applied).polling_hz = Some(500);
        let json = serde_json::to_value(&applied).unwrap();
        assert_eq!(json["polling_hz"], 500, "{json}");
        let claimed: Vec<&String> = json
            .as_object()
            .unwrap()
            .iter()
            .filter(|(part, value)| *part != "polling_hz" && !value.is_null())
            .map(|(part, _)| part)
            .collect();
        assert!(claimed.is_empty(), "reported as applied but never written: {claimed:?} in {json}");
    }
}
