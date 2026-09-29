//! GameSense: the SDK server, its files in Proton prefixes, and the games it
//! lets draw on the keyboard.

pub mod server;

use std::time::Instant;

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
