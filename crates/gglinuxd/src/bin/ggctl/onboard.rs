// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl onboard`: the keyboard's onboard profile slots. Unit kb-onboard builds it.

use anyhow::{bail, Result};

use crate::Action;

pub fn action(_rest: &[&str]) -> Result<Action> {
    bail!("not built yet")
}
