// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake actions on the spare keys F13–F24: the list is kept as the client sets it and
//! checked as the daemon checks it, and the desktop's shortcuts are always "bound (fake)".
//! Nothing runs, and nothing is announced.
//!
//! It lags as a real daemon may: the first read after a change still answers the list
//! from before it.

use std::sync::Mutex;

use gg_ipc::{ActionsState, Request, ResponseData};

use crate::log::method_name;
use crate::{error, Fake};

/// The actions as set, and the state the next read answers while that's still the old
/// one. Kept here rather than in [`Fake`], so the fake's actions stay in this file.
struct Actions {
    state: ActionsState,
    stale: Option<ActionsState>,
}

static ACTIONS: Mutex<Actions> = Mutex::new(Actions {
    state: ActionsState {
        bindings: Vec::new(),
        shortcuts: None,
        // As on a desktop without wtype: the app says why.
        typing: false,
        error: None,
    },
    stale: None,
});

pub fn handle(req: Request, _s: &mut Fake) -> ResponseData {
    let mut a = ACTIONS.lock().unwrap_or_else(|e| e.into_inner());
    match req {
        Request::GetActions => ResponseData::Actions {
            state: a.stale.take().unwrap_or_else(|| a.state.clone()),
        },
        Request::SetActions { bindings } => {
            if let Err(message) = gg_ipc::validate_actions(&bindings) {
                return error(message);
            }
            if bindings != a.state.bindings {
                a.stale = Some(a.state.clone());
                a.state.shortcuts = (!bindings.is_empty()).then(|| "bound (fake)".into());
                a.state.bindings = bindings;
            }
            ResponseData::Success
        }
        other => error(format!(
            "{} is not an actions request",
            method_name(&other)
        )),
    }
}
