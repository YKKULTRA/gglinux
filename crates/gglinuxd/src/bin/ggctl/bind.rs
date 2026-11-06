// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl bind`: key bindings: what each key does, on its own, with Fn held and past its second actuation point. Unit kb-bindings builds it.

use anyhow::{bail, Result};

use crate::Action;

pub fn action(_rest: &[&str]) -> Result<Action> {
    bail!("not built yet")
}
