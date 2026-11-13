// SPDX-License-Identifier: GPL-3.0-or-later
//! A mouse that isn't there, for tests (this crate's, and gglinuxd's).
//!
//! [`FakeMouse`] behaves like the Rival on the owner's desk as far as the tests need:
//!
//! - it keeps every report written, in order ([`FakeMouse::sent`]), and every pause the
//!   code asked for instead of waiting ([`FakeMouse::waits`]), so tests run fast and can
//!   still check the pacing;
//! - it runs the settings it's sent (`2C`, `17`, `2E`, `2B`, `19`, `1A`), counts saves
//!   (`09`) and answers the queries (`AA 01`, `AC`, `97`, `AE`, `AB`) with one 64-byte input
//!   report each, as the dongle does;
//! - it **lags**: after a command that changes a setting, the first answer to each kind of
//!   query is still the one from before the command. Code that reads once, straight after a
//!   write, and trusts what it gets fails here;
//! - asleep ([`FakeMouse::set_asleep`]) it takes nothing it's sent and every answer is all
//!   zeros, as the dongle answers for a mouse that sleeps;
//! - it can drop the next command of one kind ([`FakeMouse::drop_next`]), as the real mouse
//!   drops a command that comes too close to another.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{ensure, Result};

use crate::proto::{self, DpiStage, PollingRate, Radio};
use crate::{Mouse, Transport, REPORT_SIZE};

/// The settings the fake mouse runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The DPI stages and which one is active, from 0.
    pub dpi: (Vec<DpiStage>, usize),
    pub polling: PollingRate,
    pub radio: Radio,
    pub sleep_secs: u16,
    /// The buttons table's arguments, as last sent (`19` without its command byte).
    pub buttons: Option<Vec<u8>>,
    /// The wheel's arguments, as last sent (`1A` without its command byte).
    pub wheel: Option<Vec<u8>>,
}

impl Default for Settings {
    /// What GG left on the owner's mouse: its DPI stages with stage 1 active, 1000 Hz,
    /// normal wireless, sleep after 20 minutes.
    fn default() -> Self {
        Settings {
            dpi: (
                [400, 800, 1600, 2400, 3200]
                    .map(DpiStage::uniform)
                    .to_vec(),
                0,
            ),
            polling: PollingRate::Hz1000,
            radio: Radio::Normal,
            sleep_secs: 1200,
            buttons: None,
            wheel: None,
        }
    }
}

/// The queries whose answers lag after a change.
const SETTING_QUERIES: [u8; 4] = [
    proto::CMD_READ_SENSITIVITY,
    proto::CMD_READ_POLLING_RATE,
    proto::CMD_READ_RADIO,
    proto::CMD_READ_SLEEP_TIMER,
];

#[derive(Debug)]
struct Inner {
    now: Settings,
    /// The settings before the last change, and the queries that still get them.
    before: Option<(Settings, HashSet<u8>)>,
    battery: u8,
    asleep: bool,
    drop_next: Option<u8>,
    saves: usize,
    sent: Vec<Vec<u8>>,
    waits: Vec<Duration>,
    answers: VecDeque<Vec<u8>>,
}

/// The fake mouse. Clones share one mouse: keep one in the test and give
/// [`FakeMouse::mouse`] to the code under test.
#[derive(Debug, Clone)]
pub struct FakeMouse {
    inner: Arc<Mutex<Inner>>,
    /// Its own record of the last command, so tests don't pace each other.
    last_command: Arc<Mutex<Option<(Instant, u8)>>>,
}

impl Default for FakeMouse {
    fn default() -> Self {
        FakeMouse::new()
    }
}

impl FakeMouse {
    /// An awake mouse at 80 %, running [`Settings::default`].
    pub fn new() -> FakeMouse {
        FakeMouse {
            inner: Arc::new(Mutex::new(Inner {
                now: Settings::default(),
                before: None,
                battery: 80,
                asleep: false,
                drop_next: None,
                saves: 0,
                sent: Vec::new(),
                waits: Vec::new(),
                answers: VecDeque::new(),
            })),
            last_command: Arc::new(Mutex::new(None)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The code's handle on this mouse.
    pub fn mouse(&self) -> Mouse {
        Mouse::with_transport(self.clone())
    }

    /// Put the mouse to sleep, or wake it.
    pub fn set_asleep(&self, asleep: bool) -> &Self {
        self.lock().asleep = asleep;
        self
    }

    /// Ignore the next command that starts with `command`.
    pub fn drop_next(&self, command: u8) -> &Self {
        self.lock().drop_next = Some(command);
        self
    }

    /// Every report written so far, each the 64 bytes after the report id.
    pub fn sent(&self) -> Vec<Vec<u8>> {
        self.lock().sent.clone()
    }

    /// The first byte of each report written: the commands, in order.
    pub fn commands(&self) -> Vec<u8> {
        self.lock().sent.iter().map(|r| r[0]).collect()
    }

    pub fn clear_sent(&self) {
        let mut inner = self.lock();
        inner.sent.clear();
        inner.waits.clear();
    }

    /// Every pause asked for, in order: one before each command, and any [`crate::READ_AGAIN`].
    pub fn waits(&self) -> Vec<Duration> {
        self.lock().waits.clone()
    }

    pub fn settings(&self) -> Settings {
        self.lock().now.clone()
    }

    /// How many saves (`09`) it has taken.
    pub fn saves(&self) -> usize {
        self.lock().saves
    }
}

impl Inner {
    /// What a query gets: the settings before the last change on its first read after
    /// it, else the settings now.
    fn view(&mut self, query: u8) -> Settings {
        if let Some((before, unread)) = &mut self.before {
            if unread.remove(&query) {
                return before.clone();
            }
        }
        self.now.clone()
    }

    fn take(&mut self, report: &[u8]) {
        let command = report[0];
        if self.asleep {
            // The dongle answers for a sleeping mouse with zeros, and the mouse takes nothing.
            if is_query(command) {
                self.answers.push_back(vec![0; REPORT_SIZE]);
            }
            return;
        }
        if self.drop_next == Some(command) {
            self.drop_next = None;
            return;
        }
        let before = self.now.clone();
        let args = &report[1..];
        match command {
            proto::CMD_SENSITIVITY => {
                if let Some(dpi) = proto::decode_sensitivity(args) {
                    self.now.dpi = dpi;
                }
            }
            proto::CMD_POLLING_RATE => {
                if let Some(rate) = proto::decode_polling_rate(args) {
                    self.now.polling = rate;
                }
            }
            proto::CMD_RADIO => {
                if let Some(radio) = proto::decode_radio(args) {
                    self.now.radio = radio;
                }
            }
            proto::CMD_SLEEP_TIMER => {
                if let Some(secs) = proto::decode_sleep_timer(args) {
                    self.now.sleep_secs = secs;
                }
            }
            proto::CMD_BUTTONS => self.now.buttons = Some(trimmed(args)),
            proto::CMD_WHEEL => self.now.wheel = Some(trimmed(args)),
            proto::CMD_SAVE => self.saves += 1,
            query => {
                let answer = self.answer(query, args);
                self.answers.push_back(answer);
            }
        }
        if is_change(command) {
            self.before = Some((before, SETTING_QUERIES.into_iter().collect()));
        }
    }

    /// The input report a query gets; zeros for one the fake doesn't know.
    fn answer(&mut self, query: u8, args: &[u8]) -> Vec<u8> {
        let mut answer = vec![0u8; REPORT_SIZE];
        match query {
            0xAA if args.first() == Some(&0x01) => answer[0] = self.battery,
            proto::CMD_READ_SENSITIVITY => {
                let (stages, active) = self.view(query).dpi;
                let packet = proto::sensitivity_packet(&stages, active)
                    .expect("the fake runs only stages it could take");
                answer[..packet.len() - 1].copy_from_slice(&packet[1..]);
            }
            proto::CMD_READ_POLLING_RATE => answer[0] = self.view(query).polling.code(),
            proto::CMD_READ_RADIO => answer[..2].copy_from_slice(&self.view(query).radio.bytes()),
            proto::CMD_READ_SLEEP_TIMER => {
                answer[4..6].copy_from_slice(&self.view(query).sleep_secs.to_le_bytes());
            }
            _ => {}
        }
        answer
    }
}

fn is_query(command: u8) -> bool {
    command == 0xAA || SETTING_QUERIES.contains(&command)
}

fn is_change(command: u8) -> bool {
    [
        proto::CMD_SENSITIVITY,
        proto::CMD_POLLING_RATE,
        proto::CMD_RADIO,
        proto::CMD_SLEEP_TIMER,
        proto::CMD_BUTTONS,
        proto::CMD_WHEEL,
    ]
    .contains(&command)
}

/// The arguments without the zeros that pad the report.
fn trimmed(args: &[u8]) -> Vec<u8> {
    let end = args.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    args[..end].to_vec()
}

impl Transport for FakeMouse {
    fn write(&self, report: &[u8]) -> Result<()> {
        ensure!(
            report.len() == 1 + REPORT_SIZE && report[0] == 0,
            "the mouse takes a report id of 0 and 64 bytes, not {:02X?}",
            report
        );
        let mut inner = self.lock();
        inner.sent.push(report[1..].to_vec());
        inner.take(&report[1..]);
        Ok(())
    }

    fn read_timeout(&self, buf: &mut [u8], _ms: i32) -> Result<usize> {
        let Some(answer) = self.lock().answers.pop_front() else {
            return Ok(0);
        };
        let n = answer.len().min(buf.len());
        buf[..n].copy_from_slice(&answer[..n]);
        Ok(n)
    }

    fn wait(&self, time: Duration) {
        self.lock().waits.push(time);
    }

    fn last_command(&self) -> &Mutex<Option<(Instant, u8)>> {
        &self.last_command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ButtonAction, ReadBack, COMMAND_PACE, LINK_PACE, READ_AGAIN};

    /// A pace as asked for: the full gap less the moment since the last command.
    fn about(wait: Duration, gap: Duration) -> bool {
        wait <= gap && wait + Duration::from_millis(20) > gap
    }

    #[test]
    fn the_wheel_goes_out_after_its_pace_and_is_run() {
        let fake = FakeMouse::new();
        let mouse = fake.mouse();
        mouse.set_buttons(&proto::default_buttons()).unwrap();
        mouse
            .set_wheel(ButtonAction::Button(4), ButtonAction::ScrollDown)
            .unwrap();
        mouse.save().unwrap();
        assert_eq!(fake.commands(), [0x19, 0x1A, 0x09]);
        let wheel = &fake.sent()[1];
        assert_eq!(wheel[..7], [0x1A, 0x04, 0, 0, 0, 0, 0x32]);
        assert!(wheel[7..].iter().all(|&b| b == 0), "the rest is zeros");
        assert_eq!(fake.settings().wheel, Some(vec![0x04, 0, 0, 0, 0, 0x32]));
        assert_eq!(fake.saves(), 1);
        // Each command after the first keeps GG's distance.
        let waits = fake.waits();
        assert_eq!(waits.len(), 3);
        assert!(waits[1..].iter().all(|&w| about(w, COMMAND_PACE)), "{waits:?}");
        // An action no capture shows sends nothing at all.
        fake.clear_sent();
        assert!(mouse
            .set_wheel(ButtonAction::DpiCycle, ButtonAction::ScrollDown)
            .is_err());
        assert!(fake.sent().is_empty());
    }

    /// The fake lags as the brief asks: the first answer after a change is the old one.
    #[test]
    fn a_change_shows_from_the_second_read_and_read_back_waits_for_it() {
        let fake = FakeMouse::new();
        let mouse = fake.mouse();
        mouse.set_polling_rate(PollingRate::Hz500).unwrap();
        assert_eq!(
            mouse
                .read_back(&PollingRate::Hz500, Mouse::checked_polling_rate)
                .unwrap(),
            ReadBack::Took
        );
        // 17, then AC 97 (old), a pause, AC 97 (new).
        assert_eq!(fake.commands(), [0x17, 0xAC, 0x97, 0xAC, 0x97]);
        let waits = fake.waits();
        assert!(about(waits[1], LINK_PACE), "the first read waits for the link: {waits:?}");
        assert!(waits.contains(&READ_AGAIN), "{waits:?}");
    }

    #[test]
    fn a_dropped_command_reads_back_as_the_old_value() {
        let fake = FakeMouse::new();
        let mouse = fake.mouse();
        fake.drop_next(proto::CMD_SLEEP_TIMER);
        mouse.set_sleep_timer(5).unwrap();
        assert_eq!(
            mouse.read_back(&300, Mouse::sleep_timer).unwrap(),
            ReadBack::Other(1200)
        );
        assert_eq!(fake.settings().sleep_secs, 1200);
        // Sent again, it takes.
        mouse.set_sleep_timer(5).unwrap();
        assert_eq!(
            mouse.read_back(&300, Mouse::sleep_timer).unwrap(),
            ReadBack::Took
        );
    }

    #[test]
    fn an_asleep_mouse_takes_nothing_and_answers_zeros() {
        let fake = FakeMouse::new();
        let mouse = fake.mouse();
        fake.set_asleep(true);
        assert_eq!(mouse.battery().unwrap(), None, "zeros: no battery");
        mouse.set_radio(Radio::Stable).unwrap();
        assert_eq!(fake.settings(), Settings::default());
        // Zeros would read as High-Efficiency Mode: the checked read sees no answer.
        assert_eq!(
            mouse.read_back(&Radio::Stable, Mouse::checked_radio).unwrap(),
            ReadBack::Silent
        );
        fake.set_asleep(false);
        assert_eq!(mouse.battery().unwrap().map(|b| b.level), Some(80));
    }
}
