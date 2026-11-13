// SPDX-License-Identifier: GPL-3.0-or-later
//! Control for the SteelSeries **Rival 3 Wireless Gen 2** (`1038:1872`).
//!
//! Talks to the 2.4 GHz dongle's vendor HID interface. Bluetooth mode exposes a
//! different (unsolved) protocol and is not supported.
//!
//! ```no_run
//! # fn main() -> anyhow::Result<()> {
//! let api = hidapi::HidApi::new()?;
//! let mouse = gg_device_mouse::Mouse::open(&api)?;
//! println!("{:?}", mouse.battery()?);
//! mouse.set_polling_rate(gg_device_mouse::PollingRate::Hz1000)?;
//! mouse.save()?;
//! # Ok(()) }
//! ```

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use hidapi::{HidApi, HidDevice};
use tracing::debug;

pub mod proto;
pub mod tables;
pub mod testing;

pub use proto::{
    Battery, ButtonAction, DpiStage, PollingRate, Radio, BUTTON_COUNT, DEFAULT_WHEEL, MAX_PRESETS,
};

pub const VENDOR_ID: u16 = 0x1038;
pub const PRODUCT_ID: u16 = 0x1872;

/// The dongle exposes four HID interfaces; only interface 3 speaks the vendor
/// protocol. rivalcfg selects it purely by interface number, with no usage-page
/// filter, and we do the same.
pub const CONTROL_INTERFACE: i32 = 3;

/// Output/input report size from the device's HID descriptor.
const REPORT_SIZE: usize = 64;

/// How long to wait for a reply before treating the mouse as asleep.
const READ_TIMEOUT_MS: i32 = 300;

/// Upper bound on reports discarded by [`Mouse::drain`] in one go.
const MAX_DRAIN_REPORTS: usize = 16;

/// The least time between two commands. A command sent straight after another makes the
/// mouse drop the first: on the owner's mouse (2026-11-13) a setting followed at once by
/// the save (`09`) was lost every time, while 5 ms apart it was kept. GG 123 leaves 30 to
/// 40 ms between its commands, and so do we.
pub const COMMAND_PACE: Duration = Duration::from_millis(35);

/// The time the mouse is given after a command that makes its radio link settle again: the
/// polling rate (`17`) and the radio mode (`2E`). On the owner's mouse (2026-11-13) the
/// dongle answered no query, or answered late so that answers and queries got mixed up, for
/// about a second after either, and a sleep timer sent 0.5 to 0.6 s after one was lost;
/// 1.5 s after it was kept. GG 123 waits 3.0 s after `17` in both of our captures, and 1.4
/// and 2.7 s after `2E` before it sends anything it relies on.
pub const LINK_PACE: Duration = Duration::from_secs(3);

/// How long to wait before asking a second time when the first answer after a change was
/// still the old value. The mouse can answer late (seen on the owner's mouse, 2026-11-13),
/// so the first answer may be the one from before.
pub const READ_AGAIN: Duration = Duration::from_millis(300);

/// When the last command went out, and which it was. For the whole process, not per
/// [`Mouse`]: the daemon opens the mouse afresh for each request, and two requests in a
/// row must keep their distance as well.
static LAST_COMMAND: Mutex<Option<(Instant, u8)>> = Mutex::new(None);

/// How long to wait before the next command, the last one (`command`) having gone out at
/// `at`.
fn pace(last: Option<(Instant, u8)>, now: Instant) -> Duration {
    last.map_or(Duration::ZERO, |(at, command)| {
        let gap = if matches!(command, proto::CMD_POLLING_RATE | proto::CMD_RADIO) {
            LINK_PACE
        } else {
            COMMAND_PACE
        };
        gap.saturating_sub(now.saturating_duration_since(at))
    })
}

/// What the mouse code talks through: the dongle's hidraw node ([`HidDevice`]), or a fake
/// in tests ([`testing::FakeMouse`]).
pub trait Transport {
    /// Send one output report: byte 0 is the report id (0: unnumbered), then the 64 bytes.
    fn write(&self, report: &[u8]) -> Result<()>;
    /// Read one input report, waiting at most `ms` milliseconds; 0 bytes when none came.
    fn read_timeout(&self, buf: &mut [u8], ms: i32) -> Result<usize>;
    /// Wait `time` before the next command. A fake keeps the time instead of waiting.
    fn wait(&self, time: Duration) {
        std::thread::sleep(time);
    }
    /// When the last command went out through this transport, and which it was. The real
    /// mouse has one for the whole process ([`LAST_COMMAND`]).
    fn last_command(&self) -> &Mutex<Option<(Instant, u8)>> {
        &LAST_COMMAND
    }
}

impl Transport for HidDevice {
    fn write(&self, report: &[u8]) -> Result<()> {
        HidDevice::write(self, report)?;
        Ok(())
    }

    fn read_timeout(&self, buf: &mut [u8], ms: i32) -> Result<usize> {
        Ok(HidDevice::read_timeout(self, buf, ms)?)
    }
}

/// What reading a setting back after a change found ([`Mouse::read_back`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadBack<T> {
    /// The mouse reports the value sent.
    Took,
    /// It still reports this other value, asked twice.
    Other(T),
    /// It didn't answer.
    Silent,
}

/// An open handle to the mouse's control interface.
pub struct Mouse {
    device: Box<dyn Transport>,
}

impl Mouse {
    /// A mouse behind `transport`: a fake in tests ([`testing::FakeMouse::mouse`]).
    pub fn with_transport(transport: impl Transport + 'static) -> Self {
        Self {
            device: Box::new(transport),
        }
    }

    /// Open the control interface of the first connected Rival 3 Wireless Gen 2.
    pub fn open(api: &HidApi) -> Result<Self> {
        let path = api
            .device_list()
            .find(|d| {
                d.vendor_id() == VENDOR_ID
                    && d.product_id() == PRODUCT_ID
                    && d.interface_number() == CONTROL_INTERFACE
            })
            .map(|d| d.path().to_owned())
            .ok_or_else(|| {
                anyhow!(
                    "Rival 3 Wireless Gen 2 control interface (iface {CONTROL_INTERFACE}) not found \
                     — is the 2.4 GHz dongle plugged in? Bluetooth mode is not supported."
                )
            })?;

        let device = api.open_path(&path).with_context(|| {
            format!(
                "opening {} — if this is a permission error, install the udev rule \
                 (scripts/install-local.sh) so your session owns SteelSeries hidraw nodes",
                path.to_string_lossy()
            )
        })?;
        debug!(path = %path.to_string_lossy(), "opened mouse control interface");
        Ok(Self::with_transport(device))
    }

    /// Send one vendor command.
    ///
    /// Byte 0 of a hidraw write is always the report id. This device's descriptor
    /// declares **unnumbered** reports, so byte 0 is `0x00`; the kernel recognises
    /// that as "not numbered" and strips it, putting `payload` on the wire as a
    /// 64-byte output report. Getting this wrong fails silently — the write
    /// succeeds and the mouse simply ignores it.
    fn send(&self, payload: &[u8]) -> Result<()> {
        anyhow::ensure!(
            payload.len() <= REPORT_SIZE,
            "command of {} bytes exceeds the {REPORT_SIZE}-byte report",
            payload.len()
        );
        let mut buf = [0u8; 1 + REPORT_SIZE];
        buf[1..=payload.len()].copy_from_slice(payload);
        // Held across the wait and the write: commands leave one at a time, paced.
        let mut last = self
            .device
            .last_command()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.device.wait(pace(*last, Instant::now()));
        let sent = self.device.write(&buf);
        *last = Some((Instant::now(), payload[0]));
        drop(last);
        sent.with_context(|| format!("writing command 0x{:02X}", payload[0]))?;
        Ok(())
    }

    /// Discard any input reports queued before we asked for anything.
    ///
    /// The protocol has no request/response correlation id, so `recv` returns
    /// whichever report arrives first. A report left over from an earlier exchange
    /// would then be decoded as the answer to the current command — observed in
    /// practice as a battery reading with a bogus charging flag. Draining first
    /// makes the next report the one we actually asked for.
    fn drain(&self) {
        let mut buf = [0u8; REPORT_SIZE];
        // Bounded: a device streaming input reports faster than we drain must not
        // trap us here.
        for _ in 0..MAX_DRAIN_REPORTS {
            match self.device.read_timeout(&mut buf, 0) {
                Ok(n) if n > 0 => continue,
                _ => break,
            }
        }
    }

    /// Read the reply to a command that returns data, as an input report.
    ///
    /// Returns `None` on timeout, which for a wireless mouse normally just means
    /// it is asleep rather than that anything failed.
    fn recv(&self) -> Result<Option<Vec<u8>>> {
        let mut buf = [0u8; REPORT_SIZE];
        let n = self
            .device
            .read_timeout(&mut buf, READ_TIMEOUT_MS)
            .context("reading device reply")?;
        Ok((n > 0).then(|| buf[..n].to_vec()))
    }

    /// Query the battery. `None` means asleep or not reporting.
    pub fn battery(&self) -> Result<Option<Battery>> {
        self.drain();
        self.send(&proto::battery_packet())?;
        match self.recv()? {
            Some(data) => Ok(proto::decode_battery(&data)),
            None => Ok(None),
        }
    }

    /// Set the DPI stages and which one is active (`selected` is 0-based).
    ///
    /// See [`proto::sensitivity_packet`] for why the encoding pads to five stages —
    /// a short packet permanently disables the mouse's vertical axis.
    pub fn set_dpi(&self, stages: &[DpiStage], selected: usize) -> Result<()> {
        let packet = proto::sensitivity_packet(stages, selected)?;
        debug!(?stages, selected, "setting DPI stages");
        self.send(&packet)
    }

    pub fn set_polling_rate(&self, rate: PollingRate) -> Result<()> {
        debug!(hz = rate.hz(), "setting polling rate");
        self.send(&proto::polling_rate_packet(rate))
    }

    pub fn set_buttons(&self, actions: &[ButtonAction; BUTTON_COUNT]) -> Result<()> {
        debug!(?actions, "setting button mapping");
        self.send(&proto::buttons_packet(actions))
    }

    /// Set what the wheel does when turned up and down. Only the actions a capture shows
    /// are taken ([`proto::WHEEL_ACTIONS`]); nothing reads the wheel back.
    pub fn set_wheel(&self, up: ButtonAction, down: ButtonAction) -> Result<()> {
        let packet = proto::wheel_packet(up, down)?;
        debug!(?up, ?down, "setting the wheel");
        self.send(&packet)
    }

    /// Read a setting back after a change until it shows `want`: twice at most, the second
    /// time [`READ_AGAIN`] later, since the first answer after a change can still be the old
    /// value. The read itself waits for the pace after the change, as every command does.
    pub fn read_back<T: PartialEq>(
        &self,
        want: &T,
        read: impl Fn(&Mouse) -> Result<Option<T>>,
    ) -> Result<ReadBack<T>> {
        let mut found = ReadBack::Silent;
        for n in 0..2 {
            if n > 0 {
                self.device.wait(READ_AGAIN);
            }
            found = match read(self)? {
                Some(value) if value == *want => return Ok(ReadBack::Took),
                Some(value) => ReadBack::Other(value),
                None => ReadBack::Silent,
            };
        }
        Ok(found)
    }

    /// Ask `query` and decode the answer; `None` for no answer.
    fn ask<T>(&self, query: &[u8], decode: impl FnOnce(&[u8]) -> Option<T>) -> Result<Option<T>> {
        self.drain();
        self.send(query)?;
        Ok(self.recv()?.and_then(|data| decode(&data)))
    }

    /// Read the DPI stages and which one is active (from 0). `None` means no answer:
    /// a mouse that answers always has at least one stage.
    pub fn dpi(&self) -> Result<Option<(Vec<DpiStage>, usize)>> {
        self.ask(&proto::read_sensitivity_packet(), proto::decode_sensitivity)
    }

    /// Read the polling rate. All zeros reads as 1000 Hz, so only trust it next to a
    /// [`Mouse::dpi`] that answered.
    pub fn polling_rate(&self) -> Result<Option<PollingRate>> {
        self.ask(
            &proto::read_polling_rate_packet(),
            proto::decode_polling_rate,
        )
    }

    /// Read the radio mode. All zeros reads as High-Efficiency Mode, so only trust it
    /// next to a [`Mouse::dpi`] that answered.
    pub fn radio(&self) -> Result<Option<Radio>> {
        self.ask(&proto::read_radio_packet(), proto::decode_radio)
    }

    /// Read the polling rate, believed only if the DPI stages answer first: all zeros (no
    /// answer) would read as 1000 Hz. `None` for no answer.
    pub fn checked_polling_rate(&self) -> Result<Option<PollingRate>> {
        if self.dpi()?.is_none() {
            return Ok(None);
        }
        self.polling_rate()
    }

    /// Read the radio mode, believed only if the DPI stages answer first: all zeros (no
    /// answer) would read as High-Efficiency Mode. `None` for no answer.
    pub fn checked_radio(&self) -> Result<Option<Radio>> {
        if self.dpi()?.is_none() {
            return Ok(None);
        }
        self.radio()
    }

    /// Set the radio mode, as GG does on each switch: at once, without a save.
    pub fn set_radio(&self, radio: Radio) -> Result<()> {
        debug!(?radio, "setting the radio mode");
        self.send(&proto::radio_packet(radio))
    }

    /// Read the sleep timer, in seconds. `None` means asleep or no answer.
    pub fn sleep_timer(&self) -> Result<Option<u16>> {
        self.drain();
        self.send(&proto::read_sleep_timer_packet())?;
        Ok(self
            .recv()?
            .and_then(|data| proto::decode_sleep_timer(&data)))
    }

    /// Set the sleep timer and save, as GG does. The mouse takes the timer at once (seen on
    /// the owner's mouse), but like every setting it would lose it when it next sleeps,
    /// which a sleep timer mustn't; so it's saved, and the save also commits the other
    /// settings the mouse is running to its onboard memory.
    pub fn set_sleep_timer(&self, minutes: u8) -> Result<()> {
        let packet = proto::sleep_timer_packet(minutes)?;
        debug!(minutes, "setting the sleep timer");
        self.send(&packet)?;
        self.save()
    }

    /// Commit the current settings to the mouse's onboard memory so they survive
    /// a power cycle. Settings applied without this are lost when the mouse sleeps.
    pub fn save(&self) -> Result<()> {
        debug!("saving settings to onboard memory");
        self.send(&proto::save_packet())
    }
}

/// Whether a Rival 3 Wireless Gen 2 control interface is currently present.
pub fn is_connected(api: &HidApi) -> bool {
    api.device_list().any(|d| {
        d.vendor_id() == VENDOR_ID
            && d.product_id() == PRODUCT_ID
            && d.interface_number() == CONTROL_INTERFACE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A setting and its save 0 ms apart lost the setting on the owner's mouse; 5 ms apart
    /// kept it. Commands keep GG's distance.
    #[test]
    fn commands_keep_their_distance() {
        let now = Instant::now();
        let ago = |ms| now.checked_sub(Duration::from_millis(ms)).unwrap();
        let dpi = proto::CMD_SENSITIVITY;
        assert_eq!(pace(None, now), Duration::ZERO, "the first goes at once");
        assert_eq!(pace(Some((now, dpi)), now), COMMAND_PACE);
        assert_eq!(pace(Some((ago(10), dpi)), now), Duration::from_millis(25));
        assert_eq!(pace(Some((ago(1000), dpi)), now), Duration::ZERO);
        assert!(COMMAND_PACE >= Duration::from_millis(30), "GG's own pacing");
        // After the polling rate or the radio mode the link is given three seconds; a save
        // needs no more than any other command (a timer 40 ms after one was kept).
        for settling in [proto::CMD_POLLING_RATE, proto::CMD_RADIO] {
            assert_eq!(pace(Some((now, settling)), now), LINK_PACE);
            assert_eq!(
                pace(Some((ago(1000), settling)), now),
                LINK_PACE - Duration::from_secs(1)
            );
            assert_eq!(pace(Some((ago(3000), settling)), now), Duration::ZERO);
        }
        assert_eq!(pace(Some((now, proto::CMD_SAVE)), now), COMMAND_PACE);
    }
}
