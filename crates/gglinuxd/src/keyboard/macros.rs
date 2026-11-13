// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard macros. Not built yet: every request is answered with that.

use gg_ipc::{Request, ResponseData};

use super::Handle;

/// A macros request from a client; `main` has already refused it if the keyboard part is
/// off.
pub async fn handle(_req: Request, _kb: &Handle) -> ResponseData {
    ResponseData::Error {
        message: "keyboard macros aren't built yet".into(),
    }
}
