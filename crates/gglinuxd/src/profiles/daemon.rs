// SPDX-License-Identifier: GPL-3.0-or-later
//! Profiles' way into the rest of the daemon: [`Daemon`] for the daemon's workers, through the
//! same `dispatch` the user's requests go through, and the routing arm's entry ([`serve`]).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gg_ipc::{Part, Request, RequestFrame, ResponseData, ServerMessage};
use tokio::sync::watch;

use super::Daemon;

impl Daemon for crate::Workers {
    fn ask(&self, req: Request) -> Pin<Box<dyn Future<Output = ResponseData> + Send + '_>> {
        Box::pin(async move {
            // Profiles answer one request at a time: asking themselves would wait forever.
            if req.part() == Some(Part::Profiles) {
                return ResponseData::Error {
                    message: "profiles can't apply a profiles request".into(),
                };
            }
            let line = match serde_json::to_string(&RequestFrame { id: 0, req }) {
                Ok(line) => line,
                Err(e) => {
                    return ResponseData::Error {
                        message: e.to_string(),
                    }
                }
            };
            // No section asks for the device list, so an empty one does.
            let (_devices, snapshot) = watch::channel(Vec::new());
            match crate::dispatch(&line, &snapshot, self).await {
                ServerMessage::Response { result, .. } => result,
                ServerMessage::Notification { .. } => ResponseData::Error {
                    message: "the daemon didn't answer".into(),
                },
            }
        })
    }
}

/// Hand profiles the daemon's workers, the first time only.
pub fn connect(workers: &crate::Workers) {
    workers.profiles.connect(|| Arc::new(workers.clone()));
}

/// A profiles request from a client; `main` has already refused it if the part is off.
pub async fn serve(req: Request, workers: &crate::Workers) -> ResponseData {
    connect(workers);
    workers.profiles.ask(req).await
}
