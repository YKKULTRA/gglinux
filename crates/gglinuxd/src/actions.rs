// SPDX-License-Identifier: GPL-3.0-or-later
//! Actions on the computer, started by a mouse button (or any key) bound to one of the
//! spare keys F13–F24: the desktop reports the key through its global-shortcuts portal,
//! and the daemon runs what it's bound to. See `gg_ipc::Action`.
//!
//! A stub until unit mouse-actions builds it: `get_actions` answers the default state
//! (nothing bound), `set_actions` "not built yet".

use gg_ipc::{ActionsState, Request, ResponseData};

/// `get_actions` and `set_actions`.
pub async fn handle(req: Request) -> ResponseData {
    match req {
        Request::GetActions => ResponseData::Actions {
            state: ActionsState::default(),
        },
        _ => ResponseData::Error {
            message: "not built yet".into(),
        },
    }
}
