// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake GameSense server: games as the client last set them.

use gg_ipc::{GameInfo, Request, ResponseData};

use crate::log::method_name;
use crate::{error, Fake};

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    match req {
        Request::GetGamesenseState => ResponseData::Gamesense {
            state: s.gamesense.clone(),
        },
        Request::SetGamesenseEnabled { enabled } => {
            s.gamesense.enabled = enabled;
            s.gamesense.address = enabled.then(|| "127.0.0.1:51234".to_string());
            s.gamesense.prefixes = if enabled { 17 } else { 0 };
            ResponseData::Success
        }
        Request::SetGameEnabled { game, enabled } => {
            match s.gamesense.games.iter_mut().find(|g| g.name == game) {
                Some(g) => {
                    g.enabled = enabled;
                    ResponseData::Success
                }
                None => ResponseData::Error {
                    message: format!("unknown game {game}"),
                },
            }
        }
        Request::RemoveGame { game } => {
            s.gamesense.games.retain(|g| g.name != game);
            ResponseData::Success
        }
        Request::GamesenseTest => {
            if !s.gamesense.games.iter().any(|g| g.name == "GGLINUX_TEST") {
                s.gamesense.games.push(GameInfo {
                    name: "GGLINUX_TEST".into(),
                    display_name: Some("gglinux test".into()),
                    developer: Some("gglinux".into()),
                    enabled: true,
                    active: true,
                    events: vec!["HEALTH".into()],
                });
            }
            ResponseData::Success
        }

        other => error(format!(
            "{} is not a GameSense request",
            method_name(&other)
        )),
    }
}
