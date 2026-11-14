// SPDX-License-Identifier: GPL-3.0-or-later
//! Actions on the computer, started by a mouse button (or any key) bound to one of the
//! spare keys F13–F24: the desktop reports the key through its global-shortcuts portal,
//! and the daemon runs what it's bound to. See `gg_ipc::Action`. gglinux never reads an
//! input device for this: the mouse sends the key, the desktop sees it.
//!
//! [`Service`] keeps the list (saved in `actions.json`, see [`store`]), binds its keys
//! through a [`Portal`](portal::Portal) and runs an action when its key comes in:
//! programs through a [`Spawner`](run::Spawner), profiles and clips through the
//! daemon's own workers ([`Daemon`]). The three are traits so tests never start a
//! program, type text or open a D-Bus session.
//!
//! The desktop is asked to bind keys only when the actions change: at start the daemon
//! binds again the keys it bound last time, which the desktop takes without asking (KDE
//! shows its dialog only for keys it hasn't confirmed for gglinux before).

pub mod portal;
pub mod run;
pub mod store;
#[cfg(test)]
pub mod testing;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::future::BoxFuture;
use gg_ipc::{Action, ActionBinding, ActionsState, Event, Request, ResponseData};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::{clips, profiles};
use portal::{DesktopPortal, Portal, Shortcut};
use run::Spawner;
use store::Saved;

/// How often the daemon tries again to bind the keys of last time, while the desktop
/// isn't there yet (the daemon can start at boot, before anyone logs in).
const RESTORE_RETRY: Duration = Duration::from_secs(30);

/// The daemon's own workers, for the actions that use them.
pub trait Daemon: Send + Sync {
    fn apply_profile(&self, name: String) -> BoxFuture<'_, Result<(), String>>;
    fn save_clip(&self) -> BoxFuture<'_, Result<(), String>>;
}

/// The real [`Daemon`]: the profiles and clips workers' handles.
pub struct Handles {
    pub profiles: profiles::Handle,
    pub clips: clips::Handle,
}

impl Daemon for Handles {
    fn apply_profile(&self, name: String) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(
            self.profiles
                .send::<()>(|reply| profiles::Command::Apply { name, reply }),
        )
    }
    fn save_clip(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(self.clips.send(clips::Command::Save))
    }
}

/// The actions, the keys bound for them, and what runs them.
pub struct Service {
    path: PathBuf,
    spawner: Box<dyn Spawner>,
    portal: Box<dyn Portal>,
    daemon: Box<dyn Daemon>,
    /// Whether text can be typed on this desktop, or why not.
    typing: Result<(), String>,
    event_tx: broadcast::Sender<Event>,
    now: Mutex<Now>,
    /// Held through a change or a restore, the desktop's dialog included, so they
    /// happen one at a time. Reading the state doesn't wait for it.
    changing: tokio::sync::Mutex<()>,
}

#[derive(Debug, Default)]
struct Now {
    bindings: Vec<ActionBinding>,
    /// The ids bound in this daemon's live portal session.
    bound: Vec<String>,
    /// The ids the desktop bound last time, as saved.
    recorded: Vec<String>,
    /// What the desktop said about the session; `None` while there's none.
    shortcuts: Option<String>,
    error: Option<String>,
    /// A change has bound keys since the start, so the old ones aren't restored.
    changed: bool,
}

impl Service {
    /// The actions saved at `path`. Nothing is bound until [`Service::restore`].
    pub fn new(
        path: PathBuf,
        spawner: Box<dyn Spawner>,
        portal: Box<dyn Portal>,
        daemon: Box<dyn Daemon>,
        typing: Result<(), String>,
        event_tx: broadcast::Sender<Event>,
    ) -> Service {
        let Saved { bindings, bound } = store::load(&path);
        Service {
            path,
            spawner,
            portal,
            daemon,
            typing,
            event_tx,
            now: Mutex::new(Now {
                bindings,
                recorded: bound,
                ..Now::default()
            }),
            changing: tokio::sync::Mutex::new(()),
        }
    }

    fn now(&self) -> MutexGuard<'_, Now> {
        self.now.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> ActionsState {
        let now = self.now();
        ActionsState {
            bindings: now.bindings.clone(),
            shortcuts: now.shortcuts.clone(),
            typing: self.typing.is_ok(),
            error: now.error.clone(),
        }
    }

    fn announce(&self) {
        let _ = self.event_tx.send(Event::ActionsChanged {
            state: self.state(),
        });
    }

    /// Keep `error` as the last thing that didn't go through; announce it if it's new.
    fn set_error(&self, error: Option<String>) {
        let changed = {
            let mut now = self.now();
            let changed = now.error != error;
            now.error = error;
            changed
        };
        if changed {
            self.announce();
        }
    }

    /// Bind again the keys the desktop bound last time, which it takes without asking.
    /// A key it never bound waits for the next change of the actions: starting the
    /// daemon must not bring up the desktop's dialog. Done once a change has bound keys.
    pub async fn restore(&self) -> Result<(), String> {
        let _one_at_a_time = self.changing.lock().await;
        let shortcuts: Vec<Shortcut> = {
            let now = self.now();
            if now.changed || !now.bound.is_empty() {
                return Ok(());
            }
            now.bindings
                .iter()
                .filter_map(Shortcut::of)
                .filter(|s| now.recorded.contains(&s.id))
                .collect()
        };
        if shortcuts.is_empty() {
            return Ok(());
        }
        match self.portal.bind(shortcuts).await {
            Ok(bound) => {
                {
                    let mut now = self.now();
                    now.shortcuts = (!bound.ids.is_empty()).then_some(bound.status);
                    now.bound = bound.ids;
                    now.error = None;
                }
                self.announce();
                Ok(())
            }
            Err(e) => {
                self.set_error(Some(e.clone()));
                Err(e)
            }
        }
    }

    /// [`Service::restore`] until it works, trying again `every` so long.
    pub async fn keep_restoring(&self, every: Duration) {
        while let Err(e) = self.restore().await {
            debug!(error = %e, "the action keys aren't bound yet; trying again");
            tokio::time::sleep(every).await;
        }
    }

    /// Change the actions: check, bind the keys if the set of keys changed, save,
    /// announce. If the desktop doesn't bind the new keys nothing changes: the old keys
    /// keep working, and the error says why.
    pub async fn set(&self, bindings: Vec<ActionBinding>) -> Result<(), String> {
        gg_ipc::validate_actions(&bindings)?;
        let _one_at_a_time = self.changing.lock().await;
        let wanted: Vec<Shortcut> = bindings.iter().filter_map(Shortcut::of).collect();
        let (same, rebind) = {
            let now = self.now();
            let mut want: Vec<&str> = wanted.iter().map(|s| s.id.as_str()).collect();
            let mut have: Vec<&str> = now.bound.iter().map(String::as_str).collect();
            want.sort_unstable();
            have.sort_unstable();
            (now.bindings == bindings, want != have)
        };
        if same && !rebind {
            return Ok(());
        }
        if rebind {
            match self.portal.bind(wanted).await {
                Ok(bound) => {
                    let mut now = self.now();
                    now.shortcuts = (!bound.ids.is_empty()).then_some(bound.status);
                    now.bound = bound.ids;
                    now.changed = true;
                }
                Err(e) => {
                    warn!(error = %e, "the desktop didn't bind the new action keys");
                    self.set_error(Some(e.clone()));
                    return Err(e);
                }
            }
        }
        let bound = self.now().bound.clone();
        store::save(
            &self.path,
            &Saved {
                bindings: bindings.clone(),
                bound: bound.clone(),
            },
        )?;
        {
            let mut now = self.now();
            now.bindings = bindings;
            now.recorded = bound;
            now.error = None;
        }
        info!("actions changed");
        self.announce();
        Ok(())
    }

    /// A key came in from the desktop: run its action.
    pub async fn activated(&self, id: &str) {
        let Some(key) = portal::key(id) else {
            return;
        };
        let binding = self.now().bindings.iter().find(|b| b.key == key).cloned();
        let Some(b) = binding else {
            debug!(id, "a spare key with no action");
            return;
        };
        let name = ActionBinding::key_name(key).unwrap_or_default();
        let error = self
            .run(&b.action)
            .await
            .err()
            .map(|e| format!("{name} ({}): {e}", b.label));
        if let Some(e) = &error {
            warn!(error = %e, "an action didn't go through");
        }
        self.set_error(error);
    }

    async fn run(&self, action: &Action) -> Result<(), String> {
        match action {
            Action::Run { argv } => self.spawner.spawn(argv),
            Action::Type { text } => {
                self.typing.clone()?;
                self.spawner.spawn(&run::typing(text))
            }
            Action::Media { usage } => self
                .spawner
                .spawn(&run::media(*usage, self.spawner.as_ref())?),
            Action::ProfileApply { name } => self.daemon.apply_profile(name.clone()).await,
            Action::ClipSave => self.daemon.save_clip().await,
        }
    }
}

/// The actions as the daemon runs them: keys bound through the desktop's portal (those
/// of last time again, without asking), programs started for real, profiles and clips
/// through their workers.
pub fn spawn(
    event_tx: broadcast::Sender<Event>,
    profiles: profiles::Handle,
    clips: clips::Handle,
) -> Arc<Service> {
    let (keys_tx, mut keys) = mpsc::unbounded_channel();
    let desktop = portal::desktop();
    let typing = run::can_type(
        &desktop,
        std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some(),
        run::Processes.has("wtype"),
    );
    let service = Arc::new(Service::new(
        store::path(),
        Box::new(run::Processes),
        Box::new(DesktopPortal::new(keys_tx)),
        Box::new(Handles { profiles, clips }),
        typing,
        event_tx,
    ));
    let restoring = Arc::clone(&service);
    tokio::spawn(async move { restoring.keep_restoring(RESTORE_RETRY).await });
    let running = Arc::clone(&service);
    tokio::spawn(async move {
        while let Some(id) = keys.recv().await {
            running.activated(&id).await;
        }
    });
    service
}

/// `get_actions` and `set_actions`.
pub async fn serve(req: Request, service: &Service) -> ResponseData {
    match req {
        Request::GetActions => ResponseData::Actions {
            state: service.state(),
        },
        Request::SetActions { bindings } => match service.set(bindings).await {
            Ok(()) => ResponseData::Success,
            Err(message) => ResponseData::Error { message },
        },
        _ => ResponseData::Error {
            message: "not an actions request".into(),
        },
    }
}
