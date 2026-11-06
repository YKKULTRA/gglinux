// SPDX-License-Identifier: GPL-3.0-or-later
//! The keyboard's screen: what gglinux draws on it, and screen care.
//!
//! A picture is sent when it changes (each second for a clock with seconds) and again every
//! [`SCREEN_REFRESH`], in case the keyboard's own menus drew over it. It's handed back when
//! the screen is set to the keyboard's own, or the daemon exits. With screen care on, a
//! picture of gglinux's moves a pixel every couple of minutes and goes dark while nobody is
//! at the computer, so the OLED doesn't burn in. [`ScreenState`] is all of it, held by the
//! keyboard worker.
//!
//! # Sources
//!
//! What a screen setting draws comes from a [`ScreenSource`]. [`ScreenState`] keeps one
//! for the setting in force (none for the keyboard's own screen, which gglinux leaves
//! alone), asks it for the picture each time the worker's loop comes round, and asks it
//! when the picture next changes by itself, so the loop wakes in time and sleeps until
//! then. Screen care is applied on top, to every source alike: the shift moves the
//! source's picture, and resting shows a dark screen without asking the source at all
//! (nor waking the loop for it).
//!
//! The sources, picked in [`source_for`]:
//! - [`Still`] for text and pictures;
//! - [`ClockFace`], which wakes the loop each second;
//! - [`anim::Animation`], each picture for its delay, looping, timed by [`Now::instant`];
//! - [`sysmon::Monitor`], the System Monitor's readings, read and drawn once a second
//!   (the graphics card's under the rule in [`gpu`]: a sleeping card is never woken).
//!
//! A new kind of screen is a new source. One fed from outside the worker (M12's
//! GameSense screen handlers) needs a command that hands it in, and implements the same
//! trait: `frame` gives the picture for a moment (cheaply: it's asked every loop, and an
//! unchanged picture isn't sent again until the refresh), `next_due` says when it next
//! changes by itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gg_device_keyboard::{oled, Bitmap, Keyboard};
use gg_ipc::{Screen, ScreenCare};
use tracing::{debug, warn};

use crate::clock::is_older_than;

mod anim;
mod gpu;
mod sysmon;

/// How often an unchanged screen picture is sent again, in case the keyboard's own menus
/// (the volume roller, the media button) drew over it.
pub const SCREEN_REFRESH: Duration = Duration::from_secs(10);
/// How long the picture stays put before it moves a pixel, with screen care's shift on.
pub const SHIFT_EVERY: u64 = 120;

/// What one screen setting draws, picture by picture.
pub trait ScreenSource: Send {
    /// The picture at `now`, before screen care moves it; `None` leaves the screen to the
    /// keyboard. Asked each time the worker's loop comes round, so keep it cheap: an
    /// unchanged picture isn't sent again until the refresh.
    fn frame(&mut self, now: &Now) -> Option<Bitmap>;

    /// How long after `now` the picture changes by itself, for the loop to wake then;
    /// `None` if it only changes with its setting.
    fn next_due(&self, now: &Now) -> Option<Duration>;
}

/// The moment a picture is drawn for, by each clock a source may want.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Now {
    /// Since the Unix epoch: what follows the wall clock (a clock's next second, the shift).
    pub since_epoch: Duration,
    /// The local time, for a clock face.
    pub local: LocalTime,
    /// The monotonic clock, for what times itself (an animation's pictures, how long
    /// since the graphics card was last asked): it doesn't jump with the wall clock.
    pub instant: Instant,
}

impl Now {
    pub fn current() -> Now {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Now {
            since_epoch,
            local: local_time(since_epoch.as_secs()),
            instant: Instant::now(),
        }
    }
}

/// The local time, for the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 0 is Sunday.
    pub weekday: u8,
    pub day: u8,
    /// 0 is January.
    pub month: u8,
}

fn local_time(unix_secs: u64) -> LocalTime {
    let t = unix_secs as libc::time_t;
    // SAFETY: `localtime_r` reads only `t` and writes only the `tm` it's given, both on
    // this stack.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    LocalTime {
        hour: tm.tm_hour as u8,
        minute: tm.tm_min as u8,
        second: tm.tm_sec.min(59) as u8,
        weekday: tm.tm_wday as u8,
        day: tm.tm_mday as u8,
        month: tm.tm_mon as u8,
    }
}

/// A picture that stays as it is: text, or a picture.
pub struct Still(pub Bitmap);

impl ScreenSource for Still {
    fn frame(&mut self, _now: &Now) -> Option<Bitmap> {
        Some(self.0.clone())
    }

    fn next_due(&self, _now: &Now) -> Option<Duration> {
        None
    }
}

/// The clock: the time in big digits, the date or AM/PM under it if asked for.
pub struct ClockFace {
    pub seconds: bool,
    pub h24: bool,
    pub date: bool,
}

impl ScreenSource for ClockFace {
    fn frame(&mut self, now: &Now) -> Option<Bitmap> {
        let (time, under) = clock_text(now.local, self.seconds, self.h24, self.date);
        Some(oled::clock(&time, under.as_deref()))
    }

    /// Just after each second turns.
    fn next_due(&self, now: &Now) -> Option<Duration> {
        let into_second = now.since_epoch.subsec_millis();
        Some(Duration::from_millis(u64::from(1000 - into_second) + 5))
    }
}

/// The source that draws `screen`; `None` for the keyboard's own.
pub fn source_for(screen: &Screen) -> Option<Box<dyn ScreenSource>> {
    Some(match screen {
        Screen::Keyboard => return None,
        Screen::Text { text } => Box::new(Still(oled::text(text))),
        Screen::Clock { seconds, h24, date } => Box::new(ClockFace {
            seconds: *seconds,
            h24: *h24,
            date: *date,
        }),
        Screen::Image { pixels } => Box::new(Still(Bitmap::from_rows(pixels).unwrap_or_default())),
        Screen::Animation { frames, delays_ms } => Box::new(anim::Animation::new(frames, delays_ms)),
        Screen::Monitor { items } => Box::new(sysmon::Monitor::new(
            items,
            sysmon::Roots::system(),
            Arc::new(gpu::System),
        )),
    })
}

/// The screen as the keyboard worker keeps it: the setting, screen care, and what was
/// last sent to the open keyboard.
pub struct ScreenState {
    setting: Screen,
    path: PathBuf,
    /// What draws the setting; `None` for the keyboard's own screen.
    source: Option<Box<dyn ScreenSource>>,
    /// Whether gglinux has drawn on this keyboard's screen since it connected.
    taken: bool,
    /// The picture last sent, and when, so an unchanged one isn't sent every loop.
    shown: Option<Bitmap>,
    last: Option<Instant>,
    error: Option<String>,
    care: ScreenCare,
    care_path: PathBuf,
    /// Nobody is at the computer, as [`crate::presence`] last said.
    away: bool,
    /// The screen is dark for that right now.
    resting: bool,
}

impl ScreenState {
    /// The saved screen and screen care in `config_dir`.
    pub fn load(config_dir: &Path) -> ScreenState {
        let path = config_dir.join("screen.json");
        let care_path = config_dir.join("screen-care.json");
        let setting: Screen = super::load(&path, "screen");
        ScreenState {
            source: source_for(&setting),
            setting,
            path,
            taken: false,
            shown: None,
            last: None,
            error: None,
            care: super::load(&care_path, "screen care"),
            care_path,
            away: false,
            resting: false,
        }
    }

    /// A new screen setting, checked and saved; drawn at the next loop.
    pub fn set(&mut self, screen: Screen) -> Result<(), String> {
        screen.validate()?;
        self.source = source_for(&screen);
        self.setting = screen;
        super::save(&self.path, &self.setting, "screen");
        self.shown = None; // draw the change now
        self.error = None;
        Ok(())
    }

    /// New screen care, saved; drawn at the next loop.
    pub fn set_care(&mut self, care: ScreenCare) {
        self.care = care;
        super::save(&self.care_path, &self.care, "screen care");
        self.shown = None; // draw the change now
    }

    /// Whether anybody is at the computer; true if that changed.
    pub fn set_away(&mut self, away: bool) -> bool {
        let changed = away != self.away;
        self.away = away;
        changed
    }

    /// How long the loop may sleep before the picture changes by itself. Resting, it
    /// doesn't: the dark screen changes only with a command (somebody's back).
    pub fn wait(&self, connected: bool) -> Option<Duration> {
        if !connected || resting(&self.setting, &self.care, self.away) {
            return None;
        }
        self.source.as_ref()?.next_due(&Now::current())
    }

    /// Draw the screen if its picture changed (or is due a refresh), or hand it back.
    pub fn render(&mut self, device: Option<&Keyboard>) {
        self.resting = device.is_some() && resting(&self.setting, &self.care, self.away);
        let Some(device) = device else {
            return;
        };
        let Some(picture) = self.frame(&Now::current()) else {
            self.error = None;
            self.release(Some(device));
            return;
        };
        if self.shown.as_ref() == Some(&picture) && !is_older_than(self.last, SCREEN_REFRESH) {
            return;
        }
        match device.show_screen(&picture) {
            Ok(()) => {
                self.taken = true;
                self.error = None;
            }
            Err(e) => {
                let message = format!("{e:#}");
                if self.error.as_ref() != Some(&message) {
                    warn!(error = %message, "the keyboard did not take the screen picture");
                }
                self.error = Some(message);
            }
        }
        // Kept even after a failure, so it's tried again at the refresh, not every loop.
        self.shown = Some(picture);
        self.last = Some(Instant::now());
    }

    /// Give the screen back to the keyboard's own menus, if gglinux drew on it.
    pub fn release(&mut self, device: Option<&Keyboard>) {
        if !self.taken {
            return;
        }
        if let Some(device) = device {
            match device.release_screen() {
                Ok(()) => debug!("keyboard screen handed back"),
                Err(e) => {
                    warn!(error = %format!("{e:#}"), "could not hand the keyboard screen back")
                }
            }
        }
        self.taken = false;
        self.shown = None;
    }

    /// Forget what was sent: a keyboard that (re)connects has its own screen.
    pub fn forget(&mut self) {
        self.taken = false;
        self.shown = None;
        self.last = None;
    }

    pub fn setting(&self) -> &Screen {
        &self.setting
    }

    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }

    pub fn care(&self) -> ScreenCare {
        self.care
    }

    pub fn resting(&self) -> bool {
        self.resting
    }

    /// What the screen should show at `now`, `None` for the keyboard's own: dark while
    /// resting, otherwise the source's picture, moved as screen care says.
    fn frame(&mut self, now: &Now) -> Option<Bitmap> {
        if resting(&self.setting, &self.care, self.away) {
            return Some(Bitmap::default());
        }
        let picture = self.source.as_mut()?.frame(now)?;
        Some(if self.care.shift {
            let (dx, dy) = shift_at(now.since_epoch.as_secs());
            picture.shifted(dx, dy)
        } else {
            picture
        })
    }
}

/// The clock's big line, and the small one under it if any.
fn clock_text(t: LocalTime, seconds: bool, h24: bool, date: bool) -> (String, Option<String>) {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut time = if h24 {
        format!("{:02}:{:02}", t.hour, t.minute)
    } else {
        let h = match t.hour % 12 {
            0 => 12,
            h => h,
        };
        format!("{h}:{:02}", t.minute)
    };
    if seconds {
        time.push_str(&format!(":{:02}", t.second));
    }
    let mut under = Vec::new();
    if date {
        under.push(format!(
            "{} {} {}",
            DAYS[usize::from(t.weekday % 7)],
            t.day,
            MONTHS[usize::from(t.month % 12)]
        ));
    }
    if !h24 {
        under.push(if t.hour < 12 { "AM" } else { "PM" }.to_owned());
    }
    (time, (!under.is_empty()).then(|| under.join(" ")))
}

/// Whether the screen rests dark now: nobody is there, and it's a picture of gglinux's,
/// or the keyboard's own and care covers that too.
fn resting(screen: &Screen, care: &ScreenCare, away: bool) -> bool {
    away && care.dark_when_away && (care.own_screen_too || !matches!(screen, Screen::Keyboard))
}

/// Where the picture sits at `unix_secs`, as an offset from where it's drawn: a 5 × 5
/// square walked row by row, back and forth, a pixel every [`SHIFT_EVERY`] seconds. Each
/// lit pixel's wear is spread over up to 25 neighbours.
fn shift_at(unix_secs: u64) -> (i32, i32) {
    let step = (unix_secs / SHIFT_EVERY % 25) as i32;
    let (row, col) = (step / 5, step % 5);
    let x = if row % 2 == 0 { col } else { 4 - col };
    (x - 2, row - 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::{spawn_fake, Command};
    use crate::scratch::Scratch;
    use gg_device_keyboard::testing::{FakeTransport, Kind};
    use gg_ipc::{Experimental, MonitorItem};
    use tokio::sync::broadcast;

    fn at(hour: u8, minute: u8, second: u8) -> Now {
        Now {
            since_epoch: Duration::ZERO,
            local: LocalTime {
                hour,
                minute,
                second,
                weekday: 4,
                day: 29,
                month: 9,
            },
            instant: Instant::now(),
        }
    }

    /// A picture with only pixel (`x`, `y`) lit.
    fn lit(x: usize, y: usize) -> Vec<u8> {
        let mut rows = vec![0u8; Screen::IMAGE_BYTES];
        rows[y * 16 + x / 8] = 0x80 >> (x % 8);
        rows
    }

    fn picture(screen: &Screen, now: &Now) -> Option<Bitmap> {
        source_for(screen)?.frame(now)
    }

    /// A screen state for `setting` that never touches a settings file.
    fn bare(setting: Screen) -> ScreenState {
        ScreenState {
            source: source_for(&setting),
            setting,
            path: PathBuf::new(),
            taken: false,
            shown: None,
            last: None,
            error: None,
            care: ScreenCare::default(),
            care_path: PathBuf::new(),
            away: false,
            resting: false,
        }
    }

    #[test]
    fn the_clock_reads_as_chosen() {
        let t = at(13, 5, 9).local;
        assert_eq!(clock_text(t, false, true, false), ("13:05".into(), None));
        assert_eq!(
            clock_text(t, true, true, true),
            ("13:05:09".into(), Some("Thu 29 Oct".into()))
        );
        assert_eq!(
            clock_text(t, false, false, false),
            ("1:05".into(), Some("PM".into()))
        );
        assert_eq!(
            clock_text(at(0, 30, 0).local, false, false, true),
            ("12:30".into(), Some("Thu 29 Oct AM".into())),
            "midnight is 12 AM"
        );
    }

    #[test]
    fn the_keyboard_s_own_screen_draws_nothing_and_a_clock_changes_each_second() {
        assert!(picture(&Screen::Keyboard, &at(1, 2, 3)).is_none());
        let clock = Screen::Clock {
            seconds: true,
            h24: true,
            date: false,
        };
        assert_ne!(picture(&clock, &at(1, 2, 3)), picture(&clock, &at(1, 2, 4)));
        let minutes = Screen::Clock {
            seconds: false,
            h24: true,
            date: false,
        };
        assert_eq!(
            picture(&minutes, &at(1, 2, 3)),
            picture(&minutes, &at(1, 2, 4)),
            "without seconds, the same picture: nothing is sent"
        );
    }

    #[test]
    fn a_picture_is_drawn_from_its_rows() {
        let mut pixels = vec![0u8; 640];
        pixels[0] = 0x80;
        let b = picture(&Screen::Image { pixels }, &at(0, 0, 0)).unwrap();
        assert!(b.get(0, 0) && !b.get(1, 0));
    }

    #[test]
    fn text_and_pictures_never_wake_the_loop_and_an_animation_and_the_monitor_do() {
        let now = at(1, 2, 3);
        let text = Screen::Text { text: "GG".into() };
        assert_eq!(picture(&text, &now), Some(oled::text("GG")));
        let image = Screen::Image { pixels: lit(0, 0) };
        for still in [text, image] {
            assert_eq!(source_for(&still).unwrap().next_due(&now), None, "{still:?}");
        }
        let animation = Screen::Animation {
            frames: vec![lit(0, 0), lit(1, 0)],
            delays_ms: vec![100, 100],
        };
        let mut a = source_for(&animation).unwrap();
        assert_eq!(a.frame(&now), Bitmap::from_rows(&lit(0, 0)).ok(), "the first picture");
        assert!(a.next_due(&now).is_some_and(|d| d <= Duration::from_millis(102)));
        // The monitor's source is only made here: drawing it would read this computer.
        let clock_only = Screen::Monitor {
            items: vec![MonitorItem::Clock],
        };
        let monitor = source_for(&clock_only).unwrap();
        assert!(monitor
            .next_due(&now)
            .is_some_and(|d| d <= Duration::from_millis(1005)));
    }

    #[test]
    fn screen_care_moves_and_darkens_every_picture_of_an_animation() {
        let mut screen = bare(Screen::Animation {
            frames: vec![lit(10, 10), lit(20, 12)],
            delays_ms: vec![100, 100],
        });
        let base = Instant::now();
        let step = 7 * SHIFT_EVERY;
        let when = |ms: u64| Now {
            since_epoch: Duration::from_secs(step),
            instant: base + Duration::from_millis(ms),
            ..at(0, 0, 0)
        };
        let (dx, dy) = shift_at(step);
        assert_ne!((dx, dy), (0, 0));
        let moved = |x: usize, y: usize| Bitmap::from_rows(&lit(x, y)).unwrap().shifted(dx, dy);
        assert_eq!(screen.frame(&when(0)), Some(moved(10, 10)));
        assert_eq!(screen.frame(&when(150)), Some(moved(20, 12)));
        assert_eq!(screen.frame(&when(250)), Some(moved(10, 10)), "round again");

        screen.care.shift = false;
        assert_eq!(screen.frame(&when(350)), Bitmap::from_rows(&lit(20, 12)).ok());

        screen.set_away(true);
        for ms in [400, 500, 600] {
            assert!(screen.frame(&when(ms)).unwrap().is_dark(), "dark at {ms} ms");
        }
    }

    #[test]
    fn a_resting_screen_doesn_t_wake_the_loop() {
        let mut screen = bare(Screen::Animation {
            frames: vec![lit(0, 0), lit(1, 0)],
            delays_ms: vec![100, 100],
        });
        assert!(screen.wait(true).is_some());
        screen.set_away(true);
        assert_eq!(screen.wait(true), None, "dark until somebody's back");
        screen.care.dark_when_away = false;
        assert!(screen.wait(true).is_some(), "care off: it plays on");
    }

    #[tokio::test]
    async fn an_animation_at_100_ms_goes_out_at_ten_pictures_a_second_at_most() {
        let fake = FakeTransport::new();
        let d = Scratch::new("anim-rate");
        let (event_tx, _) = broadcast::channel(64);
        let kb = spawn_fake(
            fake.keyboard(),
            d.0.clone(),
            event_tx,
            Experimental::default(),
        );
        let screen = Screen::Animation {
            // Well inside the screen, so the shift never moves one off it: two dark
            // pictures in a row would go out as one.
            frames: (0..10).map(|i| lit(10 + 4 * i, 20)).collect(),
            delays_ms: vec![100; 10],
        };
        kb.send(|reply| Command::SetScreen { screen, reply })
            .await
            .unwrap();
        let start = Instant::now();
        tokio::time::sleep(Duration::from_secs(3)).await;
        let sent = fake.sent();
        let secs = start.elapsed().as_secs_f64();
        kb.send::<()>(Command::Shutdown).await.unwrap();
        let frames: Vec<_> = sent
            .iter()
            .filter(|s| s.report[1..3] == [0x1F, 0x81])
            .collect();
        let bytes: usize = frames.iter().map(|s| s.report.len()).sum();
        eprintln!(
            "animation at 100 ms: {} pictures in {secs:.2} s = {:.2} a second, {:.0} bytes a second",
            frames.len(),
            frames.len() as f64 / secs,
            bytes as f64 / secs
        );
        assert_eq!(frames.len(), sent.len(), "nothing but pictures");
        assert!(frames.len() <= 32, "never faster than the delays: {}", frames.len());
        assert!(frames.len() >= 15, "and not far behind: {}", frames.len());
        for pair in frames.windows(2) {
            assert_ne!(pair[0].report, pair[1].report, "each picture goes out once");
        }
    }

    #[test]
    fn a_clock_wakes_the_loop_within_a_second_and_only_while_the_keyboard_is_here() {
        let clock = Screen::Clock {
            seconds: false,
            h24: true,
            date: false,
        };
        let state = bare(clock.clone());
        let wait = state.wait(true).unwrap();
        assert!(wait > Duration::ZERO && wait <= Duration::from_millis(1005));
        assert!(state.wait(false).is_none());
        assert!(bare(Screen::Keyboard).wait(true).is_none());
        let mut half = at(0, 0, 0);
        half.since_epoch = Duration::from_millis(1_500);
        assert_eq!(
            source_for(&clock).unwrap().next_due(&half),
            Some(Duration::from_millis(505))
        );
    }

    #[test]
    fn a_picture_is_sent_once_until_it_changes_dark_while_away_and_handed_back() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        let mut screen = bare(Screen::Text { text: "GG".into() });
        screen.care.shift = false;
        screen.render(Some(&kb));
        screen.render(Some(&kb));
        assert_eq!(fake.sent().len(), 1, "unchanged: not sent again");
        assert_eq!(fake.sent()[0].report[..3], [0x00, 0x1F, 0x81]);
        assert_eq!(
            fake.sent()[0].report[3..643],
            oled::text("GG").packed()[..],
            "the text, unshifted with shift off"
        );

        assert!(screen.set_away(true));
        assert!(!screen.set_away(true), "no change");
        screen.render(Some(&kb));
        assert!(screen.resting());
        assert_eq!(fake.sent().len(), 2);
        assert!(
            fake.sent()[1].report[3..643].iter().all(|&b| b == 0),
            "dark"
        );

        screen.set_away(false);
        screen.setting = Screen::Keyboard;
        screen.source = None;
        screen.render(Some(&kb));
        let last = fake.sent().last().cloned().unwrap();
        assert_eq!(
            (last.kind, &last.report[..3]),
            (Kind::Output, &[0x00, 0x1F, 0x82][..])
        );
        screen.render(Some(&kb));
        assert_eq!(fake.sent().len(), 3, "handed back once");

        // Nothing at all without a keyboard.
        screen.render(None);
        assert!(!screen.resting());
    }

    #[test]
    fn the_screen_rests_only_while_away_and_the_keyboards_own_only_when_asked() {
        let clock = Screen::Clock {
            seconds: false,
            h24: true,
            date: false,
        };
        let care = ScreenCare::default();
        assert!(resting(&clock, &care, true));
        assert!(!resting(&clock, &care, false), "somebody's there");
        assert!(
            !resting(&Screen::Keyboard, &care, true),
            "the keyboard's own screen is left alone by default"
        );
        let own = ScreenCare {
            own_screen_too: true,
            ..care
        };
        assert!(resting(&Screen::Keyboard, &own, true));
        let never = ScreenCare {
            dark_when_away: false,
            own_screen_too: true,
            ..care
        };
        assert!(!resting(&clock, &never, true) && !resting(&Screen::Keyboard, &never, true));
    }

    #[test]
    fn the_shift_walks_a_five_by_five_square_a_pixel_at_a_time() {
        let steps: Vec<(i32, i32)> = (0..25).map(|i| shift_at(i * SHIFT_EVERY)).collect();
        let mut seen = steps.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 25, "every offset once a round: {steps:?}");
        assert!(steps.iter().all(|&(x, y)| x.abs() <= 2 && y.abs() <= 2));
        for pair in steps.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert_eq!((a.0 - b.0).abs() + (a.1 - b.1).abs(), 1, "{a:?} to {b:?}");
        }
        assert_eq!(
            shift_at(SHIFT_EVERY - 1),
            shift_at(0),
            "it holds for a whole step"
        );
        assert_eq!(shift_at(25 * SHIFT_EVERY), shift_at(0), "and starts over");
    }
}
