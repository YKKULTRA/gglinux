// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl gamesense`: the GameSense server.

use anyhow::{bail, Result};
use gg_ipc::Request;

pub fn request(rest: &[&str]) -> Result<Request> {
    Ok(match rest {
        ["test"] => Request::GamesenseTest,
        ["on"] => Request::SetGamesenseEnabled { enabled: true },
        ["off"] => Request::SetGamesenseEnabled { enabled: false },
        _ => bail!("usage: ggctl gamesense test|on|off"),
    })
}
