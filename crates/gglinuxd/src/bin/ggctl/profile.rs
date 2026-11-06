// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl profile`: saved lighting profiles.

use anyhow::{bail, Result};
use gg_ipc::{ProfilesState, Request};

pub fn request(rest: &[&str]) -> Result<Request> {
    Ok(match rest {
        ["list"] => Request::GetProfiles,
        // Names may have spaces, so `profile apply Late Night` needs no quotes.
        ["apply", name @ ..] if !name.is_empty() => Request::ApplyProfile {
            name: name.join(" "),
        },
        _ => bail!("usage: ggctl profile list | ggctl profile apply <name>"),
    })
}

/// What `ggctl profile list` prints: one name a line.
pub fn print(state: &ProfilesState) {
    for name in &state.profiles {
        println!("{name}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sends;

    #[test]
    fn profile_names_may_contain_spaces() {
        match sends("profile apply Late Night") {
            Request::ApplyProfile { name } => assert_eq!(name, "Late Night"),
            other => panic!("{other:?}"),
        }
    }
}
