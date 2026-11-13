// SPDX-License-Identifier: GPL-3.0-or-later
//! The experimental gates, as the headset worker follows them.
//!
//! The worker is started with only the event bus, so it follows the gates there:
//! the saved file at start, then every [`Event::ExperimentalChanged`]. A change is
//! saved and announced before the client hears "ok" (`crate::experimental`), and
//! the worker looks before it handles each command, so a request sent after the
//! change's answer finds the change. If the worker fell so far behind that the
//! bus dropped news, the file has the latest gates: it is saved before each
//! announcement.

use std::path::PathBuf;

use gg_ipc::{Event, Experimental};
use tokio::sync::broadcast::{self, error::TryRecvError};

pub struct Gates {
    news: broadcast::Receiver<Event>,
    now: Experimental,
    path: PathBuf,
}

impl Gates {
    /// Follow the gates saved at `path` and announced on `event_tx`.
    pub fn follow(event_tx: &broadcast::Sender<Event>, path: PathBuf) -> Gates {
        // Listen first, so a change made while the file is read isn't missed.
        let news = event_tx.subscribe();
        Gates {
            news,
            now: crate::experimental::load(&path),
            path,
        }
    }

    /// The gates now, with every change announced since the last look.
    pub fn now(&mut self) -> Experimental {
        loop {
            match self.news.try_recv() {
                Ok(Event::ExperimentalChanged { experimental }) => self.now = experimental,
                Ok(_) => {}
                Err(TryRecvError::Lagged(_)) => {
                    self.now = crate::experimental::load(&self.path);
                }
                Err(TryRecvError::Empty | TryRecvError::Closed) => return self.now,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use gg_ipc::{Feature, HeadsetPower};

    fn on() -> Experimental {
        let mut e = Experimental::default();
        e.set(Feature::HeadsetExtras, true);
        e
    }

    #[test]
    fn the_saved_gates_hold_until_a_change_is_announced() {
        let d = Scratch::new("headset-gates-follow");
        let path = d.0.join("experimental.json");
        crate::experimental::save(&path, &on()).unwrap();
        let (event_tx, _) = broadcast::channel(8);
        let mut gates = Gates::follow(&event_tx, path);
        assert_eq!(gates.now(), on(), "as saved");
        // Other news passes by.
        let _ = event_tx.send(Event::MouseBattery { battery: None });
        assert_eq!(gates.now(), on());
        let _ = event_tx.send(Event::ExperimentalChanged {
            experimental: Experimental::default(),
        });
        assert_eq!(gates.now(), Experimental::default());
    }

    #[test]
    fn a_missing_file_is_all_off() {
        let d = Scratch::new("headset-gates-missing");
        let (event_tx, _) = broadcast::channel(8);
        let mut gates = Gates::follow(&event_tx, d.0.join("experimental.json"));
        assert!(!gates.now().on(Feature::HeadsetExtras));
    }

    /// The bus drops the oldest news for a listener that falls behind; the file,
    /// saved before each announcement, has the latest.
    #[test]
    fn news_dropped_while_behind_is_read_from_the_file() {
        let d = Scratch::new("headset-gates-lagged");
        let path = d.0.join("experimental.json");
        let (event_tx, _) = broadcast::channel(2);
        let mut gates = Gates::follow(&event_tx, path.clone());
        crate::experimental::save(&path, &on()).unwrap();
        let _ = event_tx.send(Event::ExperimentalChanged { experimental: on() });
        for _ in 0..3 {
            let _ = event_tx.send(Event::HeadsetChanged {
                state: gg_ipc::HeadsetState {
                    connected: false,
                    power: HeadsetPower::Unknown,
                    headset_battery: None,
                    spare_battery: None,
                    screen: gg_ipc::ScreenSettings::default(),
                    screen_dark: false,
                    controls: gg_ipc::HeadsetControls::default(),
                },
            });
        }
        assert_eq!(gates.now(), on());
    }
}
