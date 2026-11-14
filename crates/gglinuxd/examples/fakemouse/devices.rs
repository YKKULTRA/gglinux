// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake's devices, and how they look (`Looks`): the Engine page's states.

use anyhow::{bail, Result};
use gg_ipc::{
    DeviceInfo, DeviceKind, HeadsetControls, HeadsetPower, HeadsetState, InterfaceInfo,
    ScreenSettings,
};

/// The three real devices, with the interface layout this machine reports, so the
/// UI's device cards look like the ones it will see in production.
pub fn devices(with_mouse: bool) -> Vec<DeviceInfo> {
    let iface = |n: i32, page: u16, usage: u16, path: &str| InterfaceInfo {
        interface_number: n,
        usage_page: page,
        usage,
        path: path.to_string(),
    };
    let mut out = vec![
        DeviceInfo {
            id: "1038:12e0".into(),
            kind: DeviceKind::Headset,
            vendor_id: 0x1038,
            product_id: 0x12e0,
            name: "Arctis Nova Pro Wireless".into(),
            serial: None,
            connected: true,
            interfaces: vec![
                iface(3, 12, 1, "/dev/hidraw13"),
                iface(4, 0xffc0, 1, "/dev/hidraw14"),
            ],
        },
        DeviceInfo {
            id: "1038:1642".into(),
            kind: DeviceKind::Keyboard,
            vendor_id: 0x1038,
            product_id: 0x1642,
            name: "Apex Pro TKL Gen 3".into(),
            serial: None,
            connected: true,
            interfaces: vec![
                iface(0, 1, 6, "/dev/hidraw4"),
                iface(1, 0xffc0, 1, "/dev/hidraw5"),
                iface(4, 0xffc1, 1, "/dev/hidraw8"),
            ],
        },
    ];
    if with_mouse {
        out.push(DeviceInfo {
            id: "1038:1872".into(),
            kind: DeviceKind::Mouse,
            vendor_id: 0x1038,
            product_id: 0x1872,
            name: "Rival 3 Wireless Gen 2".into(),
            serial: None,
            connected: true,
            interfaces: vec![
                iface(0, 1, 2, "/dev/hidraw0"),
                iface(2, 0xff00, 1, "/dev/hidraw2"),
                iface(3, 0xffc0, 1, "/dev/hidraw3"),
            ],
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// How the fake's devices look, beyond the defaults, so the Engine page's states
/// can be drawn: devices missing, a sleeping mouse, a headset off or a base
/// station not open. Flags in the module docs; none of them changes a default.
#[derive(Debug, Clone, Copy)]
pub struct Looks {
    headset: bool,
    keyboard: bool,
    power: HeadsetPower,
    spare: bool,
    base_closed: bool,
    /// The readings a real base station sends every second, not left unknown.
    reported: bool,
}

impl Looks {
    pub fn from_args(args: &[String]) -> Result<Looks> {
        let flag = |name: &str| args.iter().any(|a| a == name);
        let power = match args
            .iter()
            .position(|a| a == "--headset")
            .map(|i| args.get(i + 1).map(String::as_str))
        {
            None => HeadsetPower::Online,
            Some(Some("online" | "on")) => HeadsetPower::Online,
            Some(Some("charging")) => HeadsetPower::Charging,
            Some(Some("offline" | "off")) => HeadsetPower::Offline,
            Some(Some("unknown")) => HeadsetPower::Unknown,
            Some(other) => bail!("--headset {other:?}: online, charging, off or unknown"),
        };
        Ok(Looks {
            headset: !flag("--no-headset"),
            keyboard: !flag("--no-keyboard"),
            power,
            spare: !flag("--no-spare"),
            base_closed: flag("--base-closed"),
            reported: flag("--reported"),
        })
    }

    pub fn listed(&self, devices: Vec<DeviceInfo>) -> Vec<DeviceInfo> {
        devices
            .into_iter()
            .filter(|d| match d.kind {
                DeviceKind::Headset => self.headset,
                DeviceKind::Keyboard => self.keyboard,
                _ => true,
            })
            .collect()
    }

    /// The headset's settings at start.
    pub fn controls(&self) -> HeadsetControls {
        if !self.reported {
            return HeadsetControls::default();
        }
        HeadsetControls {
            noise_mode: Some(gg_ipc::NoiseMode::Anc),
            transparency: Some(5),
            mic_led: Some(7),
            mic_muted: Some(false),
            auto_off_minutes: Some(30),
            radio_mode: Some(gg_ipc::RadioMode::Speed),
            ..HeadsetControls::default()
        }
    }

    /// As the daemon builds it: no readings at all from a base station that
    /// isn't open, and no headset battery while it's off.
    pub fn headset_state(
        &self,
        listed: bool,
        screen: ScreenSettings,
        controls: &HeadsetControls,
    ) -> HeadsetState {
        let connected = listed && !self.base_closed;
        let power = if connected {
            self.power
        } else {
            HeadsetPower::Unknown
        };
        let controls = if connected {
            controls.clone()
        } else {
            HeadsetControls {
                sidetone: controls.sidetone,
                mic_volume: controls.mic_volume,
                eq_preset: controls.eq_preset,
                eq_bands: controls.eq_bands.clone(),
                ..HeadsetControls::default()
            }
        };
        HeadsetState {
            connected,
            power,
            headset_battery: matches!(power, HeadsetPower::Online | HeadsetPower::Charging)
                .then_some(75),
            spare_battery: (connected && self.spare).then_some(100),
            screen,
            screen_dark: connected && self.reported && screen.protect,
            controls,
        }
    }
}
