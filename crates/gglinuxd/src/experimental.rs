// SPDX-License-Identifier: GPL-3.0-or-later
//! The experimental gates, saved in `~/.config/gglinux/experimental.json`: which features
//! that write to a device but haven't been tried on the owner's hardware yet are on. All
//! off by default, and a missing or unreadable file means all off.
//!
//! [`Gates`] holds the gates in a `watch` channel, for workers to follow
//! ([`Gates::subscribe`]). A change from a client is saved first, then put in the channel,
//! pushed to the keyboard worker (whose open keyboard lets a gated command family out only
//! while its gate is on) and sent to every client as [`Event::ExperimentalChanged`]. The
//! push is direct, in the same step: a request sent after the change's answer reaches the
//! keyboard worker after the change does.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gg_ipc::{Event, Experimental, Request, ResponseData};
use tokio::sync::{broadcast, watch};
use tracing::{info, warn};

use crate::keyboard;

pub fn path() -> PathBuf {
    gg_ipc::config_dir().join("experimental.json")
}

/// The saved gates; a missing file means all off. An unreadable one is moved aside.
pub fn load(path: &Path) -> Experimental {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            let moved_to = gg_ipc::set_aside(path);
            warn!(path = %path.display(), error = %e, %moved_to, "ignoring unreadable experimental settings");
            Experimental::default()
        }),
        Err(_) => Experimental::default(),
    }
}

/// Save atomically.
pub fn save(path: &Path, experimental: &Experimental) -> Result<(), String> {
    let write = || -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(experimental)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    write().map_err(|e| format!("could not save the experimental settings: {e:#}"))
}

/// The gates as they are now, and everyone a change goes to.
pub struct Gates {
    path: PathBuf,
    tx: watch::Sender<Experimental>,
    keyboard: keyboard::Handle,
    event_tx: broadcast::Sender<Event>,
    /// Held from saving a change to passing it on, so the file, the channel and the
    /// keyboard worker see changes in one order.
    changing: Mutex<()>,
}

impl Gates {
    /// The gates `current`, as loaded from `path` at start.
    pub fn new(
        path: PathBuf,
        current: Experimental,
        event_tx: broadcast::Sender<Event>,
        keyboard: keyboard::Handle,
    ) -> Gates {
        Gates {
            path,
            tx: watch::Sender::new(current),
            keyboard,
            event_tx,
            changing: Mutex::new(()),
        }
    }

    pub fn current(&self) -> Experimental {
        *self.tx.borrow()
    }

    /// Follow the gates from a worker: the receiver has the value now, and is told of
    /// each change. For the workers whose gates come with wave B (mouse, headset).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the mouse and headset workers follow it from wave B on"
        )
    )]
    pub fn subscribe(&self) -> watch::Receiver<Experimental> {
        self.tx.subscribe()
    }

    fn set(&self, experimental: Experimental) -> Result<(), String> {
        let _in_order = self.changing.lock().unwrap_or_else(|e| e.into_inner());
        save(&self.path, &experimental)?;
        info!(?experimental, "experimental features changed");
        self.tx.send_replace(experimental);
        self.keyboard.set_experimental(experimental);
        let _ = self
            .event_tx
            .send(Event::ExperimentalChanged { experimental });
        Ok(())
    }
}

/// `get_experimental` and `set_experimental`. A set is checked by its type alone: any
/// mix of gates is allowed.
pub fn handle(req: Request, gates: &Gates) -> ResponseData {
    match req {
        Request::GetExperimental => ResponseData::Experimental {
            experimental: gates.current(),
        },
        Request::SetExperimental { experimental } => match gates.set(experimental) {
            Ok(()) => ResponseData::Success,
            Err(message) => ResponseData::Error { message },
        },
        _ => ResponseData::Error {
            message: "not an experimental-gates request".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::Command;
    use crate::scratch::Scratch;
    use gg_device_keyboard::testing::FakeTransport;
    use gg_ipc::Feature;

    #[test]
    fn a_missing_file_is_all_off_and_a_saved_one_loads_back() {
        let d = Scratch::new("experimental-round-trip");
        let path = d.0.join("experimental.json");
        assert_eq!(load(&path), Experimental::default());
        let mut e = Experimental::default();
        e.set(Feature::KeyboardOnboard, true);
        e.set(Feature::HeadsetExtras, true);
        save(&path, &e).unwrap();
        assert_eq!(load(&path), e);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""keyboard_onboard": true"#), "{text}");
        assert!(
            !d.0.join("experimental.json.tmp").exists(),
            "written atomically"
        );
    }

    #[test]
    fn an_unreadable_file_is_set_aside_and_everything_is_off() {
        let d = Scratch::new("experimental-unreadable");
        let path = d.0.join("experimental.json");
        std::fs::write(&path, b"{\"keyboard_onboard\": tru").unwrap();
        assert_eq!(load(&path), Experimental::default());
        assert!(!path.exists(), "moved out of the way");
        let aside: Vec<String> = std::fs::read_dir(&d.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            aside
                .iter()
                .any(|n| n.starts_with("experimental.json.unreadable-")),
            "{aside:?}"
        );
    }

    /// The bindings family's read request: `B6`, normal layer, one key (A).
    fn bindings_read() -> Vec<u8> {
        let mut r = vec![0u8; gg_device_keyboard::proto::FEATURE_REPORT];
        r[1..5].copy_from_slice(&[0xB6, 0x00, 0x01, 0x04]);
        r
    }

    #[tokio::test]
    async fn a_set_is_saved_announced_followed_and_reaches_the_keyboards_families() {
        let d = Scratch::new("experimental-set");
        let path = d.0.join("experimental.json");
        let (event_tx, mut events) = broadcast::channel(16);
        let fake = FakeTransport::new();
        let kb = keyboard::spawn_fake_unasked(
            fake.keyboard(),
            d.0.join("nothing-saved-here"),
            event_tx.clone(),
            load(&path),
        );
        let gates = Gates::new(path.clone(), load(&path), event_tx, kb.clone());
        let mut follower = gates.subscribe();

        let refused = kb
            .send(|reply| Command::Exchange {
                report: bindings_read(),
                reply,
            })
            .await
            .unwrap_err();
        assert!(refused.contains("keyboard_bindings"), "{refused}");
        assert!(fake.sent().is_empty(), "nothing reached the keyboard");

        let mut on = Experimental::default();
        on.set(Feature::KeyboardBindings, true);
        assert!(matches!(
            handle(Request::SetExperimental { experimental: on }, &gates),
            ResponseData::Success
        ));
        // The very next request finds the keyboard's bindings family on.
        kb.send(|reply| Command::Exchange {
            report: bindings_read(),
            reply,
        })
        .await
        .unwrap();
        assert_eq!(fake.sent().len(), 1);
        assert_eq!(fake.sent()[0].report, bindings_read());

        assert_eq!(load(&path), on, "saved");
        // Past the keyboard worker's own news.
        let announced = std::iter::from_fn(|| events.try_recv().ok()).find_map(|e| match e {
            Event::ExperimentalChanged { experimental } => Some(experimental),
            _ => None,
        });
        assert_eq!(announced, Some(on));
        assert!(follower.has_changed().unwrap());
        assert_eq!(*follower.borrow_and_update(), on);
        match handle(Request::GetExperimental, &gates) {
            ResponseData::Experimental { experimental } => assert_eq!(experimental, on),
            other => panic!("{other:?}"),
        }

        // And off again.
        handle(
            Request::SetExperimental {
                experimental: Experimental::default(),
            },
            &gates,
        );
        assert!(kb
            .send(|reply| Command::Exchange {
                report: bindings_read(),
                reply,
            })
            .await
            .is_err());
        assert_eq!(fake.sent().len(), 1);
        kb.send::<()>(Command::Shutdown).await.unwrap();
    }

    #[test]
    fn a_set_that_cannot_be_saved_changes_nothing() {
        let d = Scratch::new("experimental-unsaved");
        // A file where the folder should be.
        std::fs::write(d.0.join("blocked"), b"").unwrap();
        let (event_tx, mut events) = broadcast::channel(4);
        let gates = Gates::new(
            d.0.join("blocked").join("experimental.json"),
            Experimental::default(),
            event_tx,
            keyboard::Handle::off(),
        );
        let mut on = Experimental::default();
        on.set(Feature::MousePower, true);
        match handle(Request::SetExperimental { experimental: on }, &gates) {
            ResponseData::Error { message } => {
                assert!(message.contains("could not save"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(gates.current(), Experimental::default());
        assert!(events.try_recv().is_err(), "nothing announced");
    }
}
