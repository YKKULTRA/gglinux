// SPDX-License-Identifier: GPL-3.0-or-later
//! Headset worker: the Arctis Nova Pro Wireless base station.
//!
//! Like the mouse, all device I/O happens on one dedicated OS thread, because
//! hidapi handles are neither `Send` nor `Sync` and every call blocks. Unlike the
//! mouse, the device stays open: volume changes arrive as unsolicited interrupts,
//! and the screen policy has to hear them.
//!
//! The thread runs the base-station screen's burn-in protection: it polls status
//! and audio settings every second, turns what changed into [`screen::Input`]s, and
//! carries out the [`screen::Action`]s that come back.
//!
//! The base station's own settings (Bluetooth, gain, line out, home screen, dim
//! timeout) need the `headset_extras` gate, which the worker follows on the event
//! bus ([`gates`]). While it is on, the worker also asks for the screen settings
//! (`06 80`) now and then ([`ux`]), for the home screen and the dim timeout.

mod controls;
mod gates;
mod screen;
mod ux;
mod wake;

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use gg_device_headset::{proto, AudioSettings, Headset, PowerStatus, Status};
use gg_ipc::{Event, Feature, HeadsetChange, HeadsetPower, HeadsetState, ScreenSettings};
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
    SetScreen {
        settings: ScreenSettings,
        reply: Reply<()>,
    },
    /// Apply one setting and save it on the base station.
    SetControl {
        change: HeadsetChange,
        reply: Reply<()>,
    },
    /// Leave the screen dark and stop. Replies once the device has been told.
    Shutdown(Reply<()>),
}

/// Cloneable handle used by client tasks to reach the worker.
#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    /// A handle with no worker, for when this part is off: every send fails.
    pub fn off() -> Handle {
        let (tx, _) = mpsc::channel();
        Handle { tx }
    }

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
    // Before the thread starts, so no change of the gates after this call is missed.
    let gates = gates::Gates::follow(&event_tx, crate::experimental::path());
    std::thread::Builder::new()
        .name("gg-headset".into())
        .spawn(move || worker(rx, event_tx, gates))
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
    /// The latest `06 20` reply, decoded; cleared with the status.
    audio: Option<AudioSettings>,
    settings: ScreenSettings,
    screen: Screen,
    /// Settings as last written here, shown where the base station reports nothing.
    written: controls::Written,
    gates: gates::Gates,
    /// The screen settings reply: when to ask, and what was read.
    ux: ux::UxRead,
    /// Last state sent to clients, so unchanged iterations stay quiet.
    published: Option<HeadsetState>,
}

fn worker(rx: Receiver<Command>, event_tx: broadcast::Sender<Event>, gates: gates::Gates) {
    let hid = gg_hid::lock();
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            error!(error = %e, "headset worker could not initialise hidapi");
            return;
        }
    };
    drop(hid);
    let settings = ScreenSettings::default();
    let mut state = State {
        api,
        device: None,
        last_open_attempt: None,
        last_poll: None,
        status: None,
        volume: None,
        audio: None,
        settings,
        screen: Screen::new(&settings, Instant::now()),
        written: controls::Written::default(),
        gates,
        ux: ux::UxRead::default(),
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
        Command::SetControl { change, reply } => {
            let _ = reply.send(set_control(state, &change));
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

/// Check the gate and the values, apply, remember what was written, and show the
/// change.
fn set_control(state: &mut State, change: &HeadsetChange) -> Result<(), String> {
    let gates = state.gates.now();
    match controls::send_change(state.device.as_ref(), &gates, change) {
        Ok(()) => {}
        Err(controls::Failed::Refused(why)) => return Err(why),
        Err(controls::Failed::Write(e)) => {
            warn!(?change, error = %e, "setting write failed; reopening");
            drop_device(state);
            return Err(e);
        }
    }
    controls::record(&mut state.written, change);
    // The change is shown for one lit period, and read-back values refresh now.
    let action = state.screen.on(Input::OurEvent, Instant::now());
    perform(state, action);
    state.last_poll = None;
    if matches!(
        change,
        HeadsetChange::HomeScreen(_) | HeadsetChange::ScreenDimMinutes(_)
    ) {
        state.ux.changed_here(Instant::now());
    }
    Ok(())
}

/// Open the base station if it has appeared, at most every [`REOPEN_INTERVAL`].
fn try_open(state: &mut State) {
    if !is_older_than(state.last_open_attempt, REOPEN_INTERVAL) {
        return;
    }
    state.last_open_attempt = Some(Instant::now());
    let hid = gg_hid::lock();
    if let Err(e) = state.api.refresh_devices() {
        debug!(error = %e, "could not refresh HID devices");
        return;
    }
    let opened = Headset::open(&state.api);
    drop(hid);
    match opened {
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
    let Some(device) = state.device.as_ref() else {
        return;
    };
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
    state.audio = polled.audio.or(state.audio.take());

    let now = Instant::now();
    if let Some(next) = polled.status {
        if let Some(reason) = state
            .status
            .and_then(|prev| wake::wake_reason(&prev, &next))
        {
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
    let extras = state.gates.now().on(Feature::HeadsetExtras);
    if !extras {
        // Not asked for any more, so what was read would only grow old.
        state.ux.forget();
    } else if state.ux.due(extras, Instant::now()) {
        poll_ux(state);
    }
}

/// Ask for the screen settings, and take the reply.
fn poll_ux(state: &mut State) {
    state.ux.asked(Instant::now());
    let Some(device) = state.device.as_ref() else {
        return;
    };
    let reply = match device.ux_settings() {
        Ok(r) => r,
        Err(e) => {
            info!(error = %format!("{e:#}"), "base station disconnected");
            drop_device(state);
            return;
        }
    };
    for report in &reply.events {
        on_report(state, report);
    }
    if state.ux.took(&reply, Instant::now()) {
        let raw = reply.raw.as_deref().unwrap_or_default();
        debug!(
            decoded = ?reply.decoded,
            raw = %hex(&raw[..raw.len().min(8)]),
            "base station screen settings (06 80)"
        );
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
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
    let (Some(action), Some(device)) = (action, state.device.as_ref()) else {
        return;
    };
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
    state.audio = None;
    state.ux.forget();
}

fn shutdown(state: &mut State) {
    let action = state.screen.on(Input::Shutdown, Instant::now());
    perform(state, action);
    debug!("headset worker stopped");
}

fn snapshot(state: &State) -> HeadsetState {
    let connected = state.device.is_some();
    let status = state.status.filter(|_| connected);
    let audio = state.audio.as_ref().filter(|_| connected);
    let ux = state.ux.settings.as_ref().filter(|_| connected);
    HeadsetState {
        connected,
        power: status.map_or(HeadsetPower::Unknown, |s| power(s.power)),
        headset_battery: status.and_then(|s| s.headset_battery),
        spare_battery: status.and_then(|s| s.spare_battery),
        screen: state.settings,
        screen_dark: connected && state.screen.is_dark(),
        controls: controls::merged(status.as_ref(), audio, ux, &state.written),
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

