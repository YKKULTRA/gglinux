// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake's experimental gates: kept as the client sets them. Nothing is announced, and
//! nothing is gated: the fake has no device to protect.

use gg_ipc::{Request, ResponseData};

use crate::log::method_name;
use crate::{error, Fake};

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    match req {
        Request::GetExperimental => ResponseData::Experimental {
            experimental: s.experimental,
        },
        Request::SetExperimental { experimental } => {
            s.experimental = experimental;
            ResponseData::Success
        }
        other => error(format!(
            "{} is not an experimental-gates request",
            method_name(&other)
        )),
    }
}
