// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl parts`: which parts of gglinux run.

use anyhow::{bail, Context, Result};
use gg_ipc::{Part, Parts, Request};

use crate::{on_off, Action};

pub fn action(rest: &[&str]) -> Result<Action> {
    Ok(match rest {
        [] => Action::Send(Request::GetParts),
        [name, v] => {
            let part = Part::ALL
                .into_iter()
                .find(|p| p.label().eq_ignore_ascii_case(name))
                .with_context(|| {
                    format!("no part '{name}' (try: keyboard, headset, mouse, sonar, gamesense, profiles, clips)")
                })?;
            Action::SetPart(part, on_off(v)?)
        }
        _ => bail!("usage: ggctl parts [<part> on|off]"),
    })
}

pub fn print(parts: &Parts) {
    let running = parts.running();
    for part in Part::ALL {
        let state = match (parts.on(part), running.on(part)) {
            (true, true) => "on",
            (true, false) => "on, but needs the keyboard",
            (false, _) => "off",
        };
        println!("{:<10} {state}", part.label().to_lowercase());
    }
}
