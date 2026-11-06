// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake keyboard's onboard profile slots. A stub until unit kb-onboard builds its fake: it answers as the daemon's
//! stub does, the default state for `GetOnboard` and "not built yet" for the rest.

use gg_ipc::{OnboardState, Request, ResponseData};

use crate::{error, Fake};

pub fn handle(req: Request, _s: &mut Fake) -> ResponseData {
    match req {
        Request::GetOnboard => ResponseData::Onboard {
            state: OnboardState::default(),
        },
        _ => error("not built yet".into()),
    }
}
