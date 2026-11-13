// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl headset`: the headset's settings. Not built yet.

use anyhow::{bail, Result};
use gg_ipc::Request;

pub fn request(_words: &[&str]) -> Result<Request> {
    bail!("`ggctl headset` isn't built yet")
}
