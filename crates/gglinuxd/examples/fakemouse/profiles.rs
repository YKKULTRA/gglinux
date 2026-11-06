// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake profiles and game rules, as the client last set them.

use gg_ipc::{Request, ResponseData};

use crate::log::method_name;
use crate::{error, Fake};

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    match req {
        Request::GetProfiles => ResponseData::Profiles {
            state: s.profiles.clone(),
        },
        Request::SaveProfile { name } => {
            if !s.profiles.profiles.contains(&name) {
                s.profiles.profiles.push(name);
            }
            ResponseData::Success
        }
        Request::ApplyProfile { name } => {
            if s.profiles.profiles.contains(&name) {
                ResponseData::Success
            } else {
                ResponseData::Error {
                    message: format!("no profile named \"{name}\""),
                }
            }
        }
        Request::DeleteProfile { name } => {
            s.profiles.profiles.retain(|p| p != &name);
            ResponseData::Success
        }
        Request::SetGameRules { rules } => {
            s.profiles.rules = rules;
            ResponseData::Success
        }
        other => error(format!("{} is not a profiles request", method_name(&other))),
    }
}
