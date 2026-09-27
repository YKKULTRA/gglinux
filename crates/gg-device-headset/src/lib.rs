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

use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use hidapi::{HidApi, HidDevice};
use tracing::debug;

pub mod proto;

pub use proto::{AutoOff, PowerStatus, Status, WirelessMode, EQ_BANDS, EQ_CUSTOM_PRESET};

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

/// How many times to re-send a request that drew no reply.
///
/// Nothing acknowledges a write, so a request the base station never acted on is
/// indistinguishable from one whose reply we missed — and reading for longer
/// cannot recover the first case, only re-asking can. Observed once in ~44 reads.
const REQUEST_ATTEMPTS: usize = 3;

/// An open handle to the base station's control interface.
pub struct Headset {
    device: HidDevice,
}

/// One polling round: see [`Headset::poll`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Poll {
    /// `None` if the base station did not answer in time.
    pub status: Option<Status>,
    /// Master volume as attenuation, 0 loudest; `None` if unanswered.
    pub volume: Option<u8>,
    /// Unsolicited `07 ..` reports read along the way, oldest first.
    pub events: Vec<Vec<u8>>,
}

impl Headset {
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
        Ok(Self { device })
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
        self.device
            .write(&buf)
            .with_context(|| format!("writing command 0x{:02X}", payload[0]))?;
        Ok(())
    }

    /// Send a frame payload as a feature report. Frames are 1024 bytes, far past
    /// the 64-byte output reports [`Headset::send`] writes.
    fn send_feature(&self, payload: &[u8]) -> Result<()> {
        let mut buf = Vec::with_capacity(payload.len() + 1);
        buf.push(REPORT_TX);
        buf.extend_from_slice(payload);
        self.device
            .send_feature_report(&buf)
            .with_context(|| format!("writing frame command 0x{:02X}", payload[0]))
    }

    /// Read one input report, waiting at most `timeout_ms`. `None` on timeout.
    pub fn read_event_timeout(&self, timeout_ms: i32) -> Result<Option<Vec<u8>>> {
        let mut buf = [0u8; READ_SIZE];
        let n = self
            .device
            .read_timeout(&mut buf, timeout_ms)
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

    /// Status and volume in one round, plus every unsolicited report that arrived
    /// meanwhile — which [`Headset::status`] throws away, and a caller watching the
    /// device cannot afford to lose.
    pub fn poll(&self) -> Result<Poll> {
        let mut events = Vec::new();
        let status = self
            .request(&proto::status_packet(), &mut events)?
            .and_then(|r| Status::decode(&r));
        let volume = self
            .request(&proto::audio_settings_packet(), &mut events)?
            .and_then(|r| proto::decode_volume(&r));
        Ok(Poll { status, volume, events })
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
                let Some(report) = self.read_event_timeout(READ_TIMEOUT_MS)? else { continue };
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

    pub fn set_analog_gain_high(&self, high: bool) -> Result<()> {
        self.send(&proto::analog_gain_packet(high))
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
        self.send(&proto::transparency_packet(level))
    }

    pub fn set_anc(&self, value: u8) -> Result<()> {
        self.send(&proto::anc_packet(value))
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
