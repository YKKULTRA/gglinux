// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl macro`: keyboard macros. Not built yet.

use anyhow::{bail, Result};
use gg_ipc::Request;

pub fn request(_words: &[&str]) -> Result<Request> {
    bail!("`ggctl macro` isn't built yet")
}
