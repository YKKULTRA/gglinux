// SPDX-License-Identifier: GPL-3.0-or-later
//! Control for the SteelSeries **Apex Pro TKL Gen 3** (`1038:1642`): its lighting, its
//! screen and its switches.
//!
//! Everything sent is framed by [`proto`], which only frames commands on its
//! allowlist, and checked against [`allow`] once more at the write, with the gated
//! command families the daemon has turned on ([`Families`], all off by default). With
//! them off nothing here writes the keyboard's flash: the screen and switch settings last
//! until it loses power.
//!
//! The bytes reach the keyboard through a [`Transport`]: the hidapi device, or in tests a
//! fake ([`testing::FakeTransport`], also built for other crates with the `testing`
//! feature).

use std::cell::Cell;
use std::ffi::CString;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use tracing::{debug, warn};

pub mod allow;
pub mod bindings;
pub mod font;
pub mod keys;
pub mod layout;
pub mod oled;
pub mod onboard;
pub mod proto;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use allow::Families;
pub use oled::Bitmap;
pub use proto::{Rgb, TapMode, TapPair};

pub const VENDOR_ID: u16 = 0x1038;
pub const PRODUCT_ID: u16 = 0x1642;

/// The vendor interface OpenRGB drives, and the usage page that identifies it.
const CONTROL_INTERFACE: i32 = 1;
const VENDOR_USAGE_PAGE: u16 = 0xffc0;

const READ_TIMEOUT_MS: i32 = 200;
/// The most input reports [`Keyboard::drain_input`] throws away in one go.
const DRAIN_MAX: usize = 256;

/// How reports reach the keyboard's control interface. Every report starts with its
/// report id, 0, as hidapi takes them.
pub trait Transport: Send {
    /// Send a feature report.
    fn send_feature(&self, report: &[u8]) -> Result<()>;
    /// Read a feature report into `buf`, which holds the report id (0) in `buf[0]` on
    /// entry, as hidapi expects. Returns how many bytes were read, the report id included.
    fn get_feature(&self, buf: &mut [u8]) -> Result<usize>;
    /// Send an output report.
    fn write(&self, report: &[u8]) -> Result<()>;
    /// Read one input report, waiting at most `ms` milliseconds; 0 bytes when none came.
    fn read_timeout(&self, buf: &mut [u8], ms: i32) -> Result<usize>;
}

impl Transport for HidDevice {
    fn send_feature(&self, report: &[u8]) -> Result<()> {
        Ok(self.send_feature_report(report)?)
    }

    fn get_feature(&self, buf: &mut [u8]) -> Result<usize> {
        Ok(self.get_feature_report(buf)?)
    }

    fn write(&self, report: &[u8]) -> Result<()> {
        HidDevice::write(self, report)?;
        Ok(())
    }

    fn read_timeout(&self, buf: &mut [u8], ms: i32) -> Result<usize> {
        Ok(HidDevice::read_timeout(self, buf, ms)?)
    }
}

/// What [`Keyboard::set_switches`] sends, per analog key by HID code.
pub struct Switches<'a> {
    /// Actuation point, tenths of a millimetre (1..=40).
    pub actuation: &'a dyn Fn(u8) -> u8,
    pub rapid_trigger: &'a dyn Fn(u8) -> bool,
    /// Protection Mode; needs the protection family ([`Families::protection`]).
    pub protection: &'a dyn Fn(u8) -> bool,
    /// Rapid Trigger's re-trigger distance, tenths of a millimetre (1..=20).
    pub sensitivity: &'a dyn Fn(u8) -> u8,
    pub rapid_tap: bool,
    pub tap_pairs: &'a [TapPair],
}

pub struct Keyboard {
    transport: Box<dyn Transport>,
    /// The lighting interface's node, to tell whether it's still there; `None` for a
    /// transport that isn't a listed device.
    path: Option<CString>,
    /// The gated command families [`allow::check`] lets through, checked at every send.
    families: Cell<Families>,
}

/// Whether `d` is an Apex Pro TKL Gen 3's lighting interface.
fn is_lighting(d: &DeviceInfo) -> bool {
    d.vendor_id() == VENDOR_ID
        && d.product_id() == PRODUCT_ID
        && d.interface_number() == CONTROL_INTERFACE
        && d.usage_page() == VENDOR_USAGE_PAGE
}

/// Which kind of report a send is.
#[derive(Clone, Copy)]
enum Kind {
    Feature,
    Output,
}

impl Keyboard {
    /// Open the lighting interface of the first connected Apex Pro TKL Gen 3.
    pub fn open(api: &HidApi) -> Result<Self> {
        let path = api
            .device_list()
            .find(|d| is_lighting(d))
            .map(|d| d.path().to_owned())
            .ok_or_else(|| {
                anyhow!("Apex Pro TKL Gen 3 lighting interface not found — is it plugged in?")
            })?;
        let device = api
            .open_path(&path)
            .with_context(|| format!("opening {}", path.to_string_lossy()))?;
        debug!(path = %path.to_string_lossy(), "opened keyboard lighting interface");
        Ok(Self {
            transport: Box::new(device),
            path: Some(path),
            families: Cell::new(Families::default()),
        })
    }

    /// A keyboard reached through `transport`, a fake in tests. It's never in a device
    /// list, so [`Keyboard::is_listed`] says no.
    pub fn with_transport(transport: Box<dyn Transport>) -> Keyboard {
        Keyboard {
            transport,
            path: None,
            families: Cell::new(Families::default()),
        }
    }

    /// Whether `api`'s device list, as last refreshed, still has this
    /// keyboard's lighting interface.
    pub fn is_listed(&self, api: &HidApi) -> bool {
        self.path.as_ref().is_some_and(|path| {
            api.device_list()
                .any(|d| is_lighting(d) && d.path() == path.as_c_str())
        })
    }

    /// Let the gated command families through from now on (or stop them).
    pub fn set_families(&self, families: Families) {
        self.families.set(families);
    }

    /// The gated command families let through now.
    pub fn families(&self) -> Families {
        self.families.get()
    }

    /// Take over the lighting; send before the first frame.
    pub fn init(&self) -> Result<()> {
        self.feature(&proto::command(proto::OP_INIT, &[])?)
    }

    /// Show one frame: a colour for every LED, by HID code.
    pub fn show(&self, color_of: impl Fn(u8) -> Rgb) -> Result<()> {
        self.feature(&proto::direct_frame(color_of))
    }

    /// Give the lighting back to the keyboard's onboard profile.
    pub fn hand_back(&self) -> Result<()> {
        self.output(&proto::command(proto::OP_ONBOARD, &[])?)
    }

    /// The region (layout) byte; 1 is US ANSI.
    pub fn region(&self) -> Result<Option<u8>> {
        let reply =
            self.exchange_output(&proto::command(proto::OP_REGION, &[])?, READ_TIMEOUT_MS)?;
        Ok(reply.and_then(|r| proto::decode_region(&r)))
    }

    /// Show a picture on the screen until the next one, or until the screen is released.
    pub fn show_screen(&self, picture: &Bitmap) -> Result<()> {
        self.feature(&proto::oled_frame(&picture.packed()))
    }

    /// Give the screen back to the keyboard's own menus.
    pub fn release_screen(&self) -> Result<()> {
        self.output(&proto::oled_release())
    }

    /// Set every analog key's switch: its actuation point and Rapid Trigger distance in
    /// tenths of a millimetre, whether it has Rapid Trigger, and Rapid Tap. Lasts until
    /// the keyboard loses power.
    pub fn set_switches(&self, switches: &Switches<'_>) -> Result<()> {
        // Built first, so a bad pair list sends nothing at all.
        let pairs = proto::rapid_tap_pairs(switches.tap_pairs)?;
        self.feature(&proto::actuation_table(switches.actuation))?;
        self.feature(&proto::release_mode_table(
            switches.rapid_trigger,
            switches.protection,
        ))?;
        self.feature(&proto::sensitivity_table(switches.sensitivity))?;
        self.output(&pairs)?;
        self.output(&proto::rapid_tap_enable(switches.rapid_tap))
    }

    /// Send a feature report and read the keyboard's answer: the feature report it holds
    /// for the last command, [`proto::FEATURE_REPORT`] bytes long. Returns what follows the
    /// report id.
    pub fn exchange_feature(&self, report: &[u8]) -> Result<Vec<u8>> {
        self.feature(report)?;
        let mut buf = vec![0u8; proto::FEATURE_REPORT];
        let n = self
            .transport
            .get_feature(&mut buf)
            .with_context(|| format!("reading the reply to keyboard command {:#04x}", report[1]))?;
        Ok(buf.get(1..n.min(buf.len())).unwrap_or_default().to_vec())
    }

    /// Send an output report and read one input report back, waiting at most
    /// `timeout_ms`; `None` if none came in that time.
    pub fn exchange_output(&self, report: &[u8], timeout_ms: i32) -> Result<Option<Vec<u8>>> {
        self.output(report)?;
        let mut buf = [0u8; proto::INPUT_REPORT];
        let n = self
            .transport
            .read_timeout(&mut buf, timeout_ms)
            .with_context(|| format!("reading the reply to keyboard command {:#04x}", report[1]))?;
        Ok((n > 0).then(|| buf[..n.min(buf.len())].to_vec()))
    }

    /// Throw away the input reports waiting to be read: answers nobody read, which the
    /// next query would otherwise take for its own. Returns how many there were. A read
    /// error ends it; the next send or read meets that error again.
    pub fn drain_input(&self) -> usize {
        let mut buf = [0u8; proto::INPUT_REPORT];
        let mut dropped = 0;
        // The kernel keeps at most 64 for an open file; more means they keep coming.
        while dropped < DRAIN_MAX {
            match self.transport.read_timeout(&mut buf, 0) {
                Ok(0) => break,
                Ok(n) => {
                    debug!(report = ?&buf[..n.min(4)], "dropped an input report nobody read");
                    dropped += 1;
                }
                Err(e) => {
                    warn!(error = %e, "couldn't empty the keyboard's input queue");
                    break;
                }
            }
        }
        dropped
    }

    /// Wait until `deadline` for the input report that answers command `op`: the first
    /// that starts with `op`. Reports that start with anything else answer something
    /// else, and are skipped. `None` if none came in time.
    pub fn await_input(&self, op: u8, deadline: Instant) -> Result<Option<Vec<u8>>> {
        let mut buf = [0u8; proto::INPUT_REPORT];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let ms = i32::try_from(left.as_millis()).unwrap_or(i32::MAX);
            let n = self
                .transport
                .read_timeout(&mut buf, ms)
                .with_context(|| format!("reading the answer to keyboard command {op:#04x}"))?;
            // Nothing within the time left.
            if n == 0 {
                return Ok(None);
            }
            let report = &buf[..n.min(buf.len())];
            if report[0] == op {
                return Ok(Some(report.to_vec()));
            }
            debug!(
                waiting_for = format_args!("{op:#04x}"),
                report = ?&report[..report.len().min(4)],
                "skipped an input report that answers something else"
            );
        }
    }

    /// [`Keyboard::exchange_feature`], with a pause between the send and the read: some
    /// answers aren't ready on a read straight after the request (Apex Control's code waits
    /// 13 ms after each `83`).
    pub fn exchange_feature_settled(&self, report: &[u8], settle: Duration) -> Result<Vec<u8>> {
        self.feature(report)?;
        std::thread::sleep(settle);
        let mut buf = vec![0u8; proto::FEATURE_REPORT];
        let n = self
            .transport
            .get_feature(&mut buf)
            .with_context(|| format!("reading the reply to keyboard command {:#04x}", report[1]))?;
        Ok(buf.get(1..n.min(buf.len())).unwrap_or_default().to_vec())
    }

    fn feature(&self, report: &[u8]) -> Result<()> {
        self.send(Kind::Feature, report)
    }

    fn output(&self, report: &[u8]) -> Result<()> {
        self.send(Kind::Output, report)
    }

    /// The one place bytes reach the wire: the allowlist, with the families on now, is
    /// checked again right before the transport gets them.
    fn send(&self, kind: Kind, report: &[u8]) -> Result<()> {
        allow::check(report, self.families())?;
        match kind {
            Kind::Feature => self.transport.send_feature(report),
            Kind::Output => self.transport.write(report),
        }
        .with_context(|| format!("sending keyboard command {:#04x}", report[1]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testing::{FakeTransport, Key, Kind as Sent};

    #[test]
    fn todays_commands_go_out_through_the_transport_as_before() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        kb.init().unwrap();
        kb.show(|_| Rgb { r: 1, g: 2, b: 3 }).unwrap();
        kb.show_screen(&Bitmap::default()).unwrap();
        kb.release_screen().unwrap();
        kb.set_switches(&Switches {
            actuation: &|_| 15,
            rapid_trigger: &|_| false,
            protection: &|_| false,
            sensitivity: &|_| 2,
            rapid_tap: false,
            tap_pairs: &[],
        })
        .unwrap();
        kb.hand_back().unwrap();
        let sent: Vec<(Sent, u8, usize)> = fake
            .sent()
            .iter()
            .map(|s| (s.kind, s.report[1], s.report.len()))
            .collect();
        assert_eq!(
            sent,
            [
                (Sent::Feature, 0x4B, 65),
                (Sent::Feature, 0x40, 643),
                (Sent::Feature, 0x1F, 645),
                (Sent::Output, 0x1F, 65),
                (Sent::Feature, 0x38, 645),
                (Sent::Feature, 0x38, 645),
                (Sent::Feature, 0x38, 645),
                (Sent::Output, 0x38, 65),
                (Sent::Output, 0x38, 65),
                (Sent::Output, 0x41, 65),
            ]
        );
        assert_eq!(fake.sent()[0].report, proto::command(0x4B, &[]).unwrap());
        assert_eq!(fake.sent()[4].report, proto::actuation_table(|_| 15));
    }

    #[test]
    fn the_region_is_read_from_the_input_report_after_asking() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        assert_eq!(kb.region().unwrap(), None, "no reply: unknown");
        fake.answer_input(Key::op(0xF5), &[0xF5, 0x00, 0x01]);
        assert_eq!(kb.region().unwrap(), Some(1));
        assert_eq!(fake.sent()[1].report[..2], [0x00, 0xF5]);
        assert_eq!(fake.sent()[1].kind, Sent::Output);
    }

    #[test]
    fn exchange_feature_sends_then_reads_a_full_feature_report_after_the_id() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        kb.set_families(Families {
            bindings: true,
            onboard: false,
            protection: false,
        });
        fake.answer_feature(Key::op(0xB6), &[0xB6, 0x00, 0x00, 0x01, 0x04, 0x51, 0x04]);
        let mut request = vec![0u8; proto::FEATURE_REPORT];
        request[1..5].copy_from_slice(&[0xB6, 0x00, 0x01, 0x04]);
        let reply = kb.exchange_feature(&request).unwrap();
        assert_eq!(
            reply.len(),
            proto::FEATURE_REPORT - 1,
            "the report id is left off"
        );
        assert_eq!(reply[..7], [0xB6, 0x00, 0x00, 0x01, 0x04, 0x51, 0x04]);
        assert!(reply[7..].iter().all(|&b| b == 0));
        assert_eq!(fake.sent().len(), 1);
        assert_eq!(fake.sent()[0].kind, Sent::Feature);
    }

    #[test]
    fn exchange_output_writes_then_reads_one_input_report_or_none() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        let ask = proto::command(proto::OP_FIRMWARE, &[0x00]).unwrap();
        assert_eq!(kb.exchange_output(&ask, 10).unwrap(), None);
        fake.answer_input(Key::op(0x90), b"\x90\x001.19.7");
        let reply = kb.exchange_output(&ask, 10).unwrap().unwrap();
        assert_eq!(&reply[..8], b"\x90\x001.19.7");
        assert_eq!(reply.len(), proto::INPUT_REPORT, "a whole input report");
        assert!(fake.sent().iter().all(|s| s.kind == Sent::Output));
    }

    #[test]
    fn draining_throws_away_the_input_reports_nobody_read() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        assert_eq!(kb.drain_input(), 0);
        fake.queue_input(&[0x90, 0x00]).queue_input(&[0xB1, 0x00]);
        assert_eq!(kb.drain_input(), 2);
        assert_eq!(fake.queued_input(), 0);
        // A read error ends it.
        fake.queue_input(&[0x90, 0x00]).fail_reads(5..);
        assert_eq!(kb.drain_input(), 0);
    }

    #[test]
    fn awaiting_an_answer_skips_the_ones_for_other_commands() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        let ask = proto::command(proto::OP_FIRMWARE, &[0x00]).unwrap();
        fake.answer_input(Key::op(0x90), b"\x90\x001.19.7")
            .queue_input(&[0xF5, 0x00, 0x01])
            .queue_input(&[0x40, 0x00]);
        kb.output(&ask).unwrap();
        let soon = Instant::now() + Duration::from_millis(50);
        let reply = kb.await_input(0x90, soon).unwrap().unwrap();
        assert_eq!(&reply[..8], b"\x90\x001.19.7");
        assert_eq!(fake.queued_input(), 0, "both skipped");
        // Only others, then nothing: none came.
        fake.queue_input(&[0x40, 0x00]);
        assert_eq!(kb.await_input(0x90, soon).unwrap(), None);
        // A deadline already past still reads what's there.
        kb.output(&ask).unwrap();
        let past = Instant::now() - Duration::from_millis(1);
        assert!(kb.await_input(0x90, past).unwrap().is_some());
    }

    #[test]
    fn a_settled_feature_read_waits_between_the_request_and_the_read() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        let init = proto::command(proto::OP_INIT, &[]).unwrap();
        let start = Instant::now();
        let reply = kb
            .exchange_feature_settled(&init, Duration::from_millis(13))
            .unwrap();
        assert!(start.elapsed() >= Duration::from_millis(13));
        assert_eq!(reply[..2], [0x4B, 0x00]);
        assert_eq!(reply.len(), proto::FEATURE_REPORT - 1);
    }

    #[test]
    fn every_send_path_checks_the_allowlist_with_the_families_on_now() {
        let fake = FakeTransport::new();
        let kb = fake.keyboard();
        let mut read = vec![0u8; proto::FEATURE_REPORT];
        read[1..5].copy_from_slice(&[0xB6, 0x00, 0x01, 0x04]);
        let mut fn_key = vec![0u8; proto::COMMAND_REPORT];
        fn_key[1..3].copy_from_slice(&[0x35, 0xF0]);
        let refused = kb.exchange_feature(&read).unwrap_err();
        assert!(format!("{refused:#}").contains("bindings"), "{refused:#}");
        assert!(kb.exchange_output(&fn_key, 10).is_err());
        assert!(fake.sent().is_empty(), "nothing reached the transport");

        kb.set_families(Families {
            bindings: true,
            onboard: false,
            protection: false,
        });
        assert!(kb.families().bindings);
        kb.exchange_feature(&read).unwrap();
        kb.exchange_output(&fn_key, 10).unwrap();
        assert_eq!(fake.sent().len(), 2);

        kb.set_families(Families::default());
        assert!(kb.exchange_feature(&read).is_err(), "turned off again");
        assert_eq!(fake.sent().len(), 2);
    }

    #[test]
    fn a_failed_send_is_an_error_naming_the_command() {
        let fake = FakeTransport::new();
        fake.fail_send(2);
        let kb = fake.keyboard();
        kb.init().unwrap();
        let e = kb.hand_back().unwrap_err();
        assert!(format!("{e:#}").contains("0x41"), "{e:#}");
        kb.hand_back().unwrap();
        assert_eq!(fake.sent().len(), 2);
    }

    #[test]
    fn a_keyboard_on_a_transport_is_never_in_a_device_list() {
        let kb = FakeTransport::new().keyboard();
        assert!(kb.path.is_none());
        assert_eq!(kb.families(), Families::default());
    }
}
