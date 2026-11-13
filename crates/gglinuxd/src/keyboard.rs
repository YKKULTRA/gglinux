// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard worker: the Apex Pro TKL Gen 3's lighting, screen and switches.
//!
//! One OS thread owns the device, like the other workers. With gglinux lighting
//! on it sends frames — 30 a second for animated effects, on change (and a slow
//! refresh) for static ones; with it off, or when the daemon exits, it hands the
//! lighting back to the keyboard's onboard profile. The screen works the same way:
//! a picture on change (each second for a clock), handed back when it's set to the
//! keyboard's own or the daemon exits. A picture of gglinux's moves a pixel every
//! couple of minutes and goes dark while nobody is at the computer (screen care),
//! so the OLED doesn't burn in. The switch settings live in the keyboard's
//! RAM, so they're sent once each time it connects. All off by default: a fresh
//! install never touches the keyboard.
//!
//! The board drawn follows the keyboard's region (US, ISO or JIS). It's asked once a
//! connection, the first time gglinux takes the keyboard anyway ([`read_region`]), so
//! a fresh install still sends nothing; the US board is drawn until then.
//!
//! This file keeps the loop, the lighting, opening and closing the keyboard and the
//! routing of [`Command`]s. The screen is [`screen`] (with its [`screen::ScreenSource`]s)
//! and the switches [`switches`]. Key bindings and the onboard slots each have a module
//! their unit fills in without touching this one: [`bindings`] and [`onboard`], each a
//! `Store` the worker holds, an `on_command` for its `Cmd`s, an `on_connect` run once the
//! keyboard has opened and had its switches again, and a `handle` for `main`'s requests.
//! The open keyboard lets a gated command family out only while its experimental gate
//! is on ([`Command::SetExperimental`]).

mod audio;
pub mod bindings;
mod effects;
pub mod macros;
mod mic;
pub mod onboard;
mod reader;
mod screen;
mod spectrum;
mod switches;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gg_device_keyboard::layout::{for_region, Key, REGIONS};
use gg_device_keyboard::proto::ANALOG_KEYS;
use gg_device_keyboard::{Families, Keyboard, Rgb};
use gg_ipc::{
    Effect, Event, Experimental, KeyInfo, KeyboardState, Lighting, MicMuteLight, ReactiveStatus,
    Screen, ScreenCare, Switches,
};
use hidapi::HidApi;
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, error, info, warn};

use crate::clock::is_older_than;
use audio::{Capture, Levels};
use reader::{OpenError, Reader};
use screen::ScreenState;
use spectrum::BANDS;
use switches::SwitchesState;

const FRAME: Duration = Duration::from_millis(33);
const STATIC_REFRESH: Duration = Duration::from_secs(2);
const REOPEN_INTERVAL: Duration = Duration::from_secs(2);
const IDLE_WAIT: Duration = Duration::from_millis(500);
/// The most key presses held for reactive lighting; older ones are dropped first.
const MAX_HITS: usize = 32;
const MIC_POLL: Duration = Duration::from_millis(500);

/// Colours by HID code, drawn over the lighting.
pub type Layer = HashMap<u8, Rgb>;

/// Something that paints over the lighting while it's active: GameSense now,
/// built-in effects later. Asked once per loop, so keep `paint` cheap.
pub trait Overlay: Send + Sync {
    /// The keys it paints at `now`, or `None` while inactive.
    fn paint(&self, now: Instant) -> Option<Layer>;
}

type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum Command {
    GetState(Reply<KeyboardState>),
    SetLighting {
        lighting: Lighting,
        reply: Reply<()>,
    },
    SetSwitches {
        switches: Switches,
        reply: Reply<()>,
    },
    SetScreen {
        screen: Screen,
        reply: Reply<()>,
    },
    SetScreenCare {
        care: ScreenCare,
        reply: Reply<()>,
    },
    /// Whether anybody is at the computer, from [`crate::presence`]. Replies once the
    /// screen shows it, so the computer can be let go to sleep after.
    SetAway {
        away: bool,
        reply: Reply<()>,
    },
    /// A key went down on the Apex, as the reader saw it: the code of its LED.
    /// Never logged.
    Pressed {
        code: u8,
        at: Instant,
    },
    /// The reader ended by itself (keyboard gone, read error); it retries later.
    ReaderStopped,
    /// Something an overlay shows changed: draw soon. No reply.
    Wake,
    /// The audio capture's program ended by itself; retried later.
    AudioStopped,
    /// The default microphone was muted or unmuted.
    MicMuted(bool),
    /// The visualizer should listen to this output (a `node.name`), or to the
    /// default output when `None`. From Sonar; no reply.
    ListenTo(Option<String>),
    /// For the key bindings: see [`bindings`].
    Bindings(bindings::Cmd),
    /// For the onboard profile slots: see [`onboard`].
    Onboard(onboard::Cmd),
    /// The experimental gates as they are now. The open keyboard, and every keyboard
    /// opened from now on, lets a gated command family out only while its gate is on.
    /// No reply.
    SetExperimental(Experimental),
    /// Hand the lighting back and stop. Replies once the keyboard has been told.
    Shutdown(Reply<()>),
    /// Send a feature report through the open keyboard's allowlist and read the answer.
    #[cfg(test)]
    Exchange {
        report: Vec<u8>,
        reply: Reply<Vec<u8>>,
    },
}

/// Names only: a `Pressed` never prints its key.
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Command::GetState(_) => "GetState",
            Command::SetLighting { .. } => "SetLighting",
            Command::SetSwitches { .. } => "SetSwitches",
            Command::SetScreen { .. } => "SetScreen",
            Command::SetScreenCare { .. } => "SetScreenCare",
            Command::SetAway { .. } => "SetAway",
            Command::Pressed { .. } => "Pressed",
            Command::ReaderStopped => "ReaderStopped",
            Command::Wake => "Wake",
            Command::AudioStopped => "AudioStopped",
            Command::MicMuted(_) => "MicMuted",
            Command::ListenTo(_) => "ListenTo",
            Command::Bindings(_) => "Bindings",
            Command::Onboard(_) => "Onboard",
            Command::SetExperimental(_) => "SetExperimental",
            Command::Shutdown(_) => "Shutdown",
            #[cfg(test)]
            Command::Exchange { .. } => "Exchange",
        })
    }
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
    /// Slot writes asked for and not ended yet, shared by every clone: `main` waits for
    /// them before it stops the daemon.
    slot_writes: onboard::Writes,
}

impl Handle {
    /// A handle with no worker, for when the keyboard part is off: every send fails, and
    /// a wake or a `listen_to` from another part goes nowhere.
    pub fn off() -> Handle {
        let (tx, _) = mpsc::channel();
        Handle::on(tx)
    }

    fn on(tx: Sender<Command>) -> Handle {
        Handle {
            tx,
            slot_writes: onboard::Writes::default(),
        }
    }

    /// The onboard slot writes asked for and not ended yet.
    pub fn slot_writes(&self) -> &onboard::Writes {
        &self.slot_writes
    }

    pub async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| "keyboard worker is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "keyboard worker dropped the request".to_string())?
    }

    /// Ask for a prompt frame; fire and forget.
    pub fn wake(&self) {
        let _ = self.tx.send(Command::Wake);
    }

    /// Point the visualizer at the output everything playing mixes into
    /// (`None`: the default output); fire and forget.
    pub fn listen_to(&self, output: Option<String>) {
        let _ = self.tx.send(Command::ListenTo(output));
    }

    /// Tell the worker the experimental gates changed; fire and forget. Commands sent
    /// after this one find them changed.
    pub fn set_experimental(&self, experimental: Experimental) {
        let _ = self.tx.send(Command::SetExperimental(experimental));
    }
}

/// Start the keyboard worker, with the experimental gates as they are at start.
pub fn spawn(
    event_tx: broadcast::Sender<Event>,
    overlays: Vec<Arc<dyn Overlay>>,
    experimental: Experimental,
) -> Handle {
    let (tx, rx) = mpsc::channel();
    let readers_tx = tx.clone();
    std::thread::Builder::new()
        .name("gg-keyboard".into())
        .spawn(move || worker(rx, readers_tx, event_tx, overlays, experimental))
        .expect("spawn gg-keyboard thread");
    Handle::on(tx)
}

/// A worker on `device`, a keyboard over a fake transport, open from the start, with its
/// settings in `config_dir` and no device list: it never opens anything else. The
/// keyboard counts as asked its region already, with no answer (the US board), so a test
/// sees only the reports of what it tests; [`spawn_fake_unasked`] opens it as a real one
/// opens.
#[cfg(test)]
pub fn spawn_fake(
    device: Keyboard,
    config_dir: PathBuf,
    event_tx: broadcast::Sender<Event>,
    experimental: Experimental,
) -> Handle {
    spawn_fake_on(device, config_dir, event_tx, experimental, Some(None))
}

/// [`spawn_fake`] on a keyboard not asked its region yet, as every real one opens: it's
/// asked the first time gglinux takes it.
#[cfg(test)]
pub fn spawn_fake_unasked(
    device: Keyboard,
    config_dir: PathBuf,
    event_tx: broadcast::Sender<Event>,
    experimental: Experimental,
) -> Handle {
    spawn_fake_on(device, config_dir, event_tx, experimental, None)
}

#[cfg(test)]
fn spawn_fake_on(
    device: Keyboard,
    config_dir: PathBuf,
    event_tx: broadcast::Sender<Event>,
    experimental: Experimental,
    region: Option<Option<u8>>,
) -> Handle {
    let (tx, rx) = mpsc::channel();
    let worker_tx = tx.clone();
    std::thread::Builder::new()
        .name("gg-keyboard-fake".into())
        .spawn(move || {
            let mut state = State::new(None, &config_dir, worker_tx, Vec::new(), experimental);
            connected(&mut state, device);
            state.region = region;
            run(state, &rx, &event_tx);
        })
        .expect("spawn gg-keyboard-fake thread");
    Handle::on(tx)
}

struct State {
    /// The device list; `None` in tests, which open nothing.
    api: Option<HidApi>,
    device: Option<Keyboard>,
    /// Whether this handle has taken the lighting over (init sent).
    taken: bool,
    /// When the device list was last read: to open the keyboard, or to see
    /// that the open one is still there.
    last_open_attempt: Option<Instant>,
    last_frame: Option<Instant>,
    lighting: Lighting,
    settings_path: PathBuf,
    positions: HashMap<u8, effects::Pos>,
    started: Instant,
    published: Option<KeyboardState>,
    /// Hands key presses to this worker; cloned into each reader.
    tx: Sender<Command>,
    /// Reading key presses, while reactive lighting wants them.
    reader: Option<Reader>,
    last_reader_attempt: Option<Instant>,
    reactive: ReactiveStatus,
    /// Recent presses, newest last; never logged, never saved.
    hits: Vec<effects::Hit>,
    /// Painters over the lighting, and what they painted this loop.
    overlays: Vec<Arc<dyn Overlay>>,
    layers: Vec<Layer>,
    /// Whether any overlay painted on the last pass, to tell when they stop.
    overlay_was_active: bool,
    /// Visualizer band levels, written by the capture thread.
    levels: Levels,
    capture: Option<Capture>,
    last_capture_attempt: Option<Instant>,
    /// The output the capture listens to; `None` is the default output.
    listen_to: Option<String>,
    mic_watch: Option<mic::Watch>,
    mic_muted: bool,
    switches: SwitchesState,
    screen: ScreenState,
    bindings: bindings::Store,
    onboard: onboard::Store,
    /// The experimental gates, as last told; the open keyboard's families follow them.
    experimental: Experimental,
    /// The keyboard opened since the last loop: `on_connect` is due once its switches
    /// have gone out again.
    just_opened: bool,
    /// The open keyboard's region byte, once it's been asked on this connection
    /// (`Some(None)`: it didn't say). See [`read_region`].
    region: Option<Option<u8>>,
}

impl State {
    fn new(
        api: Option<HidApi>,
        config_dir: &Path,
        tx: Sender<Command>,
        overlays: Vec<Arc<dyn Overlay>>,
        experimental: Experimental,
    ) -> State {
        let settings_path = config_dir.join("lighting.json");
        State {
            api,
            device: None,
            taken: false,
            last_open_attempt: None,
            last_frame: None,
            lighting: on_board(load(&settings_path, "lighting"), None),
            settings_path,
            positions: effects::positions(board(None)),
            started: Instant::now(),
            published: None,
            tx,
            reader: None,
            last_reader_attempt: None,
            reactive: ReactiveStatus::Off,
            hits: Vec::new(),
            overlays,
            layers: Vec::new(),
            overlay_was_active: false,
            levels: Arc::new(Mutex::new([0.0; BANDS])),
            capture: None,
            last_capture_attempt: None,
            listen_to: None,
            mic_watch: None,
            mic_muted: false,
            switches: SwitchesState::load(config_dir),
            screen: ScreenState::load(config_dir),
            bindings: bindings::Store::load(config_dir),
            onboard: onboard::Store::load(config_dir),
            experimental,
            just_opened: false,
            region: None,
        }
    }
}

fn worker(
    rx: Receiver<Command>,
    tx: Sender<Command>,
    event_tx: broadcast::Sender<Event>,
    overlays: Vec<Arc<dyn Overlay>>,
    experimental: Experimental,
) {
    let hid = gg_hid::lock();
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            error!(error = %e, "keyboard worker could not initialise hidapi");
            return;
        }
    };
    drop(hid);
    let state = State::new(Some(api), &gg_ipc::config_dir(), tx, overlays, experimental);
    run(state, &rx, &event_tx);
}

fn run(mut state: State, rx: &Receiver<Command>, event_tx: &broadcast::Sender<Event>) {
    loop {
        match rx.recv_timeout(next_wait(&state)) {
            Ok(command) => {
                if !handle(command, &mut state, event_tx) {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Unreachable while `state.tx` lives — the worker stops on Shutdown,
            // which main sends on exit. Kept as a fallback.
            Err(RecvTimeoutError::Disconnected) => {
                hand_back(&mut state);
                state.screen.release(state.device.as_ref());
                return;
            }
        }
        if state.device.is_none() {
            try_open(&mut state);
        }
        let now = Instant::now();
        state.layers = state.overlays.iter().filter_map(|o| o.paint(now)).collect();
        let overlay_active = !state.layers.is_empty();
        if overlay_ended(state.overlay_was_active, overlay_active) {
            state.last_frame = None;
        }
        state.overlay_was_active = overlay_active;
        if should_look_for(
            state.device.is_some(),
            drawing(state.lighting.enabled, !state.layers.is_empty()),
        ) {
            look_for(&mut state);
        }
        sync_reader(&mut state);
        sync_audio(&mut state);
        sync_mic(&mut state);
        if should_hand_back(
            state.taken,
            state.lighting.enabled,
            !state.layers.is_empty(),
        ) {
            hand_back(&mut state);
        }
        // Before anything below sends: lighting, switches, bindings, the screen.
        read_region(&mut state, event_tx);
        render(&mut state);
        state.switches.sync(state.device.as_ref());
        // A deep-press binding sticks only on a key with a point: the points go first.
        state.bindings.set_deep_keys(state.switches.points());
        let points_sent = state.switches.take_points_sent();
        if std::mem::take(&mut state.just_opened) {
            if let Some(device) = state.device.as_ref() {
                let exp = &state.experimental;
                bindings::on_connect(&mut state.bindings, device, exp, event_tx);
                onboard::on_connect(&mut state.onboard, device, exp, event_tx);
            }
        } else if points_sent {
            if let Some(device) = state.device.as_ref() {
                bindings::on_points_sent(
                    &mut state.bindings,
                    device,
                    &state.experimental,
                    event_tx,
                );
            }
        }
        state.screen.render(state.device.as_ref());
        publish(&mut state, event_tx);
    }
}

/// How often frames are due: fast for animated effects, while presses show,
/// and while an overlay draws.
fn interval(lighting: &Lighting, live_hits: bool, overlay_active: bool) -> Duration {
    if effects::animated(&lighting.effect) || live_hits || overlay_active {
        FRAME
    } else {
        STATIC_REFRESH
    }
}

/// How long to wait for a command before the next frame is due.
fn next_wait(state: &State) -> Duration {
    let lighting =
        if state.device.is_none() || !drawing(state.lighting.enabled, !state.layers.is_empty()) {
            IDLE_WAIT
        } else {
            state.last_frame.map_or(Duration::ZERO, |t| {
                interval(
                    &state.lighting,
                    !state.hits.is_empty(),
                    !state.layers.is_empty(),
                )
                .saturating_sub(t.elapsed())
            })
        };
    // A screen that changes by itself (a clock) wakes the loop in time.
    match state.screen.wait(state.device.is_some()) {
        Some(screen) => lighting.min(screen),
        None => lighting,
    }
}

/// Whether key presses should be read right now.
fn wants_reader(lighting: &Lighting, connected: bool) -> bool {
    lighting.enabled && lighting.reactive.enabled && connected
}

/// Whether gglinux draws on the keyboard: its own lighting, or an overlay taking it over.
fn drawing(lighting_enabled: bool, overlay_active: bool) -> bool {
    lighting_enabled || overlay_active
}

/// Whether to look for the open keyboard in the device list: with nothing
/// drawn nothing is written, so no failed write tells that it went.
fn should_look_for(open: bool, drawing: bool) -> bool {
    open && !drawing
}

/// Give the keyboard back: gglinux holds it, but nothing wants to draw any more.
fn should_hand_back(taken: bool, lighting_enabled: bool, overlay_active: bool) -> bool {
    taken && !drawing(lighting_enabled, overlay_active)
}

/// The colour overlays give this key: the last active overlay that paints it.
fn overlay_color(layers: &[Layer], code: u8) -> Option<Rgb> {
    layers.iter().rev().find_map(|l| l.get(&code).copied())
}

/// Whether the overlays just stopped: what's under them is drawn at once, not
/// at a static effect's next slow refresh.
fn overlay_ended(was_active: bool, active: bool) -> bool {
    was_active && !active
}

/// A wake draws at once only if the last frame is at least a frame old, so a
/// game sending hundreds of events a second still gets 30 frames, not hundreds.
fn wake_forces_frame(last_frame: Option<Instant>) -> bool {
    is_older_than(last_frame, FRAME)
}

fn wants_audio(lighting: &Lighting, drawing: bool) -> bool {
    drawing && matches!(lighting.effect, Effect::Visualizer { .. })
}

fn wants_mic(lighting: &Lighting, connected: bool) -> bool {
    lighting.enabled && lighting.mic_mute.enabled && connected
}

/// The mute light's colour for this key, if it's the mute key.
fn mic_color(mic: Option<(u8, gg_ipc::Rgb)>, code: u8) -> Option<Rgb> {
    mic.filter(|&(key, _)| key == code).map(|(_, c)| Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    })
}

/// The board drawn for `region` as [`State::region`] keeps it: the keyboard's own once
/// it's been asked, US until then.
fn board(region: Option<Option<u8>>) -> &'static [Key] {
    for_region(region.flatten())
}

/// Whether the keyboard has a key with this code: on its own board once its region has
/// been asked, on any of the boards until then, so a key saved on an ISO board isn't
/// lost before the keyboard is asked.
fn has_key(region: Option<Option<u8>>, code: u8) -> bool {
    let on = |board: &[Key]| board.iter().any(|k| k.0 == code);
    match region {
        Some(region) => on(for_region(region)),
        None => REGIONS.iter().any(|&(_, r)| on(for_region(Some(r)))),
    }
}

/// Settings that name keys must name keys this keyboard has.
fn check_keys(lighting: &Lighting, region: Option<Option<u8>>) -> Result<(), String> {
    let key = lighting.mic_mute.key;
    if !has_key(region, key) {
        return Err(format!(
            "the mic-mute key {key:#04x} is not on this keyboard"
        ));
    }
    Ok(())
}

/// `lighting` with its mute light on the default key if the keyboard lacks the saved one:
/// older settings have Pause, which this keyboard doesn't have, and a change would
/// otherwise be refused for it.
fn on_board(mut lighting: Lighting, region: Option<Option<u8>>) -> Lighting {
    if check_keys(&lighting, region).is_err() {
        lighting.mic_mute.key = MicMuteLight::default().key;
    }
    lighting
}

/// Whether gglinux sends the open keyboard anything of its own now: its lighting or an
/// overlay's, switches that are on, a screen of gglinux's, or key bindings that are on
/// with their gate. The onboard slots' reads and writes don't count.
fn takes_keyboard(state: &mut State, event_tx: &broadcast::Sender<Event>) -> bool {
    drawing(state.lighting.enabled, !state.layers.is_empty())
        || state.switches.settings().enabled
        || !matches!(state.screen.setting(), Screen::Keyboard)
        || (state.experimental.keyboard_bindings && bindings_on(state, event_tx))
}

/// Whether the key bindings are on, asked through the bindings module's own `Get`, as
/// `main` asks.
fn bindings_on(state: &mut State, event_tx: &broadcast::Sender<Event>) -> bool {
    let (reply, mut answer) = oneshot::channel();
    bindings::on_command(
        bindings::Cmd::Get(reply),
        &mut state.bindings,
        None,
        &state.experimental,
        event_tx,
    );
    answer
        .try_recv()
        .is_ok_and(|got| got.is_ok_and(|b| b.enabled))
}

/// Ask the open keyboard its region the first time gglinux takes it on this connection
/// ([`takes_keyboard`]), before anything else is sent. Until then nothing is, not even
/// this question.
fn read_region(state: &mut State, event_tx: &broadcast::Sender<Event>) {
    if state.region.is_none() && state.device.is_some() && takes_keyboard(state, event_tx) {
        ask_region(state);
    }
}

/// Ask the open keyboard its region, if it hasn't been asked on this connection: the
/// board drawn, where the effects find each key and the mute key follow it. A keyboard
/// that doesn't answer gets the US board, and isn't asked again until it reconnects.
fn ask_region(state: &mut State) {
    let Some(device) = state.device.as_ref() else {
        return;
    };
    if state.region.is_some() {
        return;
    }
    let region = device.region().unwrap_or_else(|e| {
        debug!(error = %format!("{e:#}"), "the keyboard did not say its region");
        None
    });
    info!(?region, "keyboard region read");
    state.region = Some(region);
    state.positions = effects::positions(board(state.region));
    state.lighting = on_board(std::mem::take(&mut state.lighting), state.region);
}

/// Run the visualizer's audio capture exactly while it's drawn.
fn sync_audio(state: &mut State) {
    let on_screen =
        state.device.is_some() && drawing(state.lighting.enabled, !state.layers.is_empty());
    if !wants_audio(&state.lighting, on_screen) {
        if state.capture.take().is_some() {
            *state.levels.lock().unwrap_or_else(|e| e.into_inner()) = [0.0; BANDS];
        }
        return;
    }
    if state.capture.is_some() || !is_older_than(state.last_capture_attempt, REOPEN_INTERVAL) {
        return;
    }
    state.last_capture_attempt = Some(Instant::now());
    match Capture::start(
        &audio::pw_record(state.listen_to.as_deref()),
        Arc::clone(&state.levels),
        state.tx.clone(),
    ) {
        Ok(c) => {
            debug!("capturing audio for the visualizer");
            state.capture = Some(c);
        }
        Err(e) => debug!(error = %e, "could not start pw-record for the visualizer"),
    }
}

/// Watch the microphone exactly while the mute light is wanted.
fn sync_mic(state: &mut State) {
    if !wants_mic(&state.lighting, state.device.is_some()) {
        state.mic_watch = None;
        state.mic_muted = false;
        return;
    }
    if state.mic_watch.is_none() {
        let argv = mic::WPCTL.iter().map(|s| s.to_string()).collect();
        state.mic_watch = Some(mic::Watch::start(argv, MIC_POLL, state.tx.clone()));
    }
}

/// Keep a press, dropping the oldest past [`MAX_HITS`].
fn push_hit(hits: &mut Vec<effects::Hit>, hit: effects::Hit) {
    if hits.len() == MAX_HITS {
        hits.remove(0);
    }
    hits.push(hit);
}

/// Start or stop the key reader to match the settings.
fn sync_reader(state: &mut State) {
    if !wants_reader(&state.lighting, state.device.is_some()) {
        if state.reader.take().is_some() {
            debug!("stopped reading key presses");
        }
        state.hits.clear();
        state.reactive = ReactiveStatus::Off;
        return;
    }
    if state.reader.is_some() || !is_older_than(state.last_reader_attempt, REOPEN_INTERVAL) {
        return;
    }
    let Some(api) = state.api.as_ref() else {
        return;
    };
    state.last_reader_attempt = Some(Instant::now());
    match Reader::start(api, state.tx.clone()) {
        Ok(reader) => {
            info!("reading key presses for reactive lighting");
            state.reader = Some(reader);
            state.reactive = ReactiveStatus::Reading;
        }
        Err(OpenError::NoAccess) => {
            if state.reactive != ReactiveStatus::NoAccess {
                info!("reactive lighting needs the optional udev rule 61-gglinux-reactive.rules");
            }
            state.reactive = ReactiveStatus::NoAccess;
        }
        Err(OpenError::Unavailable(why)) => {
            debug!(%why, "keyboard typing interfaces not available");
            state.reactive = ReactiveStatus::Off;
        }
    }
}

fn handle(command: Command, state: &mut State, event_tx: &broadcast::Sender<Event>) -> bool {
    match command {
        Command::GetState(reply) => {
            let _ = reply.send(Ok(snapshot(state)));
        }
        Command::SetSwitches { switches, reply } => {
            // The loop sends them, if they're on.
            if let Err(message) = state.switches.set(switches) {
                let _ = reply.send(Err(message));
                return true;
            }
            let _ = reply.send(Ok(()));
            publish(state, event_tx);
        }
        Command::SetScreen { screen, reply } => {
            if let Err(message) = state.screen.set(screen) {
                let _ = reply.send(Err(message));
                return true;
            }
            let _ = reply.send(Ok(()));
            publish(state, event_tx);
        }
        Command::SetScreenCare { care, reply } => {
            state.screen.set_care(care);
            let _ = reply.send(Ok(()));
            publish(state, event_tx);
        }
        Command::SetAway { away, reply } => {
            if state.screen.set_away(away) {
                // Now, not at the next loop: sleep may be next.
                read_region(state, event_tx);
                state.screen.render(state.device.as_ref());
                publish(state, event_tx);
            }
            let _ = reply.send(Ok(()));
        }
        Command::SetLighting { lighting, reply } => {
            let checked = lighting.validate().and_then(|()| {
                if lighting.enabled {
                    ask_region(state); // lighting that's on takes the keyboard
                }
                check_keys(&lighting, state.region)
            });
            if let Err(message) = checked {
                let _ = reply.send(Err(message));
                return true;
            }
            state.lighting = lighting;
            save(&state.settings_path, &state.lighting, "lighting");
            // Handing back, if nothing draws any more, is the loop's call
            // (`should_hand_back`), so a game that has taken over isn't interrupted.
            state.last_frame = None; // draw the change now
            let _ = reply.send(Ok(()));
            publish(state, event_tx);
        }
        Command::Pressed { code, at } => {
            if state.reader.is_some() {
                let t = at.saturating_duration_since(state.started).as_secs_f64();
                push_hit(&mut state.hits, effects::Hit { code, t });
                state.last_frame = None; // show it now
            }
        }
        Command::Wake => {
            if wake_forces_frame(state.last_frame) {
                state.last_frame = None;
            }
        }
        Command::AudioStopped => {
            state.capture = None;
            *state.levels.lock().unwrap_or_else(|e| e.into_inner()) = [0.0; BANDS];
            state.last_capture_attempt = Some(Instant::now());
        }
        Command::MicMuted(m) => {
            state.mic_muted = m;
            state.last_frame = None;
        }
        Command::ListenTo(output) => {
            if output != state.listen_to {
                state.listen_to = output;
                state.capture = None; // the loop starts it again, listening there
                state.last_capture_attempt = None;
            }
        }
        Command::ReaderStopped => {
            state.reader = None;
            state.reactive = ReactiveStatus::Off;
            state.last_reader_attempt = Some(Instant::now()); // retry after REOPEN_INTERVAL
        }
        Command::Bindings(cmd) => {
            if state.experimental.keyboard_bindings
                && matches!(cmd, bindings::Cmd::Set { enabled: true, .. })
            {
                ask_region(state); // bindings that are on take the keyboard
            }
            state.bindings.set_deep_keys(state.switches.points());
            bindings::on_command(
                cmd,
                &mut state.bindings,
                state.device.as_ref(),
                &state.experimental,
                event_tx,
            )
        }
        Command::Onboard(onboard::Cmd::Load { slot, reply }) => {
            // The keyboard runs the slot's own bindings and switches once it loads: the
            // bindings are read again before the next write, gglinux's switches go again.
            let (tx, mut rx) = oneshot::channel();
            let cmd = onboard::Cmd::Load { slot, reply: tx };
            let device = state.device.as_ref();
            onboard::on_command(
                cmd,
                &mut state.onboard,
                device,
                &state.experimental,
                event_tx,
            );
            let answer = rx
                .try_recv()
                .unwrap_or_else(|_| Err("the load didn't answer".into()));
            if answer.is_ok() {
                bindings::on_slot_loaded(&mut state.bindings, event_tx);
                state.switches.forget();
            }
            let _ = reply.send(answer);
        }
        Command::Onboard(cmd) => onboard::on_command(
            cmd,
            &mut state.onboard,
            state.device.as_ref(),
            &state.experimental,
            event_tx,
        ),
        Command::SetExperimental(experimental) => {
            state.experimental = experimental;
            if let Some(device) = state.device.as_ref() {
                device.set_families(families(&experimental));
            }
        }
        Command::Shutdown(reply) => {
            hand_back(state);
            state.screen.release(state.device.as_ref());
            let _ = reply.send(Ok(()));
            return false;
        }
        #[cfg(test)]
        Command::Exchange { report, reply } => {
            let answer = match state.device.as_ref() {
                Some(device) => device
                    .exchange_feature(&report)
                    .map_err(|e| format!("{e:#}")),
                None => Err("no keyboard".to_owned()),
            };
            let _ = reply.send(answer);
        }
    }
    true
}

/// The command families the open keyboard lets out under these gates.
fn families(experimental: &Experimental) -> Families {
    Families {
        bindings: experimental.keyboard_bindings,
        onboard: experimental.keyboard_onboard,
        protection: experimental.keyboard_protection,
    }
}

fn try_open(state: &mut State) {
    if !is_older_than(state.last_open_attempt, REOPEN_INTERVAL) {
        return;
    }
    let Some(api) = state.api.as_mut() else {
        return;
    };
    state.last_open_attempt = Some(Instant::now());
    let hid = gg_hid::lock();
    if api.refresh_devices().is_err() {
        return;
    }
    let opened = Keyboard::open(api);
    drop(hid);
    match opened {
        Ok(keyboard) => {
            info!("keyboard connected");
            connected(state, keyboard);
        }
        Err(e) => debug!(error = %format!("{e:#}"), "keyboard not available"),
    }
}

/// The keyboard just opened: it has its own settings, and lets out only the command
/// families the experimental gates allow.
fn connected(state: &mut State, keyboard: Keyboard) {
    keyboard.set_families(families(&state.experimental));
    state.region = None; // asked again: this may be another keyboard
    state.positions = effects::positions(board(None));
    state.device = Some(keyboard);
    forget_device(state);
    state.last_frame = None;
    state.just_opened = true;
}

/// Let the keyboard go if it's gone from the device list, at most every
/// [`REOPEN_INTERVAL`].
fn look_for(state: &mut State) {
    if !is_older_than(state.last_open_attempt, REOPEN_INTERVAL) {
        return;
    }
    let Some(api) = state.api.as_mut() else {
        return;
    };
    state.last_open_attempt = Some(Instant::now());
    let hid = gg_hid::lock();
    let refreshed = api.refresh_devices();
    drop(hid);
    if refreshed.is_err() {
        return;
    }
    if state.device.as_ref().is_some_and(|d| !d.is_listed(api)) {
        info!("keyboard disconnected");
        state.device = None;
        forget_device(state);
    }
}

/// Forget what was sent to the keyboard: after it (re)connects, it has its own settings.
fn forget_device(state: &mut State) {
    state.taken = false;
    state.switches.forget();
    state.screen.forget();
}

/// Send a frame if one is due.
fn render(state: &mut State) {
    let overlay_active = !state.layers.is_empty();
    if !drawing(state.lighting.enabled, overlay_active)
        || !is_older_than(
            state.last_frame,
            interval(&state.lighting, !state.hits.is_empty(), overlay_active),
        )
    {
        return;
    }
    let t = state.started.elapsed().as_secs_f64();
    let duration = f64::from(state.lighting.reactive.duration_s);
    state.hits.retain(|h| t - h.t < duration);
    let hits = &state.hits;
    let layers = &state.layers;
    let bands = *state.levels.lock().unwrap_or_else(|e| e.into_inner());
    let m = &state.lighting.mic_mute;
    let mic = (state.lighting.enabled && m.enabled && state.mic_muted).then_some((m.key, m.color));
    let Some(device) = state.device.as_ref() else {
        return;
    };
    let result = if state.taken { Ok(()) } else { device.init() }.and_then(|()| {
        device.show(
            |code| match mic_color(mic, code).or_else(|| overlay_color(layers, code)) {
                Some(c) => effects::dim(c, state.lighting.brightness),
                None => effects::color(&state.lighting, &state.positions, code, t, hits, &bands),
            },
        )
    });
    match result {
        Ok(()) => {
            state.taken = true;
            state.last_frame = Some(Instant::now());
        }
        Err(e) => {
            info!(error = %format!("{e:#}"), "keyboard disconnected");
            state.device = None;
            forget_device(state);
        }
    }
}

/// Give the lighting back to the keyboard's onboard profile, if gglinux had it.
fn hand_back(state: &mut State) {
    if !state.taken {
        return;
    }
    if let Some(device) = state.device.as_ref() {
        match device.hand_back() {
            Ok(()) => debug!("keyboard lighting handed back"),
            Err(e) => warn!(error = %format!("{e:#}"), "could not hand the keyboard lighting back"),
        }
    }
    state.taken = false;
}

fn snapshot(state: &State) -> KeyboardState {
    KeyboardState {
        connected: state.device.is_some(),
        layout: board(state.region)
            .iter()
            .map(|&(code, label, x, y, w)| KeyInfo {
                code,
                label: label.to_owned(),
                x,
                y,
                w,
            })
            .collect(),
        lighting: state.lighting.clone(),
        reactive: state.reactive,
        analog_keys: ANALOG_KEYS.to_vec(),
        switches: state.switches.settings().clone(),
        switches_error: state.switches.error(),
        screen: state.screen.setting().clone(),
        screen_error: state.screen.error(),
        screen_care: state.screen.care(),
        screen_resting: state.screen.resting(),
    }
}

fn publish(state: &mut State, event_tx: &broadcast::Sender<Event>) {
    let now = snapshot(state);
    if state.published.as_ref() != Some(&now) {
        state.published = Some(now.clone());
        let _ = event_tx.send(Event::KeyboardChanged { state: now });
    }
}

/// Saved settings; a missing or unreadable file means the defaults (all off). An
/// unreadable one is moved aside first. `what` names them in the log.
fn load<T: DeserializeOwned + Default>(path: &Path, what: &str) -> T {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            let moved_to = gg_ipc::set_aside(path);
            warn!(path = %path.display(), error = %e, %moved_to, "ignoring unreadable {what} settings");
            T::default()
        }),
        Err(_) => T::default(),
    }
}

/// Save atomically; a failure is logged, never fatal.
fn save<T: Serialize>(path: &Path, settings: &T, what: &str) {
    let write = || -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(settings)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    if let Err(e) = write() {
        warn!(path = %path.display(), error = %format!("{e:#}"), "could not save {what} settings");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use gg_device_keyboard::proto;
    use gg_device_keyboard::testing::{FakeTransport, Kind};
    use gg_ipc::{Effect, Reactive};

    fn lighting(enabled: bool, reactive: bool, effect: Effect) -> Lighting {
        Lighting {
            enabled,
            effect,
            reactive: Reactive {
                enabled: reactive,
                ..Reactive::default()
            },
            ..Lighting::default()
        }
    }

    #[test]
    fn the_reader_runs_only_with_lighting_and_reactive_on_and_the_keyboard_here() {
        assert!(wants_reader(&lighting(true, true, Effect::Static), true));
        assert!(
            !wants_reader(&lighting(false, true, Effect::Static), true),
            "lighting off"
        );
        assert!(
            !wants_reader(&lighting(true, false, Effect::Static), true),
            "reactive off"
        );
        assert!(
            !wants_reader(&lighting(true, true, Effect::Static), false),
            "keyboard away"
        );
    }

    #[test]
    fn at_most_32_presses_are_kept_oldest_dropped_first() {
        let mut hits = Vec::new();
        for i in 0..40u8 {
            push_hit(
                &mut hits,
                effects::Hit {
                    code: i,
                    t: f64::from(i),
                },
            );
        }
        assert_eq!(hits.len(), MAX_HITS);
        assert_eq!(hits[0].code, 8, "the eight oldest went");
        assert_eq!(hits[MAX_HITS - 1].code, 39);
    }

    #[test]
    fn live_presses_animate_even_a_static_effect() {
        let still = lighting(true, true, Effect::Static);
        assert_eq!(interval(&still, false, false), STATIC_REFRESH);
        assert_eq!(interval(&still, true, false), FRAME);
        assert_eq!(
            interval(
                &lighting(true, true, Effect::Wave { period_s: 4.0 }),
                false,
                false
            ),
            FRAME
        );
    }

    #[test]
    fn gglinux_draws_for_its_own_lighting_or_an_active_overlay_and_hands_back_otherwise() {
        assert!(drawing(true, false));
        assert!(drawing(false, true), "a game takes the keyboard over");
        assert!(!drawing(false, false));
        assert!(
            should_hand_back(true, false, false),
            "held, but nothing wants it"
        );
        assert!(!should_hand_back(true, false, true), "a game still draws");
        assert!(
            !should_hand_back(true, true, false),
            "gglinux lighting still draws"
        );
        assert!(!should_hand_back(false, false, false), "not held");
    }

    #[test]
    fn an_open_keyboard_is_looked_for_while_nothing_is_drawn_since_no_write_would_fail() {
        assert!(
            should_look_for(true, false),
            "open and idle: only the device list can tell it went"
        );
        assert!(
            !should_look_for(true, true),
            "drawing: a failed frame tells"
        );
        assert!(
            !should_look_for(false, false),
            "not open: opening looks already"
        );
    }

    #[test]
    fn the_last_active_overlay_that_paints_a_key_wins() {
        let red = gg_device_keyboard::Rgb { r: 255, g: 0, b: 0 };
        let blue = gg_device_keyboard::Rgb { r: 0, g: 0, b: 255 };
        let layers = vec![
            Layer::from([(0x04, red), (0x05, red)]),
            Layer::from([(0x05, blue)]),
        ];
        assert_eq!(overlay_color(&layers, 0x04), Some(red));
        assert_eq!(overlay_color(&layers, 0x05), Some(blue));
        assert_eq!(overlay_color(&layers, 0x06), None);
    }

    #[test]
    fn an_active_overlay_animates_even_a_static_effect() {
        let still = lighting(true, false, Effect::Static);
        assert_eq!(interval(&still, false, true), FRAME);
        assert_eq!(interval(&still, false, false), STATIC_REFRESH);
    }

    #[test]
    fn the_lighting_under_an_overlay_is_drawn_at_once_when_it_ends() {
        assert!(
            overlay_ended(true, false),
            "a static effect would otherwise wait out its slow refresh"
        );
        assert!(
            !overlay_ended(true, true),
            "still painting: frames are due 30 times a second"
        );
        assert!(
            !overlay_ended(false, true),
            "starting: its frames are due at once anyway"
        );
        assert!(!overlay_ended(false, false));
    }

    #[test]
    fn wake_forces_a_frame_at_most_once_per_frame_interval() {
        assert!(wake_forces_frame(None));
        assert!(
            !wake_forces_frame(Some(Instant::now())),
            "a frame just went out: wait for the cadence"
        );
        assert!(wake_forces_frame(Some(Instant::now() - FRAME)));
    }

    #[test]
    fn the_capture_and_the_mic_watch_run_only_when_wanted() {
        let viz = Lighting {
            effect: Effect::Visualizer { sensitivity: 1.0 },
            ..lighting(true, false, Effect::Static)
        };
        assert!(wants_audio(&viz, true));
        assert!(!wants_audio(&viz, false), "not drawing");
        assert!(
            !wants_audio(&lighting(true, false, Effect::Static), true),
            "another effect"
        );
        let mut m = lighting(true, false, Effect::Static);
        m.mic_mute.enabled = true;
        assert!(wants_mic(&m, true));
        assert!(!wants_mic(&m, false), "keyboard away");
        m.enabled = false;
        assert!(!wants_mic(&m, true), "lighting off");
    }

    #[test]
    fn the_mute_light_wins_over_overlays() {
        let red = gg_ipc::Rgb { r: 255, g: 0, b: 0 };
        assert_eq!(
            mic_color(Some((0x48, red)), 0x48),
            Some(gg_device_keyboard::Rgb { r: 255, g: 0, b: 0 })
        );
        assert_eq!(mic_color(Some((0x48, red)), 0x47), None);
        assert_eq!(mic_color(None, 0x48), None);
    }

    #[test]
    fn a_mute_key_must_be_on_the_keyboard() {
        let mut l = lighting(true, false, Effect::Static);
        assert!(check_keys(&l, None).is_ok());
        l.mic_mute.key = 0x59; // keypad 1: not on a TKL
        assert!(check_keys(&l, None).is_err());
        l.mic_mute.key = 0x48; // Pause: not on this one
        assert!(check_keys(&l, None).is_err());
        l.mic_mute.key = 0x64; // beside left Shift, on ISO boards
        assert!(
            check_keys(&l, None).is_ok(),
            "not asked yet: any board's key"
        );
        assert!(check_keys(&l, Some(Some(4))).is_ok(), "German");
        assert!(check_keys(&l, Some(Some(1))).is_err(), "US");
        assert!(check_keys(&l, Some(None)).is_err(), "no answer: US");
    }

    #[test]
    fn a_saved_mute_key_the_board_lacks_moves_to_the_default() {
        let mut l = lighting(true, false, Effect::Static);
        l.mic_mute.key = 0x48; // Pause, older settings' default
        assert_eq!(on_board(l.clone(), None).mic_mute.key, 0xFB);
        l.mic_mute.key = 0x64; // ISO only
        assert_eq!(
            on_board(l.clone(), Some(None)).mic_mute.key,
            0xFB,
            "not on US"
        );
        assert_eq!(
            on_board(l.clone(), Some(Some(4))).mic_mute.key,
            0x64,
            "kept on German"
        );
        assert_eq!(
            on_board(l, None).mic_mute.key,
            0x64,
            "kept until it's asked"
        );
    }

    /// A state with no device list and its settings in a folder that isn't there: every
    /// setting at its default, and nothing here saves one.
    fn nowhere() -> PathBuf {
        std::env::temp_dir().join(format!("gglinuxd-keyboard-nowhere-{}", std::process::id()))
    }

    fn bare_state() -> State {
        State::new(
            None,
            &nowhere(),
            mpsc::channel().0,
            Vec::new(),
            Experimental::default(),
        )
    }

    #[test]
    fn the_open_keyboard_lets_a_gated_family_out_only_while_its_gate_is_on() {
        let (event_tx, _) = broadcast::channel(8);
        let mut state = bare_state();
        connected(&mut state, FakeTransport::new().keyboard());
        let families_now = |s: &State| s.device.as_ref().unwrap().families();
        assert_eq!(families_now(&state), Families::default());

        let bindings_on = Experimental {
            keyboard_bindings: true,
            ..Experimental::default()
        };
        assert!(handle(
            Command::SetExperimental(bindings_on),
            &mut state,
            &event_tx
        ));
        let bindings = Families {
            bindings: true,
            onboard: false,
            protection: false,
        };
        assert_eq!(families_now(&state), bindings);
        connected(&mut state, FakeTransport::new().keyboard());
        assert_eq!(
            families_now(&state),
            bindings,
            "and every keyboard opened later"
        );

        let onboard_and_a_mouse_gate = Experimental {
            keyboard_onboard: true,
            mouse_power: true,
            ..Experimental::default()
        };
        handle(
            Command::SetExperimental(onboard_and_a_mouse_gate),
            &mut state,
            &event_tx,
        );
        assert_eq!(
            families_now(&state),
            Families {
                bindings: false,
                onboard: true,
                protection: false,
            }
        );

        // Told while no keyboard is open: kept for the next one.
        state.device = None;
        handle(
            Command::SetExperimental(Experimental::default()),
            &mut state,
            &event_tx,
        );
        connected(&mut state, FakeTransport::new().keyboard());
        assert_eq!(families_now(&state), Families::default());
        assert!(state.just_opened, "the units' on_connect is due");
    }

    #[tokio::test]
    async fn a_worker_on_a_fresh_install_sends_the_keyboard_nothing_and_answers_the_units() {
        let fake = FakeTransport::new();
        let (event_tx, _) = broadcast::channel(8);
        let kb = spawn_fake_unasked(
            fake.keyboard(),
            nowhere(),
            event_tx,
            Experimental::default(),
        );
        assert!(kb.send(Command::GetState).await.unwrap().connected);
        assert_eq!(
            kb.send(|r| Command::Bindings(bindings::Cmd::Get(r)))
                .await
                .unwrap(),
            gg_ipc::BindingsState::default()
        );
        assert_eq!(
            kb.send(|r| Command::Onboard(onboard::Cmd::Get(r)))
                .await
                .unwrap(),
            gg_ipc::OnboardState::default()
        );
        kb.send::<()>(Command::Shutdown).await.unwrap();
        assert!(
            fake.sent().is_empty(),
            "lighting, switches and screen are all off by default: {:?}",
            fake.sent()
        );
    }

    /// The colour a direct lighting frame gives `code`.
    fn frame_color(frame: &[u8], code: u8) -> Rgb {
        let i = proto::KEYS.iter().position(|&c| c == code).unwrap();
        let at = 3 + i * 4;
        assert_eq!(frame[at], code);
        Rgb {
            r: frame[at + 1],
            g: frame[at + 2],
            b: frame[at + 3],
        }
    }

    /// A keyboard whose region is `region`.
    fn keyboard_of(region: u8) -> FakeTransport {
        let fake = FakeTransport::new();
        fake.answer_input(
            gg_device_keyboard::testing::Key::op(proto::OP_REGION),
            &[0xF5, 0x00, region],
        );
        fake
    }

    /// How often a worker with these saved settings, and `setup` done to it, asks a
    /// German keyboard its region over two loops, and what it keeps of the answer.
    fn region_queries(
        saved: &[(&str, &str)],
        experimental: Experimental,
        setup: impl FnOnce(&mut State),
    ) -> (usize, Option<Option<u8>>) {
        let d = Scratch::new("keyboard-takes");
        for (name, json) in saved {
            std::fs::write(d.0.join(name), json).unwrap();
        }
        let fake = keyboard_of(4);
        let mut state = State::new(None, &d.0, mpsc::channel().0, Vec::new(), experimental);
        connected(&mut state, fake.keyboard());
        setup(&mut state);
        let (event_tx, _) = broadcast::channel(8);
        read_region(&mut state, &event_tx);
        read_region(&mut state, &event_tx);
        let sent = fake.sent();
        assert!(
            sent.iter().all(|s| s.report[1] == proto::OP_REGION),
            "nothing but the question: {sent:?}"
        );
        (sent.len(), state.region)
    }

    #[test]
    fn the_region_is_asked_once_and_only_when_gglinux_takes_the_keyboard_anyway() {
        let off = Experimental::default();
        let bindings_gate = Experimental {
            keyboard_bindings: true,
            ..off
        };
        let every_gate = Experimental {
            keyboard_onboard: true,
            ..bindings_gate
        };
        let bindings_on = [("bindings.json", r#"{"enabled": true}"#)];
        let not_asked = (0, None);
        assert_eq!(
            region_queries(&[], off, |_| {}),
            not_asked,
            "a fresh install"
        );
        assert_eq!(
            region_queries(&[], every_gate, |_| {}),
            not_asked,
            "gates alone, the onboard slots' too"
        );
        assert_eq!(
            region_queries(&bindings_on, off, |_| {}),
            not_asked,
            "bindings on, their gate off: nothing goes out"
        );

        let german = (1, Some(Some(4)));
        let lit = |s: &mut State| s.lighting.enabled = true;
        assert_eq!(region_queries(&[], off, lit), german, "lighting");
        let painted = |s: &mut State| s.layers = vec![Layer::from([(0x04, Rgb::default())])];
        assert_eq!(
            region_queries(&[], off, painted),
            german,
            "a game's overlay"
        );
        let switches = |s: &mut State| {
            let on = Switches {
                enabled: true,
                ..Switches::default()
            };
            s.switches.set(on).unwrap()
        };
        assert_eq!(region_queries(&[], off, switches), german, "switches");
        let text = |s: &mut State| {
            let hello = Screen::Text {
                text: "hello".into(),
            };
            s.screen.set(hello).unwrap()
        };
        assert_eq!(
            region_queries(&[], off, text),
            german,
            "a screen of gglinux's"
        );
        assert_eq!(
            region_queries(&bindings_on, bindings_gate, |_| {}),
            german,
            "bindings on with their gate"
        );
    }

    #[test]
    fn each_connection_is_asked_again_and_the_mute_key_follows_the_board() {
        let (event_tx, _) = broadcast::channel(8);
        let mut state = bare_state();
        state.lighting.enabled = true;
        state.lighting.mic_mute.key = 0x64; // saved on an ISO board
        for (fake, region) in [
            (keyboard_of(4), Some(4)),
            (keyboard_of(1), Some(1)),
            (FakeTransport::new(), None), // no answer
        ] {
            connected(&mut state, fake.keyboard());
            assert_eq!(state.region, None, "not asked yet");
            read_region(&mut state, &event_tx);
            read_region(&mut state, &event_tx);
            assert_eq!(state.region, Some(region));
            assert_eq!(fake.sent().len(), 1, "asked once: {region:?}");
            assert_eq!(state.positions, effects::positions(for_region(region)));
            if region == Some(4) {
                assert_eq!(state.lighting.mic_mute.key, 0x64, "kept on German");
            }
        }
        assert_eq!(state.lighting.mic_mute.key, 0xFB, "moved on the US board");
        assert!(!state.positions.contains_key(&0x64));
    }

    #[test]
    fn lighting_turned_on_is_checked_against_the_keyboard_s_own_board() {
        let (event_tx, _) = broadcast::channel(8);
        let mut state = bare_state();
        let fake = keyboard_of(1);
        connected(&mut state, fake.keyboard());
        let mut iso_key = lighting(true, false, Effect::Static);
        iso_key.mic_mute.key = 0x64;
        let (reply, mut answer) = oneshot::channel();
        handle(
            Command::SetLighting {
                lighting: iso_key,
                reply,
            },
            &mut state,
            &event_tx,
        );
        let refused = answer.try_recv().unwrap().unwrap_err();
        assert_eq!(refused, "the mic-mute key 0x64 is not on this keyboard");
        let sent = fake.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].report[1], proto::OP_REGION, "asked first");
    }

    #[test]
    fn bindings_turned_on_with_their_gate_ask_the_region_before_they_go_out() {
        let d = Scratch::new("keyboard-bindings-region");
        let (event_tx, _) = broadcast::channel(8);
        let gate = Experimental {
            keyboard_bindings: true,
            ..Experimental::default()
        };
        let mut state = State::new(None, &d.0, mpsc::channel().0, Vec::new(), gate);
        let fake = keyboard_of(4);
        connected(&mut state, fake.keyboard());
        let (reply, _answer) = oneshot::channel();
        let set = bindings::Cmd::Set {
            bindings: gg_ipc::Bindings::default(),
            enabled: true,
            reply,
        };
        handle(Command::Bindings(set), &mut state, &event_tx);
        let sent = fake.sent();
        assert_eq!(sent[0].report[1], proto::OP_REGION, "asked first: {sent:?}");
        assert!(sent.len() > 1, "then the bindings' own reports");
        assert_eq!(state.region, Some(Some(4)));
    }

    fn label_of(layout: &[KeyInfo], code: u8) -> &str {
        &layout.iter().find(|k| k.code == code).unwrap().label
    }

    #[tokio::test]
    async fn once_lighting_is_on_the_region_is_asked_first_and_picks_the_board() {
        let d = Scratch::new("keyboard-region");
        let fake = FakeTransport::new();
        fake.answer_input(
            gg_device_keyboard::testing::Key::op(proto::OP_REGION),
            &[0xF5, 0x00, 0x04], // German
        );
        let (event_tx, _) = broadcast::channel(8);
        let kb = spawn_fake_unasked(
            fake.keyboard(),
            d.0.clone(),
            event_tx,
            Experimental::default(),
        );
        let before = kb.send(Command::GetState).await.unwrap().layout;
        assert_eq!(
            label_of(&before, 0x1C),
            "Y",
            "the US board until it's asked"
        );
        assert!(before.iter().all(|k| k.code != 0x48), "no Pause");
        assert!(
            fake.sent().is_empty(),
            "not asked while gglinux leaves it alone"
        );

        let wave = Lighting {
            enabled: true,
            effect: Effect::Wave { period_s: 60.0 },
            ..Lighting::default()
        };
        kb.send(|reply| Command::SetLighting {
            lighting: wave,
            reply,
        })
        .await
        .unwrap();
        let layout = kb.send(Command::GetState).await.unwrap().layout;
        kb.send::<()>(Command::Shutdown).await.unwrap();

        let sent = fake.sent();
        assert_eq!(
            (sent[0].kind, &sent[0].report),
            (
                Kind::Output,
                &proto::command(proto::OP_REGION, &[]).unwrap()
            ),
            "the region query goes first"
        );
        let asked = sent.iter().filter(|s| s.report[1] == proto::OP_REGION);
        assert_eq!(asked.count(), 1, "once");
        assert_eq!(label_of(&layout, 0x1C), "Z", "German: QWERTZ");
        assert_eq!(
            label_of(&layout, 0x64),
            "<",
            "the ISO key beside left Shift"
        );
        // The wave's colours run along the board: on the German board 0x64 sits between
        // 1 and Q, so its colour does too (this early in the wave, green grows with x).
        let frame = &sent
            .iter()
            .find(|s| s.report[1] == proto::OP_DIRECT)
            .unwrap()
            .report;
        let green = |code| frame_color(frame, code).g;
        assert!(
            green(0x1E) < green(0x64) && green(0x64) < green(0x14),
            "the effects know where 0x64 is: {} {} {}",
            green(0x1E),
            green(0x64),
            green(0x14)
        );
    }
}
