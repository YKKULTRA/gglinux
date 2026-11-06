// SPDX-License-Identifier: GPL-3.0-or-later
//! Key bindings on the keyboard: the worker's part ([`Store`], [`on_command`],
//! [`on_connect`]) and the clients' requests ([`handle`]).
//!
//! A stub until unit kb-bindings builds it: [`handle`] answers `get_bindings` with the
//! store's state and the rest "not built yet". The reports are framed in
//! `gg_device_keyboard::bindings`. The keyboard lets them out only while
//! `keyboard_bindings` is on: the worker keeps the open keyboard's families in step with
//! the experimental gates ([`super::Command::SetExperimental`]), and a refused request
//! should say so with [`gg_ipc::Experimental::refusal`].

use std::path::Path;

use gg_device_keyboard::Keyboard;
use gg_ipc::{BindingsState, Event, Experimental, Request, ResponseData};
use tokio::sync::broadcast;

use super::{Command, Handle, Reply};

/// What the keyboard worker keeps for the bindings.
#[derive(Default)]
pub struct Store {
    state: BindingsState,
}

impl Store {
    /// The saved bindings in `config_dir`, read when the worker starts.
    pub fn load(_config_dir: &Path) -> Store {
        Store::default()
    }
}

/// The keyboard worker's commands for the bindings.
pub enum Cmd {
    Get(Reply<BindingsState>),
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

/// The keyboard just opened and its switches were sent again. Bindings live in its RAM,
/// so here is where they're sent again too.
pub fn on_connect(
    _store: &mut Store,
    _device: &Keyboard,
    _exp: &Experimental,
    _event_tx: &broadcast::Sender<Event>,
) {
}

/// A bindings request from a client; `main` has already refused it if the keyboard part
/// is off.
pub async fn handle(req: Request, kb: &Handle) -> ResponseData {
    match req {
        Request::GetBindings => match kb.send(|reply| Command::Bindings(Cmd::Get(reply))).await {
            Ok(state) => ResponseData::Bindings { state },
            Err(message) => ResponseData::Error { message },
        },
        _ => ResponseData::Error {
            message: "not built yet".into(),
        },
    }
}
