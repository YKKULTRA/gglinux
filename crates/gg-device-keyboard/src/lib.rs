//! Control for the SteelSeries **Apex Pro TKL Gen 3** (`1038:1642`) lighting.
//!
//! Everything sent is framed by [`proto`], which only frames opcodes on its
//! allowlist, and checked against it once more at the write.

use std::ffi::CString;

use anyhow::{anyhow, Context, Result};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use tracing::debug;

pub mod keys;
pub mod layout;
pub mod proto;

pub use proto::Rgb;

pub const VENDOR_ID: u16 = 0x1038;
pub const PRODUCT_ID: u16 = 0x1642;

/// The vendor interface OpenRGB drives, and the usage page that identifies it.
const CONTROL_INTERFACE: i32 = 1;
const VENDOR_USAGE_PAGE: u16 = 0xffc0;

const READ_TIMEOUT_MS: i32 = 200;

pub struct Keyboard {
    device: HidDevice,
    /// The lighting interface's node, to tell whether it's still there.
    path: CString,
}

/// Whether `d` is an Apex Pro TKL Gen 3's lighting interface.
fn is_lighting(d: &DeviceInfo) -> bool {
    d.vendor_id() == VENDOR_ID
        && d.product_id() == PRODUCT_ID
        && d.interface_number() == CONTROL_INTERFACE
        && d.usage_page() == VENDOR_USAGE_PAGE
}

impl Keyboard {
    /// Open the lighting interface of the first connected Apex Pro TKL Gen 3.
    pub fn open(api: &HidApi) -> Result<Self> {
        let path = api
            .device_list()
            .find(|d| is_lighting(d))
            .map(|d| d.path().to_owned())
            .ok_or_else(|| anyhow!("Apex Pro TKL Gen 3 lighting interface not found — is it plugged in?"))?;
        let device = api
            .open_path(&path)
            .with_context(|| format!("opening {}", path.to_string_lossy()))?;
        debug!(path = %path.to_string_lossy(), "opened keyboard lighting interface");
        Ok(Self { device, path })
    }

    /// Whether `api`'s device list, as last refreshed, still has this
    /// keyboard's lighting interface.
    pub fn is_listed(&self, api: &HidApi) -> bool {
        api.device_list().any(|d| is_lighting(d) && d.path() == self.path.as_c_str())
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
        self.output(&proto::command(proto::OP_REGION, &[])?)?;
        let mut buf = [0u8; 64];
        let n = self
            .device
            .read_timeout(&mut buf, READ_TIMEOUT_MS)
            .context("reading the region reply")?;
        Ok(proto::decode_region(&buf[..n]))
    }

    /// Send a feature report. The allowlist is checked again here, at the one
    /// place bytes reach the wire.
    fn feature(&self, report: &[u8]) -> Result<()> {
        proto::check(report[1])?;
        self.device
            .send_feature_report(report)
            .with_context(|| format!("sending keyboard command {:#04x}", report[1]))
    }

    fn output(&self, report: &[u8]) -> Result<()> {
        proto::check(report[1])?;
        self.device
            .write(report)
            .map(|_| ())
            .with_context(|| format!("sending keyboard command {:#04x}", report[1]))
    }
}
