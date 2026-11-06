// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake keyboard's key bindings. A stub until unit kb-bindings builds its fake: it answers as the daemon's
//! stub does, the default state for `GetBindings` and "not built yet" for the rest.

use gg_ipc::{BindingsState, Request, ResponseData};

use crate::{error, Fake};

pub fn handle(req: Request, _s: &mut Fake) -> ResponseData {
    match req {
        Request::GetBindings => ResponseData::Bindings {
            state: BindingsState::default(),
        },
        _ => error("not built yet".into()),
    }
}
