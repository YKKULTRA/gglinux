// SPDX-License-Identifier: GPL-3.0-or-later
//! The idle scene: once the desktop has said for a while that nobody is at the computer,
//! the keyboard shows the idle zones instead of the usual ones, and goes back to them as
//! soon as somebody is there again.
//!
//! "Nobody there" is what screen care rests the keyboard's screen on: [`crate::presence`]
//! tells the worker ([`super::Command::SetAway`]) when no session of this user is active
//! and unlocked (logind's `LockedHint`/`IdleHint`, switched away), when the screensaver
//! service says the screen is locked, or while the computer goes to sleep. That message is
//! the seam: tests send it themselves. Input devices are never read for it.

use std::time::{Duration, Instant};

use gg_ipc::IdleLighting;

/// When the desktop said nobody is there, while it still says so.
#[derive(Debug, Default)]
pub struct Idle {
    away_since: Option<Instant>,
}

impl Idle {
    /// The desktop says somebody is there, or not. Saying "away" again keeps the first
    /// moment.
    pub fn set_away(&mut self, away: bool, now: Instant) {
        if away {
            self.away_since.get_or_insert(now);
        } else {
            self.away_since = None;
        }
    }

    /// Whether the idle scene is drawn at `now`.
    pub fn showing(&self, settings: &IdleLighting, now: Instant) -> bool {
        settings.enabled
            && self
                .away_since
                .is_some_and(|t| now.saturating_duration_since(t) >= delay(settings))
    }

    /// How long until the idle scene is due, while it's on and still waited for.
    pub fn due_in(&self, settings: &IdleLighting, now: Instant) -> Option<Duration> {
        let since = self.away_since.filter(|_| settings.enabled)?;
        let left = delay(settings).checked_sub(now.saturating_duration_since(since))?;
        (!left.is_zero()).then_some(left)
    }
}

fn delay(settings: &IdleLighting) -> Duration {
    Duration::from_secs(u64::from(settings.after_minutes) * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(minutes: u16) -> IdleLighting {
        IdleLighting {
            enabled: true,
            after_minutes: minutes,
            zones: Vec::new(),
        }
    }

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn the_idle_scene_starts_minutes_after_the_desktop_says_nobody_is_there() {
        let t0 = Instant::now();
        let mut idle = Idle::default();
        let five = after(5);
        assert!(!idle.showing(&five, t0), "somebody's there");
        idle.set_away(true, t0);
        assert!(!idle.showing(&five, t0 + 4 * MIN));
        assert_eq!(idle.due_in(&five, t0 + 4 * MIN), Some(MIN));
        assert!(idle.showing(&five, t0 + 5 * MIN));
        assert_eq!(idle.due_in(&five, t0 + 5 * MIN), None, "due already");
        idle.set_away(true, t0 + 4 * MIN);
        assert!(
            idle.showing(&five, t0 + 5 * MIN),
            "a second 'away' doesn't start the wait again"
        );
        idle.set_away(false, t0 + 6 * MIN);
        assert!(!idle.showing(&five, t0 + 6 * MIN), "back: the usual zones");
        assert_eq!(idle.due_in(&five, t0 + 6 * MIN), None);
    }

    #[test]
    fn zero_minutes_is_at_once_and_an_idle_scene_turned_off_never_shows() {
        let t0 = Instant::now();
        let mut idle = Idle::default();
        idle.set_away(true, t0);
        assert!(idle.showing(&after(0), t0));
        let off = IdleLighting {
            enabled: false,
            ..after(0)
        };
        assert!(!idle.showing(&off, t0 + 60 * MIN));
        assert_eq!(idle.due_in(&off, t0), None);
    }
}
