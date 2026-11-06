// SPDX-License-Identifier: GPL-3.0-or-later
//! Actions on the computer bound to spare keys (F13–F24), from
//! [`State::actions`]: a card on the Settings page, after the parts. Not built
//! yet: the card stays hidden (`ActionsUi.shown`).

use std::rc::Rc;

use crate::client::{Client, State};
use crate::{AppWindow, Forms};

/// What the panel keeps between the daemon's answers. Nothing yet.
#[derive(Debug, Default)]
pub struct ActionsForm {}

/// Show the daemon's actions. Nothing to show yet.
pub fn show(_ui: &AppWindow, _s: &State) {}

/// Answer the panel's callbacks. It has none yet.
pub fn bind(_ui: &AppWindow, _client: &Client, _forms: &Rc<Forms>) {}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.actions.borrow_mut() = ActionsForm::default();
}

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]).
/// None yet.
pub fn poke(_ui: &AppWindow, _name: &str, _n: i32) -> bool {
    false
}
