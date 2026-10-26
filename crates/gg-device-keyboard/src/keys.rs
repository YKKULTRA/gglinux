// SPDX-License-Identifier: GPL-3.0-or-later
//! The keyboard's typing reports, decoded to the set of keys held down.
//!
//! Interface 0 sends the 8-byte boot report (a modifier bitmask, a reserved
//! byte, six key slots); interface 2 sends a 32-byte N-key-rollover bitmap,
//! one bit per usage. Both use HID keyboard usage codes — the numbering of
//! [`crate::proto::KEYS`] and [`crate::layout::ANSI_TKL`], all but Menu's:
//! [`led`] gives the LED a usage lights.
//!
//! Which keys someone types is private: nothing here logs, and `Debug` on a
//! [`KeySet`] prints only how many keys it holds.

/// A set of HID keyboard usages, 0..=255.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct KeySet([u64; 4]);

impl KeySet {
    pub fn insert(&mut self, code: u8) {
        self.0[usize::from(code >> 6)] |= 1 << (code & 63);
    }

    pub fn contains(&self, code: u8) -> bool {
        self.0[usize::from(code >> 6)] & (1 << (code & 63)) != 0
    }

    pub fn iter(self) -> impl Iterator<Item = u8> {
        (0..=255u8).filter(move |&c| self.contains(c))
    }
}

impl std::fmt::Debug for KeySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n: u32 = self.0.iter().map(|w| w.count_ones()).sum();
        write!(f, "KeySet({n} keys)")
    }
}

pub const BOOT_LEN: usize = 8;
pub const NKRO_LEN: usize = 32;

/// HID usage `ErrorRollOver`: too many keys held for the boot report to say which.
const ERROR_ROLL_OVER: u8 = 0x01;
const FIRST_MODIFIER: u8 = 0xE0;

/// The keys held down in a boot report; `None` for a rollover error or a wrong length.
pub fn boot_keys(report: &[u8]) -> Option<KeySet> {
    if report.len() != BOOT_LEN {
        return None;
    }
    let slots = &report[2..];
    if slots.contains(&ERROR_ROLL_OVER) {
        return None;
    }
    let mut set = KeySet::default();
    for bit in 0..8 {
        if report[0] & (1 << bit) != 0 {
            set.insert(FIRST_MODIFIER + bit);
        }
    }
    slots
        .iter()
        .filter(|&&c| c != 0)
        .for_each(|&c| set.insert(c));
    Some(set)
}

/// The keys held down in an N-key-rollover bitmap; `None` for a wrong length.
pub fn nkro_keys(report: &[u8]) -> Option<KeySet> {
    if report.len() != NKRO_LEN {
        return None;
    }
    let mut set = KeySet::default();
    for (i, &byte) in report.iter().enumerate() {
        for bit in 0..8 {
            if byte & (1 << bit) != 0 {
                set.insert((i * 8 + bit) as u8);
            }
        }
    }
    Some(set)
}

/// Keys down in `now` that were not down in `prev`.
pub fn pressed(prev: KeySet, now: KeySet) -> impl Iterator<Item = u8> {
    KeySet(std::array::from_fn(|i| now.0[i] & !prev.0[i])).iter()
}

/// HID usage `Keyboard Application`: what a Menu key types.
const APPLICATION: u8 = 0x65;
/// The Menu key's LED. The lighting table has no Application slot; Menu sits
/// where Right GUI would, and is lit by its code.
const MENU_LED: u8 = 0xE7;

/// The HID code of the LED on the key that typed `usage`: the same code for
/// every key but Menu.
pub fn led(usage: u8) -> u8 {
    if usage == APPLICATION {
        MENU_LED
    } else {
        usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::ANSI_TKL;

    fn codes(set: KeySet) -> Vec<u8> {
        set.iter().collect()
    }

    fn set(codes: &[u8]) -> KeySet {
        let mut s = KeySet::default();
        codes.iter().for_each(|&c| s.insert(c));
        s
    }

    #[test]
    fn boot_reports_give_modifiers_and_keys() {
        // Left Ctrl + Right Shift held, A and Enter pressed.
        let report = [0b0010_0001, 0, 0x04, 0x28, 0, 0, 0, 0];
        assert_eq!(
            codes(boot_keys(&report).unwrap()),
            vec![0x04, 0x28, 0xE0, 0xE5]
        );
        assert_eq!(
            codes(boot_keys(&[0; 8]).unwrap()),
            Vec::<u8>::new(),
            "all released"
        );
    }

    #[test]
    fn boot_rollover_errors_and_wrong_lengths_are_not_a_state() {
        assert!(
            boot_keys(&[0, 0, 1, 1, 1, 1, 1, 1]).is_none(),
            "ErrorRollOver says nothing about which keys"
        );
        assert!(boot_keys(&[0; 7]).is_none());
        assert!(boot_keys(&[0; 9]).is_none());
    }

    #[test]
    fn nkro_bits_are_usage_codes() {
        let mut report = [0u8; NKRO_LEN];
        report[0] |= 1 << 4; // A, usage 0x04
        report[0xE1 / 8] |= 1 << (0xE1 % 8); // Left Shift
        assert_eq!(codes(nkro_keys(&report).unwrap()), vec![0x04, 0xE1]);
        assert!(nkro_keys(&[0; 31]).is_none());
        assert!(nkro_keys(&[0; 33]).is_none());
    }

    #[test]
    fn pressed_reports_only_new_keys() {
        assert_eq!(
            pressed(set(&[]), set(&[0x04])).collect::<Vec<_>>(),
            vec![0x04]
        );
        assert_eq!(
            pressed(set(&[0x04]), set(&[0x04, 0x05])).collect::<Vec<_>>(),
            vec![0x05],
            "a held key does not fire again"
        );
        assert_eq!(
            pressed(set(&[0x04, 0x05]), set(&[0x05])).count(),
            0,
            "releases are not presses"
        );
    }

    #[test]
    fn every_key_a_us_tkl_types_lights_a_drawn_key() {
        let typed = (0x04..=0x52u8)
            .filter(|&u| u != 0x32) // Non-US #: ISO boards only
            .chain([APPLICATION]) // Menu
            .chain(0xE0..=0xE6); // the modifiers; there's no Right GUI, and Fn types nothing
        for usage in typed {
            let led = led(usage);
            assert!(
                ANSI_TKL.iter().any(|k| k.0 == led),
                "usage {usage:#04x} lights nothing"
            );
        }
        let menu = ANSI_TKL.iter().find(|k| k.1 == "Menu").unwrap().0;
        assert_eq!(led(APPLICATION), menu, "the key the layout calls Menu");
    }

    #[test]
    fn debug_never_shows_which_keys() {
        let s = set(&[0x04, 0x05]);
        assert_eq!(format!("{s:?}"), "KeySet(2 keys)");
    }
}
