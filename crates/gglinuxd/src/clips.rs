//! Instant replay: a supervised `gpu-screen-recorder` in replay mode, saved
//! on `SIGUSR1`. Off by default. With `games_only`, it records only while a
//! game from the profiles' rules runs (the profiles watcher says which). A
//! waiter task per child reports its exit, so the controller's loop only ever
//! waits on channels and timers.

mod hotkey;
mod recorder;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gg_ipc::{ClipsSettings, ClipsState, Event};
use tokio::process::Command as Process;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tracing::{info, warn};

type Reply<T> = oneshot::Sender<Result<T, String>>;

const STOP_GRACE: Duration = Duration::from_secs(3);
const SAVE_SETTLE: Duration = Duration::from_millis(1500);
/// How long games-only clips keep recording after the game has gone: time to
/// save its last moments, and to bridge a launcher handing over to the game.
const GAME_GRACE: Duration = Duration::from_secs(15);
/// How often clips that are on look for a missing `gpu-screen-recorder`, so
/// they start once it's installed.
const INSTALL_POLL: Duration = Duration::from_secs(5);

pub enum Command {
    GetState(Reply<ClipsState>),
    SetSettings {
        settings: ClipsSettings,
        reply: Reply<()>,
    },
    Save(Reply<()>),
    Delete {
        name: String,
        reply: Reply<()>,
    },
    /// Stop the recorder before the daemon exits; it's a separate process.
    Shutdown(Reply<()>),
}

pub enum Internal {
    Exited { generation: u64 },
    Refresh,
    Hotkey(Result<String, String>),
    SaveFromHotkey,
}

#[derive(Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Command>,
}

impl Handle {
    pub async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| "clips are not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "clips dropped the request".to_string())?
    }
}

struct Running {
    pid: i32,
    generation: u64,
    started: Instant,
    /// Fires as soon as the waiter has reaped the child.
    exited: oneshot::Receiver<()>,
}

impl Running {
    /// Send `sig` to the child, unless the waiter has already reaped it: from
    /// then on the PID is free for another process, even though the `Exited`
    /// notice may still be queued. Returns whether it was sent; once it says
    /// no, `exited` is spent and mustn't be awaited.
    fn signal(&mut self, sig: i32) -> bool {
        if !matches!(
            self.exited.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ) {
            return false;
        }
        // SAFETY: plain kill(2) on our own child. The waiter fires `exited`
        // right after reaping it, and Linux hands out PIDs in turn, so the PID
        // can't have come round to another process in that gap.
        unsafe {
            libc::kill(self.pid, sig);
        }
        true
    }
}

pub struct Controller {
    settings: ClipsSettings,
    path: PathBuf,
    /// Set in tests, installed once the file is there; `None` searches `PATH`.
    program: Option<PathBuf>,
    event_tx: broadcast::Sender<Event>,
    internal_tx: mpsc::UnboundedSender<Internal>,
    running: Option<Running>,
    generation: u64,
    delay: Duration,
    retry_at: Option<Instant>,
    /// The game from the rules that's running now, from the profiles watcher.
    game: Option<String>,
    /// Games-only, the game gone: when to stop.
    linger_until: Option<Instant>,
    /// Where everything playing mixes, from Sonar while it's on: desktop
    /// audio records its monitor. `None` is the default output.
    mixed: Option<String>,
    error: Option<String>,
    hotkey: Option<String>,
    /// Why there's no hotkey. Apart from `error`, which every start clears:
    /// the hotkey is bound once, not with each recorder.
    hotkey_error: Option<String>,
    hotkey_task: Option<tokio::task::JoinHandle<()>>,
    /// Only the daemon binds the portal hotkey; tests never touch the desktop.
    hotkey_allowed: bool,
    published: Option<ClipsState>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

impl Controller {
    /// The controller, and the receiver for its internal messages (kept apart so
    /// the task's `select!` can wait on it while handlers borrow the controller).
    pub fn new(
        settings: ClipsSettings,
        path: PathBuf,
        program: Option<PathBuf>,
        event_tx: broadcast::Sender<Event>,
    ) -> (Self, mpsc::UnboundedReceiver<Internal>) {
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();
        let c = Controller {
            settings,
            path,
            program,
            event_tx,
            internal_tx,
            running: None,
            generation: 0,
            delay: Duration::ZERO,
            retry_at: None,
            game: None,
            linger_until: None,
            mixed: None,
            error: None,
            hotkey: None,
            hotkey_error: None,
            hotkey_task: None,
            hotkey_allowed: false,
            published: None,
        };
        (c, internal_rx)
    }

    fn program(&self) -> Option<PathBuf> {
        match &self.program {
            Some(p) => p.is_file().then(|| p.clone()),
            None => recorder::find_program(std::env::var_os("PATH").as_deref()),
        }
    }

    fn dir(&self) -> PathBuf {
        recorder::resolve_dir(&self.settings, &home())
    }

    /// Whether the recorder should run: clips on, and a game running if they're games-only.
    fn wanted(&self) -> bool {
        self.settings.enabled && (!self.settings.games_only || self.game.is_some())
    }

    pub async fn start(&mut self) {
        if self.running.is_some() || !self.wanted() {
            return;
        }
        let Some(program) = self.program() else {
            self.retry_at = Some(Instant::now() + INSTALL_POLL);
            self.error = Some("gpu-screen-recorder is not installed".into());
            return self.publish();
        };
        let dir = self.dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return self.retry_later(format!("cannot create {}: {e}", dir.display()));
        }
        let argv = recorder::argv(&self.settings, &dir, self.mixed.as_deref());
        let mut child = match Process::new(&program)
            .args(&argv[1..])
            .kill_on_drop(false)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return self.retry_later(format!("could not start gpu-screen-recorder: {e}")),
        };
        let pid = child.id().map_or(0, |p| p as i32);
        self.generation += 1;
        let generation = self.generation;
        let (exited_tx, exited) = oneshot::channel();
        let internal = self.internal_tx.clone();
        tokio::spawn(async move {
            let _ = child.wait().await;
            let _ = exited_tx.send(());
            let _ = internal.send(Internal::Exited { generation });
        });
        info!(pid, "replay buffer recording");
        self.running = Some(Running {
            pid,
            generation,
            started: Instant::now(),
            exited,
        });
        self.error = None;
        self.retry_at = None;
        self.ensure_hotkey();
        self.publish();
    }

    /// Starting failed (a folder on a drive not mounted yet, a binary being
    /// replaced): say why, and try again after the backoff, as after a crash.
    fn retry_later(&mut self, why: String) {
        self.delay = recorder::next_delay(Duration::ZERO, self.delay);
        self.retry_at = Some(Instant::now() + self.delay);
        warn!(error = %why, delay_s = self.delay.as_secs(), "could not start gpu-screen-recorder");
        self.error = Some(format!("{why}; retrying in {} s", self.delay.as_secs()));
        self.publish();
    }

    pub async fn stop(&mut self) {
        if let Some(mut r) = self.running.take() {
            if r.signal(libc::SIGINT)
                && tokio::time::timeout(STOP_GRACE, &mut r.exited)
                    .await
                    .is_err()
            {
                warn!("gpu-screen-recorder ignored SIGINT; killing it");
                if r.signal(libc::SIGKILL) {
                    let _ = tokio::time::timeout(Duration::from_secs(1), &mut r.exited).await;
                }
            }
            info!("replay buffer stopped");
        }
        // Nothing is being started now, so a failure to start is over.
        self.retry_at = None;
        self.error = None;
        self.linger_until = None;
        self.publish();
    }

    pub async fn save(&mut self) -> Result<(), String> {
        // A child that's reaped but not yet handled stays in `running`, so its
        // `Exited` notice still schedules the restart.
        if !self
            .running
            .as_mut()
            .is_some_and(|r| r.signal(libc::SIGUSR1))
        {
            return Err(if self.settings.enabled {
                "the recorder isn't running".into()
            } else {
                "clips are off".into()
            });
        }
        let internal = self.internal_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SAVE_SETTLE).await;
            let _ = internal.send(Internal::Refresh);
        });
        Ok(())
    }

    pub async fn set_settings(&mut self, settings: ClipsSettings) -> Result<(), String> {
        settings.validate()?;
        let changed = settings != self.settings;
        self.settings = settings;
        recorder::save(&self.path, &self.settings);
        if changed {
            self.stop().await;
            self.delay = Duration::ZERO;
            if self.settings.enabled {
                self.start().await;
            } else if let Some(t) = self.hotkey_task.take() {
                t.abort();
                self.hotkey = None;
                self.hotkey_error = None;
            }
        }
        Ok(())
    }

    /// The game from the rules that's running now changed. Games-only clips
    /// start with a game, and stop [`GAME_GRACE`] after it has gone.
    pub async fn set_game(&mut self, game: Option<String>) {
        self.game = game;
        if self.game.is_some() {
            self.linger_until = None;
            self.start().await;
        } else if self.settings.games_only && self.running.is_some() && self.linger_until.is_none()
        {
            self.linger_until = Some(Instant::now() + GAME_GRACE);
        } else if self.running.is_none() && !self.wanted() {
            // A restart pending for the game that's gone won't happen.
            self.stop().await;
        }
        self.publish();
    }

    /// Sonar was turned on or off, or moved its channels to another output.
    /// Desktop audio follows, so a recorder taking it starts again there.
    pub async fn set_mixed(&mut self, mixed: Option<String>) {
        if mixed == self.mixed {
            return;
        }
        self.mixed = mixed;
        if self.settings.desktop_audio && self.running.is_some() {
            info!(output = ?self.mixed, "desktop audio moved; restarting the replay buffer");
            self.stop().await;
            self.start().await;
        }
    }

    /// The grace after a game is over: stop, unless the game came back.
    pub async fn linger_over(&mut self) {
        self.linger_until = None;
        if self.running.is_some() && !self.wanted() {
            info!("the game has gone; stopping the replay buffer");
            self.stop().await;
        }
    }

    pub async fn on_internal(&mut self, msg: Internal) {
        match msg {
            Internal::Exited { generation } => {
                if self
                    .running
                    .as_ref()
                    .is_some_and(|r| r.generation == generation)
                {
                    let r = self.running.take().expect("checked");
                    self.linger_until = None;
                    // After the game has gone, an exit just ends it.
                    if self.wanted() {
                        self.delay = recorder::next_delay(r.started.elapsed(), self.delay);
                        self.retry_at = Some(Instant::now() + self.delay);
                        self.error = Some(format!(
                            "gpu-screen-recorder stopped; retrying in {} s",
                            self.delay.as_secs()
                        ));
                        warn!(
                            delay_s = self.delay.as_secs(),
                            "gpu-screen-recorder stopped unexpectedly"
                        );
                    }
                }
            }
            Internal::Refresh => {}
            Internal::Hotkey(result) => match result {
                Ok(trigger) => {
                    self.hotkey = Some(trigger);
                    self.hotkey_error = None;
                }
                Err(e) => {
                    self.hotkey = None;
                    self.hotkey_error = Some(format!(
                        "no hotkey ({e}); use the button or `ggctl clip save`"
                    ));
                }
            },
            Internal::SaveFromHotkey => {
                if let Err(e) = self.save().await {
                    warn!(error = %e, "hotkey save failed");
                }
            }
        }
        self.publish();
    }

    fn ensure_hotkey(&mut self) {
        if self.hotkey_task.is_some() || !self.hotkey_allowed {
            return;
        }
        let internal = self.internal_tx.clone();
        self.hotkey_task = Some(tokio::spawn(hotkey::run(internal)));
    }

    pub fn state(&self) -> ClipsState {
        let dir = self.dir();
        ClipsState {
            available: self.program().is_some(),
            enabled: self.settings.enabled,
            recording: self.running.is_some(),
            settings: self.settings.clone(),
            dir: dir.to_string_lossy().into_owned(),
            clips: recorder::list(&dir),
            error: self.error.clone(),
            hotkey: self.hotkey.clone(),
            hotkey_error: self.hotkey_error.clone(),
            game: self.game.clone(),
            waiting: self.settings.enabled
                && self.settings.games_only
                && self.running.is_none()
                && self.game.is_none(),
        }
    }

    fn publish(&mut self) {
        let now = self.state();
        if self.published.as_ref() != Some(&now) {
            self.published = Some(now.clone());
            let _ = self.event_tx.send(Event::ClipsChanged { state: now });
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::GetState(reply) => {
                let _ = reply.send(Ok(self.state()));
            }
            Command::SetSettings { settings, reply } => {
                let _ = reply.send(self.set_settings(settings).await);
            }
            Command::Save(reply) => {
                let _ = reply.send(self.save().await);
            }
            Command::Delete { name, reply } => {
                let result = recorder::clip_path(&self.dir(), &name).and_then(|p| {
                    std::fs::remove_file(&p).map_err(|e| format!("could not delete {name}: {e}"))
                });
                let _ = reply.send(result);
                self.publish();
            }
            Command::Shutdown(reply) => {
                self.stop().await;
                let _ = reply.send(Ok(()));
            }
        }
    }
}

pub fn spawn(
    event_tx: broadcast::Sender<Event>,
    mut games: watch::Receiver<Option<String>>,
    mut mixed: watch::Receiver<Option<String>>,
) -> Handle {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let path = gg_ipc::config_dir().join("clips.json");
    let (mut c, mut internal) = Controller::new(recorder::load(&path), path, None, event_tx);
    c.hotkey_allowed = true;
    tokio::spawn(async move {
        c.game = games.borrow_and_update().clone();
        c.mixed = mixed.borrow_and_update().clone();
        c.start().await;
        loop {
            let retry = c.retry_at;
            let linger = c.linger_until;
            tokio::select! {
                command = rx.recv() => match command {
                    Some(command) => c.handle(command).await,
                    None => break,
                },
                Some(msg) = internal.recv() => c.on_internal(msg).await,
                Ok(()) = games.changed() => {
                    let game = games.borrow_and_update().clone();
                    c.set_game(game).await;
                }
                Ok(()) = mixed.changed() => {
                    let output = mixed.borrow_and_update().clone();
                    c.set_mixed(output).await;
                }
                _ = async { tokio::time::sleep_until(retry.expect("guarded").into()).await }, if retry.is_some() => {
                    c.retry_at = None;
                    c.start().await;
                }
                _ = async { tokio::time::sleep_until(linger.expect("guarded").into()).await }, if linger.is_some() => {
                    c.linger_over().await;
                }
            }
        }
        c.stop().await;
    });
    Handle { tx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A stand-in recorder: logs its arguments and the signals it gets.
    fn stand_in(dir: &Path, exit_at_once: bool) -> PathBuf {
        let log = dir.join("log");
        let script = dir.join("gpu-screen-recorder");
        let body = if exit_at_once {
            "exit 3".to_string()
        } else {
            format!("trap 'echo usr1 >> {l}' USR1\ntrap 'echo int >> {l}; exit 0' INT\nwhile true; do sleep 0.05; done", l = log.display())
        };
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"start $*\" >> {}\n{body}\n",
                log.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// A new folder for one test, made here with a name nobody can guess and
    /// private: never one another account made first, since the stand-in in it
    /// gets run.
    fn scratch(tag: &str) -> PathBuf {
        use std::hash::BuildHasher;
        use std::os::unix::fs::DirBuilderExt;
        let random = std::collections::hash_map::RandomState::new().hash_one(tag);
        let d = std::env::temp_dir().join(format!(
            "gglinux-clipctl-{}-{tag}-{random:016x}",
            std::process::id()
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&d).unwrap();
        d
    }

    #[test]
    fn scratch_folders_are_new_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let (a, b) = (scratch("probe"), scratch("probe"));
        assert_ne!(a, b, "a name nobody can guess, so nobody can make it first");
        assert_eq!(
            std::fs::metadata(&a).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_dir_all(a).unwrap();
        std::fs::remove_dir_all(b).unwrap();
    }

    fn controller(dir: &Path, program: PathBuf) -> (Controller, mpsc::UnboundedReceiver<Internal>) {
        let (event_tx, _) = broadcast::channel(8);
        let settings = ClipsSettings {
            enabled: true,
            dir: Some(dir.join("clips").to_string_lossy().into_owned()),
            ..ClipsSettings::default()
        };
        Controller::new(settings, dir.join("clips.json"), Some(program), event_tx)
    }

    /// `c.start()`, retried while exec says "Text file busy": a fork in another
    /// test can briefly hold a stand-in that's still being written.
    async fn start(c: &mut Controller) {
        let busy = format!("(os error {})", libc::ETXTBSY);
        for _ in 0..50 {
            c.start().await;
            if !c.error.as_deref().is_some_and(|e| e.contains(&busy)) {
                return;
            }
            // The test's doing, not the recorder's: no backoff from it.
            c.delay = Duration::ZERO;
            c.retry_at = None;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn log_has(dir: &Path, line: &str) -> bool {
        log_has_line(dir, |l| l.starts_with(line)).await
    }

    async fn log_has_line(dir: &Path, wanted: impl Fn(&str) -> bool) -> bool {
        for _ in 0..40 {
            if std::fs::read_to_string(dir.join("log"))
                .unwrap_or_default()
                .lines()
                .any(&wanted)
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    #[tokio::test]
    async fn start_save_and_the_right_flags() {
        let d = scratch("save");
        let (mut c, _internal) = controller(&d, stand_in(&d, false));
        start(&mut c).await;
        assert!(c.state().recording);
        assert!(log_has(&d, "start -w portal").await);
        assert!(d.join("clips").is_dir(), "the folder is created");
        c.save().await.unwrap();
        assert!(log_has(&d, "usr1").await, "SIGUSR1 saves");
        c.stop().await;
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn turning_clips_off_interrupts_the_recorder() {
        let d = scratch("off");
        let (mut c, _internal) = controller(&d, stand_in(&d, false));
        start(&mut c).await;
        assert!(log_has(&d, "start").await);
        c.set_settings(ClipsSettings {
            enabled: false,
            ..c.settings.clone()
        })
        .await
        .unwrap();
        assert!(
            log_has(&d, "int").await,
            "SIGINT, so the file being written is finished"
        );
        assert!(!c.state().recording);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn an_unexpected_exit_is_reported_and_retried() {
        let d = scratch("exit");
        let (mut c, mut internal) = controller(&d, stand_in(&d, true));
        start(&mut c).await;
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(2), internal.recv())
                .await
                .unwrap()
                .unwrap();
            let exited = matches!(msg, Internal::Exited { .. });
            c.on_internal(msg).await;
            if exited {
                break;
            }
        }
        let s = c.state();
        assert!(!s.recording);
        assert!(
            s.error.as_deref().is_some_and(|e| e.contains("stopped")),
            "{:?}",
            s.error
        );
        assert_eq!(c.delay, Duration::from_secs(5));
        assert!(c.retry_at.is_some());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn desktop_audio_follows_sonar() {
        let d = scratch("sonar");
        let (mut c, _internal) = controller(&d, stand_in(&d, false));
        start(&mut c).await;
        assert!(
            log_has_line(&d, |l| l.starts_with("start")
                && l.ends_with("-a default_output"))
            .await
        );
        // Sonar on: the default output is now the Game channel alone.
        c.set_mixed(Some("alsa_output.usb-SteelSeries".into()))
            .await;
        start(&mut c).await; // retried if the stand-in was busy
        assert!(
            log_has(&d, "int").await,
            "the recorder hearing the default output is stopped"
        );
        let monitor = "-a device:alsa_output.usb-SteelSeries.monitor";
        assert!(
            log_has_line(&d, |l| l.starts_with("start") && l.ends_with(monitor)).await,
            "and started again on every channel"
        );
        c.set_mixed(Some("alsa_output.usb-SteelSeries".into()))
            .await;
        let log = std::fs::read_to_string(d.join("log")).unwrap();
        assert_eq!(
            log.lines().filter(|l| l.starts_with("start")).count(),
            2,
            "the same output changes nothing:\n{log}"
        );
        c.stop().await;
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn a_folder_that_cannot_be_made_yet_is_tried_again() {
        let d = scratch("no-folder");
        let (mut c, _internal) = controller(&d, stand_in(&d, false));
        // Something in the way, like a drive that's mounted only later.
        std::fs::write(d.join("clips"), b"").unwrap();
        c.start().await;
        assert!(
            c.error
                .as_deref()
                .is_some_and(|e| e.starts_with("cannot create")),
            "{:?}",
            c.error
        );
        assert!(c.retry_at.is_some(), "tried again after the backoff");
        assert_eq!(c.delay, Duration::from_secs(5));
        std::fs::remove_file(d.join("clips")).unwrap();
        c.retry_at = None; // the backoff is over, as in spawn()
        start(&mut c).await;
        let s = c.state();
        assert!(s.recording && s.error.is_none(), "{:?}", s.error);
        c.stop().await;
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn a_recorder_that_cannot_be_run_yet_is_tried_again() {
        let d = scratch("no-exec");
        let program = d.join("gpu-screen-recorder");
        std::fs::write(&program, "#!/bin/sh\n").unwrap(); // not executable
        let (mut c, _internal) = controller(&d, program);
        c.start().await;
        assert!(
            c.error
                .as_deref()
                .is_some_and(|e| e.starts_with("could not start")),
            "{:?}",
            c.error
        );
        assert!(c.retry_at.is_some(), "tried again after the backoff");
        assert!(!c.state().recording);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn clips_start_once_the_recorder_is_installed() {
        let d = scratch("install");
        let (mut c, _internal) = controller(&d, d.join("gpu-screen-recorder"));
        c.start().await;
        let s = c.state();
        assert!(!s.available && !s.recording);
        assert!(c.retry_at.is_some(), "it looks again later");
        stand_in(&d, false); // installed
        assert!(c.state().available);
        c.retry_at = None; // the wait is over, as in spawn()
        start(&mut c).await;
        let s = c.state();
        assert!(s.recording && s.error.is_none(), "{:?}", s.error);
        c.stop().await;
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn saving_while_off_is_an_error() {
        let d = scratch("save-off");
        let (mut c, _internal) = controller(&d, stand_in(&d, false));
        start(&mut c).await;
        assert!(log_has(&d, "start").await);
        c.set_settings(ClipsSettings {
            enabled: false,
            ..c.settings.clone()
        })
        .await
        .unwrap();
        assert_eq!(c.save().await, Err("clips are off".into()));
        let log = std::fs::read_to_string(d.join("log")).unwrap();
        assert!(
            !log.lines().any(|l| l == "usr1"),
            "no signal was sent:\n{log}"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    /// The next exit notice, left unhandled.
    async fn next_exit(internal: &mut mpsc::UnboundedReceiver<Internal>) -> Internal {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(2), internal.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(msg, Internal::Exited { .. }) {
                return msg;
            }
        }
    }

    #[tokio::test]
    async fn a_reaped_recorder_is_never_signalled() {
        let d = scratch("reaped");
        let (mut c, mut internal) = controller(&d, stand_in(&d, true));
        start(&mut c).await;
        // The waiter has reaped the child; hold its notice back, as a busy
        // `select!` might. The PID is free for anyone now.
        let exited = next_exit(&mut internal).await;
        assert_eq!(c.save().await, Err("the recorder isn't running".into()));
        c.on_internal(exited).await;
        assert!(
            c.retry_at.is_some(),
            "the notice still schedules the restart"
        );
        // Stopping signals nothing either, and never awaits the notice its check used up.
        start(&mut c).await;
        let _exited = next_exit(&mut internal).await;
        tokio::time::timeout(Duration::from_secs(1), c.stop())
            .await
            .unwrap();
        assert!(!c.state().recording);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn turning_clips_off_ends_a_pending_retry_and_its_message() {
        let d = scratch("retry-off");
        let (mut c, mut internal) = controller(&d, stand_in(&d, true));
        start(&mut c).await;
        let exited = next_exit(&mut internal).await;
        c.on_internal(exited).await;
        assert!(c.state().error.is_some_and(|e| e.contains("retrying")));
        c.set_settings(ClipsSettings {
            enabled: false,
            ..c.settings.clone()
        })
        .await
        .unwrap();
        assert_eq!(
            (c.state().error, c.retry_at),
            (None, None),
            "nothing will be retried"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    fn games_only(dir: &Path, program: PathBuf) -> (Controller, mpsc::UnboundedReceiver<Internal>) {
        let (mut c, internal) = controller(dir, program);
        c.settings.games_only = true;
        (c, internal)
    }

    #[tokio::test]
    async fn games_only_waits_for_a_game_and_stops_after_it_leaves() {
        let d = scratch("games-only");
        let (mut c, _internal) = games_only(&d, stand_in(&d, false));
        start(&mut c).await;
        let s = c.state();
        assert!(!s.recording && s.waiting, "no game yet");
        assert!(!d.join("log").exists(), "nothing was started");
        c.set_game(Some("game-a".into())).await;
        start(&mut c).await; // retried if the stand-in was busy
        assert!(log_has(&d, "start").await);
        let s = c.state();
        assert!(s.recording && !s.waiting);
        assert_eq!(s.game.as_deref(), Some("game-a"));
        c.set_game(None).await;
        assert!(
            c.state().recording,
            "kept through the grace, so the game's last moments can be saved"
        );
        assert!(c.linger_until.is_some());
        c.linger_over().await;
        assert!(log_has(&d, "int").await, "stopped with SIGINT");
        let s = c.state();
        assert!(!s.recording && s.waiting);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn a_game_back_within_the_grace_keeps_the_recorder() {
        let d = scratch("games-back");
        let (mut c, _internal) = games_only(&d, stand_in(&d, false));
        c.set_game(Some("game-a".into())).await;
        start(&mut c).await;
        assert!(log_has(&d, "start").await);
        c.set_game(None).await;
        c.set_game(Some("game-a".into())).await;
        assert!(c.linger_until.is_none());
        c.linger_over().await; // a timer that fires late changes nothing
        assert!(c.state().recording);
        let log = std::fs::read_to_string(d.join("log")).unwrap();
        assert_eq!(
            log.lines().filter(|l| l.starts_with("start")).count(),
            1,
            "one recorder throughout:\n{log}"
        );
        assert!(!log.lines().any(|l| l == "int"), "{log}");
        c.stop().await;
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn a_retry_ends_with_the_game() {
        let d = scratch("retry-game");
        let (mut c, mut internal) = games_only(&d, stand_in(&d, true));
        c.set_game(Some("game-a".into())).await;
        start(&mut c).await;
        let exited = next_exit(&mut internal).await;
        c.on_internal(exited).await;
        assert!(c.retry_at.is_some());
        c.set_game(None).await;
        let s = c.state();
        assert!(s.waiting);
        assert_eq!(
            (s.error, c.retry_at),
            (None, None),
            "no game, nothing to retry"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn a_refused_hotkey_is_still_reported_when_the_next_game_starts() {
        let d = scratch("hotkey");
        let (mut c, _internal) = games_only(&d, stand_in(&d, false));
        c.set_game(Some("game-a".into())).await;
        start(&mut c).await;
        c.on_internal(Internal::Hotkey(Err("the portal refused".into())))
            .await;
        let note = c.state().hotkey_error;
        assert!(
            note.as_deref()
                .is_some_and(|n| n.contains("the portal refused")),
            "{note:?}"
        );
        // The game ends and another starts: a new recorder, and no new binding.
        c.set_game(None).await;
        c.linger_over().await;
        c.set_game(Some("game-b".into())).await;
        start(&mut c).await;
        let s = c.state();
        assert!(s.recording);
        assert_eq!((s.hotkey, s.hotkey_error), (None, note));
        c.stop().await;
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn turning_games_only_on_with_no_game_stops_the_recorder() {
        let d = scratch("games-on");
        let (mut c, _internal) = controller(&d, stand_in(&d, false));
        start(&mut c).await;
        assert!(log_has(&d, "start").await);
        c.set_settings(ClipsSettings {
            games_only: true,
            ..c.settings.clone()
        })
        .await
        .unwrap();
        assert!(log_has(&d, "int").await);
        assert!(c.state().waiting);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn a_crash_after_the_game_is_not_retried() {
        let d = scratch("games-crash");
        let (mut c, mut internal) = games_only(&d, stand_in(&d, true));
        c.set_game(Some("game-a".into())).await;
        start(&mut c).await;
        c.set_game(None).await;
        let exited = next_exit(&mut internal).await;
        c.on_internal(exited).await;
        assert!(
            c.retry_at.is_none() && c.linger_until.is_none(),
            "no restart without the game"
        );
        assert!(c.state().waiting);
        std::fs::remove_dir_all(d).unwrap();
    }
}
