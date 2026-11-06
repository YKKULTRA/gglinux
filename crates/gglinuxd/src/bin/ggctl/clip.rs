// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl clip`: the instant replay.

use anyhow::{bail, Result};
use gg_ipc::Request;

pub fn request(rest: &[&str]) -> Result<Request> {
    Ok(match rest {
        ["save"] => Request::SaveClip,
        _ => bail!("usage: ggctl clip save"),
    })
}
