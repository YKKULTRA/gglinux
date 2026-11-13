// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard macros: named key sequences, the keys that play them, and the onboard slot
//! they were written into.
//!
//! The Apex Pro TKL Gen 3 plays a macro itself, from the profile slot it runs. GG 123 keeps
//! macros in the slot's image and sends nothing live (our captures apex-20 to apex-22,
//! `docs/PROTOCOLS.md` 3f), so a macro works only once it's written into a slot
//! ([`crate::Request::WriteMacros`]).

use serde::{Deserialize, Serialize};

/// One step of a macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MacroEvent {
    /// A keyboard key goes down or up, by HID usage.
    Key { code: u8, down: bool },
    /// A mouse button goes down or up.
    Button { button: u8, down: bool },
    /// Wait, in milliseconds.
    Delay { ms: u16 },
}

/// How a macro plays when its key is pressed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MacroMode {
    /// Once per press.
    #[default]
    Once,
    /// Again and again while the key is held.
    WhileHeld,
}

/// A named sequence of steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Macro {
    pub name: String,
    pub events: Vec<MacroEvent>,
}

/// A key that plays a macro (on the normal layer), by HID code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroBinding {
    pub code: u8,
    /// The [`Macro::name`] it plays.
    pub name: String,
    #[serde(default)]
    pub mode: MacroMode,
}

/// The macros and the keys that play them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroLibrary {
    #[serde(default)]
    pub macros: Vec<Macro>,
    #[serde(default)]
    pub bindings: Vec<MacroBinding>,
}

/// The macros as the daemon keeps them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacrosState {
    pub library: MacroLibrary,
    /// The slot (0..=4) the library was last written into, if it was.
    pub written_slot: Option<u8>,
    /// A write is in progress.
    pub busy: bool,
    /// Why the last write didn't go through, and what was done about it.
    pub error: Option<String>,
}
