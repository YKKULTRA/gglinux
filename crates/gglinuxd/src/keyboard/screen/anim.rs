// SPDX-License-Identifier: GPL-3.0-or-later
//! An animation on the keyboard's screen: its pictures in turn, each for its own delay,
//! looping. It's timed by the monotonic clock in [`Now`], from the moment its first
//! picture is asked for, so a change of the wall clock doesn't make it jump.

use std::time::{Duration, Instant};

use gg_device_keyboard::Bitmap;
use gg_ipc::Screen;

use super::{Now, ScreenSource};

/// How long after a picture's end the loop wakes, so it lands inside the next one.
const LATE: Duration = Duration::from_millis(2);

pub struct Animation {
    frames: Vec<Bitmap>,
    /// When each picture ends, counted from the start of a round.
    ends: Vec<Duration>,
    /// When the first picture was asked for: every round is counted from it.
    start: Option<Instant>,
}

impl Animation {
    /// The pictures (laid out as [`Screen::Image`]'s) and how long each shows. A setting
    /// read from disk may not have been checked: a picture that doesn't parse is dark,
    /// and a delay outside the contract's range is pulled into it.
    pub fn new(frames: &[Vec<u8>], delays_ms: &[u16]) -> Animation {
        let range = Screen::FRAME_DELAY_MS;
        let mut round = Duration::ZERO;
        let ends = frames
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let ms = delays_ms.get(i).copied().unwrap_or(*range.start());
                round += Duration::from_millis(u64::from(ms.clamp(*range.start(), *range.end())));
                round
            })
            .collect();
        Animation {
            frames: frames
                .iter()
                .map(|f| Bitmap::from_rows(f).unwrap_or_default())
                .collect(),
            ends,
            start: None,
        }
    }

    /// The picture showing at `at`, and how long it has left.
    fn at(&self, at: Instant) -> Option<(usize, Duration)> {
        let start = self.start?;
        let round = self.ends.last()?.as_nanos();
        let into = at.saturating_duration_since(start).as_nanos() % round;
        let into = Duration::from_nanos(into as u64);
        let i = self.ends.partition_point(|&end| end <= into);
        Some((i, self.ends[i] - into))
    }
}

impl ScreenSource for Animation {
    fn frame(&mut self, now: &Now) -> Option<Bitmap> {
        self.start.get_or_insert(now.instant);
        let (i, _) = self.at(now.instant)?;
        self.frames.get(i).cloned()
    }

    /// When the picture showing now ends. Before the first one is shown, its delay: the
    /// loop draws it at once anyway, right after a setting changes.
    fn next_due(&self, now: &Now) -> Option<Duration> {
        if self.frames.len() < 2 {
            return None;
        }
        match self.at(now.instant) {
            Some((_, left)) => Some(left + LATE),
            None => self.ends.first().copied(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::screen::LocalTime;

    /// A picture with only pixel (`n`, 0) lit, so each frame is told apart.
    fn lit(n: usize) -> Vec<u8> {
        let mut rows = vec![0u8; Screen::IMAGE_BYTES];
        rows[n / 8] = 0x80 >> (n % 8);
        rows
    }

    fn shown(b: &Bitmap) -> usize {
        (0..128).find(|&x| b.get(x, 0)).expect("one pixel lit")
    }

    fn at(base: Instant, ms: u64) -> Now {
        Now {
            since_epoch: Duration::ZERO,
            local: LocalTime {
                hour: 0,
                minute: 0,
                second: 0,
                weekday: 0,
                day: 1,
                month: 0,
            },
            instant: base + Duration::from_millis(ms),
        }
    }

    #[test]
    fn each_picture_shows_for_its_delay_and_the_round_starts_over() {
        let base = Instant::now();
        let mut a = Animation::new(&[lit(0), lit(1), lit(2)], &[100, 300, 200]);
        let seen: Vec<usize> = [0, 99, 100, 399, 400, 599, 600, 700, 1300]
            .iter()
            .map(|&ms| shown(&a.frame(&at(base, ms)).unwrap()))
            .collect();
        assert_eq!(seen, [0, 0, 1, 1, 2, 2, 0, 1, 1]);
    }

    #[test]
    fn the_loop_is_woken_when_the_picture_changes_and_not_before() {
        let base = Instant::now();
        let mut a = Animation::new(&[lit(0), lit(1)], &[100, 250]);
        assert_eq!(
            a.next_due(&at(base, 0)),
            Some(Duration::from_millis(100)),
            "not shown yet: its first delay"
        );
        a.frame(&at(base, 0));
        assert_eq!(a.next_due(&at(base, 0)), Some(Duration::from_millis(102)));
        assert_eq!(a.next_due(&at(base, 40)), Some(Duration::from_millis(62)));
        assert_eq!(a.next_due(&at(base, 100)), Some(Duration::from_millis(252)));
        assert_eq!(a.next_due(&at(base, 349)), Some(Duration::from_millis(3)));
        // Woken where it said: the next picture is up.
        let woke = 40 + 62;
        assert_eq!(shown(&a.frame(&at(base, woke)).unwrap()), 1);
    }

    #[test]
    fn one_picture_is_a_still_and_never_wakes_the_loop() {
        let base = Instant::now();
        let mut a = Animation::new(&[lit(5)], &[100]);
        assert_eq!(shown(&a.frame(&at(base, 0)).unwrap()), 5);
        assert_eq!(a.next_due(&at(base, 50)), None);
    }

    #[test]
    fn an_unchecked_setting_from_disk_still_plays_safely() {
        let base = Instant::now();
        // Delays of 0 would make a round of no time at all.
        let mut a = Animation::new(&[lit(0), lit(1)], &[0, 60_000]);
        assert_eq!(shown(&a.frame(&at(base, 0)).unwrap()), 0);
        assert_eq!(shown(&a.frame(&at(base, 100)).unwrap()), 1, "0 ms is 100");
        assert_eq!(
            shown(&a.frame(&at(base, 10_099)).unwrap()),
            1,
            "60 s is 10 s"
        );
        assert_eq!(shown(&a.frame(&at(base, 10_100)).unwrap()), 0);
        // A broken picture is dark, and no pictures leave the screen to the keyboard.
        let mut broken = Animation::new(&[vec![1, 2, 3]], &[100]);
        assert!(broken.frame(&at(base, 0)).unwrap().is_dark());
        assert!(Animation::new(&[], &[]).frame(&at(base, 0)).is_none());
    }
}
