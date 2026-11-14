// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake keyboard's screen and screen care, checked as the daemon checks them and echoed back.
//! Every kind is taken, an animation or the System Monitor too; nothing is drawn, so the
//! monitor reads nothing of this computer (and never asks its graphics card).

use gg_ipc::{Request, ResponseData};

use crate::log::method_name;
use crate::{error, Fake};

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    match req {
        Request::SetScreen { screen } => match screen.validate() {
            Ok(()) => {
                s.keyboard.screen = screen;
                ResponseData::Success
            }
            Err(message) => ResponseData::Error { message },
        },
        Request::SetScreenCare { care } => {
            s.keyboard.screen_care = care;
            ResponseData::Success
        }
        other => error(format!(
            "{} is not a keyboard screen request",
            method_name(&other)
        )),
    }
}
