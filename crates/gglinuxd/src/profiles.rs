// SPDX-License-Identifier: GPL-3.0-or-later
//! Profiles: saved settings for any of six sections (headset, mouse, keyboard switches, key
//! bindings, keyboard screen, lighting), gglinux's own presets, and rules that switch to a
//! profile while a program runs or while its window has the focus.
//!
//! A tokio task answers the requests one at a time, scans running programs every 2 s while
//! there's anything to watch for, and follows the focused window ([`focus`]) while a rule acts
//! on focus. Sections are read and applied through the daemon's own request handlers
//! ([`Daemon`], wired in by the routing arm, [`serve`]), so each one is gated as the user's
//! own request for it. When a rule's profile goes on, your own settings for the sections it
//! replaces are kept (the Default) and put back when no rule holds any more. The program from
//! the rules that's running is also published for games-only clips.

mod daemon;
mod focus;
mod presets;
mod procs;
mod sections;
mod store;
#[cfg(test)]
mod tests;

pub use daemon::{connect, serve};
pub use sections::Daemon;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gg_ipc::{
    Applied, Event, FocusStatus, FocusedWindow, GameRule, PresetInfo, Profile, ProfileFile,
    ProfilesState, Request, ResponseData, Section,
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tracing::{info, warn};

use crate::keyboard;
use focus::{Debounce, FocusEvent, Source};
use sections::{Outcome, Sent};
use store::{decide, pick, Action, Seen, Store, Window, DEFAULT};

const SCAN_EVERY: Duration = Duration::from_secs(2);
/// How long after a focus source stopped, or wasn't found, it's looked for again.
const RETRY_FOCUS: Duration = Duration::from_secs(10);

type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum Command {
    GetState(Reply<ProfilesState>),
    /// Apply a saved profile (the actions on the spare keys use this).
    Apply { name: String, reply: Reply<()> },
    /// Any other profiles request, answered as the client gets it.
    Request(Request, oneshot::Sender<ResponseData>),
    /// The daemon's request handlers, which sections are applied through.
    Connect(Arc<dyn Daemon>),
}

#[derive(Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Command>,
    connected: Arc<AtomicBool>,
}

impl Handle {
    fn new(tx: mpsc::UnboundedSender<Command>) -> Handle {
        Handle {
            tx,
            connected: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A handle with no worker, for when this part is off: every send fails.
    pub fn off() -> Handle {
        let (tx, _) = mpsc::unbounded_channel();
        Handle::new(tx)
    }

    pub async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| "profiles are not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "profiles dropped the request".to_string())?
    }

    /// Answer a profiles request.
    pub async fn ask(&self, req: Request) -> ResponseData {
        let (reply_tx, reply_rx) = oneshot::channel();
        let gone = |what: &str| ResponseData::Error {
            message: what.to_string(),
        };
        if self.tx.send(Command::Request(req, reply_tx)).is_err() {
            return gone("profiles are not running");
        }
        reply_rx
            .await
            .unwrap_or_else(|_| gone("profiles dropped the request"))
    }

    /// Give profiles the daemon's request handlers; only the first call sends them.
    pub fn connect(&self, daemon: impl FnOnce() -> Arc<dyn Daemon>) {
        if !self.connected.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(Command::Connect(daemon()));
        }
    }
}

/// The focused window as reported, as the panel shows it and as rules see it.
#[derive(Debug, Clone, PartialEq)]
struct Focused {
    shown: FocusedWindow,
    seen: Window,
}

pub struct Controller {
    store: Store,
    path: PathBuf,
    daemon: Option<Arc<dyn Daemon>>,
    event_tx: broadcast::Sender<Event>,
    /// The rule applied now; your own settings to go back to are in the store.
    active: Option<GameRule>,
    /// The program from the rules that's running now, for games-only clips.
    game_tx: watch::Sender<Option<String>>,
    published: Option<ProfilesState>,
    presets: Vec<PresetInfo>,
    applied: Option<Applied>,
    /// Lower-cased names of the programs running, as last scanned.
    running: HashSet<String>,
    /// While a desktop reports the focused window: the window, or `None` when none has the
    /// focus. `None` while it isn't followed.
    focus: Option<Option<Focused>>,
    /// The window just reported, until it has held for [`focus::DEBOUNCE`].
    pending: Option<Option<Focused>>,
    debounce: Debounce,
    focus_status: FocusStatus,
    sent: Sent,
    /// How long what was sent counts over what's read back.
    grace: Duration,
    proc_root: PathBuf,
}

/// Said while focus rules act while their program runs instead.
const NO_FOCUS: &str = "No desktop here reports the focused window (Hyprland does, and KDE \
                        Plasma does with gglinux's KWin script), so rules on focus act while their \
                        program runs.";
const WAITING_FOR_KWIN: &str = "Waiting for gglinux's KWin script to report the focused window; \
                                until it does, rules on focus act while their program runs.";

impl Controller {
    pub fn new(
        store: Store,
        path: PathBuf,
        event_tx: broadcast::Sender<Event>,
        game_tx: watch::Sender<Option<String>>,
    ) -> Self {
        Controller {
            store,
            path,
            daemon: None,
            event_tx,
            active: None,
            game_tx,
            published: None,
            presets: presets::infos(),
            applied: None,
            running: HashSet::new(),
            focus: None,
            pending: None,
            debounce: Debounce::default(),
            focus_status: FocusStatus::default(),
            sent: Sent::default(),
            grace: sections::GRACE,
            proc_root: PathBuf::from("/proc"),
        }
    }

    pub fn connect(&mut self, daemon: Arc<dyn Daemon>) {
        self.daemon = Some(daemon);
    }

    /// The programs running now (any case).
    pub fn set_running<'a>(&mut self, names: impl IntoIterator<Item = &'a String>) {
        self.running = names.into_iter().map(|n| n.to_lowercase()).collect();
    }

    fn resolve(&self, w: focus::Window) -> Focused {
        let names = w
            .pid
            .map(|pid| procs::program_names(&self.proc_root, pid))
            .unwrap_or_default();
        Focused {
            shown: FocusedWindow {
                class: w.class.clone(),
                program: names.first().cloned(),
            },
            seen: Window {
                class: w.class.to_lowercase(),
                names: names.into_iter().collect(),
            },
        }
    }

    /// What a focus source said, at `now`. A window counts once it has held for
    /// [`focus::DEBOUNCE`] ([`Controller::focus_due`]); a source that stops counts at once.
    /// Whether rules need checking again now.
    pub fn on_focus(&mut self, event: FocusEvent, now: Instant) -> bool {
        match event {
            FocusEvent::Up(name) => {
                self.focus_status = FocusStatus {
                    source: Some(name.to_string()),
                    ..FocusStatus::default()
                };
                self.publish();
                false
            }
            FocusEvent::Window(w) => {
                let w = w.map(|w| self.resolve(w));
                self.focus_status.window = w.as_ref().map(|f| f.shown.clone());
                self.pending = Some(w);
                self.debounce.moved(now);
                self.publish();
                false
            }
            FocusEvent::Down(why) => {
                info!(%why, "the focused window isn't followed any more");
                self.focus_lost(Some(format!(
                    "Following the focused window stopped ({why}), so rules on focus act while \
                     their program runs until it's back."
                )));
                true
            }
        }
    }

    /// The focused window isn't followed any more; `note` says why, if it matters.
    fn focus_lost(&mut self, note: Option<String>) {
        self.focus = None;
        self.pending = None;
        self.debounce = Debounce::default();
        self.focus_status = FocusStatus {
            note,
            ..FocusStatus::default()
        };
    }

    /// Whether a reported window has held long enough; it's then the focused one.
    pub fn focus_due(&mut self, now: Instant) -> bool {
        if !self.debounce.due(now) {
            return false;
        }
        if let Some(w) = self.pending.take() {
            self.focus = Some(w);
        }
        true
    }

    /// Apply or undo rules for what runs and what has the focus.
    pub async fn follow(&mut self) {
        let focus = self
            .focus
            .as_ref()
            .map(|w| w.as_ref().map(|f| &f.seen));
        let seen = Seen {
            running: &self.running,
            focus,
        };
        let picked = pick(&self.store, &seen).cloned();
        // Before the sections, which can fail: clips follow the game regardless.
        let game = store::running_game(&self.store, &self.running)
            .or(picked.as_ref())
            .map(|r| r.process.clone());
        self.game_tx.send_if_modified(|now| {
            if *now == game {
                return false;
            }
            *now = game;
            true
        });
        // Until the daemon is wired in, nothing can be applied: wait for it.
        let Some(daemon) = self.daemon.clone() else {
            return self.publish();
        };
        let action = match decide(self.active.as_ref(), picked.as_ref()) {
            // A rule's profile left on by a daemon that stopped while it held.
            Action::Stay if self.active.is_none() && self.store.before.is_some() => {
                Action::Restore
            }
            action => action,
        };
        match action {
            Action::Stay => {}
            Action::Apply(rule) => self.switch_to(&*daemon, rule).await,
            Action::Restore => self.restore(&*daemon).await,
        }
        self.publish();
    }

    /// The current settings of `wanted`, with what was just sent over what's read.
    async fn read_now(
        &mut self,
        d: &dyn Daemon,
        wanted: &[Section],
        gates: &gg_ipc::Experimental,
    ) -> (Profile, Vec<(Section, String)>) {
        if wanted.is_empty() {
            return (Profile::named(""), Vec::new());
        }
        let (mut p, missing) = sections::read(d, wanted, gates).await;
        self.sent.over(&mut p, Instant::now(), self.grace);
        (p, missing)
    }

    /// Apply `p`, reading first what's needed to send only what differs.
    async fn apply_profile(&mut self, d: &dyn Daemon, p: &Profile, gates: &gg_ipc::Experimental) -> Outcome {
        let diffed: Vec<Section> = [Section::Headset, Section::Mouse]
            .into_iter()
            .filter(|s| p.has(*s))
            .collect();
        let (now, _) = self.read_now(d, &diffed, gates).await;
        sections::apply(d, p, &now, gates, &mut self.sent, Instant::now()).await
    }

    async fn switch_to(&mut self, d: &dyn Daemon, rule: GameRule) {
        let Some(profile) = self.store.profile(&rule.profile).cloned() else {
            return;
        };
        let gates = sections::gates(d).await;
        // Your own settings for each section it replaces that isn't kept yet: still yours,
        // since no rule's profile has had it.
        let kept = self
            .store
            .before
            .as_ref()
            .map(Profile::sections)
            .unwrap_or_default();
        let missing: Vec<Section> = profile
            .sections()
            .into_iter()
            .filter(|s| !kept.contains(s))
            .collect();
        if !missing.is_empty() {
            let (yours, unread) = self.read_now(d, &missing, &gates).await;
            for (s, why) in unread {
                warn!(section = s.label(), %why, "could not keep your own settings before a rule's profile");
            }
            let before = self
                .store
                .before
                .get_or_insert_with(|| Profile::named(DEFAULT));
            for s in yours.sections() {
                before.take_section(&yours, s);
            }
            store::save(&self.path, &self.store);
        }
        // The profile over your own settings: what it holds is its, the rest yours again.
        let mut target = self
            .store
            .before
            .clone()
            .unwrap_or_else(|| Profile::named(DEFAULT));
        for s in profile.sections() {
            target.take_section(&profile, s);
        }
        target.name = profile.name.clone();
        let out = self.apply_profile(d, &target, &gates).await;
        let nothing = out.applied == 0 && out.failed > 0;
        self.applied = Some(Applied {
            profile: profile.name.clone(),
            notes: out.notes,
        });
        if nothing {
            // Tried again at the next check.
            return warn!(profile = %profile.name, "could not apply a rule's profile");
        }
        info!(rule = %rule.reason(), profile = %profile.name, "rule applied");
        self.active = Some(rule);
    }

    async fn restore(&mut self, d: &dyn Daemon) {
        if let Some(before) = self.store.before.clone() {
            let gates = sections::gates(d).await;
            let out = self.apply_profile(d, &before, &gates).await;
            self.applied = Some(Applied {
                profile: DEFAULT.into(),
                notes: out.notes,
            });
            self.store.before = None;
            store::save(&self.path, &self.store);
        }
        info!("no rule holds; your own settings are back");
        self.active = None;
    }

    fn daemon(&self) -> Result<Arc<dyn Daemon>, String> {
        self.daemon
            .clone()
            .ok_or_else(|| "profiles can't reach the devices yet: try again in a moment".into())
    }

    /// Apply a saved profile, or a preset, by hand.
    pub async fn apply(&mut self, name: &str, preset: bool) -> Result<(), String> {
        let profile = if preset {
            presets::find(name).map(|f| f.profile)
        } else {
            self.store.profile(name).cloned()
        }
        .ok_or_else(|| {
            let what = if preset { "preset" } else { "profile" };
            format!("no {what} named \"{name}\"")
        })?;
        let d = self.daemon()?;
        let gates = sections::gates(&*d).await;
        let out = self.apply_profile(&*d, &profile, &gates).await;
        let nothing = out.applied == 0 && out.failed > 0;
        let notes = out.notes.join("; ");
        self.applied = Some(Applied {
            profile: profile.name,
            notes: out.notes,
        });
        self.publish();
        if nothing {
            return Err(notes);
        }
        Ok(())
    }

    /// Save the current settings of `sections` under `name`. Without `sections`, those of
    /// the profile it replaces, else every section that can be read now.
    pub async fn save_current(&mut self, name: &str, sections: Option<Vec<Section>>) -> Result<(), String> {
        let name = Profile::clean_name(name)?;
        let d = self.daemon()?;
        let gates = sections::gates(&*d).await;
        let explicit = sections.is_some();
        let wanted = sections
            .or_else(|| self.store.profile(&name).map(Profile::sections))
            .unwrap_or_else(|| Section::ALL.to_vec());
        if wanted.is_empty() {
            return Err("pick at least one part to save".into());
        }
        let (mut p, missing) = self.read_now(&*d, &wanted, &gates).await;
        if let Some((s, why)) = missing.first().filter(|_| explicit) {
            return Err(format!("{} can't be saved: {why}", s.label()));
        }
        if p.sections().is_empty() {
            let why: Vec<String> = missing
                .iter()
                .map(|(s, why)| format!("{}: {why}", s.label()))
                .collect();
            return Err(format!("nothing could be read to save ({})", why.join("; ")));
        }
        p.name = name;
        self.store.put(p)?;
        self.saved();
        Ok(())
    }

    fn saved(&mut self) {
        store::save(&self.path, &self.store);
        self.publish();
    }

    /// A saved profile, or a preset, as a file to share.
    fn file(&self, name: &str, preset: bool) -> Result<ProfileFile, String> {
        if preset {
            return presets::find(name).ok_or_else(|| format!("no preset named \"{name}\""));
        }
        self.store
            .profile(name)
            .map(|p| ProfileFile::new(p.clone(), ""))
            .ok_or_else(|| format!("no profile named \"{name}\""))
    }

    fn import(&mut self, profile: Profile, replace: bool) -> Result<(), String> {
        profile.validate()?;
        let name = Profile::clean_name(&profile.name)?;
        if !replace && self.store.profile(&name).is_some() {
            return Err(format!(
                "a profile named \"{name}\" is saved already: replace it, or delete it first"
            ));
        }
        self.store.put(profile)?;
        self.saved();
        Ok(())
    }

    fn copy(&mut self, from: &str, preset: bool, to: Option<String>) -> Result<(), String> {
        let source = self.file(from, preset)?;
        let name = match to {
            Some(to) => {
                let to = Profile::clean_name(&to)?;
                if self.store.profile(&to).is_some() {
                    return Err(format!("a profile named \"{to}\" is saved already"));
                }
                to
            }
            None => self.store.free_name(&source.profile.name),
        };
        self.store.put(presets::as_profile(&source, name))?;
        self.saved();
        Ok(())
    }

    fn edit(&mut self, result: Result<(), String>) -> Result<ResponseData, String> {
        result?;
        self.saved();
        Ok(ResponseData::Success)
    }

    async fn answer(&mut self, req: Request) -> ResponseData {
        let ok = |r: Result<(), String>| r.map(|()| ResponseData::Success);
        let result = match req {
            Request::GetProfiles => Ok(ResponseData::Profiles {
                state: self.state_with_running().await,
            }),
            Request::SaveProfile { name, sections } => ok(self.save_current(&name, sections).await),
            Request::ApplyProfile { name, preset } => ok(self.apply(&name, preset).await),
            Request::DeleteProfile { name } => {
                let r = self.store.delete_profile(&name);
                self.edit(r)
            }
            Request::SetGameRules { rules } => {
                let r = self.store.set_rules(rules);
                self.edit(r)
            }
            Request::GetProfile { name, preset } => {
                self.file(&name, preset).map(|file| ResponseData::Profile { file })
            }
            Request::ImportProfile { profile, replace } => ok(self.import(*profile, replace)),
            Request::CopyProfile { from, preset, to } => ok(self.copy(&from, preset, to)),
            _ => Err("not a profiles request".into()),
        };
        result.unwrap_or_else(|message| ResponseData::Error { message })
    }

    async fn state_with_running(&self) -> ProfilesState {
        let root = self.proc_root.clone();
        let running = tokio::task::spawn_blocking(move || procs::running(&root, unsafe { libc::getuid() }))
            .await
            .unwrap_or_default();
        ProfilesState {
            running,
            ..self.state()
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::GetState(reply) => {
                let _ = reply.send(Ok(self.state_with_running().await));
            }
            Command::Apply { name, reply } => {
                let _ = reply.send(self.apply(&name, false).await);
            }
            Command::Request(req, reply) => {
                let _ = reply.send(self.answer(req).await);
            }
            Command::Connect(daemon) => self.connect(daemon),
        }
    }

    fn state(&self) -> ProfilesState {
        ProfilesState {
            profiles: self.store.profiles.iter().map(|p| p.name.clone()).collect(),
            rules: self.store.rules.clone(),
            active: self.active.clone(),
            running: Vec::new(),
            sections: self.store.profiles.iter().map(Profile::sections).collect(),
            presets: self.presets.clone(),
            focus: self.focus_status.clone(),
            applied: self.applied.clone(),
            default_sections: self
                .store
                .before
                .as_ref()
                .map(Profile::sections)
                .unwrap_or_default(),
        }
    }

    fn publish(&mut self) {
        let now = self.state();
        if self.published.as_ref() != Some(&now) {
            self.published = Some(now.clone());
            let _ = self.event_tx.send(Event::ProfilesChanged { state: now });
        }
    }

    /// Whether there's anything to scan running programs for.
    fn watching(&self) -> bool {
        !self.store.rules.is_empty()
            || self.active.is_some()
            || self.store.before.is_some()
            || self.game_tx.borrow().is_some()
    }
}

/// Where focus sources are looked for: the daemon's environment, or a test's folder.
#[derive(Debug, Clone)]
pub struct FocusEnv {
    /// `$XDG_RUNTIME_DIR`.
    pub runtime: Option<PathBuf>,
    /// `$HYPRLAND_INSTANCE_SIGNATURE`.
    pub signature: Option<String>,
    /// Whether a session bus can be reached.
    pub bus: bool,
}

impl FocusEnv {
    fn from_env() -> FocusEnv {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|r| !r.is_empty())
            .map(PathBuf::from);
        FocusEnv {
            bus: focus::session_bus(runtime.as_deref()),
            signature: std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok(),
            runtime,
        }
    }
}

/// The focus source running now, if any.
#[derive(Default)]
struct FocusRunner {
    task: Option<tokio::task::JoinHandle<()>>,
    rx: Option<mpsc::UnboundedReceiver<FocusEvent>>,
    retry_at: Option<Instant>,
}

impl FocusRunner {
    async fn next(&mut self) -> Option<FocusEvent> {
        match &mut self.rx {
            Some(rx) => rx.recv().await,
            None => std::future::pending().await,
        }
    }

    fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.rx = None;
    }

    /// Start a source if a rule acts on focus and none runs; stop it when none does.
    fn want(&mut self, c: &mut Controller, env: &FocusEnv, now: Instant) {
        if !c.store.wants_focus() {
            if self.task.is_some() || c.focus_status != FocusStatus::default() {
                self.stop();
                c.focus_lost(None);
                self.retry_at = None;
            }
            return;
        }
        if self.task.is_some() || self.retry_at.is_some_and(|t| now < t) {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        match focus::find(env.runtime.as_deref(), env.signature.as_deref(), env.bus) {
            Some(Source::Hyprland(dir)) => {
                info!(dir = %dir.display(), "following the focused window through Hyprland");
                self.task = Some(tokio::spawn(focus::hyprland(dir, tx)));
            }
            Some(Source::Kwin) => {
                info!("waiting for gglinux's KWin script to report the focused window");
                c.focus_lost(Some(WAITING_FOR_KWIN.into()));
                self.task = Some(tokio::spawn(async move {
                    match focus::kwin(tx.clone()).await {
                        // Served while the task runs; stopping it gives the name back.
                        Ok(_conn) => std::future::pending::<()>().await,
                        Err(why) => {
                            let _ = tx.send(FocusEvent::Down(why));
                        }
                    }
                }));
            }
            None => {
                c.focus_lost(Some(NO_FOCUS.into()));
                self.retry_at = Some(now + RETRY_FOCUS);
                return;
            }
        }
        self.rx = Some(rx);
        self.retry_at = None;
    }
}

/// The task: requests, scans, and the focused window, until the handles are gone.
async fn run(
    mut c: Controller,
    mut rx: mpsc::UnboundedReceiver<Command>,
    env: FocusEnv,
    scan_every: Duration,
) {
    let mut tick = tokio::time::interval(scan_every);
    let mut runner = FocusRunner::default();
    loop {
        let deadline = c.debounce.deadline();
        tokio::select! {
            command = rx.recv() => match command {
                Some(command) => c.handle(command).await,
                None => break,
            },
            event = runner.next() => match event {
                Some(event) => {
                    let down = matches!(event, FocusEvent::Down(_));
                    if c.on_focus(event, Instant::now()) {
                        c.follow().await;
                    }
                    if down {
                        runner.stop();
                        runner.retry_at = Some(Instant::now() + RETRY_FOCUS);
                    }
                }
                // The source's task ended without a word.
                None => {
                    runner.stop();
                    runner.retry_at = Some(Instant::now() + RETRY_FOCUS);
                }
            },
            _ = sleep_until(deadline), if deadline.is_some() => {
                if c.focus_due(Instant::now()) {
                    c.follow().await;
                }
            }
            _ = tick.tick() => {
                runner.want(&mut c, &env, Instant::now());
                c.publish();
                if !c.watching() {
                    continue;
                }
                let root = c.proc_root.clone();
                let names = tokio::task::spawn_blocking(move || procs::running(&root, unsafe { libc::getuid() }))
                    .await
                    .unwrap_or_default();
                c.set_running(&names);
                c.follow().await;
            }
        }
    }
    runner.stop();
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d.into()).await,
        None => std::future::pending().await,
    }
}

/// Start profiles. Sections reach the devices once the daemon's handlers are wired in
/// ([`Handle::connect`]); `_keyboard` is from when profiles held lighting only.
pub fn spawn(
    event_tx: broadcast::Sender<Event>,
    _keyboard: keyboard::Handle,
    game_tx: watch::Sender<Option<String>>,
) -> Handle {
    let (tx, rx) = mpsc::unbounded_channel();
    let path = gg_ipc::config_dir().join("profiles.json");
    let c = Controller::new(store::load(&path), path, event_tx, game_tx);
    tokio::spawn(run(c, rx, FocusEnv::from_env(), SCAN_EVERY));
    Handle::new(tx)
}

/// For tests: start the task with this controller, folder of focus sockets and scan period.
#[cfg(test)]
fn spawn_with(c: Controller, env: FocusEnv, scan_every: Duration) -> Handle {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(c, rx, env, scan_every));
    Handle::new(tx)
}

#[cfg(test)]
impl Controller {
    fn at(path: &std::path::Path, store: Store) -> Controller {
        Controller::new(store, path.to_path_buf(), broadcast::channel(64).0, watch::channel(None).0)
    }
}
