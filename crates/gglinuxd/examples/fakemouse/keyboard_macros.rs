// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake's keyboard macros. Not built yet.

use gg_ipc::{Request, ResponseData};

use crate::Fake;

pub fn handle(_req: Request, _s: &mut Fake) -> ResponseData {
    crate::error("keyboard macros aren't built yet".to_string())
}
