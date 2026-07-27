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

use anyhow::{anyhow, Context, Result};
use hidapi::{HidApi, HidDevice};
use tracing::debug;

pub mod proto;
pub mod tables;

pub use proto::{
    Battery, ButtonAction, DpiStage, PollingRate, BUTTON_COUNT, MAX_PRESETS,
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

/// An open handle to the mouse's control interface.
pub struct Mouse {
    device: HidDevice,
}

impl Mouse {
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
        Ok(Self { device })
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
        self.device
            .write(&buf)
            .with_context(|| format!("writing command 0x{:02X}", payload[0]))?;
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
