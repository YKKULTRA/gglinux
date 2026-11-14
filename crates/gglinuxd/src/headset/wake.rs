// SPDX-License-Identifier: GPL-3.0-or-later
//! Which status changes are worth lighting the base-station screen for.
//!
//! The worker compares consecutive `06 b0` reads with [`wake_reason`]. A `Some`
//! lights the screen for one lit period; `None` leaves it as it is.

use gg_device_headset::Status;

/// Why a status change lights the screen. Only used for logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReason {
    /// Headset turned on or off, or put on / taken off the charge cable.
    Power,
    /// Headset battery dropped a step while low.
    Battery,
    /// Spare pack inserted or removed.
    Spare,
    /// ANC, transparency, or mic mute changed.
    Controls,
}

/// Headset battery at or below this is "low": each drop lights the screen.
pub const LOW_BATTERY: u8 = 25;

/// Compare two consecutive status reads.
///
/// Battery is reported in eighths (0, 12, 25, 37, 50, 62, 75, 87, 100 %), and is
/// `None` while the headset is off.
pub fn wake_reason(prev: &Status, next: &Status) -> Option<WakeReason> {
    if prev.power != next.power {
        return Some(WakeReason::Power);
    }
    if prev.spare_battery.is_some() != next.spare_battery.is_some() {
        return Some(WakeReason::Spare);
    }
    if let (Some(before), Some(after)) = (prev.headset_battery, next.headset_battery) {
        if after < before && after <= LOW_BATTERY {
            return Some(WakeReason::Battery);
        }
    }
    let controls = |s: &Status| (s.anc_raw, s.transparency_raw, s.mic_status_raw);
    (controls(prev) != controls(next)).then_some(WakeReason::Controls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_device_headset::PowerStatus;

    fn status() -> Status {
        Status {
            headset_battery: Some(100),
            spare_battery: Some(100),
            power: PowerStatus::Online,
            auto_off: None,
            wireless_mode: None,
            mic_led_brightness: 10,
            mic_status_raw: 0,
            anc_raw: 2,
            transparency_raw: 8,
            bluetooth_at_power_on: None,
            bluetooth_call: None,
            bluetooth_on: None,
            bluetooth_link: None,
            pairing: None,
        }
    }

    #[test]
    fn identical_reads_do_not_wake() {
        assert_eq!(wake_reason(&status(), &status()), None);
    }

    #[test]
    fn power_changes_wake() {
        let off = Status {
            power: PowerStatus::Offline,
            headset_battery: None,
            ..status()
        };
        assert_eq!(wake_reason(&status(), &off), Some(WakeReason::Power));
        assert_eq!(wake_reason(&off, &status()), Some(WakeReason::Power));
        let cable = Status {
            power: PowerStatus::Charging,
            ..status()
        };
        assert_eq!(wake_reason(&status(), &cable), Some(WakeReason::Power));
    }

    #[test]
    fn battery_wakes_only_when_dropping_while_low() {
        let at = |pct| Status {
            headset_battery: Some(pct),
            ..status()
        };
        assert_eq!(wake_reason(&at(50), &at(37)), None, "not low yet");
        assert_eq!(wake_reason(&at(37), &at(25)), Some(WakeReason::Battery));
        assert_eq!(wake_reason(&at(25), &at(12)), Some(WakeReason::Battery));
        assert_eq!(wake_reason(&at(12), &at(25)), None, "rising is not news");
    }

    #[test]
    fn spare_pack_in_or_out_wakes() {
        let none = Status {
            spare_battery: None,
            ..status()
        };
        assert_eq!(wake_reason(&status(), &none), Some(WakeReason::Spare));
        assert_eq!(wake_reason(&none, &status()), Some(WakeReason::Spare));
        let lower = Status {
            spare_battery: Some(87),
            ..status()
        };
        assert_eq!(
            wake_reason(&status(), &lower),
            None,
            "a charging spare is not news"
        );
    }

    #[test]
    fn control_changes_wake() {
        assert_eq!(
            wake_reason(
                &status(),
                &Status {
                    anc_raw: 0,
                    ..status()
                }
            ),
            Some(WakeReason::Controls)
        );
        assert_eq!(
            wake_reason(
                &status(),
                &Status {
                    mic_status_raw: 1,
                    ..status()
                }
            ),
            Some(WakeReason::Controls)
        );
        assert_eq!(
            wake_reason(
                &status(),
                &Status {
                    transparency_raw: 5,
                    ..status()
                }
            ),
            Some(WakeReason::Controls)
        );
    }
}
