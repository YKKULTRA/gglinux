// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl actions`: actions on the spare keys F13–F24. Unit mouse-actions builds it.

use anyhow::{bail, Result};

use crate::Action;

pub fn action(_rest: &[&str]) -> Result<Action> {
    bail!("not built yet")
}
