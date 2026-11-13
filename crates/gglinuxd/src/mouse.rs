// SPDX-License-Identifier: GPL-3.0-or-later
//! Mouse control worker.
//!
//! `hidapi` handles are neither `Send` nor `Sync` and every call blocks, so all
//! mouse I/O happens on one dedicated OS thread. Async tasks talk to it by sending
//! a [`Command`] carrying a oneshot channel for the reply — the same shape as the
//! `gg-hid` monitor bridge, and it keeps blocking device I/O off the tokio runtime.
//!
//! The same thread also polls the battery, so a slow or asleep mouse can never
//! block a client request and a request can never race a poll on the HID handle.
//!
//! Every write starts with a status read made for it (the battery query): an asleep mouse
//! drops whatever it's sent, so it's sent nothing and the client hears
//! [`MOUSE_ASLEEP`]. After a write, a setting the mouse can report (DPI `AC`, polling rate
//! `97`, sleep timer `AB`, wireless mode `AE`) is read back while those reads are allowed
//! ([`Feature::MousePower`]), and a mouse that still reports something else is an error
//! naming the setting. The buttons and the wheel have no read, so they can't be checked.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use gg_device_mouse::proto::{self, nearest_dpi};
use gg_device_mouse::{
    ButtonAction, Mouse, PollingRate, Radio, ReadBack, BUTTON_COUNT, DEFAULT_WHEEL,
};
use gg_ipc::{
    BatteryState, DpiStage, Event, Experimental, Feature, MouseDpi, MouseRadio, MouseSettings,
    MouseState, MOUSE_ASLEEP, MOUSE_SLEEP_MINUTES,
};
use hidapi::HidApi;
use tokio::sync::{broadcast, oneshot, watch};
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
    /// Six button actions, or eight with the wheel's two (see
    /// [`gg_ipc::Request::SetMouseButtons`]).
    SetButtons {
        actions: Vec<String>,
        save: bool,
        reply: Reply<()>,
    },
    Save(Reply<()>),
    /// The sleep timer, saved with it (see [`gg_ipc::Request::SetMouseSleep`]).
    SetSleep {
        minutes: u8,
        reply: Reply<()>,
    },
    /// The radio mode (see [`gg_ipc::Request::SetMouseRadio`]).
    SetRadio {
        radio: MouseRadio,
        save: bool,
        reply: Reply<()>,
    },
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

/// Start the worker thread and return a handle to it. `gates` are the experimental
/// gates: the mouse's own settings are only read, and the sleep timer and radio mode only
/// written, while [`Feature::MousePower`] is on; the wheel only while [`Feature::MouseWheel`]
/// is.
pub fn spawn(event_tx: broadcast::Sender<Event>, gates: watch::Receiver<Experimental>) -> Handle {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("gg-mouse".into())
        .spawn(move || worker(rx, event_tx, gates))
        .expect("spawn gg-mouse thread");
    Handle { tx }
}

/// Where the worker finds the mouse: through hidapi, or a fake in tests.
trait Port {
    /// Open the mouse afresh.
    fn open(&mut self) -> Result<Mouse, String>;
    /// Whether the mouse's control interface was there when last looked for.
    fn connected(&self) -> bool;
}

/// The real mouse, through hidapi.
struct Hid(HidApi);

impl Port for Hid {
    fn open(&mut self) -> Result<Mouse, String> {
        // Re-enumerate so a mouse plugged in after startup is found.
        let _hid = gg_hid::lock();
        self.0
            .refresh_devices()
            .map_err(|e| format!("could not refresh HID devices: {e}"))?;
        Mouse::open(&self.0).map_err(|e| format!("{e:#}"))
    }

    fn connected(&self) -> bool {
        gg_device_mouse::is_connected(&self.0)
    }
}

/// Everything the worker remembers between commands.
struct State {
    port: Box<dyn Port>,
    applied: Option<MouseSettings>,
    battery: Option<BatteryState>,
    /// When the battery was last read, however that read was triggered.
    last_poll: Option<Instant>,
    gates: watch::Receiver<Experimental>,
    /// The mouse's own settings as last read: dropped when it falls asleep or goes away
    /// (they can't be checked then), and when the gate goes off.
    reported: Reported,
}

/// What the mouse last reported of its own settings.
#[derive(Debug, Clone, Default, PartialEq)]
struct Reported {
    sleep_secs: Option<u16>,
    radio: Option<MouseRadio>,
    dpi: Option<MouseDpi>,
    polling_hz: Option<u16>,
}

/// One round of answers from the mouse, as decoded.
#[derive(Debug, Default)]
struct Answers {
    dpi: Option<(Vec<gg_device_mouse::DpiStage>, usize)>,
    polling: Option<PollingRate>,
    radio: Option<Radio>,
    sleep_secs: Option<u16>,
}

impl Reported {
    /// Take in a round of answers from an awake mouse. A mouse that isn't answering reads
    /// as all zeros, which would pass for 1000 Hz and High-Efficiency Mode: those two are
    /// believed only next to DPI stages that answered (zeros there are no stages at all).
    /// What isn't answered keeps the reading it had. An asleep mouse doesn't get here:
    /// its readings are dropped first ([`take_battery`]).
    fn take(&mut self, answers: Answers) {
        if let Some(secs) = answers.sleep_secs {
            self.sleep_secs = Some(secs);
        }
        let Some((stages, active)) = answers.dpi else {
            return;
        };
        self.dpi = Some(MouseDpi {
            stages: stages.iter().map(|s| DpiStage { x: s.x, y: s.y }).collect(),
            active,
        });
        if let Some(rate) = answers.polling {
            self.polling_hz = Some(rate.hz());
        }
        if let Some(radio) = answers.radio {
            self.radio = Some(from_device(radio));
        }
    }
}

fn to_device(radio: MouseRadio) -> Radio {
    match radio {
        MouseRadio::HighEfficiency => Radio::HighEfficiency,
        MouseRadio::Normal => Radio::Normal,
        MouseRadio::Stable => Radio::Stable,
    }
}

fn from_device(radio: Radio) -> MouseRadio {
    match radio {
        Radio::HighEfficiency => MouseRadio::HighEfficiency,
        Radio::Normal => MouseRadio::Normal,
        Radio::Stable => MouseRadio::Stable,
    }
}

impl State {
    fn new(port: Box<dyn Port>, gates: watch::Receiver<Experimental>) -> State {
        State {
            port,
            applied: None,
            battery: None,
            last_poll: None,
            gates,
            reported: Reported::default(),
        }
    }

    fn power_on(&self) -> bool {
        self.gates.borrow().on(Feature::MousePower)
    }
}

fn worker(
    rx: Receiver<Command>,
    event_tx: broadcast::Sender<Event>,
    gates: watch::Receiver<Experimental>,
) {
    let hid = gg_hid::lock();
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            error!(error = %e, "mouse worker could not initialise hidapi");
            return;
        }
    };
    drop(hid);
    let mut state = State::new(Box::new(Hid(api)), gates);

    // Read once at startup so a UI connecting immediately has a value.
    poll_battery(&mut state, &event_tx);

    loop {
        let timeout = state.last_poll.map_or(Duration::ZERO, |t| {
            POLL_INTERVAL.saturating_sub(t.elapsed())
        });
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
            let reported = if state.power_on() {
                state.reported.clone()
            } else {
                Reported::default()
            };
            let _ = reply.send(Ok(MouseState {
                connected: state.port.connected(),
                battery: state.battery,
                applied: state.applied.clone(),
                sleep_secs: reported.sleep_secs,
                radio: reported.radio,
                reported_dpi: reported.dpi,
                reported_polling_hz: reported.polling_hz,
            }));
            return;
        }
        Command::SetDpi {
            stages,
            active,
            save,
            reply,
        } => {
            let _ = reply.send(set_dpi(state, event_tx, &stages, active, save));
        }
        Command::SetPolling { hz, save, reply } => {
            let _ = reply.send(set_polling(state, event_tx, hz, save));
        }
        Command::SetButtons {
            actions,
            save,
            reply,
        } => {
            let _ = reply.send(set_buttons(state, event_tx, &actions, save));
        }
        Command::Save(reply) => {
            let saved = awake_mouse(state, event_tx).and_then(|mouse| run(|| mouse.save()));
            let _ = reply.send(saved);
            // Nothing changed that a client would want re-read.
            return;
        }
        Command::SetSleep { minutes, reply } => {
            let _ = reply.send(set_sleep(state, event_tx, minutes));
        }
        Command::SetRadio { radio, save, reply } => {
            let _ = reply.send(set_radio(state, event_tx, radio, save));
        }
    }

    // A setting was just applied, which means the mouse is awake and talking —
    // a cheap moment for a fresh reading, pushed to clients as an event.
    poll_if_stale(state, event_tx);
}

/// Run `f`, and stringify any failure for the client.
fn run<T>(f: impl FnOnce() -> anyhow::Result<T>) -> Result<T, String> {
    f().map_err(|e| format!("{e:#}"))
}

/// Open the mouse for a write, after a status read made for this request (not the last
/// poll's): an asleep mouse drops whatever it's sent. On the owner's mouse (2026-11-13) a
/// sleep timer sent to it answered "ok" and never arrived. So nothing is sent to an asleep
/// mouse, and the client hears [`MOUSE_ASLEEP`].
fn awake_mouse(state: &mut State, event_tx: &broadcast::Sender<Event>) -> Result<Mouse, String> {
    let mouse = state.port.open()?;
    let battery = run(|| mouse.battery())?;
    take_battery(state, event_tx, battery);
    if battery.is_none() {
        return Err(MOUSE_ASLEEP.into());
    }
    Ok(mouse)
}

/// The answer for a setting read back after its write: an error naming `what` unless the
/// mouse reports what was sent.
fn took<T>(found: anyhow::Result<ReadBack<T>>, what: &str) -> Result<(), String> {
    match found.map_err(|e| format!("{e:#}"))? {
        ReadBack::Took => Ok(()),
        ReadBack::Other(_) => Err(format!("the mouse didn't take the {what}; try again")),
        ReadBack::Silent => Err(format!(
            "the mouse didn't answer after the change, so it isn't known whether it took the \
             {what}: move it, then look again"
        )),
    }
}

fn set_dpi(
    state: &mut State,
    event_tx: &broadcast::Sender<Event>,
    stages: &[DpiStage],
    active: usize,
    save: bool,
) -> Result<(), String> {
    let hw: Vec<gg_device_mouse::DpiStage> = stages
        .iter()
        .map(|s| gg_device_mouse::DpiStage { x: s.x, y: s.y })
        .collect();
    // Checked before anything goes out, the status read included.
    run(|| proto::sensitivity_packet(&hw, active))?;
    let check = state.power_on();
    let mouse = awake_mouse(state, event_tx)?;
    run(|| {
        mouse.set_dpi(&hw, active)?;
        if save {
            mouse.save()?;
        }
        Ok(())
    })?;
    let dpi = MouseDpi {
        stages: as_written(stages),
        active,
    };
    if check {
        let want: (Vec<gg_device_mouse::DpiStage>, usize) = (
            dpi.stages
                .iter()
                .map(|s| gg_device_mouse::DpiStage { x: s.x, y: s.y })
                .collect(),
            active,
        );
        took(mouse.read_back(&want, Mouse::dpi), "DPI stages")?;
        state.reported.dpi = Some(dpi.clone());
    }
    written(&mut state.applied).dpi = Some(dpi);
    Ok(())
}

fn set_polling(
    state: &mut State,
    event_tx: &broadcast::Sender<Event>,
    hz: u16,
    save: bool,
) -> Result<(), String> {
    let rate = PollingRate::from_hz(hz).map_err(|e| e.to_string())?;
    let check = state.power_on();
    let mouse = awake_mouse(state, event_tx)?;
    run(|| {
        mouse.set_polling_rate(rate)?;
        if save {
            mouse.save()?;
        }
        Ok(())
    })?;
    if check {
        took(
            mouse.read_back(&rate, Mouse::checked_polling_rate),
            "polling rate",
        )?;
        state.reported.polling_hz = Some(hz);
    }
    written(&mut state.applied).polling_hz = Some(hz);
    Ok(())
}

/// Buttons, and the wheel when eight actions came (see [`button_plan`]). Neither has a
/// read, so neither is read back.
fn set_buttons(
    state: &mut State,
    event_tx: &broadcast::Sender<Event>,
    actions: &[String],
    save: bool,
) -> Result<(), String> {
    let plan = button_plan(&state.gates.borrow(), actions)?;
    let mouse = awake_mouse(state, event_tx)?;
    run(|| {
        mouse.set_buttons(&plan.buttons)?;
        if let Some([up, down]) = plan.wheel {
            mouse.set_wheel(up, down)?;
        }
        if save {
            mouse.save()?;
        }
        Ok(())
    })?;
    let applied = written(&mut state.applied);
    applied.buttons = Some(recorded_buttons(
        applied.buttons.as_deref(),
        actions,
        plan.wheel.is_some(),
    ));
    Ok(())
}

fn set_sleep(
    state: &mut State,
    event_tx: &broadcast::Sender<Event>,
    minutes: u8,
) -> Result<(), String> {
    if let Some(why) = sleep_refusal(&state.gates.borrow(), minutes) {
        return Err(why);
    }
    let mouse = awake_mouse(state, event_tx)?;
    run(|| mouse.set_sleep_timer(minutes))?;
    let secs = u16::from(minutes) * 60;
    took(mouse.read_back(&secs, Mouse::sleep_timer), "sleep timer")?;
    written(&mut state.applied).sleep_minutes = Some(minutes);
    state.reported.sleep_secs = Some(secs);
    Ok(())
}

fn set_radio(
    state: &mut State,
    event_tx: &broadcast::Sender<Event>,
    radio: MouseRadio,
    save: bool,
) -> Result<(), String> {
    if let Some(why) = state.gates.borrow().refusal(Feature::MousePower) {
        return Err(why);
    }
    let mouse = awake_mouse(state, event_tx)?;
    run(|| {
        mouse.set_radio(to_device(radio))?;
        if save {
            mouse.save()?;
        }
        Ok(())
    })?;
    took(
        mouse.read_back(&to_device(radio), Mouse::checked_radio),
        "wireless mode",
    )?;
    written(&mut state.applied).radio = Some(radio);
    state.reported.radio = Some(radio);
    Ok(())
}

/// Read the battery, unless one was read very recently. See [`MIN_POLL_INTERVAL`].
fn poll_if_stale(state: &mut State, event_tx: &broadcast::Sender<Event>) {
    if is_older_than(state.last_poll, MIN_POLL_INTERVAL) {
        poll_battery(state, event_tx);
    }
}

/// Read the battery first, then (gate on, mouse awake) the mouse's own settings.
fn poll_battery(state: &mut State, event_tx: &broadcast::Sender<Event>) {
    // Stamped even if the read below fails, so a disconnected mouse is retried on
    // the normal schedule rather than on every request.
    state.last_poll = Some(Instant::now());
    let power = state.power_on();
    let reading = match state.port.open() {
        Ok(mouse) => {
            let battery = mouse.battery().unwrap_or_else(|e| {
                warn!(error = %format!("{e:#}"), "battery read failed");
                None
            });
            if battery.is_some() && power {
                read_reported(&mouse, &mut state.reported);
            }
            battery
        }
        // Not connected at all — report unknown rather than a stale value.
        Err(_) => None,
    };
    if !power {
        state.reported = Reported::default();
    }
    take_battery(state, event_tx, reading);
}

/// Take a fresh battery reading, from a poll or a write's status read, and tell clients
/// if it changed. No reading means the mouse is asleep (or gone): what it reported of its
/// own settings is dropped then, because it can't be checked until the mouse wakes, and a
/// client must not show it as current. The next poll after it wakes reads them again.
fn take_battery(
    state: &mut State,
    event_tx: &broadcast::Sender<Event>,
    reading: Option<gg_device_mouse::Battery>,
) {
    if reading.is_none() {
        state.reported = Reported::default();
    }
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

/// Ask the mouse for its own settings and take them into `reported` (see
/// [`Reported::take`]). The polling rate and radio mode are only asked for when the DPI
/// stages answered: without them they wouldn't be believed anyway.
fn read_reported(mouse: &Mouse, reported: &mut Reported) {
    let ask = || -> anyhow::Result<Answers> {
        let sleep_secs = mouse.sleep_timer()?;
        let Some(dpi) = mouse.dpi()? else {
            return Ok(Answers {
                sleep_secs,
                ..Answers::default()
            });
        };
        Ok(Answers {
            dpi: Some(dpi),
            polling: mouse.polling_rate()?,
            radio: mouse.radio()?,
            sleep_secs,
        })
    };
    match ask() {
        Ok(answers) => reported.take(answers),
        Err(e) => warn!(error = %format!("{e:#}"), "reading the mouse's settings failed"),
    }
}

/// Why a sleep timer of `minutes` isn't sent, or `None` if it may be.
fn sleep_refusal(gates: &Experimental, minutes: u8) -> Option<String> {
    gates.refusal(Feature::MousePower).or_else(|| {
        (!MOUSE_SLEEP_MINUTES.contains(&minutes)).then(|| {
            format!(
                "the sleep timer takes {} to {} minutes, not {minutes}",
                MOUSE_SLEEP_MINUTES.start(),
                MOUSE_SLEEP_MINUTES.end()
            )
        })
    })
}

/// What a buttons request sends.
#[derive(Debug, PartialEq)]
struct ButtonPlan {
    buttons: [ButtonAction; BUTTON_COUNT],
    /// The wheel's up and down; `None` sends no `1A` and leaves the wheel as it is.
    wheel: Option<[ButtonAction; 2]>,
}

/// Check a buttons request and plan what goes out. Six names set the buttons and leave
/// the wheel alone. Eight add the wheel's up and down: any button name, as the wheel takes
/// every action GG offers for it ([`proto::wheel_takes`]), and anything but its factory pair
/// needs [`Feature::MouseWheel`]. While that gate is off, eight names ending in the factory
/// pair send exactly what six do.
fn button_plan(gates: &Experimental, actions: &[String]) -> Result<ButtonPlan, String> {
    let (buttons, wheel) = match actions.len() {
        BUTTON_COUNT => (actions, None),
        n if n == BUTTON_COUNT + 2 => (&actions[..BUTTON_COUNT], Some(&actions[BUTTON_COUNT..])),
        n => {
            return Err(format!(
                "expected {BUTTON_COUNT} button actions, or {} with the wheel's two, got {n}",
                BUTTON_COUNT + 2
            ))
        }
    };
    let buttons = parse_actions(buttons)?;
    let Some(names) = wheel else {
        return Ok(ButtonPlan {
            buttons,
            wheel: None,
        });
    };
    let wheel = [wheel_action(&names[0])?, wheel_action(&names[1])?];
    let wheel = match gates.refusal(Feature::MouseWheel) {
        None => Some(wheel),
        Some(_) if wheel == DEFAULT_WHEEL => None,
        Some(why) => return Err(why),
    };
    Ok(ButtonPlan { buttons, wheel })
}

/// The action a wheel name stands for: any name a button takes ([`ButtonAction::parse`]),
/// as long as the wheel takes it too ([`proto::wheel_takes`], true for every parsed name).
fn wheel_action(name: &str) -> Result<ButtonAction, String> {
    let action = ButtonAction::parse(name).map_err(|e| {
        format!(
            "{e}: the wheel takes what a button takes (button1 to button6, dpi, scrollup, scrolldown, disable, a key or a media key)"
        )
    })?;
    if !proto::wheel_takes(action) {
        return Err(format!("the wheel doesn't take “{}”", name.trim()));
    }
    Ok(action)
}

/// The buttons as recorded after a write: the names sent, and the wheel's two from the
/// record before when the wheel was left alone.
fn recorded_buttons(
    before: Option<&[String]>,
    actions: &[String],
    wheel_sent: bool,
) -> Vec<String> {
    let mut names = actions[..BUTTON_COUNT].to_vec();
    if wheel_sent {
        names.extend_from_slice(&actions[BUTTON_COUNT..]);
    } else if let Some(old) = before.filter(|b| b.len() == BUTTON_COUNT + 2) {
        names.extend_from_slice(&old[BUTTON_COUNT..]);
    }
    names
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

/// The stages as the mouse runs them, which is what gets recorded: the write
/// clamps and rounds each DPI to a sensor stage, whatever the client sent.
fn as_written(stages: &[DpiStage]) -> Vec<DpiStage> {
    stages
        .iter()
        .map(|s| DpiStage {
            x: nearest_dpi(s.x),
            y: nearest_dpi(s.y),
        })
        .collect()
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
    use gg_device_mouse::testing::FakeMouse;

    /// The worker's way to the fake mouse.
    struct FakePort(FakeMouse);

    impl Port for FakePort {
        fn open(&mut self) -> Result<Mouse, String> {
            Ok(self.0.mouse())
        }

        fn connected(&self) -> bool {
            true
        }
    }

    /// A worker's state on a fake mouse, with `gates` on.
    struct Bench {
        state: State,
        fake: FakeMouse,
        events: broadcast::Sender<Event>,
        _gates: watch::Sender<Experimental>,
    }

    impl Bench {
        fn new(gates: &[Feature]) -> Bench {
            let mut on = Experimental::default();
            for &f in gates {
                on.set(f, true);
            }
            let (gates_tx, gates_rx) = watch::channel(on);
            let fake = FakeMouse::new();
            let mut state = State::new(Box::new(FakePort(fake.clone())), gates_rx);
            // Just polled: no poll of its own follows a command unless a test asks.
            state.last_poll = Some(Instant::now());
            Bench {
                state,
                fake,
                events: broadcast::channel(16).0,
                _gates: gates_tx,
            }
        }

        fn ask<T>(&mut self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
            let (tx, mut rx) = oneshot::channel();
            handle(make(tx), &mut self.state, &self.events);
            rx.try_recv().expect("answered at once")
        }

        fn buttons(&mut self, names: &[&str], save: bool) -> Result<(), String> {
            let actions = names.iter().map(|n| n.to_string()).collect();
            self.ask(|reply| Command::SetButtons {
                actions,
                save,
                reply,
            })
        }

        /// A poll, as the timer or a client's read would make it.
        fn poll(&mut self) -> MouseState {
            self.state.last_poll = None;
            self.ask(Command::GetState).unwrap()
        }
    }

    const SIX: [&str; 6] = ["button1", "button2", "button3", "button4", "button5", "dpi"];

    fn with_wheel<'a>(up: &'a str, down: &'a str) -> Vec<&'a str> {
        let mut names = SIX.to_vec();
        names.extend([up, down]);
        names
    }

    type Write = Box<dyn FnOnce(&mut Bench) -> Result<(), String>>;

    /// Every write the daemon makes, by name, each as a fresh request.
    fn every_write() -> Vec<(&'static str, Write)> {
        vec![
            (
                "dpi",
                Box::new(|b: &mut Bench| {
                    b.ask(|reply| Command::SetDpi {
                        stages: vec![DpiStage { x: 800, y: 800 }],
                        active: 0,
                        save: true,
                        reply,
                    })
                }),
            ),
            (
                "polling",
                Box::new(|b: &mut Bench| {
                    b.ask(|reply| Command::SetPolling {
                        hz: 500,
                        save: true,
                        reply,
                    })
                }),
            ),
            (
                "buttons",
                Box::new(|b: &mut Bench| b.buttons(&with_wheel("button4", "scrolldown"), true)),
            ),
            ("save", Box::new(|b: &mut Bench| b.ask(Command::Save))),
            (
                "sleep",
                Box::new(|b: &mut Bench| b.ask(|reply| Command::SetSleep { minutes: 5, reply })),
            ),
            (
                "radio",
                Box::new(|b: &mut Bench| {
                    b.ask(|reply| Command::SetRadio {
                        radio: MouseRadio::Stable,
                        save: true,
                        reply,
                    })
                }),
            ),
        ]
    }

    /// Found on the owner's mouse (2026-11-13): a sleep timer sent to it asleep answered
    /// "ok" and never arrived. Now only the status read goes out, and the answer says why.
    #[test]
    fn an_asleep_mouse_is_sent_nothing_but_the_status_read() {
        for (name, write) in every_write() {
            let mut b = Bench::new(&[Feature::MousePower, Feature::MouseWheel]);
            b.fake.set_asleep(true);
            assert_eq!(write(&mut b), Err(MOUSE_ASLEEP.to_string()), "{name}");
            assert_eq!(b.fake.commands(), [0xAA], "{name}: only the battery query");
            assert_eq!(b.state.applied, None, "{name}: nothing recorded as applied");
            assert_eq!(b.fake.saves(), 0, "{name}");
        }
    }

    #[test]
    fn an_awake_mouse_gets_the_write_and_it_is_read_back() {
        let mut b = Bench::new(&[Feature::MousePower]);
        b.ask(|reply| Command::SetPolling {
            hz: 500,
            save: true,
            reply,
        })
        .unwrap();
        // The status read, the rate, the save; then the read-back, asked twice because the
        // first answer after a change can still be the old one.
        assert_eq!(
            b.fake.commands(),
            [0xAA, 0x17, 0x09, 0xAC, 0x97, 0xAC, 0x97]
        );
        assert_eq!(b.fake.settings().polling, PollingRate::Hz500);
        assert_eq!(b.state.reported.polling_hz, Some(500));
        assert_eq!(b.state.applied.as_ref().unwrap().polling_hz, Some(500));

        b.fake.clear_sent();
        b.ask(|reply| Command::SetDpi {
            stages: vec![DpiStage { x: 500, y: 400 }, DpiStage { x: 1550, y: 1550 }],
            active: 1,
            save: false,
            reply,
        })
        .unwrap();
        assert_eq!(b.fake.commands(), [0xAA, 0x2C, 0xAC, 0xAC]);
        let dpi = b.state.reported.dpi.clone().unwrap();
        assert_eq!(
            (dpi.stages[1], dpi.active),
            (DpiStage { x: 1600, y: 1600 }, 1)
        );

        // With the reads gated off, the write goes out as it always did, unread.
        let mut off = Bench::new(&[]);
        off.ask(|reply| Command::SetPolling {
            hz: 250,
            save: true,
            reply,
        })
        .unwrap();
        assert_eq!(off.fake.commands(), [0xAA, 0x17, 0x09]);
    }

    /// A mouse that drops the command keeps reporting the old value: the answer names the
    /// setting, and it isn't recorded as applied.
    #[test]
    fn a_dropped_command_is_an_error_naming_the_setting() {
        let dropped = |name| match name {
            "dpi" => Some((proto::CMD_SENSITIVITY, "DPI stages")),
            "polling" => Some((proto::CMD_POLLING_RATE, "polling rate")),
            "sleep" => Some((proto::CMD_SLEEP_TIMER, "sleep timer")),
            "radio" => Some((proto::CMD_RADIO, "wireless mode")),
            _ => None,
        };
        let mut checked = 0;
        for (name, write) in every_write() {
            let Some((command, what)) = dropped(name) else {
                continue;
            };
            let mut b = Bench::new(&[Feature::MousePower]);
            b.fake.drop_next(command);
            let why = write(&mut b).unwrap_err();
            assert_eq!(why, format!("the mouse didn't take the {what}; try again"));
            assert_eq!(b.state.applied, None, "{what}");
            checked += 1;
        }
        assert_eq!(checked, 4);
    }

    /// No answer to the read-back: not called done, and not called refused either.
    #[test]
    fn no_answer_after_a_write_says_it_isnt_known() {
        let why = took::<u8>(Ok(ReadBack::Silent), "wireless mode").unwrap_err();
        assert!(
            why.contains("isn't known whether it took the wireless mode"),
            "{why}"
        );
        assert_eq!(took::<u8>(Ok(ReadBack::Took), "wireless mode"), Ok(()));
    }

    /// With the wheel's gate off, six buttons and eight with the factory wheel are the same
    /// request as before the wheel existed: byte for byte, and no `1A`.
    #[test]
    fn with_the_gate_off_the_factory_wheel_sends_what_six_buttons_do() {
        let mut six = Bench::new(&[]);
        six.buttons(&SIX, true).unwrap();
        let mut eight = Bench::new(&[]);
        eight
            .buttons(&with_wheel("scrollup", "scrolldown"), true)
            .unwrap();
        assert_eq!(six.fake.sent(), eight.fake.sent());
        assert_eq!(six.fake.commands(), [0xAA, 0x19, 0x09]);
        assert_eq!(eight.fake.settings().wheel, None, "no 1A");
        let applied = eight.state.applied.unwrap().buttons.unwrap();
        assert_eq!(
            applied, SIX,
            "the wheel wasn't written, so it isn't recorded"
        );
    }

    #[test]
    fn the_wheel_goes_after_the_buttons_and_before_the_save() {
        let mut b = Bench::new(&[Feature::MouseWheel]);
        b.buttons(&with_wheel("button4", "scrolldown"), true)
            .unwrap();
        assert_eq!(b.fake.commands(), [0xAA, 0x19, 0x1A, 0x09]);
        assert_eq!(
            b.fake.sent()[2][..11],
            [0x1A, 0x04, 0, 0, 0, 0, 0x32, 0, 0, 0, 0]
        );
        assert_eq!(
            b.state
                .applied
                .as_ref()
                .unwrap()
                .buttons
                .as_deref()
                .unwrap(),
            with_wheel("button4", "scrolldown")
        );
        // Six buttons leave the wheel alone, and its record with it.
        b.fake.clear_sent();
        let mut swapped = SIX;
        swapped.swap(0, 1);
        b.buttons(&swapped, false).unwrap();
        assert_eq!(b.fake.commands(), [0xAA, 0x19]);
        let recorded = b.state.applied.as_ref().unwrap().buttons.clone().unwrap();
        assert_eq!(recorded[..2], ["button2", "button1"]);
        assert_eq!(recorded[6..], ["button4", "scrolldown"]);
        // With the gate on, the factory wheel goes out too: that's how it's put back.
        b.fake.clear_sent();
        b.buttons(&with_wheel("scrollup", "scrolldown"), false)
            .unwrap();
        assert_eq!(b.fake.commands(), [0xAA, 0x19, 0x1A]);
        assert_eq!(b.fake.settings().wheel, Some(vec![0x31, 0, 0, 0, 0, 0x32]));
    }

    #[test]
    fn a_wheel_other_than_the_factory_one_is_refused_with_its_gate_off() {
        let mut b = Bench::new(&[Feature::MousePower]);
        let why = b
            .buttons(&with_wheel("button4", "scrolldown"), false)
            .unwrap_err();
        assert!(
            why.contains("scroll wheel") && why.contains("Settings → Experimental"),
            "{why}"
        );
        assert!(b.fake.sent().is_empty(), "refused before anything went out");
    }

    /// Every action a button takes goes on the wheel too (captures rival-5, rival-8 to
    /// rival-10), as its buttons-table entry, in either direction.
    #[test]
    fn the_wheel_takes_every_action_a_button_takes() {
        for (name, entry) in [
            ("button1", [0x01, 0]),
            ("button6", [0x06, 0]),
            ("dpi", [0x30, 0]),
            ("disable", [0x00, 0]),
            ("scrolldown", [0x32, 0]),
            ("a", [0x51, 0x04]),
            ("volumeup", [0x61, 0xE9]),
        ] {
            let mut b = Bench::new(&[Feature::MouseWheel]);
            b.buttons(&with_wheel(name, "scrolldown"), false).unwrap();
            let sent = &b.fake.sent()[2];
            assert_eq!(sent[..3], [0x1A, entry[0], entry[1]], "{name} up");
            b.fake.clear_sent();
            b.buttons(&with_wheel("scrollup", name), false).unwrap();
            let sent = &b.fake.sent()[2];
            assert_eq!(sent[6..8], entry, "{name} down");
        }
    }

    #[test]
    fn a_wheel_name_no_button_takes_is_refused_before_anything_is_sent() {
        for name in ["button7", "nonsense", ""] {
            let mut b = Bench::new(&[Feature::MouseWheel]);
            let why = b
                .buttons(&with_wheel(name, "scrolldown"), false)
                .unwrap_err();
            assert!(why.contains("the wheel takes what a button takes"), "{name}: {why}");
            assert!(b.buttons(&with_wheel("scrollup", name), false).is_err());
            assert!(b.fake.sent().is_empty(), "{name}");
        }
        let mut b = Bench::new(&[Feature::MouseWheel]);
        let seven = with_wheel("scrollup", "scrolldown")[..7].to_vec();
        assert!(b.buttons(&seven, false).unwrap_err().contains("or 8 with"));
    }

    /// The state never shows what an asleep mouse said before it fell asleep as current.
    #[test]
    fn an_asleep_mouse_has_no_settings_shown_until_it_wakes() {
        let mut b = Bench::new(&[Feature::MousePower]);
        let mut events = b.events.subscribe();
        let awake = b.poll();
        assert_eq!(awake.sleep_secs, Some(1200));
        assert_eq!(awake.reported_polling_hz, Some(1000));
        assert!(awake.battery.is_some() && awake.radio.is_some() && awake.reported_dpi.is_some());

        b.fake.set_asleep(true);
        let asleep = b.poll();
        assert_eq!(
            (asleep.battery, asleep.sleep_secs, asleep.radio),
            (None, None, None)
        );
        assert_eq!(
            (asleep.reported_dpi, asleep.reported_polling_hz),
            (None, None)
        );
        let told = std::iter::from_fn(|| events.try_recv().ok()).last();
        assert!(matches!(told, Some(Event::MouseBattery { battery: None })));

        b.fake.set_asleep(false);
        assert_eq!(b.poll().sleep_secs, Some(1200), "read again once awake");
    }

    /// A write that finds the mouse asleep drops its old readings too, so `ggctl mouse`
    /// right after it doesn't show them, even before the next poll.
    #[test]
    fn a_write_that_finds_the_mouse_asleep_drops_its_old_readings() {
        let mut b = Bench::new(&[Feature::MousePower]);
        assert_eq!(b.poll().sleep_secs, Some(1200));
        b.fake.set_asleep(true);
        b.ask(|reply| Command::SetSleep { minutes: 20, reply })
            .unwrap_err();
        let now = b.ask(Command::GetState).unwrap();
        assert_eq!((now.battery, now.sleep_secs), (None, None));
    }

    /// The app and ggctl offer the button names for the wheel (the actions menu and the
    /// key and media lists of `gg_ipc::mouse_keys`): each must be one the wheel takes, and
    /// the factory pair must be the mouse's.
    #[test]
    fn every_name_the_app_offers_a_button_goes_on_the_wheel() {
        let actions = [
            "button1",
            "button2",
            "button3",
            "button4",
            "button5",
            "button6",
            "dpi",
            "scrollup",
            "scrolldown",
            "disable",
        ];
        let lists = gg_ipc::mouse_keys::KEYS
            .iter()
            .chain(gg_ipc::mouse_keys::MEDIA)
            .map(|(name, _)| *name);
        for name in actions.into_iter().chain(lists) {
            assert!(wheel_action(name).is_ok(), "{name}");
        }
        let factory = gg_ipc::mouse_keys::WHEEL_FACTORY.map(|n| wheel_action(n).unwrap());
        assert_eq!(factory, DEFAULT_WHEEL);
    }

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
        assert!(
            claimed.is_empty(),
            "reported as applied but never written: {claimed:?} in {json}"
        );
    }

    #[test]
    fn the_sleep_timer_is_refused_with_its_gate_off_or_outside_ggs_range() {
        let mut gates = Experimental::default();
        let why = sleep_refusal(&gates, 20).unwrap();
        assert!(why.contains("Settings → Experimental"), "{why}");
        gates.set(Feature::MousePower, true);
        assert_eq!(sleep_refusal(&gates, 20), None);
        assert_eq!(sleep_refusal(&gates, 1), None);
        for minutes in [0, 21] {
            let why = sleep_refusal(&gates, minutes).unwrap();
            assert!(why.contains("1 to 20 minutes"), "{minutes}: {why}");
        }
    }

    #[test]
    fn a_mouse_that_isnt_answering_isnt_read_as_1000_hz_high_efficiency() {
        use gg_device_mouse::DpiStage as Hw;
        let mut r = Reported::default();
        // All zeros decode as 1000 Hz and High-Efficiency Mode, with no DPI stages.
        r.take(Answers {
            polling: Some(PollingRate::Hz1000),
            radio: Some(Radio::HighEfficiency),
            ..Answers::default()
        });
        assert_eq!(r, Reported::default());
        // A mouse that answers: everything is taken.
        r.take(Answers {
            dpi: Some((vec![Hw::uniform(400), Hw { x: 800, y: 1600 }], 1)),
            polling: Some(PollingRate::Hz125),
            radio: Some(Radio::Stable),
            sleep_secs: Some(1200),
        });
        let awake = r.clone();
        assert_eq!(
            (r.polling_hz, r.radio, r.sleep_secs),
            (Some(125), Some(MouseRadio::Stable), Some(1200))
        );
        let dpi = r.dpi.clone().unwrap();
        assert_eq!(
            (dpi.stages[1], dpi.active),
            (DpiStage { x: 800, y: 1600 }, 1)
        );
        // Zeros again (an answer missed while awake): the readings it gave stand. An
        // asleep mouse's are dropped before this, by `take_battery`.
        r.take(Answers {
            polling: Some(PollingRate::Hz1000),
            radio: Some(Radio::HighEfficiency),
            ..Answers::default()
        });
        assert_eq!(r, awake);
        for radio in MouseRadio::ALL {
            assert_eq!(from_device(to_device(radio)), radio);
        }
    }

    #[test]
    fn the_dpi_recorded_is_the_dpi_the_mouse_runs() {
        use gg_device_mouse::tables::{DPI_MAX, DPI_MIN};
        // As `ggctl mouse dpi 1550,30000:40` sends it, unrounded and out of range.
        let asked = [DpiStage { x: 1550, y: 1550 }, DpiStage { x: 30000, y: 40 }];
        assert_eq!(
            as_written(&asked),
            [
                DpiStage { x: 1600, y: 1600 },
                DpiStage {
                    x: DPI_MAX,
                    y: DPI_MIN
                }
            ]
        );
    }

    /// The app offers `gg_ipc::mouse_keys` in its pickers: every name there must be one the
    /// mouse code takes, as the right kind, and every key the mouse code knows must be offered.
    #[test]
    fn the_apps_key_names_are_the_mouse_codes_own() {
        use gg_device_mouse::tables;
        let mut offered = std::collections::BTreeSet::new();
        for (name, _) in gg_ipc::mouse_keys::KEYS {
            match ButtonAction::parse(name) {
                Ok(ButtonAction::Key(code)) => {
                    offered.insert(code);
                }
                other => panic!("{name}: {other:?}"),
            }
        }
        for (name, _) in gg_ipc::mouse_keys::MEDIA {
            assert!(
                matches!(ButtonAction::parse(name), Ok(ButtonAction::Multimedia(_))),
                "{name}"
            );
        }
        let known: std::collections::BTreeSet<u8> =
            tables::KEYBOARD.iter().map(|(_, c)| *c).collect();
        assert_eq!(offered, known);
    }
}
