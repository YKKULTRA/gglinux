// SPDX-License-Identifier: GPL-3.0-or-later
//! GameSense: the SDK server, its files in Proton prefixes, and the games it
//! lets draw on the keyboard. A tokio task owns the on/off switch, the server,
//! the prefix scan and the settings file; the registry is shared with the
//! server's handlers and with the keyboard worker (through [`Overlay`]).

pub mod server;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gg_gamesense::prefixes::{self, Written};
use gg_gamesense::{proto, Registry, SavedGame};
use gg_ipc::{Event, GameInfo, GameSenseState};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::keyboard::{self, Layer, Overlay};

const SCAN_EVERY: Duration = Duration::from_secs(1);
/// Scans between re-reads of `libraryfolders.vdf` (30 s at one scan a second).
const LIBRARY_REFRESH_SCANS: u32 = 30;
const TEST_GAME: &str = "GGLINUX_TEST";
const TEST_STEP: Duration = Duration::from_millis(100);

type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum Command {
    GetState(Reply<GameSenseState>),
    SetEnabled {
        enabled: bool,
        reply: Reply<()>,
    },
    SetGameEnabled {
        game: String,
        enabled: bool,
        reply: Reply<()>,
    },
    RemoveGame {
        game: String,
        reply: Reply<()>,
    },
    RunTest(Reply<()>),
}

#[derive(Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Command>,
}

impl Handle {
    /// A handle with no worker, for when this part is off: every send fails.
    pub fn off() -> Handle {
        let (tx, _) = mpsc::unbounded_channel();
        Handle { tx }
    }

    pub async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| "GameSense is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "GameSense dropped the request".to_string())?
    }
}

/// With GameSense's part off: take gglinux's files out of the Proton prefixes, so no
/// game looks for a server that won't come. Blocking; other files are left alone.
pub fn clear_files() {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let libraries = prefixes::libraries(&prefixes::steam_roots(&home));
    let removed = prefixes::program_data_dirs(&libraries)
        .iter()
        .filter(|pd| prefixes::remove(pd).unwrap_or(false))
        .count();
    if removed > 0 {
        info!(
            removed,
            "GameSense is turned off: took coreProps.json out of the Proton prefixes"
        );
    }
}

/// `~/.config/gglinux/gamesense.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub games: Vec<SavedGame>,
}

/// Saved settings; a missing or unreadable file means GameSense off. An
/// unreadable one is moved aside first.
pub fn load_settings(path: &Path) -> Settings {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            let moved_to = gg_ipc::set_aside(path);
            warn!(path = %path.display(), error = %e, %moved_to, "ignoring unreadable GameSense settings");
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

/// Save atomically; a failure is logged, never fatal.
pub fn save_settings(path: &Path, settings: &Settings) {
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
        warn!(path = %path.display(), error = %format!("{e:#}"), "could not save GameSense settings");
    }
}

/// The registry and clock, made before the keyboard worker so it can be given the overlay.
pub struct Engine {
    registry: Arc<Mutex<Registry>>,
    clock: Clock,
    settings_path: PathBuf,
    settings: Settings,
}

impl Engine {
    pub fn load() -> Engine {
        let settings_path = gg_ipc::config_dir().join("gamesense.json");
        let settings = load_settings(&settings_path);
        let registry = Registry::restore(settings.games.clone());
        Engine {
            registry: Arc::new(Mutex::new(registry)),
            clock: Clock::start(),
            settings_path,
            settings,
        }
    }

    pub fn overlay(&self) -> Arc<dyn Overlay> {
        Arc::new(GameOverlay {
            registry: Arc::clone(&self.registry),
            clock: self.clock,
        })
    }
}

struct GameOverlay {
    registry: Arc<Mutex<Registry>>,
    clock: Clock,
}

impl Overlay for GameOverlay {
    fn paint(&self, now: Instant) -> Option<Layer> {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .paint(self.clock.at(now))
    }
}

/// Bind on loopback: the saved port if it's free, else any free one.
pub async fn bind_loopback(port: Option<u16>) -> std::io::Result<TcpListener> {
    if let Some(p) = port {
        match TcpListener::bind(("127.0.0.1", p)).await {
            Ok(l) => return Ok(l),
            Err(e) => info!(port = p, error = %e, "saved GameSense port is taken; picking another"),
        }
    }
    TcpListener::bind(("127.0.0.1", 0)).await
}

/// Prefixes that failed this scan and didn't last time, so each is warned about once.
pub fn newly_failed(seen: &mut HashSet<PathBuf>, failed: &[PathBuf]) -> Vec<PathBuf> {
    let now: HashSet<PathBuf> = failed.iter().cloned().collect();
    let new = failed
        .iter()
        .filter(|p| !seen.contains(*p))
        .cloned()
        .collect();
    *seen = now;
    new
}

/// 100 → 0 → 100, one value per step. From 20 down it moves in fives, each
/// held for five steps: a new value restarts the flash, and one every step
/// would never reach the dark half of the 4 Hz flash.
pub fn test_values() -> Vec<i64> {
    (0..=100)
        .rev()
        .chain(1..=100)
        .map(|v| if v <= 20 { (v + 4) / 5 * 5 } else { v })
        .collect()
}

fn define_test_game(r: &mut Registry) -> Result<(), String> {
    r.game_metadata(proto::GameMetadata {
        game: TEST_GAME.into(),
        game_display_name: Some("gglinux test".into()),
        developer: Some("gglinux".into()),
        deinitialize_timer_length_ms: None,
    })?;
    r.bind_event(
        serde_json::from_value(serde_json::json!({
            "game": TEST_GAME, "event": "HEALTH", "min_value": 0, "max_value": 100,
            "handlers": [{
                "device-type": "keyboard", "zone": "function-keys", "mode": "percent",
                "color": { "gradient": { "zero": { "red": 255, "green": 0, "blue": 0 },
                                         "hundred": { "red": 0, "green": 255, "blue": 0 } } },
                "rate": { "frequency": [{ "low": 0, "high": 20, "frequency": 4 }] }
            }]
        }))
        .map_err(|e| e.to_string())?,
    )
}

fn send_test_value(r: &mut Registry, value: i64, now: f64) -> Result<(), String> {
    r.game_event(
        proto::GameEvent {
            game: TEST_GAME.into(),
            event: "HEALTH".into(),
            data: proto::EventData {
                value: Some(value as f64),
                frame: None,
            },
        },
        now,
    )
}

fn game_infos(r: &Registry, now: f64) -> Vec<GameInfo> {
    r.summaries(now)
        .into_iter()
        .map(|s| GameInfo {
            name: s.name,
            display_name: s.display_name,
            developer: s.developer,
            enabled: s.enabled,
            active: s.active,
            events: s.events,
        })
        .collect()
}

struct Running {
    port: u16,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

struct Controller {
    engine: Engine,
    event_tx: broadcast::Sender<Event>,
    keyboard: keyboard::Handle,
    changes: mpsc::UnboundedSender<Change>,
    running: Option<Running>,
    libraries: Vec<PathBuf>,
    scans: u32,
    holding: u32,
    failed: HashSet<PathBuf>,
    test: Option<JoinHandle<()>>,
    published: Option<GameSenseState>,
}

pub fn spawn(
    engine: Engine,
    event_tx: broadcast::Sender<Event>,
    keyboard: keyboard::Handle,
) -> Handle {
    let (tx, rx) = mpsc::unbounded_channel();
    let (changes, changes_rx) = mpsc::unbounded_channel();
    let c = Controller {
        engine,
        event_tx,
        keyboard,
        changes,
        running: None,
        libraries: Vec::new(),
        scans: 0,
        holding: 0,
        failed: HashSet::new(),
        test: None,
        published: None,
    };
    tokio::spawn(run(c, rx, changes_rx));
    Handle { tx }
}

async fn run(
    mut c: Controller,
    mut rx: mpsc::UnboundedReceiver<Command>,
    mut changes: mpsc::UnboundedReceiver<Change>,
) {
    if c.engine.settings.enabled {
        if let Err(e) = c.start().await {
            warn!(error = %e, "could not start the GameSense server");
        }
    } else {
        // Off, yet our files can still be there: a restored backup with
        // GameSense off, an unreadable settings file, or a stop cut short.
        c.clear_prefixes().await;
    }
    let mut tick = tokio::time::interval(SCAN_EVERY);
    loop {
        tokio::select! {
            command = rx.recv() => match command {
                Some(command) => c.handle(command).await,
                None => break,
            },
            Some(change) = changes.recv() => {
                if change == Change::Structure {
                    c.save();
                }
                c.publish();
            }
            _ = tick.tick() => {
                if c.running.is_some() {
                    c.scan().await;
                }
                c.publish(); // games go quiet with time
            }
        }
    }
}

impl Controller {
    async fn handle(&mut self, command: Command) {
        match command {
            Command::GetState(reply) => {
                let _ = reply.send(Ok(self.state()));
            }
            Command::SetEnabled { enabled, reply } => {
                let result = if enabled {
                    self.start().await
                } else {
                    self.stop().await
                };
                let _ = reply.send(result);
                self.publish();
            }
            Command::SetGameEnabled {
                game,
                enabled,
                reply,
            } => {
                let result = self.registry().set_enabled(&game, enabled);
                self.after_edit(result, reply);
            }
            Command::RemoveGame { game, reply } => {
                let result = self.registry().remove_game(&game);
                self.after_edit(result, reply);
            }
            Command::RunTest(reply) => {
                let result = define_test_game(&mut self.registry());
                if result.is_ok() {
                    self.run_test();
                }
                self.after_edit(result, reply);
            }
        }
    }

    fn after_edit(&mut self, result: Result<(), String>, reply: Reply<()>) {
        if result.is_ok() {
            self.save();
            self.keyboard.wake();
        }
        let _ = reply.send(result);
        self.publish();
    }

    fn registry(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.engine
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Start the server, remember its port, and put the files in the prefixes.
    async fn start(&mut self) -> Result<(), String> {
        if self.running.is_some() {
            return Ok(());
        }
        let listener = bind_loopback(self.engine.settings.port)
            .await
            .map_err(|e| format!("GameSense server: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let keyboard = self.keyboard.clone();
        let changes = self.changes.clone();
        let shared = server::Shared {
            registry: Arc::clone(&self.engine.registry),
            clock: self.engine.clock,
            changed: Arc::new(move |change| {
                keyboard.wake();
                let _ = changes.send(change);
            }),
        };
        let (shutdown, stop) = oneshot::channel::<()>();
        let task = tokio::spawn(server::serve(
            listener,
            server::router(shared),
            server::LIMITS,
            async {
                let _ = stop.await;
            },
        ));
        info!(port, "GameSense server listening on 127.0.0.1");
        self.running = Some(Running {
            port,
            shutdown,
            task,
        });
        self.engine.settings.enabled = true;
        self.engine.settings.port = Some(port);
        self.save();
        self.scans = 0; // re-read the libraries now
        self.scan().await;
        Ok(())
    }

    /// Stop the server and take gglinux's files back out of the prefixes.
    async fn stop(&mut self) -> Result<(), String> {
        if let Some(r) = self.running.take() {
            let _ = r.shutdown.send(());
            let _ = tokio::time::timeout(Duration::from_secs(1), r.task).await;
            info!("GameSense server stopped");
        }
        self.engine.settings.enabled = false;
        self.save();
        self.clear_prefixes().await;
        Ok(())
    }

    /// Take gglinux's files out of every prefix; other files are left alone.
    async fn clear_prefixes(&mut self) {
        let libraries = self.libraries_or_read();
        let removed = tokio::task::spawn_blocking(move || {
            prefixes::program_data_dirs(&libraries)
                .iter()
                .filter(|pd| prefixes::remove(pd).unwrap_or(false))
                .count()
        })
        .await
        .unwrap_or(0);
        debug!(removed, "removed coreProps.json from Proton prefixes");
        self.holding = 0;
    }

    fn libraries_or_read(&mut self) -> Vec<PathBuf> {
        if self.libraries.is_empty() || self.scans.is_multiple_of(LIBRARY_REFRESH_SCANS) {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            self.libraries = prefixes::libraries(&prefixes::steam_roots(&home));
        }
        self.libraries.clone()
    }

    /// Write the file into any prefix that lacks it; count the ones that hold it.
    async fn scan(&mut self) {
        let Some(port) = self.running.as_ref().map(|r| r.port) else {
            return;
        };
        let libraries = self.libraries_or_read();
        self.scans = self.scans.wrapping_add(1);
        let (holding, failed) = tokio::task::spawn_blocking(move || {
            let mut holding = 0u32;
            let mut failed = Vec::new();
            for pd in prefixes::program_data_dirs(&libraries) {
                match prefixes::write(&pd, port) {
                    Ok(Written::Wrote) => {
                        debug!(prefix = %pd.display(), "wrote coreProps.json");
                        holding += 1;
                    }
                    Ok(Written::Unchanged) => holding += 1,
                    Ok(Written::Foreign) => {}
                    Err(_) => failed.push(pd),
                }
            }
            (holding, failed)
        })
        .await
        .unwrap_or_default();
        for pd in newly_failed(&mut self.failed, &failed) {
            warn!(prefix = %pd.display(), "could not write coreProps.json into this Proton prefix");
        }
        self.holding = holding;
    }

    /// Play the test game: an F-key health bar sweeping 100 → 0 → 100.
    fn run_test(&mut self) {
        if let Some(old) = self.test.take() {
            old.abort();
        }
        let registry = Arc::clone(&self.engine.registry);
        let clock = self.engine.clock;
        let keyboard = self.keyboard.clone();
        let changes = self.changes.clone();
        self.test = Some(tokio::spawn(async move {
            for value in test_values() {
                {
                    let mut r = registry.lock().unwrap_or_else(|e| e.into_inner());
                    if send_test_value(&mut r, value, clock.now()).is_err() {
                        return; // removed from the UI mid-run
                    }
                }
                keyboard.wake();
                let _ = changes.send(Change::Live);
                tokio::time::sleep(TEST_STEP).await;
            }
            let _ = registry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .stop_game(TEST_GAME);
            keyboard.wake();
            let _ = changes.send(Change::Live);
        }));
    }

    fn state(&self) -> GameSenseState {
        GameSenseState {
            enabled: self.running.is_some(),
            address: self
                .running
                .as_ref()
                .map(|r| format!("127.0.0.1:{}", r.port)),
            prefixes: self.holding,
            games: game_infos(&self.registry(), self.engine.clock.now()),
        }
    }

    fn save(&mut self) {
        let games = self.registry().saved(); // the guard ends here, before `self` is mutated
        self.engine.settings.games = games;
        save_settings(&self.engine.settings_path, &self.engine.settings);
    }

    fn publish(&mut self) {
        let now = self.state();
        if self.published.as_ref() != Some(&now) {
            self.published = Some(now.clone());
            let _ = self.event_tx.send(Event::GamesenseChanged { state: now });
        }
    }
}

/// Monotonic seconds since the daemon started: the registry's `now`.
#[derive(Clone, Copy)]
pub struct Clock(Instant);

impl Clock {
    /// Seconds count from now.
    pub fn start() -> Clock {
        Clock(Instant::now())
    }

    pub fn now(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }

    /// The same scale for a given instant.
    pub fn at(&self, t: Instant) -> f64 {
        t.saturating_duration_since(self.0).as_secs_f64()
    }
}

/// What a request changed: live values only, or something worth saving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Live,
    Structure,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gglinux-gs-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unreadable_settings_fall_back_to_off() {
        let dir = scratch("settings");
        let path = dir.join("gamesense.json");
        assert_eq!(load_settings(&path), Settings::default(), "missing file");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_settings(&path), Settings::default(), "corrupted file");
        assert!(!path.exists(), "moved aside, so a save can't destroy it");
        assert!(!Settings::default().enabled, "GameSense is off until asked");
        let s = Settings {
            enabled: true,
            port: Some(5000),
            games: Vec::new(),
        };
        save_settings(&path, &s);
        assert_eq!(load_settings(&path), s);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_test_game_sweeps_down_and_back_up() {
        let v = test_values();
        assert_eq!(v.len(), 201);
        assert_eq!((v[0], v[100], v[200]), (100, 0, 100));
    }

    #[test]
    fn the_test_game_draws_a_red_to_green_health_bar_on_the_f_keys() {
        let mut r = Registry::default();
        define_test_game(&mut r).unwrap();
        send_test_value(&mut r, 100, 0.0).unwrap();
        let full = r.paint(0.0).unwrap();
        assert_eq!(full.len(), 12);
        assert_eq!(
            full[&0x45],
            gg_device_keyboard::Rgb { r: 0, g: 255, b: 0 },
            "F12 green at 100"
        );
        send_test_value(&mut r, 10, 1.0).unwrap();
        let low = r.paint(1.0).unwrap();
        assert_eq!(
            low[&0x45],
            gg_device_keyboard::Rgb::default(),
            "F12 dark at 10%"
        );
        assert_ne!(
            low[&0x3A],
            gg_device_keyboard::Rgb::default(),
            "F1 lit (first half of a flash)"
        );
        assert_eq!(
            r.paint(1.2).unwrap()[&0x3A],
            gg_device_keyboard::Rgb::default(),
            "…and dark in the second half at 4 Hz"
        );
    }

    #[test]
    fn the_test_game_flashes_as_it_sweeps_through_the_low_values() {
        let mut r = Registry::default();
        define_test_game(&mut r).unwrap();
        let step = TEST_STEP.as_secs_f64();
        let mut dark = Vec::new();
        for (i, value) in test_values().into_iter().enumerate() {
            let t = i as f64 * step;
            send_test_value(&mut r, value, t).unwrap();
            for frame in 0..3 {
                // the keyboard draws every 33 ms
                let f1 = r.paint(t + f64::from(frame) / 30.0).unwrap()[&0x3A];
                if value > 0 && f1 == gg_device_keyboard::Rgb::default() {
                    dark.push(value);
                }
            }
        }
        assert!(
            !dark.is_empty(),
            "F1 never went dark: every step restarted the flash"
        );
        assert!(
            dark.iter().all(|&v| v <= 20),
            "only low values flash: {dark:?}"
        );
    }

    #[test]
    fn a_failing_prefix_is_reported_once() {
        let mut seen = HashSet::new();
        let a = PathBuf::from("/a");
        let b = PathBuf::from("/b");
        assert_eq!(
            newly_failed(&mut seen, std::slice::from_ref(&a)),
            vec![a.clone()]
        );
        assert!(
            newly_failed(&mut seen, std::slice::from_ref(&a)).is_empty(),
            "already warned"
        );
        assert_eq!(newly_failed(&mut seen, &[a.clone(), b.clone()]), vec![b]);
        assert!(newly_failed(&mut seen, &[]).is_empty());
        assert_eq!(
            newly_failed(&mut seen, std::slice::from_ref(&a)),
            vec![a],
            "fixed, then broken again: warn again"
        );
    }

    #[tokio::test]
    async fn a_taken_port_falls_back_to_a_free_one() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = held.local_addr().unwrap().port();
        let l = bind_loopback(Some(taken)).await.unwrap();
        let addr = l.local_addr().unwrap();
        assert!(addr.ip().is_loopback());
        assert_ne!(addr.port(), taken);
        let fresh = bind_loopback(None).await.unwrap();
        assert!(fresh.local_addr().unwrap().ip().is_loopback());
    }

    #[test]
    fn summaries_become_ipc_game_infos() {
        let mut r = Registry::default();
        define_test_game(&mut r).unwrap();
        let infos = game_infos(&r, 0.0);
        assert_eq!(infos[0].name, "GGLINUX_TEST");
        assert_eq!(infos[0].display_name.as_deref(), Some("gglinux test"));
        assert_eq!(infos[0].events, vec!["HEALTH".to_string()]);
    }
}
