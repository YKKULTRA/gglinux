// SPDX-License-Identifier: GPL-3.0-or-later
//! What a game's GameSense handlers draw on, as the daemon read them: the keys, the
//! keyboard's screen, every key from a colour grid, or a device this machine doesn't have.

use serde::{Deserialize, Serialize};

use crate::GameInfo;

/// One handler of one of a game's events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandlerInfo {
    pub event: String,
    pub draws: Draws,
}

/// What a handler draws on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "snake_case")]
pub enum Draws {
    /// Keys, by colour.
    Keys,
    /// The keyboard's screen; `notes` says what it asks for that gglinux leaves out.
    Screen {
        #[serde(default)]
        notes: Vec<String>,
    },
    /// Every key, from the event's 22 × 6 colour grid.
    Bitmap,
    /// Every key from the grid but those of these events: a background.
    PartialBitmap { excluded: Vec<String> },
    /// A device this machine doesn't have, by its GameSense device-type.
    Elsewhere { device: String },
}

impl Draws {
    /// A few words for it, as ggctl and the app say it.
    pub fn label(&self) -> String {
        match self {
            Draws::Keys => "keys".into(),
            Draws::Screen { .. } => "screen".into(),
            Draws::Bitmap => "every key".into(),
            Draws::PartialBitmap { excluded } if excluded.is_empty() => {
                "every key, as a background".into()
            }
            Draws::PartialBitmap { excluded } => format!(
                "every key but {}'s, as a background",
                excluded.join(" and ")
            ),
            Draws::Elsewhere { device } => format!("{device}, not shown here"),
        }
    }
}

impl GameInfo {
    /// Each event with what its handlers draw on, in the game's event order; an event
    /// with no handlers draws nothing.
    pub fn draws_by_event(&self) -> Vec<(&str, Vec<&Draws>)> {
        self.events
            .iter()
            .map(|e| {
                let draws = self
                    .handlers
                    .iter()
                    .filter(|h| &h.event == e)
                    .map(|h| &h.draws)
                    .collect();
                (e.as_str(), draws)
            })
            .collect()
    }

    /// What its screen handlers leave out, each said once.
    pub fn screen_notes(&self) -> Vec<&str> {
        let mut notes: Vec<&str> = Vec::new();
        for h in &self.handlers {
            if let Draws::Screen { notes: n } = &h.draws {
                for note in n {
                    if !notes.contains(&note.as_str()) {
                        notes.push(note);
                    }
                }
            }
        }
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> GameInfo {
        let h = |event: &str, draws: Draws| HandlerInfo {
            event: event.into(),
            draws,
        };
        let icon = || Draws::Screen {
            notes: vec!["icon 7 is left out".into()],
        };
        GameInfo {
            name: "G".into(),
            display_name: None,
            developer: None,
            enabled: true,
            active: true,
            events: vec!["BG".into(), "HEALTH".into(), "KILLS".into(), "QUIET".into()],
            handlers: vec![
                h(
                    "BG",
                    Draws::PartialBitmap {
                        excluded: vec!["AMMO".into(), "HEALTH".into()],
                    },
                ),
                h("HEALTH", Draws::Keys),
                h("HEALTH", icon()),
                h("KILLS", icon()),
            ],
            on_screen: false,
        }
    }

    #[test]
    fn handlers_group_by_event_in_event_order() {
        let g = game();
        let by: Vec<(&str, Vec<String>)> = g
            .draws_by_event()
            .into_iter()
            .map(|(e, d)| (e, d.iter().map(|d| d.label()).collect()))
            .collect();
        assert_eq!(
            by,
            vec![
                (
                    "BG",
                    vec!["every key but AMMO and HEALTH's, as a background".to_string()]
                ),
                ("HEALTH", vec!["keys".to_string(), "screen".to_string()]),
                ("KILLS", vec!["screen".to_string()]),
                ("QUIET", vec![]),
            ]
        );
        assert_eq!(g.screen_notes(), vec!["icon 7 is left out"], "said once");
    }

    #[test]
    fn every_kind_has_a_label() {
        assert_eq!(Draws::Bitmap.label(), "every key");
        assert_eq!(
            Draws::PartialBitmap { excluded: vec![] }.label(),
            "every key, as a background"
        );
        assert_eq!(
            Draws::Elsewhere {
                device: "tactile".into()
            }
            .label(),
            "tactile, not shown here"
        );
    }
}
