// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake GameSense server: games as the client last set them. Its test game draws
//! what the daemon's does: the F-key health bar with the health on the screen, and the
//! screen test's sequence.

use gg_ipc::{Draws, GameInfo, HandlerInfo, Request, ResponseData};

use crate::log::method_name;
use crate::{error, Fake};

const TEST_GAME: &str = "GGLINUX_TEST";

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
            let g = test_game(s);
            add_event(g, "HEALTH", &[Draws::Keys, screen()]);
            g.on_screen = true;
            ResponseData::Success
        }
        Request::GamesenseTestScreen => {
            let g = test_game(s);
            add_event(g, "SCREEN", &[screen()]);
            g.on_screen = true;
            ResponseData::Success
        }

        other => error(format!(
            "{} is not a GameSense request",
            method_name(&other)
        )),
    }
}

fn screen() -> Draws {
    Draws::Screen { notes: Vec::new() }
}

/// The test game, added if it isn't there yet, drawing now.
fn test_game(s: &mut Fake) -> &mut GameInfo {
    let games = &mut s.gamesense.games;
    let i = match games.iter().position(|g| g.name == TEST_GAME) {
        Some(i) => i,
        None => {
            games.push(GameInfo {
                name: TEST_GAME.into(),
                display_name: Some("gglinux test".into()),
                developer: Some("gglinux".into()),
                enabled: true,
                active: true,
                events: Vec::new(),
                handlers: Vec::new(),
                on_screen: false,
            });
            games.len() - 1
        }
    };
    let g = &mut games[i];
    g.active = true;
    g
}

/// The event and its handlers, once; the events stay in name order, as the daemon's.
fn add_event(g: &mut GameInfo, event: &str, draws: &[Draws]) {
    if g.events.iter().any(|e| e == event) {
        return;
    }
    g.events.push(event.into());
    g.events.sort();
    g.handlers.extend(draws.iter().map(|d| HandlerInfo {
        event: event.into(),
        draws: d.clone(),
    }));
}
