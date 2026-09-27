//! Headset worker: the Arctis Nova Pro Wireless base station.
//!
//! Like the mouse, all device I/O happens on one dedicated OS thread, because
//! hidapi handles are neither `Send` nor `Sync` and every call blocks. Unlike the
//! mouse, the device stays open: volume changes arrive as unsolicited interrupts,
//! and the screen policy has to hear them.
//!
//! The thread runs the base-station screen's burn-in protection: it polls status
//! and volume every second, turns what changed into [`screen::Input`]s, and
//! carries out the [`screen::Action`]s that come back.

mod screen;
mod wake;

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use gg_device_headset::{proto, Headset, PowerStatus, Status};
use gg_ipc::{Event, HeadsetPower, HeadsetState, ScreenSettings};
use hidapi::HidApi;
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, error, info, warn};

use crate::clock::is_older_than;
use screen::{Action, Input, Screen};

/// How often to ask for status and volume while connected.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How often to look for the base station while it is absent.
const REOPEN_INTERVAL: Duration = Duration::from_secs(2);

/// Longest one loop iteration waits for an interrupt. Bounds command latency.
const EVENT_WAIT_MS: i32 = 100;

/// Reply channel for a command. `Err` carries a message meant for the user.
type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum Command {
    GetState(Reply<HeadsetState>),
    SetScreen { settings: ScreenSettings, reply: Reply<()> },
    /// Leave the screen dark and stop. Replies once the device has been told.
    Shutdown(Reply<()>),
}

/// Cloneable handle used by client tasks to reach the worker.
#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    /// Send a command and await its reply. A worker that has gone away reports
    /// as an error rather than hanging the client.
    pub async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| "headset worker is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "headset worker dropped the request".to_string())?
    }
}

/// Start the worker thread and return a handle to it.
pub fn spawn(event_tx: broadcast::Sender<Event>) -> Handle {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("gg-headset".into())
        .spawn(move || worker(rx, event_tx))
        .expect("spawn gg-headset thread");
    Handle { tx }
}

/// Everything the worker remembers between iterations.
struct State {
    api: HidApi,
    device: Option<Headset>,
    last_open_attempt: Option<Instant>,
    last_poll: Option<Instant>,
    /// Latest status and volume; cleared when the device goes away, so a
    /// reconnect sets a fresh baseline instead of waking on stale differences.
    status: Option<Status>,
    volume: Option<u8>,
    settings: ScreenSettings,
    screen: Screen,
    /// Last state sent to clients, so unchanged iterations stay quiet.
    published: Option<HeadsetState>,
}

fn worker(rx: Receiver<Command>, event_tx: broadcast::Sender<Event>) {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            error!(error = %e, "headset worker could not initialise hidapi");
            return;
        }
    };
    let settings = ScreenSettings::default();
    let mut state = State {
        api,
        device: None,
        last_open_attempt: None,
        last_poll: None,
        status: None,
        volume: None,
        settings,
        screen: Screen::new(&settings, Instant::now()),
        published: None,
    };

    loop {
        loop {
            match rx.try_recv() {
                Ok(command) => {
                    if !handle(command, &mut state, &event_tx) {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                // Every handle dropped without a Shutdown: still leave it dark.
                Err(TryRecvError::Disconnected) => {
                    shutdown(&mut state);
                    return;
                }
            }
        }

        if state.device.is_none() {
            try_open(&mut state);
        }
        if state.device.is_some() {
            if let Some(report) = read_event(&mut state) {
                on_report(&mut state, &report);
            }
            if is_older_than(state.last_poll, POLL_INTERVAL) {
                poll(&mut state);
            }
        } else {
            // Nothing to wait on; don't spin while the base station is absent.
            std::thread::sleep(Duration::from_millis(EVENT_WAIT_MS as u64));
        }

        let action = state.screen.on(Input::Tick, Instant::now());
        perform(&mut state, action);
        publish(&mut state, &event_tx);
    }
}

/// Handle one client command. Returns `false` when the worker should stop.
fn handle(command: Command, state: &mut State, event_tx: &broadcast::Sender<Event>) -> bool {
    match command {
        Command::GetState(reply) => {
            let _ = reply.send(Ok(snapshot(state)));
        }
        Command::SetScreen { settings, reply } => {
            if let Err(message) = settings.validate() {
                let _ = reply.send(Err(message));
                return true;
            }
            let brightness_changed = settings.brightness != state.settings.brightness;
            state.settings = settings;
            if brightness_changed {
                apply_brightness(state);
            }
            let action = state.screen.apply(&settings, Instant::now());
            perform(state, action);
            // Stored either way: a disconnected base station gets them on reconnect.
            let _ = reply.send(Ok(()));
            publish(state, event_tx);
        }
        Command::Shutdown(reply) => {
            shutdown(state);
            let _ = reply.send(Ok(()));
            return false;
        }
    }
    true
}

/// Open the base station if it has appeared, at most every [`REOPEN_INTERVAL`].
fn try_open(state: &mut State) {
    if !is_older_than(state.last_open_attempt, REOPEN_INTERVAL) {
        return;
    }
    state.last_open_attempt = Some(Instant::now());
    if let Err(e) = state.api.refresh_devices() {
        debug!(error = %e, "could not refresh HID devices");
        return;
    }
    match Headset::open(&state.api) {
        Ok(headset) => {
            info!("base station connected");
            state.device = Some(headset);
            state.last_poll = None;
            // The device forgets brightness on power loss; re-apply what the user set.
            apply_brightness(state);
            let action = state.screen.on(Input::DeviceOpened, Instant::now());
            perform(state, action);
        }
        // Absent is the normal state for an unplugged base station: stay quiet.
        Err(e) => debug!(error = %format!("{e:#}"), "base station not available"),
    }
}

/// Wait briefly for one unsolicited report.
fn read_event(state: &mut State) -> Option<Vec<u8>> {
    let result = state.device.as_ref()?.read_event_timeout(EVENT_WAIT_MS);
    match result {
        Ok(report) => report,
        Err(e) => {
            info!(error = %format!("{e:#}"), "base station disconnected");
            drop_device(state);
            None
        }
    }
}

/// Status, volume, and any events that arrived around them.
fn poll(state: &mut State) {
    state.last_poll = Some(Instant::now());
    let Some(device) = state.device.as_ref() else { return };
    let polled = match device.poll() {
        Ok(p) => p,
        Err(e) => {
            info!(error = %format!("{e:#}"), "base station disconnected");
            drop_device(state);
            return;
        }
    };
    for report in &polled.events {
        on_report(state, report);
    }

    let now = Instant::now();
    if let Some(next) = polled.status {
        if let Some(reason) = state.status.and_then(|prev| wake::wake_reason(&prev, &next)) {
            debug!(?reason, "status change lights the screen");
            let action = state.screen.on(Input::OurEvent, now);
            perform(state, action);
        }
        state.status = Some(next);
    }
    if let Some(volume) = polled.volume {
        // The firmware draws its own volume popup (verified 2026-09-27).
        if state.volume.is_some_and(|v| v != volume) {
            let action = state.screen.on(Input::FirmwareEvent, now);
            perform(state, action);
        }
        state.volume = Some(volume);
    }
}

/// Feed one unsolicited report to the screen policy.
fn on_report(state: &mut State, report: &[u8]) {
    if !proto::is_event(report) {
        return; // a late reply to an earlier request, not something that happened
    }
    let input = if let Some(volume) = proto::decode_volume_event(report) {
        state.volume = Some(volume);
        Input::FirmwareEvent
    } else {
        // Nothing confirms the firmware shows these itself, and showing the stock
        // screen when it already is costs nothing.
        debug!(report = ?&report[..report.len().min(4)], "base station event");
        Input::OurEvent
    };
    let action = state.screen.on(input, Instant::now());
    perform(state, action);
}

/// Carry out a policy action. A failed write drops the device; the policy
/// re-syncs the screen through `DeviceOpened` when it comes back.
fn perform(state: &mut State, action: Option<Action>) {
    let (Some(action), Some(device)) = (action, state.device.as_ref()) else { return };
    let result = match action {
        Action::Blank => device.oled_blank(),
        Action::ShowStock => device.oled_show_stock(),
    };
    match result {
        Ok(()) => debug!(?action, "screen"),
        Err(e) => {
            warn!(?action, error = %format!("{e:#}"), "screen write failed; reopening");
            drop_device(state);
        }
    }
}

fn apply_brightness(state: &mut State) {
    let (Some(level), Some(device)) = (state.settings.brightness, state.device.as_ref()) else {
        return;
    };
    if let Err(e) = device.set_oled_brightness(level) {
        warn!(level, error = %format!("{e:#}"), "brightness write failed; reopening");
        drop_device(state);
    }
}

fn drop_device(state: &mut State) {
    state.device = None;
    state.status = None;
    state.volume = None;
}

fn shutdown(state: &mut State) {
    let action = state.screen.on(Input::Shutdown, Instant::now());
    perform(state, action);
    debug!("headset worker stopped");
}

fn snapshot(state: &State) -> HeadsetState {
    let connected = state.device.is_some();
    let status = state.status.filter(|_| connected);
    HeadsetState {
        connected,
        power: status.map_or(HeadsetPower::Unknown, |s| power(s.power)),
        headset_battery: status.and_then(|s| s.headset_battery),
        spare_battery: status.and_then(|s| s.spare_battery),
        screen: state.settings,
        screen_dark: connected && state.screen.is_dark(),
    }
}

fn power(p: PowerStatus) -> HeadsetPower {
    match p {
        PowerStatus::Offline => HeadsetPower::Offline,
        PowerStatus::Charging => HeadsetPower::Charging,
        PowerStatus::Online => HeadsetPower::Online,
        PowerStatus::Unknown(_) => HeadsetPower::Unknown,
    }
}

/// Broadcast the state if it changed since the last broadcast.
fn publish(state: &mut State, event_tx: &broadcast::Sender<Event>) {
    let now = snapshot(state);
    if state.published.as_ref() != Some(&now) {
        state.published = Some(now.clone());
        let _ = event_tx.send(Event::HeadsetChanged { state: now });
    }
}
