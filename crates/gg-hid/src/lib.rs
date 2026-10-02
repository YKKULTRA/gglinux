//! HID enumeration + classification + hotplug monitoring for SteelSeries devices.
//!
//! [`scan`] returns the currently-connected logical devices. [`run_monitor`] blocks
//! forever on a dedicated thread, calling back with a fresh snapshot (and a diff of
//! add/remove [`Event`]s) whenever the device set changes — woken promptly by a udev
//! monitor, with a periodic rescan as a safety net.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::io::AsRawFd;
use std::sync::{Mutex, MutexGuard};

use anyhow::Result;
use gg_ipc::{DeviceInfo, DeviceKind, Event, InterfaceInfo};
use hidapi::HidApi;
use tracing::{debug, warn};

/// SteelSeries USB vendor id — the only vendor gglinux manages.
pub const STEELSERIES_VID: u16 = 0x1038;

/// Safety-net rescan interval when udev is delivering hotplug events for us. Long,
/// because udev wakes us within milliseconds of a real change; this only covers the
/// case where an event is somehow missed.
const RESCAN_INTERVAL_MS: i32 = 10_000;

/// Rescan interval when no udev monitor could be opened — polling is then the only
/// way we notice a device appearing, so it has to be frequent enough to feel live.
const POLL_ONLY_INTERVAL_MS: i32 = 1_500;

/// Hold while enumerating or opening through hidapi (`HidApi::new`,
/// `refresh_devices`, opening a device), on every thread. Its C library keeps
/// one error string for the whole process and frees and replaces it on each
/// of those calls without a lock, so two at once can free it twice. Reads and
/// writes on an open device keep their own error and need no lock.
pub fn lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Map a SteelSeries product id to its kind and display name.
fn classify(pid: u16) -> (DeviceKind, &'static str) {
    match pid {
        0x1642 => (DeviceKind::Keyboard, "Apex Pro TKL Gen 3"),
        0x1872 => (DeviceKind::Mouse, "Rival 3 Wireless Gen 2"),
        0x12e0 => (DeviceKind::Headset, "Arctis Nova Pro Wireless"),
        _ => (DeviceKind::Other, "SteelSeries device"),
    }
}

/// Enumerate connected SteelSeries devices, grouping HID interfaces by logical device.
pub fn scan(api: &HidApi) -> Vec<DeviceInfo> {
    let mut by_id: BTreeMap<String, DeviceInfo> = BTreeMap::new();

    for d in api.device_list() {
        if d.vendor_id() != STEELSERIES_VID {
            continue;
        }
        let pid = d.product_id();
        let serial = d
            .serial_number()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let id = match &serial {
            Some(s) => format!("{STEELSERIES_VID:04x}:{pid:04x}:{s}"),
            None => format!("{STEELSERIES_VID:04x}:{pid:04x}"),
        };
        let (kind, name) = classify(pid);

        let dev = by_id.entry(id.clone()).or_insert_with(|| DeviceInfo {
            id,
            kind,
            vendor_id: STEELSERIES_VID,
            product_id: pid,
            name: d
                .product_string()
                .filter(|s| !s.is_empty())
                .unwrap_or(name)
                .to_string(),
            serial,
            connected: true,
            interfaces: Vec::new(),
        });
        dev.interfaces.push(InterfaceInfo {
            interface_number: d.interface_number(),
            usage_page: d.usage_page(),
            usage: d.usage(),
            path: d.path().to_string_lossy().into_owned(),
        });
    }

    let mut devices: Vec<DeviceInfo> = by_id.into_values().collect();
    // Stable ordering so equality comparisons in the monitor are deterministic.
    for dev in &mut devices {
        dev.interfaces
            .sort_by(|a, b| a.interface_number.cmp(&b.interface_number).then(a.path.cmp(&b.path)));
    }
    devices.sort_by(|a, b| a.id.cmp(&b.id));
    devices
}

/// Compute add/remove events between two snapshots (matched by `id`).
fn diff(old: &[DeviceInfo], new: &[DeviceInfo]) -> Vec<Event> {
    let old_ids: BTreeSet<&str> = old.iter().map(|d| d.id.as_str()).collect();
    let new_ids: BTreeSet<&str> = new.iter().map(|d| d.id.as_str()).collect();
    let mut events = Vec::new();
    for d in new {
        if !old_ids.contains(d.id.as_str()) {
            events.push(Event::DeviceAdded(d.clone()));
        }
    }
    for d in old {
        if !new_ids.contains(d.id.as_str()) {
            events.push(Event::DeviceRemoved { id: d.id.clone() });
        }
    }
    events
}

/// Run the hotplug monitor loop forever, invoking `on_change(snapshot, events)`.
///
/// Called once immediately with the initial snapshot (and empty events), then again
/// on every subsequent change. Intended to run on its own OS thread — `hidapi` and
/// `udev` are blocking C libraries.
pub fn run_monitor(mut on_change: impl FnMut(Vec<DeviceInfo>, Vec<Event>)) -> Result<()> {
    let hid = lock();
    let mut api = HidApi::new()?;
    drop(hid);
    let mut known = scan(&api);
    debug!(count = known.len(), "initial device scan");
    on_change(known.clone(), Vec::new());

    // Wake promptly on USB add/remove; fall back to periodic rescans via the poll timeout.
    let monitor = udev::MonitorBuilder::new()
        .and_then(|b| b.match_subsystem("usb"))
        .and_then(|b| b.listen());
    let monitor = match monitor {
        Ok(m) => Some(m),
        Err(e) => {
            warn!(error = %e, "udev monitor unavailable; using periodic rescan only");
            None
        }
    };
    let fd = monitor.as_ref().map(|m| m.as_raw_fd()).unwrap_or(-1);
    let timeout_ms = if monitor.is_some() { RESCAN_INTERVAL_MS } else { POLL_ONLY_INTERVAL_MS };

    loop {
        // Block until a udev event or the rescan interval elapses.
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let nfds = if fd >= 0 { 1 } else { 0 };
        let rc = unsafe { libc::poll(&mut pfd, nfds, timeout_ms) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            warn!(error = %err, "poll failed; falling back to sleep");
            std::thread::sleep(std::time::Duration::from_millis(timeout_ms as u64));
        }
        // Drain pending udev events so the fd doesn't stay perpetually readable.
        if let Some(m) = &monitor {
            for _event in m.iter() {}
        }

        let hid = lock();
        let refreshed = api.refresh_devices();
        drop(hid);
        if let Err(e) = refreshed {
            warn!(error = %e, "hid refresh failed");
            continue;
        }
        let current = scan(&api);
        if current != known {
            let events = diff(&known, &current);
            debug!(events = events.len(), count = current.len(), "device set changed");
            known = current.clone();
            on_change(current, events);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str) -> DeviceInfo {
        DeviceInfo {
            id: id.to_string(),
            kind: DeviceKind::Other,
            vendor_id: STEELSERIES_VID,
            product_id: 0,
            name: id.to_string(),
            serial: None,
            connected: true,
            interfaces: Vec::new(),
        }
    }

    fn added(events: &[Event]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::DeviceAdded(d) => Some(d.id.as_str()),
                _ => None,
            })
            .collect()
    }

    fn removed(events: &[Event]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::DeviceRemoved { id } => Some(id.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn unchanged_snapshots_produce_no_events() {
        let a = vec![dev("1038:1642"), dev("1038:1872")];
        assert!(diff(&a, &a).is_empty());
    }

    #[test]
    fn plugging_in_reports_only_the_new_device() {
        let before = vec![dev("1038:1642")];
        let after = vec![dev("1038:1642"), dev("1038:12e0")];
        let events = diff(&before, &after);
        assert_eq!(added(&events), ["1038:12e0"]);
        assert!(removed(&events).is_empty());
    }

    #[test]
    fn unplugging_reports_only_the_lost_device() {
        let before = vec![dev("1038:1642"), dev("1038:12e0")];
        let after = vec![dev("1038:1642")];
        let events = diff(&before, &after);
        assert_eq!(removed(&events), ["1038:12e0"]);
        assert!(added(&events).is_empty());
    }

    /// A swap between two rescans must not be collapsed into "no change".
    #[test]
    fn simultaneous_swap_reports_both_sides() {
        let before = vec![dev("1038:1872")];
        let after = vec![dev("1038:12e0")];
        let events = diff(&before, &after);
        assert_eq!(added(&events), ["1038:12e0"]);
        assert_eq!(removed(&events), ["1038:1872"]);
    }

    /// Re-enumeration must key on `id` alone — a device whose interface list changed
    /// (e.g. a wireless receiver gaining an interface when the peripheral wakes) is
    /// still the same device, not a remove+add pair.
    #[test]
    fn interface_churn_is_not_a_hotplug_event() {
        let before = vec![dev("1038:1872")];
        let mut changed = dev("1038:1872");
        changed.interfaces.push(InterfaceInfo {
            interface_number: 3,
            usage_page: 0xffc0,
            usage: 0x01,
            path: "/dev/hidraw3".into(),
        });
        assert!(diff(&before, &[changed]).is_empty());
    }

    #[test]
    fn known_products_are_classified() {
        assert_eq!(classify(0x1642).0, DeviceKind::Keyboard);
        assert_eq!(classify(0x1872).0, DeviceKind::Mouse);
        assert_eq!(classify(0x12e0).0, DeviceKind::Headset);
        assert_eq!(classify(0x0000).0, DeviceKind::Other);
    }

    #[test]
    fn a_thread_that_panics_holding_the_hid_lock_does_not_wedge_the_others() {
        let _ = std::thread::spawn(|| {
            let _hid = lock();
            panic!("a worker died mid-enumeration");
        })
        .join();
        drop(lock());
    }
}
