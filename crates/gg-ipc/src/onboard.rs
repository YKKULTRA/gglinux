// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apex Pro TKL Gen 3's five onboard profile slots.
//!
//! Everything else gglinux sends the keyboard lives in its RAM and is gone after a power
//! cycle; a slot in flash is what the keyboard loads when it powers up. Writing one is done
//! as a single checked transaction (back up, rewrite, read back, restore on any mismatch).
//! Built from Apex Control's protocol reference (CC BY 4.0); see `docs/PROTOCOLS.md`.

use serde::{Deserialize, Serialize};

/// The keyboard has slots 0 to 4.
pub const SLOTS: u8 = 5;

/// One slot as last read.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OnboardSlot {
    pub index: u8,
    /// The name stored in the slot ("Config 1" … from the factory).
    pub name: Option<String>,
    /// Whether the slot's image read back whole with a matching checksum; `None` until read.
    pub valid: Option<bool>,
    /// The newest backup of this slot on disk, if any.
    pub backup: Option<String>,
}

/// The onboard slots as the daemon sees them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OnboardState {
    pub slots: Vec<OnboardSlot>,
    /// The slot the keyboard runs now, if known.
    pub active: Option<u8>,
    /// A read or write is in progress (a full write takes about 2 s).
    pub busy: bool,
    /// Why the last operation didn't go through, and what was done about it.
    pub error: Option<String>,
    /// Something the last operation noticed though it went through (what the keyboard's
    /// own check of a written slot answered, while what a good answer looks like isn't
    /// known).
    pub note: Option<String>,
}

/// What to write into a slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OnboardSource {
    /// The slot as it is, with gglinux's screen picture (text or a picture) and, if gglinux
    /// sets the switches, its Rapid Tap pairs written into it. Everything else is kept as
    /// the slot has it: macros, lighting, bindings, and the per-key actuation and Rapid
    /// Trigger tables (the firmware's key order for them isn't known yet).
    Current,
    /// A backup file made earlier (absolute path).
    Backup { path: String },
    /// A backup file (absolute path) put into a slot that doesn't check out any more, such
    /// as one a write left half written: `ggctl onboard restore N <file> --damaged`. The
    /// slot's bytes are saved first as `slot<N>-<UTC>-damaged.bin`. Refused for a slot that
    /// checks out, which takes [`OnboardSource::Backup`].
    Recover { path: String },
}

/// Check a slot number, with a message meant for the user.
pub fn check_slot(slot: u8) -> Result<(), String> {
    if slot < SLOTS {
        Ok(())
    } else {
        Err(format!(
            "the keyboard has slots 1 to {SLOTS}, not {}",
            u16::from(slot) + 1
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_zero_to_four() {
        for s in 0..SLOTS {
            check_slot(s).unwrap();
        }
        assert!(check_slot(5).unwrap_err().contains("slots 1 to 5"));
        // Any byte a client sends, without overflowing.
        assert!(check_slot(255).unwrap_err().contains("not 256"));
    }

    #[test]
    fn sources_have_stable_json() {
        assert_eq!(
            serde_json::to_string(&OnboardSource::Current).unwrap(),
            r#"{"kind":"current"}"#
        );
        let b = OnboardSource::Backup {
            path: "/x/slot4.bin".into(),
        };
        assert_eq!(
            serde_json::from_str::<OnboardSource>(&serde_json::to_string(&b).unwrap()).unwrap(),
            b
        );
        assert_eq!(
            serde_json::to_string(&OnboardSource::Recover {
                path: "/x/slot5.bin".into()
            })
            .unwrap(),
            r#"{"kind":"recover","path":"/x/slot5.bin"}"#
        );
    }

    #[test]
    fn a_state_from_an_older_daemon_has_no_note() {
        let state: OnboardState = serde_json::from_str(r#"{"busy":false}"#).unwrap();
        assert_eq!(state.note, None);
    }
}
