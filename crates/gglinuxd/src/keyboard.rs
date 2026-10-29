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

mod audio;
mod effects;
mod mic;
mod reader;
mod spectrum;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gg_device_keyboard::layout::ANSI_TKL;
use gg_device_keyboard::proto::ANALOG_KEYS;
use gg_device_keyboard::{oled, Bitmap, Keyboard, Rgb};
use gg_ipc::{
    Effect, Event, KeyInfo, KeyboardState, Lighting, ReactiveStatus, Screen, ScreenCare, Switches,
    TapMode,
};
use hidapi::HidApi;
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, error, info, warn};

use crate::clock::is_older_than;
use audio::{Capture, Levels};
use reader::{OpenError, Reader};
use spectrum::BANDS;

const FRAME: Duration = Duration::from_millis(33);
const STATIC_REFRESH: Duration = Duration::from_secs(2);
const REOPEN_INTERVAL: Duration = Duration::from_secs(2);
const IDLE_WAIT: Duration = Duration::from_millis(500);
/// The most key presses held for reactive lighting; older ones are dropped first.
const MAX_HITS: usize = 32;
const MIC_POLL: Duration = Duration::from_millis(500);
/// How often an unchanged screen picture is sent again, in case the keyboard's own menus
/// (the volume roller, the media button) drew over it.
const SCREEN_REFRESH: Duration = Duration::from_secs(10);
/// How long the picture stays put before it moves a pixel, with screen care's shift on.
const SHIFT_EVERY: u64 = 120;

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
    /// Hand the lighting back and stop. Replies once the keyboard has been told.
    Shutdown(Reply<()>),
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
            Command::Shutdown(_) => "Shutdown",
        })
    }
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    /// A handle with no worker, for when the keyboard part is off: every send fails, and
    /// a wake or a `listen_to` from another part goes nowhere.
    pub fn off() -> Handle {
        let (tx, _) = mpsc::channel();
        Handle { tx }
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
}

pub fn spawn(event_tx: broadcast::Sender<Event>, overlays: Vec<Arc<dyn Overlay>>) -> Handle {
    let (tx, rx) = mpsc::channel();
    let readers_tx = tx.clone();
    std::thread::Builder::new()
        .name("gg-keyboard".into())
        .spawn(move || worker(rx, readers_tx, event_tx, overlays))
        .expect("spawn gg-keyboard thread");
    Handle { tx }
}

struct State {
    api: HidApi,
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
    switches: Switches,
    switches_path: PathBuf,
    /// What sending the switches to the open keyboard gave; `None`: not sent to it yet.
    switches_sent: Option<Result<(), String>>,
    screen: Screen,
    screen_path: PathBuf,
    /// Whether gglinux has drawn on this keyboard's screen since it connected.
    screen_taken: bool,
    /// The picture last sent, and when, so an unchanged one isn't sent every loop.
    screen_shown: Option<Bitmap>,
    screen_last: Option<Instant>,
    screen_error: Option<String>,
    care: ScreenCare,
    care_path: PathBuf,
    /// Nobody is at the computer, as [`crate::presence`] last said.
    away: bool,
    /// The screen is dark for that right now.
    resting: bool,
}

fn worker(
    rx: Receiver<Command>,
    tx: Sender<Command>,
    event_tx: broadcast::Sender<Event>,
    overlays: Vec<Arc<dyn Overlay>>,
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
    let settings_path = gg_ipc::config_dir().join("lighting.json");
    let switches_path = gg_ipc::config_dir().join("switches.json");
    let screen_path = gg_ipc::config_dir().join("screen.json");
    let care_path = gg_ipc::config_dir().join("screen-care.json");
    let mut state = State {
        api,
        device: None,
        taken: false,
        last_open_attempt: None,
        last_frame: None,
        lighting: load(&settings_path, "lighting"),
        settings_path,
        positions: effects::positions(),
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
        switches: load(&switches_path, "switch"),
        switches_path,
        switches_sent: None,
        screen: load(&screen_path, "screen"),
        screen_path,
        screen_taken: false,
        screen_shown: None,
        screen_last: None,
        screen_error: None,
        care: load(&care_path, "screen care"),
        care_path,
        away: false,
        resting: false,
    };

    loop {
        match rx.recv_timeout(next_wait(&state)) {
            Ok(command) => {
                if !handle(command, &mut state, &event_tx) {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Unreachable while `state.tx` lives — the worker stops on Shutdown,
            // which main sends on exit. Kept as a fallback.
            Err(RecvTimeoutError::Disconnected) => {
                hand_back(&mut state);
                release_screen(&mut state);
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
        render(&mut state);
        sync_switches(&mut state);
        render_screen(&mut state);
        publish(&mut state, &event_tx);
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
    match screen_wait(&state.screen, state.device.is_some()) {
        Some(screen) => lighting.min(screen),
        None => lighting,
    }
}

/// A clock on the screen wakes the loop just after each second turns.
fn screen_wait(screen: &Screen, connected: bool) -> Option<Duration> {
    if !connected || !matches!(screen, Screen::Clock { .. }) {
        return None;
    }
    let into_second = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis())
        .unwrap_or(0);
    Some(Duration::from_millis(u64::from(1000 - into_second) + 5))
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

/// Settings that name keys must name keys this keyboard has.
fn check_keys(lighting: &Lighting) -> Result<(), String> {
    let key = lighting.mic_mute.key;
    if !ANSI_TKL.iter().any(|k| k.0 == key) {
        return Err(format!(
            "the mic-mute key {key:#04x} is not on this keyboard"
        ));
    }
    Ok(())
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
    state.last_reader_attempt = Some(Instant::now());
    match Reader::start(&state.api, state.tx.clone()) {
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
            if let Err(message) = switches.validate().and_then(|()| check_analog(&switches)) {
                let _ = reply.send(Err(message));
                return true;
            }
            state.switches = switches;
            save(&state.switches_path, &state.switches, "switch");
            state.switches_sent = None; // the loop sends them, if they're on
            let _ = reply.send(Ok(()));
            publish(state, event_tx);
        }
        Command::SetScreen { screen, reply } => {
            if let Err(message) = screen.validate() {
                let _ = reply.send(Err(message));
                return true;
            }
            state.screen = screen;
            save(&state.screen_path, &state.screen, "screen");
            state.screen_shown = None; // draw the change now
            state.screen_error = None;
            let _ = reply.send(Ok(()));
            publish(state, event_tx);
        }
        Command::SetScreenCare { care, reply } => {
            state.care = care;
            save(&state.care_path, &state.care, "screen care");
            state.screen_shown = None; // draw the change now
            let _ = reply.send(Ok(()));
            publish(state, event_tx);
        }
        Command::SetAway { away, reply } => {
            if away != state.away {
                state.away = away;
                render_screen(state); // now, not at the next loop: sleep may be next
                publish(state, event_tx);
            }
            let _ = reply.send(Ok(()));
        }
        Command::SetLighting { lighting, reply } => {
            if let Err(message) = lighting.validate().and_then(|()| check_keys(&lighting)) {
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
        Command::Shutdown(reply) => {
            hand_back(state);
            release_screen(state);
            let _ = reply.send(Ok(()));
            return false;
        }
    }
    true
}

fn try_open(state: &mut State) {
    if !is_older_than(state.last_open_attempt, REOPEN_INTERVAL) {
        return;
    }
    state.last_open_attempt = Some(Instant::now());
    let hid = gg_hid::lock();
    if state.api.refresh_devices().is_err() {
        return;
    }
    let opened = Keyboard::open(&state.api);
    drop(hid);
    match opened {
        Ok(keyboard) => {
            info!("keyboard connected");
            state.device = Some(keyboard);
            forget_device(state);
            state.last_frame = None;
        }
        Err(e) => debug!(error = %format!("{e:#}"), "keyboard not available"),
    }
}

/// Let the keyboard go if it's gone from the device list, at most every
/// [`REOPEN_INTERVAL`].
fn look_for(state: &mut State) {
    if !is_older_than(state.last_open_attempt, REOPEN_INTERVAL) {
        return;
    }
    state.last_open_attempt = Some(Instant::now());
    let hid = gg_hid::lock();
    let refreshed = state.api.refresh_devices();
    drop(hid);
    if refreshed.is_err() {
        return;
    }
    if state
        .device
        .as_ref()
        .is_some_and(|d| !d.is_listed(&state.api))
    {
        info!("keyboard disconnected");
        state.device = None;
        forget_device(state);
    }
}

/// Forget what was sent to the keyboard: after it (re)connects, it has its own settings.
fn forget_device(state: &mut State) {
    state.taken = false;
    state.switches_sent = None;
    state.screen_taken = false;
    state.screen_shown = None;
    state.screen_last = None;
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

/// Every key a switch setting names must have an adjustable switch.
fn check_analog(switches: &Switches) -> Result<(), String> {
    let named = switches
        .keys
        .iter()
        .map(|k| k.code)
        .chain(switches.rapid_trigger.keys.iter().copied())
        .chain(switches.rapid_tap.pairs.iter().flat_map(|p| [p.a, p.b]));
    for code in named {
        if !ANALOG_KEYS.contains(&code) {
            let label = ANSI_TKL
                .iter()
                .find(|k| k.0 == code)
                .map_or_else(|| format!("{code:#04x}"), |k| k.1.to_owned());
            return Err(format!("{label} has a fixed switch: it can't be adjusted"));
        }
    }
    Ok(())
}

fn device_tap_mode(mode: TapMode) -> gg_device_keyboard::TapMode {
    use gg_device_keyboard::TapMode as D;
    match mode {
        TapMode::LastWins => D::LastWins,
        TapMode::Neither => D::Neither,
        TapMode::FirstWins => D::FirstWins,
        TapMode::SecondWins => D::SecondWins,
    }
}

/// Send the switch settings once to a keyboard that hasn't had them since it connected.
fn sync_switches(state: &mut State) {
    if !state.switches.enabled || state.switches_sent.is_some() {
        return;
    }
    let Some(device) = state.device.as_ref() else {
        return;
    };
    let s = &state.switches;
    let pairs: Vec<gg_device_keyboard::TapPair> = s
        .rapid_tap
        .pairs
        .iter()
        .map(|p| gg_device_keyboard::TapPair {
            a: p.a,
            b: p.b,
            mode: device_tap_mode(p.mode),
            report_both: p.report_both,
        })
        .collect();
    let sensitivity = s.rapid_trigger.sensitivity;
    let result = device
        .set_switches(&gg_device_keyboard::Switches {
            actuation: &|code| s.actuation_of(code),
            rapid_trigger: &|code| s.rapid_trigger_on(code),
            sensitivity: &|_| sensitivity,
            rapid_tap: s.rapid_tap.enabled,
            tap_pairs: &pairs,
        })
        .map_err(|e| format!("{e:#}"));
    match &result {
        Ok(()) => info!("keyboard switch settings sent"),
        Err(e) => warn!(error = %e, "the keyboard did not take the switch settings"),
    }
    state.switches_sent = Some(result);
}

/// The local time, for the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalTime {
    hour: u8,
    minute: u8,
    second: u8,
    /// 0 is Sunday.
    weekday: u8,
    day: u8,
    /// 0 is January.
    month: u8,
}

fn local_now() -> LocalTime {
    // SAFETY: `time` with a null pointer only returns the time; `localtime_r` writes
    // only the `tm` it's given, which lives on this stack.
    let tm = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    LocalTime {
        hour: tm.tm_hour as u8,
        minute: tm.tm_min as u8,
        second: tm.tm_sec.min(59) as u8,
        weekday: tm.tm_wday as u8,
        day: tm.tm_mday as u8,
        month: tm.tm_mon as u8,
    }
}

/// The clock's big line, and the small one under it if any.
fn clock_text(t: LocalTime, seconds: bool, h24: bool, date: bool) -> (String, Option<String>) {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut time = if h24 {
        format!("{:02}:{:02}", t.hour, t.minute)
    } else {
        let h = match t.hour % 12 {
            0 => 12,
            h => h,
        };
        format!("{h}:{:02}", t.minute)
    };
    if seconds {
        time.push_str(&format!(":{:02}", t.second));
    }
    let mut under = Vec::new();
    if date {
        under.push(format!(
            "{} {} {}",
            DAYS[usize::from(t.weekday % 7)],
            t.day,
            MONTHS[usize::from(t.month % 12)]
        ));
    }
    if !h24 {
        under.push(if t.hour < 12 { "AM" } else { "PM" }.to_owned());
    }
    (time, (!under.is_empty()).then(|| under.join(" ")))
}

/// What a screen setting draws at `now`; `None` for the keyboard's own.
fn picture(screen: &Screen, now: LocalTime) -> Option<Bitmap> {
    Some(match screen {
        Screen::Keyboard => return None,
        Screen::Text { text } => oled::text(text),
        Screen::Clock { seconds, h24, date } => {
            let (time, under) = clock_text(now, *seconds, *h24, *date);
            oled::clock(&time, under.as_deref())
        }
        Screen::Image { pixels } => Bitmap::from_rows(pixels).unwrap_or_default(),
    })
}

/// Whether the screen rests dark now: nobody is there, and it's a picture of gglinux's,
/// or the keyboard's own and care covers that too.
fn resting(screen: &Screen, care: &ScreenCare, away: bool) -> bool {
    away && care.dark_when_away && (care.own_screen_too || !matches!(screen, Screen::Keyboard))
}

/// Where the picture sits at `unix_secs`, as an offset from where it's drawn: a 5 × 5
/// square walked row by row, back and forth, a pixel every [`SHIFT_EVERY`] seconds. Each
/// lit pixel's wear is spread over up to 25 neighbours.
fn shift_at(unix_secs: u64) -> (i32, i32) {
    let step = (unix_secs / SHIFT_EVERY % 25) as i32;
    let (row, col) = (step / 5, step % 5);
    let x = if row % 2 == 0 { col } else { 4 - col };
    (x - 2, row - 2)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What the screen should show now, `None` for the keyboard's own: dark while resting,
/// otherwise the picture, moved as screen care says.
fn frame(state: &State) -> Option<Bitmap> {
    if resting(&state.screen, &state.care, state.away) {
        return Some(Bitmap::default());
    }
    let picture = picture(&state.screen, local_now())?;
    Some(if state.care.shift {
        let (dx, dy) = shift_at(unix_now());
        picture.shifted(dx, dy)
    } else {
        picture
    })
}

/// Draw the screen if its picture changed (or is due a refresh), or hand it back.
fn render_screen(state: &mut State) {
    state.resting = state.device.is_some() && resting(&state.screen, &state.care, state.away);
    let Some(device) = state.device.as_ref() else {
        return;
    };
    let Some(picture) = frame(state) else {
        state.screen_error = None;
        release_screen(state);
        return;
    };
    if state.screen_shown.as_ref() == Some(&picture)
        && !is_older_than(state.screen_last, SCREEN_REFRESH)
    {
        return;
    }
    match device.show_screen(&picture) {
        Ok(()) => {
            state.screen_taken = true;
            state.screen_error = None;
        }
        Err(e) => {
            let message = format!("{e:#}");
            if state.screen_error.as_ref() != Some(&message) {
                warn!(error = %message, "the keyboard did not take the screen picture");
            }
            state.screen_error = Some(message);
        }
    }
    // Kept even after a failure, so it's tried again at the refresh, not every loop.
    state.screen_shown = Some(picture);
    state.screen_last = Some(Instant::now());
}

/// Give the screen back to the keyboard's own menus, if gglinux drew on it.
fn release_screen(state: &mut State) {
    if !state.screen_taken {
        return;
    }
    if let Some(device) = state.device.as_ref() {
        match device.release_screen() {
            Ok(()) => debug!("keyboard screen handed back"),
            Err(e) => warn!(error = %format!("{e:#}"), "could not hand the keyboard screen back"),
        }
    }
    state.screen_taken = false;
    state.screen_shown = None;
}

fn snapshot(state: &State) -> KeyboardState {
    KeyboardState {
        connected: state.device.is_some(),
        layout: ANSI_TKL
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
        switches: state.switches.clone(),
        switches_error: match &state.switches_sent {
            Some(Err(e)) => Some(e.clone()),
            _ => None,
        },
        screen: state.screen.clone(),
        screen_error: state.screen_error.clone(),
        screen_care: state.care,
        screen_resting: state.resting,
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
        assert!(check_keys(&l).is_ok());
        l.mic_mute.key = 0x59; // keypad 1: not on a TKL
        assert!(check_keys(&l).is_err());
    }

    fn at(hour: u8, minute: u8, second: u8) -> LocalTime {
        LocalTime {
            hour,
            minute,
            second,
            weekday: 4,
            day: 29,
            month: 9,
        }
    }

    #[test]
    fn the_clock_reads_as_chosen() {
        let t = at(13, 5, 9);
        assert_eq!(clock_text(t, false, true, false), ("13:05".into(), None));
        assert_eq!(
            clock_text(t, true, true, true),
            ("13:05:09".into(), Some("Thu 29 Oct".into()))
        );
        assert_eq!(
            clock_text(t, false, false, false),
            ("1:05".into(), Some("PM".into()))
        );
        assert_eq!(
            clock_text(at(0, 30, 0), false, false, true),
            ("12:30".into(), Some("Thu 29 Oct AM".into())),
            "midnight is 12 AM"
        );
    }

    #[test]
    fn the_keyboard_s_own_screen_draws_nothing_and_a_clock_changes_each_second() {
        assert!(picture(&Screen::Keyboard, at(1, 2, 3)).is_none());
        let clock = Screen::Clock {
            seconds: true,
            h24: true,
            date: false,
        };
        assert_ne!(picture(&clock, at(1, 2, 3)), picture(&clock, at(1, 2, 4)));
        let minutes = Screen::Clock {
            seconds: false,
            h24: true,
            date: false,
        };
        assert_eq!(
            picture(&minutes, at(1, 2, 3)),
            picture(&minutes, at(1, 2, 4)),
            "without seconds, the same picture: nothing is sent"
        );
    }

    #[test]
    fn a_picture_is_drawn_from_its_rows() {
        let mut pixels = vec![0u8; 640];
        pixels[0] = 0x80;
        let b = picture(&Screen::Image { pixels }, at(0, 0, 0)).unwrap();
        assert!(b.get(0, 0) && !b.get(1, 0));
    }

    #[test]
    fn a_clock_wakes_the_loop_within_a_second_and_only_while_the_keyboard_is_here() {
        let clock = Screen::Clock {
            seconds: false,
            h24: true,
            date: false,
        };
        let wait = screen_wait(&clock, true).unwrap();
        assert!(wait > Duration::ZERO && wait <= Duration::from_millis(1005));
        assert!(screen_wait(&clock, false).is_none());
        assert!(screen_wait(&Screen::Keyboard, true).is_none());
    }

    #[test]
    fn switch_settings_may_only_name_keys_with_adjustable_switches() {
        let mut s = Switches::default();
        s.keys.push(gg_ipc::KeyActuation {
            code: 0x1A,
            actuation: 4,
        });
        assert!(check_analog(&s).is_ok());
        s.rapid_trigger.keys.push(0x29); // Escape
        assert_eq!(
            check_analog(&s).unwrap_err(),
            "Esc has a fixed switch: it can't be adjusted"
        );
        s.rapid_trigger.keys.clear();
        s.rapid_tap.pairs.push(gg_ipc::TapPair {
            a: 0x50,
            b: 0x4F,
            mode: TapMode::LastWins,
            report_both: false,
        });
        assert!(check_analog(&s).is_err(), "the arrows are fixed too");
    }

    #[test]
    fn the_screen_rests_only_while_away_and_the_keyboards_own_only_when_asked() {
        let clock = Screen::Clock {
            seconds: false,
            h24: true,
            date: false,
        };
        let care = ScreenCare::default();
        assert!(resting(&clock, &care, true));
        assert!(!resting(&clock, &care, false), "somebody's there");
        assert!(
            !resting(&Screen::Keyboard, &care, true),
            "the keyboard's own screen is left alone by default"
        );
        let own = ScreenCare {
            own_screen_too: true,
            ..care
        };
        assert!(resting(&Screen::Keyboard, &own, true));
        let never = ScreenCare {
            dark_when_away: false,
            own_screen_too: true,
            ..care
        };
        assert!(!resting(&clock, &never, true) && !resting(&Screen::Keyboard, &never, true));
    }

    #[test]
    fn the_shift_walks_a_five_by_five_square_a_pixel_at_a_time() {
        let steps: Vec<(i32, i32)> = (0..25).map(|i| shift_at(i * SHIFT_EVERY)).collect();
        let mut seen = steps.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 25, "every offset once a round: {steps:?}");
        assert!(steps.iter().all(|&(x, y)| x.abs() <= 2 && y.abs() <= 2));
        for pair in steps.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert_eq!((a.0 - b.0).abs() + (a.1 - b.1).abs(), 1, "{a:?} to {b:?}");
        }
        assert_eq!(
            shift_at(SHIFT_EVERY - 1),
            shift_at(0),
            "it holds for a whole step"
        );
        assert_eq!(shift_at(25 * SHIFT_EVERY), shift_at(0), "and starts over");
    }
}
