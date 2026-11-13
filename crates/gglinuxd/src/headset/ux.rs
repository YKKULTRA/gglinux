// SPDX-License-Identifier: GPL-3.0-or-later
//! The base station's screen settings reply (`06 80`: dim timeout, brightness,
//! home screen): when to ask for it, and which reply to believe.
//!
//! It is asked for only while `headset_extras` is on, and slowly, as these change
//! only when someone changes them. After gglinux changes one, what was read before
//! is old, and the first reply after the write may still be (real devices answer
//! late), so that reply is skipped and the next is asked for a few seconds later.
//! Until then the daemon shows what it wrote.

use std::time::{Duration, Instant};

use gg_device_headset::{UxReply, UxSettings};

/// How often to ask while `headset_extras` is on.
pub const INTERVAL: Duration = Duration::from_secs(30);

/// How soon to ask again after a screen setting is changed here.
pub const AFTER_CHANGE: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
pub struct UxRead {
    last: Option<Instant>,
    skip: bool,
    raw: Option<Vec<u8>>,
    /// The latest reply believed, decoded.
    pub settings: Option<UxSettings>,
}

impl UxRead {
    /// Whether to ask now: only with `headset_extras` on, at most every [`INTERVAL`].
    pub fn due(&self, extras: bool, now: Instant) -> bool {
        extras
            && self
                .last
                .is_none_or(|t| now.saturating_duration_since(t) >= INTERVAL)
    }

    pub fn asked(&mut self, now: Instant) {
        self.last = Some(now);
    }

    /// A screen setting was changed here.
    pub fn changed_here(&mut self, now: Instant) {
        self.settings = None;
        self.skip = true;
        self.soon(now);
    }

    /// Take a reply; `true` if it is believed and differs from the last one.
    pub fn took(&mut self, reply: &UxReply, now: Instant) -> bool {
        if reply.raw.is_none() {
            return false;
        }
        if self.skip {
            self.skip = false;
            self.soon(now);
            return false;
        }
        self.settings = reply.decoded;
        let changed = reply.raw != self.raw;
        self.raw.clone_from(&reply.raw);
        changed
    }

    /// The gate went off or the base station went away: what was read is old.
    pub fn forget(&mut self) {
        self.settings = None;
        self.raw = None;
        self.skip = false;
    }

    /// Ask again [`AFTER_CHANGE`] from `now`.
    fn soon(&mut self, now: Instant) {
        self.last = now.checked_sub(INTERVAL - AFTER_CHANGE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_device_headset::{DimTimeout, HomeScreen};

    fn reply(dim: u8) -> UxReply {
        let raw = vec![0x06, 0x80, dim, 0x0a, 0x01, 0x00];
        UxReply {
            decoded: UxSettings::decode(&raw),
            raw: Some(raw),
            events: Vec::new(),
        }
    }

    #[test]
    fn asked_only_with_the_gate_on_and_slowly() {
        let now = Instant::now();
        let mut ux = UxRead::default();
        assert!(!ux.due(false, now), "gate off: never");
        assert!(ux.due(true, now));
        ux.asked(now);
        assert!(!ux.due(true, now + Duration::from_secs(29)));
        assert!(ux.due(true, now + INTERVAL));
        assert!(!ux.due(false, now + INTERVAL * 10));
    }

    #[test]
    fn a_reply_is_believed_and_only_a_changed_one_is_news() {
        let now = Instant::now();
        let mut ux = UxRead::default();
        ux.asked(now);
        assert!(ux.took(&reply(0x01), now));
        assert_eq!(ux.settings.unwrap().dim_timeout, Some(DimTimeout::Min1));
        assert_eq!(ux.settings.unwrap().home_screen, Some(HomeScreen::Detailed));
        assert!(!ux.took(&reply(0x01), now), "the same again");
        let unanswered = UxReply::default();
        assert!(!ux.took(&unanswered, now));
        assert!(ux.settings.is_some(), "no answer keeps the last one");
    }

    /// After a change here the first reply may still be the old one: it is
    /// skipped, and the next is asked for soon, not in 30 s.
    #[test]
    fn the_first_reply_after_a_change_here_is_skipped() {
        let now = Instant::now();
        let mut ux = UxRead::default();
        ux.took(&reply(0x01), now);
        ux.changed_here(now);
        assert_eq!(ux.settings, None, "the written value shows meanwhile");
        assert!(!ux.due(true, now));
        let then = now + AFTER_CHANGE;
        assert!(ux.due(true, then));
        ux.asked(then);
        assert!(!ux.took(&reply(0x01), then), "maybe stale: skipped");
        assert_eq!(ux.settings, None);
        assert!(ux.due(true, then + AFTER_CHANGE));
        ux.asked(then + AFTER_CHANGE);
        assert!(ux.took(&reply(0x02), then + AFTER_CHANGE));
        assert_eq!(ux.settings.unwrap().dim_timeout, Some(DimTimeout::Min5));
    }

    #[test]
    fn forgetting_drops_what_was_read() {
        let now = Instant::now();
        let mut ux = UxRead::default();
        ux.took(&reply(0x01), now);
        ux.forget();
        assert_eq!(ux.settings, None);
        assert!(ux.took(&reply(0x01), now), "news again after forgetting");
    }
}
