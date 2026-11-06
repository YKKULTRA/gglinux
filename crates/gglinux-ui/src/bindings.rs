// SPDX-License-Identifier: GPL-3.0-or-later
//! Key bindings for the Apex Pro TKL Gen 3: remapping, the Fn layer and the
//! deep-press layer, from [`State::bindings`]. A panel in the keyboard card,
//! after the switches. Not built yet: the panel stays hidden
//! (`BindingsUi.shown`).

use std::rc::Rc;

use crate::client::{Client, State};
use crate::{AppWindow, Forms};

/// What the panel keeps between the daemon's answers. Nothing yet.
#[derive(Debug, Default)]
pub struct BindingsForm {}

/// Show the daemon's key bindings. Nothing to show yet.
pub fn show(_ui: &AppWindow, _s: &State) {}

/// Answer the panel's callbacks. It has none yet.
pub fn bind(_ui: &AppWindow, _client: &Client, _forms: &Rc<Forms>) {}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.bindings.borrow_mut() = BindingsForm::default();
}

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]).
/// None yet.
pub fn poke(_ui: &AppWindow, _name: &str, _n: i32) -> bool {
    false
}
