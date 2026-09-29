//! Keyboard worker: the Apex Pro TKL Gen 3's lighting.
//!
//! One OS thread owns the device, like the other workers. With gglinux lighting
//! on it sends frames — 30 a second for animated effects, on change (and a slow
//! refresh) for static ones; with it off, or when the daemon exits, it hands the
//! lighting back to the keyboard's onboard profile. Off by default: a fresh
//! install never touches the keyboard.

mod effects;
mod reader;
#[allow(dead_code)] // used from the capture task
mod spectrum;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gg_device_keyboard::layout::ANSI_TKL;
use gg_device_keyboard::{Keyboard, Rgb};
use gg_ipc::{Event, KeyInfo, KeyboardState, Lighting, ReactiveStatus};
use hidapi::HidApi;
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, error, info, warn};

use crate::clock::is_older_than;
use reader::{OpenError, Reader};

const FRAME: Duration = Duration::from_millis(33);
const STATIC_REFRESH: Duration = Duration::from_secs(2);
const REOPEN_INTERVAL: Duration = Duration::from_secs(2);
const IDLE_WAIT: Duration = Duration::from_millis(500);
/// The most key presses held for reactive lighting; older ones are dropped first.
const MAX_HITS: usize = 32;

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
    SetLighting { lighting: Lighting, reply: Reply<()> },
    /// A key went down on the Apex, as the reader saw it. Never logged.
    Pressed { code: u8, at: Instant },
    /// The reader ended by itself (keyboard gone, read error); it retries later.
    ReaderStopped,
    /// Something an overlay shows changed: draw soon. No reply.
    Wake,
    /// Hand the lighting back and stop. Replies once the keyboard has been told.
    Shutdown(Reply<()>),
}

/// Names only: a `Pressed` never prints its key.
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Command::GetState(_) => "GetState",
            Command::SetLighting { .. } => "SetLighting",
            Command::Pressed { .. } => "Pressed",
            Command::ReaderStopped => "ReaderStopped",
            Command::Wake => "Wake",
            Command::Shutdown(_) => "Shutdown",
        })
    }
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
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
}

fn worker(rx: Receiver<Command>, tx: Sender<Command>, event_tx: broadcast::Sender<Event>, overlays: Vec<Arc<dyn Overlay>>) {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            error!(error = %e, "keyboard worker could not initialise hidapi");
            return;
        }
    };
    let settings_path = gg_ipc::config_dir().join("lighting.json");
    let mut state = State {
        api,
        device: None,
        taken: false,
        last_open_attempt: None,
        last_frame: None,
        lighting: load(&settings_path),
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
                return;
            }
        }
        if state.device.is_none() {
            try_open(&mut state);
        }
        let now = Instant::now();
        state.layers = state.overlays.iter().filter_map(|o| o.paint(now)).collect();
        sync_reader(&mut state);
        if should_hand_back(state.taken, state.lighting.enabled, !state.layers.is_empty()) {
            hand_back(&mut state);
        }
        render(&mut state);
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
    if state.device.is_none() || !drawing(state.lighting.enabled, !state.layers.is_empty()) {
        return IDLE_WAIT;
    }
    state.last_frame.map_or(Duration::ZERO, |t| {
        interval(&state.lighting, !state.hits.is_empty(), !state.layers.is_empty()).saturating_sub(t.elapsed())
    })
}

/// Whether key presses should be read right now.
fn wants_reader(lighting: &Lighting, connected: bool) -> bool {
    lighting.enabled && lighting.reactive.enabled && connected
}

/// Whether gglinux draws on the keyboard: its own lighting, or an overlay taking it over.
fn drawing(lighting_enabled: bool, overlay_active: bool) -> bool {
    lighting_enabled || overlay_active
}

/// Give the keyboard back: gglinux holds it, but nothing wants to draw any more.
fn should_hand_back(taken: bool, lighting_enabled: bool, overlay_active: bool) -> bool {
    taken && !drawing(lighting_enabled, overlay_active)
}

/// The colour overlays give this key: the last active overlay that paints it.
fn overlay_color(layers: &[Layer], code: u8) -> Option<Rgb> {
    layers.iter().rev().find_map(|l| l.get(&code).copied())
}

/// A wake draws at once only if the last frame is at least a frame old, so a
/// game sending hundreds of events a second still gets 30 frames, not hundreds.
fn wake_forces_frame(last_frame: Option<Instant>) -> bool {
    is_older_than(last_frame, FRAME)
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
        Command::SetLighting { lighting, reply } => {
            if let Err(message) = lighting.validate() {
                let _ = reply.send(Err(message));
                return true;
            }
            state.lighting = lighting;
            save(&state.settings_path, &state.lighting);
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
        Command::ReaderStopped => {
            state.reader = None;
            state.reactive = ReactiveStatus::Off;
            state.last_reader_attempt = Some(Instant::now()); // retry after REOPEN_INTERVAL
        }
        Command::Shutdown(reply) => {
            hand_back(state);
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
    if state.api.refresh_devices().is_err() {
        return;
    }
    match Keyboard::open(&state.api) {
        Ok(keyboard) => {
            info!("keyboard connected");
            state.device = Some(keyboard);
            state.taken = false;
            state.last_frame = None;
        }
        Err(e) => debug!(error = %format!("{e:#}"), "keyboard not available"),
    }
}

/// Send a frame if one is due.
fn render(state: &mut State) {
    let overlay_active = !state.layers.is_empty();
    if !drawing(state.lighting.enabled, overlay_active)
        || !is_older_than(state.last_frame, interval(&state.lighting, !state.hits.is_empty(), overlay_active))
    {
        return;
    }
    let t = state.started.elapsed().as_secs_f64();
    let duration = f64::from(state.lighting.reactive.duration_s);
    state.hits.retain(|h| t - h.t < duration);
    let hits = &state.hits;
    let layers = &state.layers;
    let Some(device) = state.device.as_ref() else { return };
    let result = if state.taken { Ok(()) } else { device.init() }
        .and_then(|()| {
            device.show(|code| match overlay_color(layers, code) {
                Some(c) => effects::dim(c, state.lighting.brightness),
                None => effects::color(&state.lighting, &state.positions, code, t, hits),
            })
        });
    match result {
        Ok(()) => {
            state.taken = true;
            state.last_frame = Some(Instant::now());
        }
        Err(e) => {
            info!(error = %format!("{e:#}"), "keyboard disconnected");
            state.device = None;
            state.taken = false;
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
        layout: ANSI_TKL
            .iter()
            .map(|&(code, label, x, y, w)| KeyInfo { code, label: label.to_owned(), x, y, w })
            .collect(),
        lighting: state.lighting.clone(),
        reactive: state.reactive,
    }
}

fn publish(state: &mut State, event_tx: &broadcast::Sender<Event>) {
    let now = snapshot(state);
    if state.published.as_ref() != Some(&now) {
        state.published = Some(now.clone());
        let _ = event_tx.send(Event::KeyboardChanged { state: now });
    }
}

/// Saved lighting; a missing or unreadable file means the defaults (lighting off).
fn load(path: &Path) -> Lighting {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            warn!(path = %path.display(), error = %e, "ignoring unreadable lighting settings");
            Lighting::default()
        }),
        Err(_) => Lighting::default(),
    }
}

/// Save atomically; a failure is logged, never fatal.
fn save(path: &Path, lighting: &Lighting) {
    let write = || -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(lighting)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    if let Err(e) = write() {
        warn!(path = %path.display(), error = %format!("{e:#}"), "could not save lighting settings");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::{Effect, Reactive};

    fn lighting(enabled: bool, reactive: bool, effect: Effect) -> Lighting {
        Lighting { enabled, effect, reactive: Reactive { enabled: reactive, ..Reactive::default() }, ..Lighting::default() }
    }

    #[test]
    fn the_reader_runs_only_with_lighting_and_reactive_on_and_the_keyboard_here() {
        assert!(wants_reader(&lighting(true, true, Effect::Static), true));
        assert!(!wants_reader(&lighting(false, true, Effect::Static), true), "lighting off");
        assert!(!wants_reader(&lighting(true, false, Effect::Static), true), "reactive off");
        assert!(!wants_reader(&lighting(true, true, Effect::Static), false), "keyboard away");
    }

    #[test]
    fn at_most_32_presses_are_kept_oldest_dropped_first() {
        let mut hits = Vec::new();
        for i in 0..40u8 {
            push_hit(&mut hits, effects::Hit { code: i, t: f64::from(i) });
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
        assert_eq!(interval(&lighting(true, true, Effect::Wave { period_s: 4.0 }), false, false), FRAME);
    }

    #[test]
    fn gglinux_draws_for_its_own_lighting_or_an_active_overlay_and_hands_back_otherwise() {
        assert!(drawing(true, false));
        assert!(drawing(false, true), "a game takes the keyboard over");
        assert!(!drawing(false, false));
        assert!(should_hand_back(true, false, false), "held, but nothing wants it");
        assert!(!should_hand_back(true, false, true), "a game still draws");
        assert!(!should_hand_back(true, true, false), "gglinux lighting still draws");
        assert!(!should_hand_back(false, false, false), "not held");
    }

    #[test]
    fn the_last_active_overlay_that_paints_a_key_wins() {
        let red = gg_device_keyboard::Rgb { r: 255, g: 0, b: 0 };
        let blue = gg_device_keyboard::Rgb { r: 0, g: 0, b: 255 };
        let layers = vec![Layer::from([(0x04, red), (0x05, red)]), Layer::from([(0x05, blue)])];
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
    fn wake_forces_a_frame_at_most_once_per_frame_interval() {
        assert!(wake_forces_frame(None));
        assert!(!wake_forces_frame(Some(Instant::now())), "a frame just went out: wait for the cadence");
        assert!(wake_forces_frame(Some(Instant::now() - FRAME)));
    }
}
