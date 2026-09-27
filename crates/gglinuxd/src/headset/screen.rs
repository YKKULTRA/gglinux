//! When the base-station screen should be lit.
//!
//! Pure: no device access, and the clock is passed in, so every rule here is
//! unit-tested without hardware. The worker feeds it [`Input`]s and carries out
//! the [`Action`]s it returns.
//!
//! The screen cannot be read back, so this tracks what it last *made* it show,
//! and treats two kinds of event differently: ones the firmware already puts on
//! screen itself (a volume change), where it only starts the countdown, and ones
//! only gglinux sees, where it brings the stock screen back first.

use std::time::{Duration, Instant};

use gg_ipc::ScreenSettings;

/// While dark, re-send the black frame this often, in case the firmware put its
/// own screen back for a reason we cannot observe. A re-send can briefly flash
/// the stock screen (seen 2026-09-27 at 2 s intervals), so this is a backstop,
/// not a refresh.
pub const SAFETY_REBLANK: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// The base station was just opened: daemon start or replug.
    DeviceOpened,
    /// Something only gglinux knows about; the firmware shows nothing for it.
    OurEvent,
    /// Something the firmware already drew by itself (a volume change).
    FirmwareEvent,
    /// Time passed.
    Tick,
    /// The daemon is exiting.
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Draw the black frame.
    Blank,
    /// Return to the firmware's own screen.
    ShowStock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Showing the stock screen until `until`.
    Lit { until: Instant },
    /// Blanked at `last_blank` (the latest blank or re-blank).
    Dark { last_blank: Instant },
    /// Protection off: the firmware's screen, always.
    Disabled,
}

#[derive(Debug)]
pub struct Screen {
    mode: Mode,
    lit_for: Duration,
}

impl Screen {
    /// Starts lit: whatever is on screen now stays up for one lit period.
    pub fn new(settings: &ScreenSettings, now: Instant) -> Self {
        let lit_for = lit_duration(settings);
        let mode = if settings.protect {
            Mode::Lit { until: now + lit_for }
        } else {
            Mode::Disabled
        };
        Screen { mode, lit_for }
    }

    /// Whether the screen was last blanked (and not woken since).
    pub fn is_dark(&self) -> bool {
        matches!(self.mode, Mode::Dark { .. })
    }

    pub fn on(&mut self, input: Input, now: Instant) -> Option<Action> {
        match (self.mode, input) {
            (Mode::Disabled, _) => None,
            (_, Input::Shutdown) => self.blank(now),
            // After a (re)open the panel may be black from an earlier run or stock;
            // show stock either way so the countdown starts from a known state.
            (_, Input::DeviceOpened) | (Mode::Dark { .. }, Input::OurEvent) => {
                self.light(now);
                Some(Action::ShowStock)
            }
            (Mode::Lit { .. }, Input::OurEvent) | (_, Input::FirmwareEvent) => {
                self.light(now);
                None
            }
            (Mode::Lit { until }, Input::Tick) if now >= until => self.blank(now),
            (Mode::Dark { last_blank }, Input::Tick)
                if now.duration_since(last_blank) >= SAFETY_REBLANK =>
            {
                self.blank(now)
            }
            (_, Input::Tick) => None,
        }
    }

    /// Take new settings from a client.
    pub fn apply(&mut self, settings: &ScreenSettings, now: Instant) -> Option<Action> {
        self.lit_for = lit_duration(settings);
        if !settings.protect {
            let was_dark = self.is_dark();
            self.mode = Mode::Disabled;
            return was_dark.then_some(Action::ShowStock);
        }
        if self.mode == Mode::Disabled {
            // The stock screen is already up; count from now.
            self.light(now);
            return None;
        }
        // A change made from gglinux is an event in its own right, so its effect
        // (a new brightness) is visible before the screen goes dark again.
        self.on(Input::OurEvent, now)
    }

    fn light(&mut self, now: Instant) {
        self.mode = Mode::Lit { until: now + self.lit_for };
    }

    fn blank(&mut self, now: Instant) -> Option<Action> {
        self.mode = Mode::Dark { last_blank: now };
        Some(Action::Blank)
    }
}

fn lit_duration(settings: &ScreenSettings) -> Duration {
    Duration::from_secs(settings.lit_seconds.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_secs(10);

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn on() -> ScreenSettings {
        ScreenSettings::default()
    }

    fn off() -> ScreenSettings {
        ScreenSettings { protect: false, ..ScreenSettings::default() }
    }

    /// A screen that has just gone dark at `t0 + T`.
    fn dark(t0: Instant) -> Screen {
        let mut s = Screen::new(&on(), t0);
        assert_eq!(s.on(Input::Tick, t0 + T), Some(Action::Blank));
        s
    }

    #[test]
    fn starts_lit_then_blanks_after_the_lit_period() {
        let t0 = Instant::now();
        let mut s = Screen::new(&on(), t0);
        assert!(!s.is_dark());
        assert_eq!(s.on(Input::Tick, t0 + secs(9)), None);
        assert_eq!(s.on(Input::Tick, t0 + T), Some(Action::Blank));
        assert!(s.is_dark());
        assert_eq!(s.on(Input::Tick, t0 + T + secs(1)), None, "blank once, not every tick");
    }

    #[test]
    fn our_event_while_dark_shows_stock_then_blanks_again() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        let t = t0 + secs(60);
        assert_eq!(s.on(Input::OurEvent, t), Some(Action::ShowStock));
        assert!(!s.is_dark());
        assert_eq!(s.on(Input::Tick, t + secs(9)), None);
        assert_eq!(s.on(Input::Tick, t + T), Some(Action::Blank));
    }

    #[test]
    fn our_event_while_lit_extends_without_resending() {
        let t0 = Instant::now();
        let mut s = Screen::new(&on(), t0);
        assert_eq!(s.on(Input::OurEvent, t0 + secs(5)), None);
        assert_eq!(s.on(Input::Tick, t0 + T), None);
        assert_eq!(s.on(Input::Tick, t0 + secs(15)), Some(Action::Blank));
    }

    #[test]
    fn firmware_event_relights_without_sending_anything() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        let t = t0 + secs(60);
        assert_eq!(s.on(Input::FirmwareEvent, t), None, "the firmware already drew its popup");
        assert!(!s.is_dark());
        assert_eq!(s.on(Input::Tick, t + T), Some(Action::Blank));
    }

    #[test]
    fn repeated_firmware_events_blank_after_the_last_one() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        let t = t0 + secs(60);
        s.on(Input::FirmwareEvent, t);
        s.on(Input::FirmwareEvent, t + secs(5));
        assert_eq!(s.on(Input::Tick, t + T), None);
        assert_eq!(s.on(Input::Tick, t + secs(15)), Some(Action::Blank));
    }

    #[test]
    fn dark_screen_is_reblanked_every_ten_minutes() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        let went_dark = t0 + T;
        assert_eq!(s.on(Input::Tick, went_dark + SAFETY_REBLANK - secs(1)), None);
        assert_eq!(s.on(Input::Tick, went_dark + SAFETY_REBLANK), Some(Action::Blank));
        assert_eq!(s.on(Input::Tick, went_dark + SAFETY_REBLANK * 2 - secs(1)), None);
        assert_eq!(s.on(Input::Tick, went_dark + SAFETY_REBLANK * 2), Some(Action::Blank));
    }

    #[test]
    fn device_opened_always_shows_stock() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        assert_eq!(s.on(Input::DeviceOpened, t0 + secs(60)), Some(Action::ShowStock));
        let mut lit = Screen::new(&on(), t0);
        assert_eq!(lit.on(Input::DeviceOpened, t0 + secs(1)), Some(Action::ShowStock));
        assert_eq!(lit.on(Input::Tick, t0 + secs(1) + T), Some(Action::Blank));
    }

    #[test]
    fn shutdown_blanks_unless_disabled() {
        let t0 = Instant::now();
        let mut s = Screen::new(&on(), t0);
        assert_eq!(s.on(Input::Shutdown, t0 + secs(1)), Some(Action::Blank));
        let mut d = Screen::new(&off(), t0);
        assert_eq!(d.on(Input::Shutdown, t0 + secs(1)), None);
    }

    #[test]
    fn disabled_never_touches_the_screen() {
        let t0 = Instant::now();
        let mut s = Screen::new(&off(), t0);
        for input in [Input::Tick, Input::OurEvent, Input::FirmwareEvent, Input::DeviceOpened] {
            assert_eq!(s.on(input, t0 + secs(3600)), None, "{input:?}");
        }
        assert!(!s.is_dark());
    }

    #[test]
    fn disabling_while_dark_shows_stock() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        assert_eq!(s.apply(&off(), t0 + secs(60)), Some(Action::ShowStock));
        assert_eq!(s.on(Input::Tick, t0 + secs(3600)), None);
    }

    #[test]
    fn disabling_while_lit_sends_nothing_and_never_blanks() {
        let t0 = Instant::now();
        let mut s = Screen::new(&on(), t0);
        assert_eq!(s.apply(&off(), t0 + secs(1)), None, "the stock screen is already up");
        assert_eq!(s.on(Input::Tick, t0 + secs(3600)), None);
    }

    #[test]
    fn reenabling_lights_for_one_period_then_blanks() {
        let t0 = Instant::now();
        let mut s = Screen::new(&off(), t0);
        let t = t0 + secs(60);
        assert_eq!(s.apply(&on(), t), None);
        assert_eq!(s.on(Input::Tick, t + T), Some(Action::Blank));
    }

    #[test]
    fn a_settings_change_while_dark_is_shown_for_the_new_period() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        let t = t0 + secs(60);
        let longer = ScreenSettings { lit_seconds: 30, ..on() };
        assert_eq!(s.apply(&longer, t), Some(Action::ShowStock));
        assert_eq!(s.on(Input::Tick, t + secs(29)), None);
        assert_eq!(s.on(Input::Tick, t + secs(30)), Some(Action::Blank));
    }
}
