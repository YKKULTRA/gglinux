//! Lighting profiles, and rules that switch to one while a game runs. A tokio
//! task scans running programs every 2 s and talks to the keyboard worker —
//! through [`LightingPort`], so the switching is testable without it.

mod procs;
mod store;

use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use gg_ipc::{Event, GameRule, Lighting, ProfilesState};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{info, warn};

use crate::keyboard::{self, Command as KeyboardCommand};
use store::{decide, pick, Action, Store};

const SCAN_EVERY: Duration = Duration::from_secs(2);

type Reply<T> = oneshot::Sender<Result<T, String>>;

/// How profiles read and set the keyboard's lighting.
pub trait LightingPort: Send + Sync + 'static {
    fn get(&self) -> impl Future<Output = Result<Lighting, String>> + Send;
    fn set(&self, lighting: Lighting) -> impl Future<Output = Result<(), String>> + Send;
}

impl LightingPort for keyboard::Handle {
    async fn get(&self) -> Result<Lighting, String> {
        self.send(KeyboardCommand::GetState).await.map(|s| s.lighting)
    }
    async fn set(&self, lighting: Lighting) -> Result<(), String> {
        self.send::<()>(|reply| KeyboardCommand::SetLighting { lighting, reply }).await
    }
}

pub enum Command {
    GetState(Reply<ProfilesState>),
    Save { name: String, reply: Reply<()> },
    Apply { name: String, reply: Reply<()> },
    Delete { name: String, reply: Reply<()> },
    SetRules { rules: Vec<GameRule>, reply: Reply<()> },
}

#[derive(Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Command>,
}

impl Handle {
    pub async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx.send(make(reply_tx)).map_err(|_| "profiles are not running".to_string())?;
        reply_rx.await.map_err(|_| "profiles dropped the request".to_string())?
    }
}

pub struct Controller<P> {
    store: Store,
    path: PathBuf,
    port: P,
    event_tx: broadcast::Sender<Event>,
    /// The rule applied now, and the lighting to go back to.
    active: Option<GameRule>,
    before: Option<Lighting>,
    published: Option<ProfilesState>,
}

impl<P: LightingPort> Controller<P> {
    pub fn new(store: Store, path: PathBuf, port: P, event_tx: broadcast::Sender<Event>) -> Self {
        Controller { store, path, port, event_tx, active: None, before: None, published: None }
    }

    /// Apply or undo game rules for what's running (lower-cased names).
    pub async fn follow(&mut self, running: &HashSet<String>) {
        let picked = pick(&self.store, running).cloned();
        match decide(self.active.as_ref(), picked.as_ref()) {
            Action::Stay => {}
            Action::Apply(rule) => {
                if self.active.is_none() {
                    match self.port.get().await {
                        Ok(l) => self.before = Some(l),
                        Err(e) => return warn!(error = %e, "could not read the lighting before a game rule"),
                    }
                }
                let lighting = self.store.profile(&rule.profile).map(|p| p.lighting.clone());
                if let Some(lighting) = lighting {
                    if let Err(e) = self.port.set(lighting).await {
                        return warn!(error = %e, "could not apply a game profile");
                    }
                    info!(process = %rule.process, profile = %rule.profile, "game rule applied");
                    self.active = Some(rule);
                }
            }
            Action::Restore => {
                if let Some(l) = self.before.take() {
                    if let Err(e) = self.port.set(l).await {
                        warn!(error = %e, "could not restore the lighting after a game");
                    }
                }
                info!("game rule ended; lighting restored");
                self.active = None;
            }
        }
        self.publish();
    }

    pub async fn apply(&mut self, name: &str) -> Result<(), String> {
        let lighting = self.store.profile(name).ok_or_else(|| format!("no profile named \"{name}\""))?.lighting.clone();
        self.port.set(lighting).await
    }

    pub async fn save_current(&mut self, name: &str) -> Result<(), String> {
        let lighting = self.port.get().await?;
        self.store.save_profile(name, lighting)?;
        store::save(&self.path, &self.store);
        self.publish();
        Ok(())
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::GetState(reply) => {
                let running = tokio::task::spawn_blocking(|| procs::running(std::path::Path::new("/proc"), unsafe { libc::getuid() }))
                    .await
                    .unwrap_or_default();
                let _ = reply.send(Ok(ProfilesState { running, ..self.state() }));
            }
            Command::Save { name, reply } => {
                let _ = reply.send(self.save_current(&name).await);
            }
            Command::Apply { name, reply } => {
                let _ = reply.send(self.apply(&name).await);
            }
            Command::Delete { name, reply } => {
                let result = self.store.delete_profile(&name);
                self.after_edit(result, reply);
            }
            Command::SetRules { rules, reply } => {
                let result = self.store.set_rules(rules);
                self.after_edit(result, reply);
            }
        }
    }

    fn after_edit(&mut self, result: Result<(), String>, reply: Reply<()>) {
        if result.is_ok() {
            store::save(&self.path, &self.store);
        }
        let _ = reply.send(result);
        self.publish();
    }

    fn state(&self) -> ProfilesState {
        ProfilesState {
            profiles: self.store.profiles.iter().map(|p| p.name.clone()).collect(),
            rules: self.store.rules.clone(),
            active: self.active.clone(),
            running: Vec::new(),
        }
    }

    fn publish(&mut self) {
        let now = self.state();
        if self.published.as_ref() != Some(&now) {
            self.published = Some(now.clone());
            let _ = self.event_tx.send(Event::ProfilesChanged { state: now });
        }
    }
}

pub fn spawn(event_tx: broadcast::Sender<Event>, keyboard: keyboard::Handle) -> Handle {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let path = gg_ipc::config_dir().join("profiles.json");
    let mut c = Controller::new(store::load(&path), path, keyboard, event_tx);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SCAN_EVERY);
        loop {
            tokio::select! {
                command = rx.recv() => match command {
                    Some(command) => c.handle(command).await,
                    None => break,
                },
                _ = tick.tick() => {
                    if c.store.rules.is_empty() && c.active.is_none() {
                        continue; // nothing to watch for
                    }
                    let names = tokio::task::spawn_blocking(|| procs::running(std::path::Path::new("/proc"), unsafe { libc::getuid() }))
                        .await
                        .unwrap_or_default();
                    let running: HashSet<String> = names.iter().map(|n| n.to_lowercase()).collect();
                    c.follow(&running).await;
                }
            }
        }
    });
    Handle { tx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::Effect;
    use std::sync::{Arc, Mutex};

    /// The keyboard's lighting, in memory.
    #[derive(Clone)]
    struct FakePort(Arc<Mutex<Lighting>>);

    impl LightingPort for FakePort {
        async fn get(&self) -> Result<Lighting, String> {
            Ok(self.0.lock().unwrap().clone())
        }
        async fn set(&self, lighting: Lighting) -> Result<(), String> {
            *self.0.lock().unwrap() = lighting;
            Ok(())
        }
    }

    fn look(p: f32) -> Lighting {
        Lighting { enabled: true, effect: Effect::Wave { period_s: p }, ..Lighting::default() }
    }

    fn controller() -> (Controller<FakePort>, Arc<Mutex<Lighting>>) {
        let now = Arc::new(Mutex::new(look(1.0))); // the owner's own lighting
        let mut store = Store::default();
        store.save_profile("A", look(2.0)).unwrap();
        store.save_profile("B", look(3.0)).unwrap();
        store.set_rules(vec![
            GameRule { process: "game-a".into(), profile: "A".into() },
            GameRule { process: "game-b".into(), profile: "B".into() },
        ]).unwrap();
        let path = std::env::temp_dir().join(format!("gglinux-ctl-{}-{:?}.json", std::process::id(), std::thread::current().id()));
        let (event_tx, _) = broadcast::channel(8);
        (Controller::new(store, path, FakePort(Arc::clone(&now)), event_tx), now)
    }

    fn running(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[tokio::test]
    async fn a_game_starting_applies_its_profile_and_ending_restores_the_lighting() {
        let (mut c, now) = controller();
        c.follow(&running(&["game-a"])).await;
        assert_eq!(*now.lock().unwrap(), look(2.0));
        assert_eq!(c.active.as_ref().map(|r| r.profile.as_str()), Some("A"));
        c.follow(&running(&[])).await;
        assert_eq!(*now.lock().unwrap(), look(1.0), "back to the owner's lighting");
        assert!(c.active.is_none());
    }

    #[tokio::test]
    async fn switching_games_keeps_the_original_to_restore() {
        let (mut c, now) = controller();
        c.follow(&running(&["game-a"])).await;
        c.follow(&running(&["game-b"])).await;
        assert_eq!(*now.lock().unwrap(), look(3.0));
        c.follow(&running(&[])).await;
        assert_eq!(*now.lock().unwrap(), look(1.0), "not A's lighting");
    }

    #[tokio::test]
    async fn restart_gaps_never_lose_the_original() {
        let (mut c, now) = controller();
        c.follow(&running(&["game-a"])).await;
        c.follow(&running(&[])).await; // the launcher exits…
        c.follow(&running(&["game-a"])).await; // …and the game starts
        c.follow(&running(&[])).await;
        assert_eq!(*now.lock().unwrap(), look(1.0));
    }

    #[tokio::test]
    async fn a_manual_apply_during_a_game_is_undone_when_the_game_ends() {
        let (mut c, now) = controller();
        c.follow(&running(&["game-a"])).await;
        c.apply("B").await.unwrap();
        assert_eq!(*now.lock().unwrap(), look(3.0));
        c.follow(&running(&[])).await;
        assert_eq!(*now.lock().unwrap(), look(1.0));
        assert!(c.apply("Nope").await.is_err());
    }

    #[tokio::test]
    async fn saving_takes_the_current_lighting() {
        let (mut c, now) = controller();
        *now.lock().unwrap() = look(9.0);
        c.save_current("Nine").await.unwrap();
        assert_eq!(c.store.profile("Nine").unwrap().lighting, look(9.0));
    }
}
