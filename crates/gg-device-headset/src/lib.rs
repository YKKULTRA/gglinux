// SPDX-License-Identifier: GPL-3.0-or-later
//! Control for the SteelSeries **Arctis Nova Pro Wireless** (`1038:12e0`).
//!
//! Talks to the base station's vendor HID interface. Unlike the Rival 3, this
//! protocol has a readback (`06 b0`), so applied settings can be confirmed rather
//! than assumed — see [`Headset::status`].
//!
//! ```no_run
//! # fn main() -> anyhow::Result<()> {
//! let api = hidapi::HidApi::new()?;
//! let headset = gg_device_headset::Headset::open(&api)?;
//! println!("{:?}", headset.status()?);
//! # Ok(()) }
//! ```
//!
//! Reports go through a [`Port`]: the base station's hidraw node, or in tests
//! [`testing::FakeBase`], a base station that isn't there.

use std::cell::Cell;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use hidapi::{HidApi, HidDevice};
use tracing::debug;

pub mod base;
pub mod proto;
pub mod testing;

pub use base::{
    AudioSettings, BluetoothCall, BluetoothLink, DimTimeout, Gain, HomeScreen, LineOut, Pairing,
    StreamMix, UxSettings,
};
pub use proto::{
    AutoOff, NoiseMode, PowerStatus, Setting, Status, WirelessMode, EQ_BANDS, EQ_CUSTOM_PRESET,
};

pub const VENDOR_ID: u16 = 0x1038;
pub const PRODUCT_ID: u16 = 0x12e0;

/// The base station exposes several HID interfaces; interface 4 carries the
/// vendor protocol (OUT `0x04` / IN `0x84`).
pub const CONTROL_INTERFACE: i32 = 4;

/// Vendor usage page on the control interface, used to disambiguate: interface 4
/// is reported twice by hidapi, once per usage collection.
const VENDOR_USAGE_PAGE: u16 = 0xffc0;

/// Report id for host-to-device commands.
const REPORT_TX: u8 = 0x06;

/// Total bytes written per command, including the report id.
const REPORT_SIZE: usize = 64;

/// Bytes in a received input report.
const READ_SIZE: usize = 64;

/// How long to wait for a reply. The base station answers promptly even when the
/// headset itself is off, since the base is what we are talking to.
const READ_TIMEOUT_MS: i32 = 300;

/// Upper bound on queued reports sorted before each request.
const MAX_DRAIN_REPORTS: usize = 16;

/// How long to keep reading while hunting for a command's reply.
///
/// Unsolicited reports — a turning volume wheel sends one every ~16 ms — can land
/// ahead of the reply, so it is not reliably the first report to arrive. This has
/// to be a deadline rather than a count of reports: a fixed budget is spent by
/// whatever the stream happens to be emitting, which makes the read succeed or
/// fail depending on timing.
const REPLY_DEADLINE: Duration = Duration::from_millis(400);

/// Backstop against a device streaming faster than we can drain.
const MAX_REPLY_REPORTS: usize = 256;

/// Least time between two commands. GG keeps this much between commands for this
/// base station (Arctis Sound Manager, `time_between_commands_ms: 50`), and its
/// commands in our captures go out about 64 ms apart. Real devices drop a command
/// that comes too close after another.
pub const COMMAND_GAP: Duration = Duration::from_millis(50);

/// How many times to re-send a request that drew no reply.
///
/// Nothing acknowledges a write, so a request the base station never acted on is
/// indistinguishable from one whose reply we missed — and reading for longer
/// cannot recover the first case, only re-asking can. Observed once in ~44 reads.
const REQUEST_ATTEMPTS: usize = 3;

/// Where a [`Headset`]'s reports go.
pub trait Port {
    /// Send one output report, report id first.
    fn write(&self, report: &[u8]) -> Result<()>;
    /// Send one feature report, report id first.
    fn send_feature(&self, report: &[u8]) -> Result<()>;
    /// Read one input report into `buf`, waiting at most `timeout_ms`. 0 on timeout.
    fn read(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize>;
}

impl Port for HidDevice {
    fn write(&self, report: &[u8]) -> Result<()> {
        HidDevice::write(self, report)?;
        Ok(())
    }

    fn send_feature(&self, report: &[u8]) -> Result<()> {
        Ok(self.send_feature_report(report)?)
    }

    fn read(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize> {
        Ok(self.read_timeout(buf, timeout_ms)?)
    }
}

/// An open handle to the base station's control interface.
pub struct Headset<P: Port = HidDevice> {
    port: P,
    /// When the last command went out, for [`COMMAND_GAP`].
    last_command: Cell<Option<Instant>>,
}

/// One polling round: see [`Headset::poll`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Poll {
    /// `None` if the base station did not answer in time.
    pub status: Option<Status>,
    /// Master volume as attenuation, 0 loudest; `None` if unanswered.
    pub volume: Option<u8>,
    /// The whole `06 20` reply, decoded; `None` if unanswered.
    pub audio: Option<AudioSettings>,
    /// Unsolicited `07 ..` reports read along the way, oldest first.
    pub events: Vec<Vec<u8>>,
}

/// The screen settings reply: see [`Headset::ux_settings`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UxReply {
    /// The reply as it came, report id first; `None` if unanswered.
    pub raw: Option<Vec<u8>>,
    pub decoded: Option<UxSettings>,
    /// Unsolicited `07 ..` reports read along the way, oldest first.
    pub events: Vec<Vec<u8>>,
}

impl Headset<HidDevice> {
    /// Open the control interface of the first connected Nova Pro Wireless base.
    pub fn open(api: &HidApi) -> Result<Self> {
        let path = api
            .device_list()
            .find(|d| {
                d.vendor_id() == VENDOR_ID
                    && d.product_id() == PRODUCT_ID
                    && d.interface_number() == CONTROL_INTERFACE
                    && d.usage_page() == VENDOR_USAGE_PAGE
            })
            .map(|d| d.path().to_owned())
            .ok_or_else(|| {
                anyhow!(
                    "Arctis Nova Pro Wireless control interface (iface {CONTROL_INTERFACE}, \
                     usage page 0x{VENDOR_USAGE_PAGE:04x}) not found — is the base station \
                     plugged in over USB?"
                )
            })?;

        let device = api.open_path(&path).with_context(|| {
            format!(
                "opening {} — if this is a permission error, install the udev rule \
                 (scripts/install-local.sh) so your session owns SteelSeries hidraw nodes",
                path.to_string_lossy()
            )
        })?;
        debug!(path = %path.to_string_lossy(), "opened headset control interface");
        Ok(Self::with_port(device))
    }
}

impl<P: Port> Headset<P> {
    /// A headset whose reports go through `port`.
    pub fn with_port(port: P) -> Self {
        Headset {
            port,
            last_command: Cell::new(None),
        }
    }

    /// Send one vendor command.
    ///
    /// Byte 0 of a hidraw write is always the report id. This device declares
    /// **numbered** reports, so unlike the mouse the `0x06` is kept by the kernel
    /// *and* transmitted — it is the first wire byte, not a placeholder the kernel
    /// strips. Callers pass the opcode onward from byte 1.
    fn send(&self, payload: &[u8]) -> Result<()> {
        anyhow::ensure!(
            payload.len() < REPORT_SIZE,
            "command of {} bytes does not fit a {REPORT_SIZE}-byte report with its id",
            payload.len()
        );
        let mut buf = [0u8; REPORT_SIZE];
        buf[0] = REPORT_TX;
        buf[1..=payload.len()].copy_from_slice(payload);
        if let Some(wait) = self
            .last_command
            .get()
            .and_then(|last| COMMAND_GAP.checked_sub(last.elapsed()))
        {
            std::thread::sleep(wait);
        }
        let sent = self
            .port
            .write(&buf)
            .with_context(|| format!("writing command 0x{:02X}", payload[0]));
        self.last_command.set(Some(Instant::now()));
        sent
    }

    /// Send a frame payload as a feature report. Frames are 1024 bytes, far past
    /// the 64-byte output reports [`Headset::send`] writes.
    fn send_feature(&self, payload: &[u8]) -> Result<()> {
        let mut buf = Vec::with_capacity(payload.len() + 1);
        buf.push(REPORT_TX);
        buf.extend_from_slice(payload);
        self.port
            .send_feature(&buf)
            .with_context(|| format!("writing frame command 0x{:02X}", payload[0]))
    }

    /// Read one input report, waiting at most `timeout_ms`. `None` on timeout.
    pub fn read_event_timeout(&self, timeout_ms: i32) -> Result<Option<Vec<u8>>> {
        let mut buf = [0u8; READ_SIZE];
        let n = self
            .port
            .read(&mut buf, timeout_ms)
            .context("reading from the base station")?;
        Ok((n > 0).then(|| buf[..n].to_vec()))
    }

    /// Read one input report without sending anything, for the event stream
    /// (volume, ANC changes made on the device itself).
    pub fn read_event(&self) -> Result<Option<Vec<u8>>> {
        self.read_event_timeout(READ_TIMEOUT_MS)
    }

    /// Query the base station: battery for both packs, power state, and the
    /// settings it reports back. `None` if it never answered.
    pub fn status(&self) -> Result<Option<Status>> {
        let reply = self.request(&proto::status_packet(), &mut Vec::new())?;
        Ok(reply.and_then(|r| Status::decode(&r)))
    }

    /// Status and audio settings in one round, plus every unsolicited report that
    /// arrived meanwhile — which [`Headset::status`] throws away, and a caller
    /// watching the device cannot afford to lose.
    pub fn poll(&self) -> Result<Poll> {
        let mut events = Vec::new();
        let status = self
            .request(&proto::status_packet(), &mut events)?
            .and_then(|r| Status::decode(&r));
        let audio = self
            .request(&proto::audio_settings_packet(), &mut events)?
            .and_then(|r| AudioSettings::decode(&r));
        Ok(Poll {
            status,
            volume: audio.as_ref().and_then(|a| a.volume),
            audio,
            events,
        })
    }

    /// Ask for the screen settings (`06 80`). A query: it sets nothing.
    pub fn ux_settings(&self) -> Result<UxReply> {
        let mut events = Vec::new();
        let raw = self.request(&base::ux_settings_packet(), &mut events)?;
        Ok(UxReply {
            decoded: raw.as_deref().and_then(UxSettings::decode),
            raw,
            events,
        })
    }

    /// Send a request and return its reply, filing events that arrive around it.
    ///
    /// The protocol carries no correlation id, so a reply is recognised by the
    /// opcode it echoes. Reports already queued predate the request and are sorted
    /// first, so a leftover reply to an earlier exchange is never taken for this
    /// one. Nothing acknowledges a write, so a request that drew no reply is sent
    /// again: reading longer cannot recover one the base station never acted on.
    fn request(&self, payload: &[u8], events: &mut Vec<Vec<u8>>) -> Result<Option<Vec<u8>>> {
        let opcode = payload[0];
        for attempt in 1..=REQUEST_ATTEMPTS {
            for _ in 0..MAX_DRAIN_REPORTS {
                match self.read_event_timeout(0)? {
                    Some(report) => {
                        proto::sort_report(report, None, events);
                    }
                    None => break,
                }
            }
            self.send(payload)?;

            let deadline = Instant::now() + REPLY_DEADLINE;
            let mut seen = 0usize;
            while Instant::now() < deadline && seen < MAX_REPLY_REPORTS {
                let Some(report) = self.read_event_timeout(READ_TIMEOUT_MS)? else {
                    continue;
                };
                seen += 1;
                if let Some(reply) = proto::sort_report(report, Some(opcode), events) {
                    if attempt > 1 {
                        debug!(attempt, opcode, "request needed a retry");
                    }
                    return Ok(Some(reply));
                }
            }
        }
        debug!(attempts = REQUEST_ATTEMPTS, opcode, "no reply");
        Ok(None)
    }

    /// Send the vendor tool's init sequence.
    ///
    /// loteran sends this before applying settings. It is reproduced verbatim
    /// because none of the sources explain what the individual commands do.
    pub fn init(&self) -> Result<()> {
        for command in proto::init_sequence() {
            self.send(&command)?;
        }
        Ok(())
    }

    /// Commit current settings to the base station's flash.
    pub fn save(&self) -> Result<()> {
        self.send(&proto::save_packet())
    }

    pub fn set_sidetone(&self, level: u8) -> Result<()> {
        self.send(&proto::sidetone_packet(level)?)
    }

    pub fn set_eq_preset(&self, preset: u8) -> Result<()> {
        self.send(&proto::eq_preset_packet(preset)?)
    }

    /// Write the ten custom band gains **and** select the custom slot, since
    /// writing bands alone leaves the change inaudible.
    pub fn set_eq_bands(&self, gains_db: &[f32]) -> Result<()> {
        let bands = proto::eq_bands_packet(gains_db)?;
        self.send(&proto::eq_preset_packet(EQ_CUSTOM_PRESET)?)?;
        self.send(&bands)
    }

    pub fn set_mic_volume(&self, volume: u8) -> Result<()> {
        self.send(&proto::mic_volume_packet(volume)?)
    }

    pub fn set_mic_led_brightness(&self, brightness: u8) -> Result<()> {
        self.send(&proto::mic_led_packet(brightness)?)
    }

    pub fn set_auto_off(&self, timer: AutoOff) -> Result<()> {
        self.send(&proto::auto_off_packet(timer))
    }

    pub fn set_wireless_mode(&self, mode: WirelessMode) -> Result<()> {
        self.send(&proto::wireless_mode_packet(mode))
    }

    /// Enable or disable the ChatMix dial's interrupt reports.
    pub fn set_chatmix(&self, enabled: bool) -> Result<()> {
        self.send(&proto::chatmix_packet(enabled))
    }

    pub fn set_transparency(&self, level: u8) -> Result<()> {
        self.send(&proto::transparency_packet(level)?)
    }

    pub fn set_noise_mode(&self, mode: NoiseMode) -> Result<()> {
        self.send(&proto::noise_mode_packet(mode))
    }

    /// Apply one setting. The first milestones' settings are then committed to
    /// flash, as HeadsetControl does after every change; the base-station settings
    /// GG 123 has go out alone, as GG sends them ([`Setting::saved`]).
    pub fn apply(&self, setting: &Setting) -> Result<()> {
        for packet in setting.packets()? {
            self.send(&packet)?;
        }
        if setting.saved() {
            self.save()?;
        }
        Ok(())
    }

    /// Master volume as attenuation: 0 is loudest, 56 silent.
    pub fn set_volume(&self, attenuation: u8) -> Result<()> {
        self.send(&proto::volume_packet(attenuation)?)
    }

    /// Blank the screen with an all-black frame. Brightness cannot do this — its
    /// floor is still lit — and an unlit OLED pixel does not wear.
    pub fn oled_blank(&self) -> Result<()> {
        for strip in proto::oled_blank_packets() {
            self.send_feature(&strip)?;
        }
        Ok(())
    }

    /// Hand the screen back to the firmware's own display.
    pub fn oled_show_stock(&self) -> Result<()> {
        self.send(&proto::oled_return_to_ui_packet())
    }

    /// OLED brightness, 1..=10. Not readable back, so callers track what they set.
    pub fn set_oled_brightness(&self, level: u8) -> Result<()> {
        self.send(&proto::oled_brightness_packet(level)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testing::FakeBase;

    fn headset() -> (Headset<FakeBase>, FakeBase) {
        let fake = FakeBase::new();
        (Headset::with_port(fake.clone()), fake)
    }

    #[test]
    fn a_base_station_setting_goes_out_alone_and_an_old_one_is_saved() {
        let (h, fake) = headset();
        h.apply(&Setting::Gain(Gain::High)).unwrap();
        h.apply(&Setting::Sidetone(2)).unwrap();
        let sent = fake.commands();
        assert_eq!(
            sent,
            vec![
                vec![0x06, 0x27, 0x02],
                vec![0x06, 0x39, 0x02],
                vec![0x06, 0x09]
            ]
        );
        assert!(
            fake.reports().iter().all(|r| r.len() == 64),
            "64-byte reports"
        );
    }

    #[test]
    fn commands_are_kept_apart_as_gg_keeps_them() {
        let (h, fake) = headset();
        h.apply(&Setting::EqBands(vec![0.0; EQ_BANDS])).unwrap();
        let gaps = fake.gaps();
        assert_eq!(gaps.len(), 2, "preset, bands, save");
        assert!(gaps.iter().all(|g| *g >= COMMAND_GAP), "{gaps:?}");
    }

    #[test]
    fn a_poll_reads_status_and_the_whole_audio_reply() {
        let (h, fake) = headset();
        let p = h.poll().unwrap();
        let status = p.status.unwrap();
        assert_eq!(status.pairing, Some(Pairing::PairedOffline));
        let audio = p.audio.unwrap();
        assert_eq!(p.volume, audio.volume);
        assert_eq!(audio.gain, Some(Gain::Low));
        assert_eq!(audio.line_out, Some(LineOut::Speakers));
        assert_eq!(
            fake.commands(),
            vec![vec![0x06, 0xb0], vec![0x06, 0x20]],
            "queries only: a poll sets nothing"
        );
    }

    /// The fake answers the first read after a change with the old state, as the
    /// base station may: only a later read shows it.
    #[test]
    fn a_change_shows_from_the_second_read_after_it() {
        let (h, _fake) = headset();
        h.apply(&Setting::LineOut(LineOut::Stream)).unwrap();
        let first = h.poll().unwrap().audio.unwrap();
        assert_eq!(first.line_out, Some(LineOut::Speakers), "stale");
        let second = h.poll().unwrap().audio.unwrap();
        assert_eq!(second.line_out, Some(LineOut::Stream));
        h.apply(&Setting::BluetoothCall(BluetoothCall::MuteGame))
            .unwrap();
        h.poll().unwrap();
        let s = h.poll().unwrap().status.unwrap();
        assert_eq!(s.bluetooth_call, Some(BluetoothCall::MuteGame));
    }

    #[test]
    fn the_screen_settings_query_is_one_80_and_comes_back_raw_and_decoded() {
        let (h, fake) = headset();
        h.apply(&Setting::ScreenDim(DimTimeout::Min5)).unwrap();
        h.ux_settings().unwrap();
        let ux = h.ux_settings().unwrap();
        assert_eq!(ux.raw.as_deref().map(|r| &r[..2]), Some(&[0x06, 0x80][..]));
        assert_eq!(ux.decoded.unwrap().dim_timeout, Some(DimTimeout::Min5));
        assert_eq!(
            fake.commands(),
            vec![vec![0x06, 0x83, 0x02], vec![0x06, 0x80], vec![0x06, 0x80]]
        );
    }
}
