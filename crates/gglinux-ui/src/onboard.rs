// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apex Pro TKL Gen 3's onboard profile slots, from [`State::onboard`]:
//! a panel at the end of the keyboard card. Not built yet: the panel stays
//! hidden (`OnboardUi.shown`).

use std::rc::Rc;

use crate::client::{Client, State};
use crate::{AppWindow, Forms};

/// What the panel keeps between the daemon's answers. Nothing yet.
#[derive(Debug, Default)]
pub struct OnboardForm {}

/// Show the daemon's onboard slots. Nothing to show yet.
pub fn show(_ui: &AppWindow, _s: &State) {}

/// Answer the panel's callbacks. It has none yet.
pub fn bind(_ui: &AppWindow, _client: &Client, _forms: &Rc<Forms>) {}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.onboard.borrow_mut() = OnboardForm::default();
}

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]).
/// None yet.
pub fn poke(_ui: &AppWindow, _name: &str, _n: i32) -> bool {
    false
}
