// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl gamesense`: the GameSense server, the games that talked to it, and what each
//! of their events draws on: keys, the keyboard's screen, or every key from a colour grid.

use anyhow::{bail, Result};
use gg_ipc::{GameSenseState, Request};

pub fn request(rest: &[&str]) -> Result<Request> {
    Ok(match rest {
        [] => Request::GetGamesenseState,
        ["test"] => Request::GamesenseTest,
        ["test", "screen"] => Request::GamesenseTestScreen,
        ["on"] => Request::SetGamesenseEnabled { enabled: true },
        ["off"] => Request::SetGamesenseEnabled { enabled: false },
        _ => bail!("usage: ggctl gamesense [test [screen]|on|off]"),
    })
}

pub fn print(state: &GameSenseState) {
    for line in lines(state) {
        println!("{line}");
    }
}

/// What `ggctl gamesense` prints.
fn lines(state: &GameSenseState) -> Vec<String> {
    let mut out = vec![match &state.address {
        Some(address) if state.enabled => format!(
            "GameSense: on, listening on {address}, in {} Proton prefix{}",
            state.prefixes,
            if state.prefixes == 1 { "" } else { "es" }
        ),
        _ => "GameSense: off".to_string(),
    }];
    if state.games.is_empty() {
        out.push("no game has talked to GameSense yet".into());
    }
    for g in &state.games {
        let mut name = g.name.clone();
        match (&g.display_name, &g.developer) {
            (Some(shown), Some(by)) => name.push_str(&format!(" ({shown}, by {by})")),
            (Some(shown), None) => name.push_str(&format!(" ({shown})")),
            (None, Some(by)) => name.push_str(&format!(" (by {by})")),
            (None, None) => {}
        }
        let mut how = vec![if g.enabled { "on" } else { "off" }];
        if g.active {
            how.push("drawing now");
        }
        if g.on_screen {
            how.push("on the screen now");
        }
        out.push(format!("{name}: {}", how.join(", ")));
        for (event, draws) in g.draws_by_event() {
            let what: Vec<String> = draws.iter().map(|d| d.label()).collect();
            let what = if what.is_empty() {
                "nothing".to_string()
            } else {
                what.join("; ")
            };
            out.push(format!("  {event}: {what}"));
        }
        for note in g.screen_notes() {
            out.push(format!("  note: {note}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::{Draws, GameInfo, HandlerInfo};

    #[test]
    fn the_words_map_to_their_requests() {
        assert!(matches!(request(&[]).unwrap(), Request::GetGamesenseState));
        assert!(matches!(
            request(&["test", "screen"]).unwrap(),
            Request::GamesenseTestScreen
        ));
        assert!(matches!(
            request(&["test"]).unwrap(),
            Request::GamesenseTest
        ));
        for bad in [
            &["test", "screen", "now"][..],
            &["screen"],
            &["test", "keys"],
        ] {
            assert!(request(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn each_game_lists_what_its_events_draw_and_what_is_left_out() {
        let h = |event: &str, draws: Draws| HandlerInfo {
            event: event.into(),
            draws,
        };
        let state = GameSenseState {
            enabled: true,
            address: Some("127.0.0.1:51234".into()),
            prefixes: 1,
            games: vec![GameInfo {
                name: "MY_GAME".into(),
                display_name: Some("My Game".into()),
                developer: None,
                enabled: true,
                active: true,
                events: vec![
                    "BACKGROUND".into(),
                    "HEALTH".into(),
                    "RUMBLE".into(),
                    "UNUSED".into(),
                ],
                handlers: vec![
                    h(
                        "BACKGROUND",
                        Draws::PartialBitmap {
                            excluded: vec!["HEALTH".into()],
                        },
                    ),
                    h("HEALTH", Draws::Keys),
                    h(
                        "HEALTH",
                        Draws::Screen {
                            notes: vec!["icon 7 is left out".into()],
                        },
                    ),
                    h(
                        "RUMBLE",
                        Draws::Elsewhere {
                            device: "tactile".into(),
                        },
                    ),
                ],
                on_screen: true,
            }],
        };
        assert_eq!(
            lines(&state),
            vec![
                "GameSense: on, listening on 127.0.0.1:51234, in 1 Proton prefix",
                "MY_GAME (My Game): on, drawing now, on the screen now",
                "  BACKGROUND: every key but HEALTH's, as a background",
                "  HEALTH: keys; screen",
                "  RUMBLE: tactile, not shown here",
                "  UNUSED: nothing",
                "  note: icon 7 is left out",
            ]
        );
        assert_eq!(
            lines(&GameSenseState::default()),
            vec!["GameSense: off", "no game has talked to GameSense yet"]
        );
    }
}
