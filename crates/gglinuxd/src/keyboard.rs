//! Keyboard worker: the Apex Pro TKL Gen 3's lighting.
//!
//! One OS thread owns the device, like the other workers. With gglinux lighting
//! on it sends frames — 30 a second for animated effects, on change (and a slow
//! refresh) for static ones; with it off, or when the daemon exits, it hands the
//! lighting back to the keyboard's onboard profile. Off by default: a fresh
//! install never touches the keyboard.

mod effects;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use gg_device_keyboard::layout::ANSI_TKL;
use gg_device_keyboard::Keyboard;
use gg_ipc::{Event, KeyInfo, KeyboardState, Lighting};
use hidapi::HidApi;
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, error, info, warn};

use crate::clock::is_older_than;

const FRAME: Duration = Duration::from_millis(33);
const STATIC_REFRESH: Duration = Duration::from_secs(2);
const REOPEN_INTERVAL: Duration = Duration::from_secs(2);
const IDLE_WAIT: Duration = Duration::from_millis(500);

type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum Command {
    GetState(Reply<KeyboardState>),
    SetLighting { lighting: Lighting, reply: Reply<()> },
    /// Hand the lighting back and stop. Replies once the keyboard has been told.
    Shutdown(Reply<()>),
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
}

pub fn spawn(event_tx: broadcast::Sender<Event>) -> Handle {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("gg-keyboard".into())
        .spawn(move || worker(rx, event_tx))
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
    positions: HashMap<u8, f32>,
    started: Instant,
    published: Option<KeyboardState>,
}

fn worker(rx: Receiver<Command>, event_tx: broadcast::Sender<Event>) {
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
    };

    loop {
        match rx.recv_timeout(next_wait(&state)) {
            Ok(command) => {
                if !handle(command, &mut state, &event_tx) {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                hand_back(&mut state);
                return;
            }
        }
        if state.device.is_none() {
            try_open(&mut state);
        }
        render(&mut state);
        publish(&mut state, &event_tx);
    }
}

/// How often frames are due for the current effect.
fn interval(lighting: &Lighting) -> Duration {
    if effects::animated(&lighting.effect) {
        FRAME
    } else {
        STATIC_REFRESH
    }
}

/// How long to wait for a command before the next frame is due.
fn next_wait(state: &State) -> Duration {
    if state.device.is_none() || !state.lighting.enabled {
        return IDLE_WAIT;
    }
    state
        .last_frame
        .map_or(Duration::ZERO, |t| interval(&state.lighting).saturating_sub(t.elapsed()))
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
            let turned_off = state.lighting.enabled && !lighting.enabled;
            state.lighting = lighting;
            save(&state.settings_path, &state.lighting);
            if turned_off {
                hand_back(state);
            }
            state.last_frame = None; // draw the change now
            let _ = reply.send(Ok(()));
            publish(state, event_tx);
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
    if !state.lighting.enabled || !is_older_than(state.last_frame, interval(&state.lighting)) {
        return;
    }
    let Some(device) = state.device.as_ref() else { return };
    let t = state.started.elapsed().as_secs_f32();
    let result = if state.taken { Ok(()) } else { device.init() }
        .and_then(|()| device.show(|code| effects::color(&state.lighting, &state.positions, code, t)));
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
