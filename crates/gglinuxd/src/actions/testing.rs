// SPDX-License-Identifier: GPL-3.0-or-later
//! Fakes for the actions' seams, for the tests here and in `main.rs`: nothing they do
//! starts a program, types text or reaches D-Bus. They only write down what they were
//! asked.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use gg_ipc::Event;
use tokio::sync::broadcast;

use super::portal::{Bound, Portal, Shortcut};
use super::run::Spawner;
use super::{Daemon, Service};

/// Writes down what it would start; has the programs it was given.
#[derive(Clone, Default)]
pub struct FakeSpawner {
    spawned: Arc<Mutex<Vec<Vec<String>>>>,
    programs: Vec<String>,
}

impl FakeSpawner {
    pub fn with(programs: &[&str]) -> FakeSpawner {
        FakeSpawner {
            programs: programs.iter().map(|p| p.to_string()).collect(),
            ..FakeSpawner::default()
        }
    }

    /// Every `argv` it was handed, in order.
    pub fn spawned(&self) -> Vec<Vec<String>> {
        self.spawned.lock().unwrap().clone()
    }
}

impl Spawner for FakeSpawner {
    fn spawn(&self, argv: &[String]) -> Result<(), String> {
        self.spawned.lock().unwrap().push(argv.to_vec());
        Ok(())
    }
    fn has(&self, program: &str) -> bool {
        self.programs.iter().any(|p| p == program)
    }
}

/// Binds whatever it's asked, unless told to fail; writes down each set of ids.
#[derive(Clone, Default)]
pub struct FakePortal {
    binds: Arc<Mutex<Vec<Vec<String>>>>,
    fail: Arc<Mutex<Option<String>>>,
}

impl FakePortal {
    /// The ids of each bind, in order.
    pub fn binds(&self) -> Vec<Vec<String>> {
        self.binds.lock().unwrap().clone()
    }

    /// Refuse every bind from now on with `why`; `None` to bind again.
    pub fn fail_with(&self, why: Option<&str>) {
        *self.fail.lock().unwrap() = why.map(String::from);
    }
}

impl Portal for FakePortal {
    fn bind(&self, shortcuts: Vec<Shortcut>) -> BoxFuture<'_, Result<Bound, String>> {
        let ids: Vec<String> = shortcuts.into_iter().map(|s| s.id).collect();
        self.binds.lock().unwrap().push(ids.clone());
        let fail = self.fail.lock().unwrap().clone();
        Box::pin(async move {
            match fail {
                Some(why) => Err(why),
                None => Ok(Bound {
                    ids,
                    status: "bound (fake)".into(),
                }),
            }
        })
    }
}

/// Writes down the profiles applied and the clips saved: `profile <name>`, `clip`.
#[derive(Clone, Default)]
pub struct FakeDaemon {
    asked: Arc<Mutex<Vec<String>>>,
}

impl FakeDaemon {
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl Daemon for FakeDaemon {
    fn apply_profile(&self, name: String) -> BoxFuture<'_, Result<(), String>> {
        self.asked.lock().unwrap().push(format!("profile {name}"));
        Box::pin(async { Ok(()) })
    }
    fn save_clip(&self) -> BoxFuture<'_, Result<(), String>> {
        self.asked.lock().unwrap().push("clip".into());
        Box::pin(async { Ok(()) })
    }
}

/// The fakes behind a test's [`Service`].
#[derive(Clone, Default)]
pub struct Fakes {
    pub spawner: FakeSpawner,
    pub portal: FakePortal,
    pub daemon: FakeDaemon,
}

impl Fakes {
    /// A service over these fakes, saving at `path`, typing as `typing` says.
    pub fn service(
        &self,
        path: PathBuf,
        typing: Result<(), String>,
        event_tx: broadcast::Sender<Event>,
    ) -> Service {
        Service::new(
            path,
            Box::new(self.spawner.clone()),
            Box::new(self.portal.clone()),
            Box::new(self.daemon.clone()),
            typing,
            event_tx,
        )
    }
}
