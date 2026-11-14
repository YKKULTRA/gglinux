// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl screen care`: burn-in care for the keyboard's screen.

use anyhow::{bail, Result};
use gg_ipc::ScreenCare;

use crate::{on_off, Action};

/// One change to the screen care.
#[derive(Debug, PartialEq)]
pub enum CareEdit {
    Shift(bool),
    DarkWhenAway(bool),
    OwnScreenToo(bool),
}

impl CareEdit {
    pub fn apply(self, c: &mut ScreenCare) {
        match self {
            CareEdit::Shift(on) => c.shift = on,
            CareEdit::DarkWhenAway(on) => c.dark_when_away = on,
            CareEdit::OwnScreenToo(on) => c.own_screen_too = on,
        }
    }
}

pub fn action(rest: &[&str]) -> Result<Action> {
    Ok(Action::EditCare(match rest {
        ["shift", v] => CareEdit::Shift(on_off(v)?),
        ["away", v] => CareEdit::DarkWhenAway(on_off(v)?),
        ["own", v] => CareEdit::OwnScreenToo(on_off(v)?),
        _ => bail!("usage: ggctl screen care shift|away|own on|off"),
    }))
}
