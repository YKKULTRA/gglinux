// SPDX-License-Identifier: GPL-3.0-or-later
//! The experimental switches, from [`State::experimental`]: features not
//! tried on real hardware yet stay off until turned on here. A card on the
//! Settings page, after the parts. Not built yet: the card stays hidden
//! (`ExperimentalUi.shown`).

use std::rc::Rc;

use crate::client::{Client, State};
use crate::{AppWindow, Forms};

/// What the panel keeps between the daemon's answers. Nothing yet.
#[derive(Debug, Default)]
pub struct ExperimentalForm {}

/// Show the daemon's experimental switches. Nothing to show yet.
pub fn show(_ui: &AppWindow, _s: &State) {}

/// Answer the panel's callbacks. It has none yet.
pub fn bind(_ui: &AppWindow, _client: &Client, _forms: &Rc<Forms>) {}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.experimental.borrow_mut() = ExperimentalForm::default();
}

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]).
/// None yet.
pub fn poke(_ui: &AppWindow, _name: &str, _n: i32) -> bool {
    false
}
