// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake actions on the spare keys F13–F24. A stub until unit mouse-actions builds its fake: it answers as the daemon's
//! stub does, the default state for `GetActions` and "not built yet" for the rest.

use gg_ipc::{ActionsState, Request, ResponseData};

use crate::{error, Fake};

pub fn handle(req: Request, _s: &mut Fake) -> ResponseData {
    match req {
        Request::GetActions => ResponseData::Actions {
            state: ActionsState::default(),
        },
        _ => error("not built yet".into()),
    }
}
