// SPDX-License-Identifier: GPL-3.0-or-later
//! The keyboard's onboard profile slots: the worker's part ([`Store`], [`on_command`],
//! [`on_connect`]) and the clients' requests ([`handle`]).
//!
//! A stub until unit kb-onboard builds it: [`handle`] answers `get_onboard` with the
//! store's state and the rest "not built yet". The reports are framed in
//! `gg_device_keyboard::onboard`. The keyboard lets them out only while `keyboard_onboard`
//! is on: the worker keeps the open keyboard's families in step with the experimental
//! gates ([`super::Command::SetExperimental`]), and a refused request should say so with
//! [`gg_ipc::Experimental::refusal`]. A slot write runs on the worker's thread, so the
//! lighting stream pauses while it does.

use std::path::Path;

use gg_device_keyboard::Keyboard;
use gg_ipc::{Event, Experimental, OnboardState, Request, ResponseData};
use tokio::sync::broadcast;

use super::{Command, Handle, Reply};

/// What the keyboard worker keeps for the onboard slots.
#[derive(Default)]
pub struct Store {
    state: OnboardState,
}

impl Store {
    /// What's kept in `config_dir` (the slots' backups), read when the worker starts.
    pub fn load(_config_dir: &Path) -> Store {
        Store::default()
    }
}

/// The keyboard worker's commands for the onboard slots.
pub enum Cmd {
    Get(Reply<OnboardState>),
}

/// One command, on the keyboard worker's thread. `device` is the open keyboard, if any.
pub fn on_command(
    cmd: Cmd,
    store: &mut Store,
    _device: Option<&Keyboard>,
    _exp: &Experimental,
    _event_tx: &broadcast::Sender<Event>,
) {
    match cmd {
        Cmd::Get(reply) => {
            let _ = reply.send(Ok(store.state.clone()));
        }
    }
}

/// The keyboard just opened and its switches were sent again.
pub fn on_connect(
    _store: &mut Store,
    _device: &Keyboard,
    _exp: &Experimental,
    _event_tx: &broadcast::Sender<Event>,
) {
}

/// An onboard request from a client; `main` has already refused it if the keyboard part
/// is off.
pub async fn handle(req: Request, kb: &Handle) -> ResponseData {
    match req {
        Request::GetOnboard => match kb.send(|reply| Command::Onboard(Cmd::Get(reply))).await {
            Ok(state) => ResponseData::Onboard { state },
            Err(message) => ResponseData::Error { message },
        },
        _ => ResponseData::Error {
            message: "not built yet".into(),
        },
    }
}
