// SPDX-License-Identifier: GPL-3.0-or-later
//! The connection to `gglinuxd`.
//!
//! A task on the app's [`runtime`] holds the socket, reconnects every second
//! while the daemon is away, and keeps [`State`]: the daemon's latest answer for
//! every part, kept current by its notifications. Whenever something in it
//! changes, `on_change` is called with what changed (on that runtime; the UI hops
//! to its own).
//!
//! Answers and notifications change the state as they're read, in the order the
//! daemon sent them: an answer that carries a part's state is kept before whoever
//! asked hears of it, so an older answer can't land after a newer notification
//! and undo it.
//!
//! [`Client::command`] sends a change and, once the daemon says it worked, reads
//! the part's state again: the daemon owns the record of what's applied, and only
//! it knows whether a write landed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use gg_ipc::{
    ActionsState, BindingsState, ClipsState, DeviceInfo, Event, Experimental, GameSenseState,
    HeadsetState, KeyboardState, MouseState, OnboardState, Part, Parts, ProfilesState, Request,
    RequestFrame, ResponseData, ServerMessage, SonarState,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

/// How long to wait before trying a refused or dropped connection again.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// How long a request may wait for its answer.
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// For requests that restart PipeWire or the recorder.
pub const SLOW: Duration = Duration::from_secs(15);
/// After a change of parts: how long the daemon has to start again before the
/// parts are read anyway (it never left) or, still away after twice this, the
/// window stops saying it's coming back.
const RESTART_WAIT: Duration = Duration::from_secs(10);

/// A request's error when the daemon left while it waited for the answer.
pub const GONE: &str = "gglinuxd went away";
/// A request's error when the daemon wasn't there to send it to.
pub const AWAY: &str = "gglinuxd isn't running";

/// Whether a request failed only because the daemon isn't there. The window
/// shows the offline view then, and the panel is drawn afresh when it's back.
pub fn lost_connection(error: &str) -> bool {
    error == GONE || error == AWAY
}

/// The runtime for everything that waits on a socket: the daemon's, the session
/// bus, the tray. It lives as long as the app and is never shut down, so quitting
/// doesn't wait for it.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("gglinux-io")
            .enable_all()
            .build()
            .expect("a tokio runtime")
    })
}

/// What the daemon last said. `None` until read, and again while it's away.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub connected: bool,
    pub devices: Vec<DeviceInfo>,
    pub mouse: Option<MouseState>,
    pub headset: Option<HeadsetState>,
    pub sonar: Option<SonarState>,
    pub keyboard: Option<KeyboardState>,
    pub gamesense: Option<GameSenseState>,
    pub profiles: Option<ProfilesState>,
    pub clips: Option<ClipsState>,
    pub experimental: Option<Experimental>,
    pub bindings: Option<BindingsState>,
    pub onboard: Option<OnboardState>,
    pub actions: Option<ActionsState>,
    pub macros: Option<gg_ipc::MacrosState>,
    /// Kept while the daemon is away: a restart for a change of parts shouldn't
    /// bring the hidden pages back for a moment.
    pub parts: Option<Parts>,
    /// A change of parts was sent and the daemon is starting again with it.
    pub restarting: bool,
}

impl State {
    /// The parts that run; all of them for a daemon too old to say.
    pub fn running(&self) -> Parts {
        self.parts.unwrap_or_default().running()
    }

    /// Keep an answer that carries a state; what that changed.
    fn keep(&mut self, answer: &ResponseData) -> Option<Changed> {
        Some(match answer {
            ResponseData::Devices { devices } => {
                self.devices = devices.clone();
                Changed::Devices
            }
            ResponseData::Mouse { state } => {
                self.mouse = Some(state.clone());
                Changed::Mouse
            }
            ResponseData::Headset { state } => {
                self.headset = Some(state.clone());
                Changed::Headset
            }
            ResponseData::Sonar { state } => {
                self.sonar = Some(state.clone());
                Changed::Sonar
            }
            ResponseData::Keyboard { state } => {
                self.keyboard = Some(state.clone());
                Changed::Keyboard
            }
            ResponseData::Gamesense { state } => {
                self.gamesense = Some(state.clone());
                Changed::Gamesense
            }
            ResponseData::Profiles { state } => {
                self.profiles = Some(state.clone());
                Changed::Profiles
            }
            ResponseData::Clips { state } => {
                self.clips = Some(state.clone());
                Changed::Clips
            }
            ResponseData::Parts { parts } => {
                // A daemon that answers is up: a restart for new parts is over.
                self.parts = Some(*parts);
                self.restarting = false;
                Changed::Parts
            }
            ResponseData::Experimental { experimental } => {
                self.experimental = Some(*experimental);
                Changed::Experimental
            }
            ResponseData::Bindings { state } => {
                self.bindings = Some(state.clone());
                Changed::Bindings
            }
            ResponseData::Onboard { state } => {
                self.onboard = Some(state.clone());
                Changed::Onboard
            }
            ResponseData::Actions { state } => {
                self.actions = Some(state.clone());
                Changed::Actions
            }
            ResponseData::Macros { state } => {
                self.macros = Some(state.clone());
                Changed::Macros
            }
            ResponseData::Pong | ResponseData::Success | ResponseData::Error { .. } => return None,
        })
    }

    /// Apply a notification; what that changed.
    fn apply(&mut self, event: Event) -> Changed {
        match event {
            Event::DevicesChanged { devices } => {
                self.devices = devices;
                Changed::Devices
            }
            Event::DeviceAdded(device) => {
                // Replace or append, so a second announcement doesn't list it twice.
                self.devices.retain(|d| d.id != device.id);
                self.devices.push(device);
                self.devices.sort_by(|a, b| a.id.cmp(&b.id));
                Changed::Devices
            }
            Event::DeviceRemoved { id } => {
                self.devices.retain(|d| d.id != id);
                Changed::Devices
            }
            Event::MouseBattery { battery } => {
                // Dropped until the state is read: the battery alone isn't one.
                if let Some(mouse) = self.mouse.as_mut() {
                    mouse.battery = battery;
                }
                Changed::Mouse
            }
            Event::HeadsetChanged { state } => {
                self.headset = Some(state);
                Changed::Headset
            }
            Event::SonarChanged { state } => {
                self.sonar = Some(state);
                Changed::Sonar
            }
            Event::KeyboardChanged { state } => {
                self.keyboard = Some(state);
                Changed::Keyboard
            }
            Event::GamesenseChanged { state } => {
                self.gamesense = Some(state);
                Changed::Gamesense
            }
            Event::ProfilesChanged { mut state } => {
                // Notifications carry no list of running programs; keep the last one read.
                state.running = self.profiles.take().map(|p| p.running).unwrap_or_default();
                self.profiles = Some(state);
                Changed::Profiles
            }
            Event::ClipsChanged { state } => {
                self.clips = Some(state);
                Changed::Clips
            }
            Event::ExperimentalChanged { experimental } => {
                self.experimental = Some(experimental);
                Changed::Experimental
            }
            Event::BindingsChanged { state } => {
                self.bindings = Some(state);
                Changed::Bindings
            }
            Event::OnboardChanged { state } => {
                self.onboard = Some(state);
                Changed::Onboard
            }
            Event::ActionsChanged { state } => {
                self.actions = Some(state);
                Changed::Actions
            }
            Event::MacrosChanged { state } => {
                self.macros = Some(state);
                Changed::Macros
            }
        }
    }

    /// The daemon went away. What a daemon process applied is that process's:
    /// one that starts again has applied nothing yet. Only the parts stay.
    fn lose(&mut self) {
        *self = State {
            parts: self.parts,
            restarting: self.restarting,
            ..State::default()
        };
    }
}

/// What changed, so the UI only redoes that part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Changed {
    Connection,
    Devices,
    Mouse,
    Headset,
    Sonar,
    Keyboard,
    Gamesense,
    Profiles,
    Clips,
    Parts,
    Experimental,
    Bindings,
    Onboard,
    Actions,
    Macros,
}

impl Changed {
    pub const ALL: [Changed; 15] = [
        Changed::Connection,
        Changed::Devices,
        Changed::Mouse,
        Changed::Headset,
        Changed::Sonar,
        Changed::Keyboard,
        Changed::Gamesense,
        Changed::Profiles,
        Changed::Clips,
        Changed::Parts,
        Changed::Experimental,
        Changed::Bindings,
        Changed::Onboard,
        Changed::Actions,
        Changed::Macros,
    ];
}

type Reply = oneshot::Sender<Result<ResponseData, String>>;
type OnChange = Arc<dyn Fn(Changed, State) + Send + Sync>;

struct Outgoing {
    req: Request,
    reply: Reply,
}

struct Shared {
    state: Mutex<State>,
    on_change: OnChange,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Change the state and say what changed. `on_change` runs with the lock
    /// held, so changes made from different threads reach the UI in the order
    /// they were made; it only hands them on, and never calls back in.
    fn update(&self, f: impl FnOnce(&mut State) -> Option<Changed>) {
        let mut s = self.state();
        if let Some(what) = f(&mut s) {
            (self.on_change)(what, s.clone());
        }
    }

    /// The daemon went away: everything it said goes, but the parts.
    fn lose(&self) {
        let mut s = self.state();
        s.lose();
        for what in Changed::ALL {
            if what != Changed::Parts {
                (self.on_change)(what, s.clone());
            }
        }
    }
}

/// A handle to the connection; cheap to clone. The connection stops when the
/// last one is dropped.
#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
    tx: mpsc::UnboundedSender<Outgoing>,
}

impl Client {
    /// Start the connection. `on_change` gets what changed and the state after
    /// it; it's called with the state locked, so it must only pass them on.
    pub fn start(on_change: impl Fn(Changed, State) + Send + Sync + 'static) -> Client {
        Client::start_at(gg_ipc::socket_path(), on_change)
    }

    /// [`Client::start`], to the daemon at `path`.
    pub fn start_at(
        path: PathBuf,
        on_change: impl Fn(Changed, State) + Send + Sync + 'static,
    ) -> Client {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = Client {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                on_change: Arc::new(on_change),
            }),
            tx,
        };
        let shared = client.shared.clone();
        let weak = client.tx.downgrade();
        runtime().spawn(run(shared, weak, path, rx));
        client
    }

    /// Run a task on the connection's runtime (the file chooser, say).
    pub fn spawn(&self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        runtime().spawn(task);
    }

    /// A copy of what the daemon last said.
    pub fn state(&self) -> State {
        self.shared.state().clone()
    }

    /// Send a request; `done` gets the answer, on the connection's runtime.
    pub fn request(
        &self,
        req: Request,
        timeout: Duration,
        done: impl FnOnce(Result<ResponseData, String>) + Send + 'static,
    ) {
        let this = self.clone();
        runtime().spawn(async move { done(this.ask(req, timeout).await) });
    }

    /// Send a change; once it worked, read its part's state again, then call
    /// `done`. An answer other than `success` is an error.
    pub fn command(
        &self,
        req: Request,
        timeout: Duration,
        done: impl FnOnce(Result<(), String>) + Send + 'static,
    ) {
        let this = self.clone();
        runtime().spawn(async move {
            let part = req.part();
            let result = success(this.ask(req, timeout).await);
            if result.is_ok() {
                if let Some(part) = part {
                    this.refresh(part).await;
                }
            }
            done(result);
        });
    }

    /// Read a part's state again.
    pub fn refresh_part(&self, part: Part) {
        let this = self.clone();
        runtime().spawn(async move { this.refresh(part).await });
    }

    /// Save which parts run. A change makes the daemon start again with them;
    /// the reconnect reads them then (or, if no restart came, a later read does).
    pub fn set_parts(&self, parts: Parts, done: impl FnOnce(Result<(), String>) + Send + 'static) {
        let this = self.clone();
        runtime().spawn(async move {
            let changed = this.state().parts != Some(parts);
            let result = success(this.ask(Request::SetParts { parts }, TIMEOUT).await);
            if result.is_ok() && changed {
                this.shared.update(|s| {
                    s.restarting = true;
                    Some(Changed::Parts)
                });
                runtime().spawn(this.clone().after_restart(RESTART_WAIT));
            }
            done(result);
        });
    }

    /// Send a request and wait for the answer, as the daemon gave it.
    async fn ask_raw(&self, req: Request, timeout: Duration) -> Result<ResponseData, String> {
        let method = method(&req);
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Outgoing { req, reply })
            .map_err(|_| "the connection to gglinuxd stopped".to_string())?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(GONE.into()),
            Err(_) => Err(format!("{method}: gglinuxd didn't answer")),
        }
    }

    /// [`Client::ask_raw`], with the daemon's `error` answer as the error.
    async fn ask(&self, req: Request, timeout: Duration) -> Result<ResponseData, String> {
        match self.ask_raw(req, timeout).await? {
            ResponseData::Error { message } => Err(message),
            answer => Ok(answer),
        }
    }

    async fn refresh(&self, part: Part) {
        let req = match part {
            Part::Mouse => Request::GetMouseState,
            Part::Headset => Request::GetHeadsetState,
            Part::Sonar => Request::GetSonarState,
            Part::Keyboard => Request::GetKeyboardState,
            Part::Gamesense => Request::GetGamesenseState,
            Part::Profiles => Request::GetProfiles,
            Part::Clips => Request::GetClipsState,
        };
        // The answer was kept as it was read.
        let _ = self.ask(req, TIMEOUT).await;
    }

    async fn read_parts(&self) {
        match self.ask_raw(Request::GetParts, TIMEOUT).await {
            // Kept as it was read.
            Ok(ResponseData::Parts { .. }) => {}
            // A daemon from before parts runs them all.
            Ok(_) => self.shared.update(|s| {
                s.parts = Some(Parts::default());
                s.restarting = false;
                Some(Changed::Parts)
            }),
            // No answer says nothing about the parts: what was known stands, and
            // the next connection asks again.
            Err(_) => {}
        }
    }

    /// What every page needs, read as soon as the daemon is there: the parts
    /// first, to know which states to read, then the rest at once.
    async fn warm_up(self) {
        self.read_parts().await;
        let running = self.state().running();
        let mut reads = tokio::task::JoinSet::new();
        let this = self.clone();
        reads.spawn(async move {
            let _ = this.ask(Request::ListDevices, TIMEOUT).await;
        });
        for part in Part::ALL.into_iter().filter(|p| running.on(*p)) {
            let this = self.clone();
            reads.spawn(async move { this.refresh(part).await });
        }
        // States outside the parts, and the keyboard's others. A daemon from before
        // they existed answers with an error, which leaves them unknown.
        let mut extra = vec![Request::GetExperimental, Request::GetActions];
        if running.keyboard {
            extra.extend([
                Request::GetBindings,
                Request::GetOnboard,
                Request::GetMacros,
            ]);
        }
        for req in extra {
            let this = self.clone();
            reads.spawn(async move {
                let _ = this.ask(req, TIMEOUT).await;
            });
        }
        while reads.join_next().await.is_some() {}
    }

    /// A change of parts was sent: if the daemon didn't come back with them
    /// in time, find out why not.
    async fn after_restart(self, wait: Duration) {
        tokio::time::sleep(wait).await;
        let (restarting, connected) = {
            let s = self.shared.state();
            (s.restarting, s.connected)
        };
        if !restarting {
            return;
        }
        if connected {
            // No restart came (it had these parts already), or the reconnect's read
            // of them was lost.
            self.read_parts().await;
            return;
        }
        tokio::time::sleep(wait).await;
        // Still away: it isn't coming back by itself, so stop saying it is.
        self.shared.update(|s| {
            if s.restarting && !s.connected {
                s.restarting = false;
                Some(Changed::Parts)
            } else {
                None
            }
        });
    }
}

/// `success`, or the error.
fn success(answer: Result<ResponseData, String>) -> Result<(), String> {
    match answer? {
        ResponseData::Success => Ok(()),
        other => Err(format!("unexpected answer: {}", kind(&other))),
    }
}

/// Connect, serve the connection while it lasts, and try again a second after
/// it's gone; until the last [`Client`] is dropped.
async fn run(
    shared: Arc<Shared>,
    tx: mpsc::WeakUnboundedSender<Outgoing>,
    path: PathBuf,
    mut rx: mpsc::UnboundedReceiver<Outgoing>,
) {
    loop {
        if let Ok(stream) = UnixStream::connect(&path).await {
            let open = serve(&shared, &tx, stream, &mut rx).await;
            shared.lose();
            if !open {
                return;
            }
        }
        // While it's away, requests fail at once instead of waiting.
        let deadline = tokio::time::sleep(RECONNECT_DELAY);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                out = rx.recv() => match out {
                    Some(out) => {
                        let _ = out.reply.send(Err(AWAY.into()));
                    }
                    None => return,
                },
            }
        }
    }
}

/// One connection, until the daemon goes or (`false`) every [`Client`] has.
async fn serve(
    shared: &Arc<Shared>,
    tx: &mpsc::WeakUnboundedSender<Outgoing>,
    stream: UnixStream,
    rx: &mut mpsc::UnboundedReceiver<Outgoing>,
) -> bool {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let mut pending: HashMap<u64, Reply> = HashMap::new();
    let mut next_id = 1u64;
    // Requests go out from a task of their own, so reading never waits on a
    // write: a daemon stuck writing to this side while this side is stuck writing
    // to it would be stuck for good.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let mut writer = tokio::spawn(async move {
        while let Some(line) = out_rx.recv().await {
            if write.write_all(line.as_bytes()).await.is_err() {
                return;
            }
        }
    });
    shared.update(|s| {
        s.connected = true;
        Some(Changed::Connection)
    });
    let warmup = tx.upgrade().map(|tx| {
        let client = Client {
            shared: shared.clone(),
            tx,
        };
        tokio::spawn(client.warm_up())
    });

    let open = loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => read_line(shared, &mut pending, &line),
                _ => break true,
            },
            // A write failed: the socket is broken.
            _ = &mut writer => break true,
            out = rx.recv() => {
                let Some(out) = out else { break false };
                let id = next_id;
                next_id += 1;
                let frame = RequestFrame { id, req: out.req };
                let mut line = serde_json::to_string(&frame).expect("a request serializes");
                line.push('\n');
                pending.insert(id, out.reply);
                let _ = out_tx.send(line);
                // Drop the replies nobody waits for any more.
                pending.retain(|_, r| !r.is_closed());
            }
        }
    };
    writer.abort();
    if let Some(warmup) = warmup {
        warmup.abort();
    }
    // Dropping the replies tells whoever waits that the daemon went away.
    drop(pending);
    open
}

/// One line from the daemon: keep what it says, then hand an answer to whoever
/// asked.
fn read_line(shared: &Shared, pending: &mut HashMap<u64, Reply>, line: &str) {
    match serde_json::from_str::<ServerMessage>(line) {
        Ok(ServerMessage::Response { id, result }) => {
            shared.update(|s| s.keep(&result));
            // None: the answer to a request that already timed out.
            if let Some(reply) = pending.remove(&id) {
                let _ = reply.send(Ok(result));
            }
        }
        Ok(ServerMessage::Notification { event }) => shared.update(|s| Some(s.apply(event))),
        // A newer daemon's message this build can't read. If it's an answer, say
        // so now rather than wait out the request's timer.
        Err(_) => {
            if let Some(reply) = unreadable_answer(line).and_then(|id| pending.remove(&id)) {
                let _ = reply.send(Err(
                    "gglinuxd answered in a way this version of the app can't read".into(),
                ));
            }
        }
    }
}

/// The request id of an answer that didn't parse.
fn unreadable_answer(line: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("kind")?.as_str()? != "response" {
        return None;
    }
    v.get("id")?.as_u64()
}

/// The request's name in the protocol, for messages.
pub fn method(req: &Request) -> String {
    serde_json::to_value(req)
        .ok()
        .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "request".into())
}

fn kind(answer: &ResponseData) -> String {
    serde_json::to_value(answer)
        .ok()
        .and_then(|v| v.get("type").and_then(|m| m.as_str()).map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
