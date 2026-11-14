// SPDX-License-Identifier: GPL-3.0-or-later
//! Actions on the computer, started by a mouse button (or any key) bound to one of the spare
//! keys F13–F24.
//!
//! The button is bound on the mouse itself to the spare key. The desktop reports that key to
//! gglinux through its global-shortcuts portal, and the daemon runs the action. gglinux never
//! reads an input device for this.

use serde::{Deserialize, Serialize};

/// HID usage of F13; F24 is `0x73`.
pub const FIRST_KEY: u8 = 0x68;
pub const LAST_KEY: u8 = 0x73;

/// What happens when the key comes in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Start a program: `argv[0]` and its arguments, no shell.
    Run { argv: Vec<String> },
    /// Type a piece of text into the focused window.
    Type { text: String },
    /// Switch to a saved profile.
    ProfileApply { name: String },
    /// Save the instant replay.
    ClipSave,
    /// A media key, as a consumer-page usage (see [`crate::KeyAction::Consumer`]).
    Media { usage: u16 },
}

/// One spare key and what it does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionBinding {
    /// `FIRST_KEY..=LAST_KEY`.
    pub key: u8,
    /// A name for Settings and the desktop's shortcut list.
    pub label: String,
    pub action: Action,
}

impl ActionBinding {
    /// The spare key's name: "F13" … "F24".
    pub fn key_name(key: u8) -> Option<String> {
        (FIRST_KEY..=LAST_KEY)
            .contains(&key)
            .then(|| format!("F{}", 13 + key - FIRST_KEY))
    }
}

/// Check a list of action bindings, with a message meant for the user.
pub fn validate(bindings: &[ActionBinding]) -> Result<(), String> {
    let mut seen = Vec::new();
    for b in bindings {
        let Some(name) = ActionBinding::key_name(b.key) else {
            return Err(format!(
                "{:#04x} is not one of the spare keys F13–F24",
                b.key
            ));
        };
        if seen.contains(&b.key) {
            return Err(format!("{name} has two actions"));
        }
        seen.push(b.key);
        if b.label.trim().is_empty() || b.label.chars().count() > 64 {
            return Err(format!(
                "the action on {name} needs a name of 1–64 characters"
            ));
        }
        match &b.action {
            Action::Run { argv } if argv.first().is_none_or(|p| p.trim().is_empty()) => {
                return Err(format!("the action on {name} needs a program to run"));
            }
            Action::Type { text } if text.is_empty() || text.chars().count() > 1000 => {
                return Err(format!("the text on {name} needs 1–1000 characters"));
            }
            Action::ProfileApply { name: p } if p.trim().is_empty() => {
                return Err(format!("the action on {name} needs a profile"));
            }
            Action::Media { usage: 0 } => {
                return Err(format!("the media key on {name} needs a usage"));
            }
            _ => {}
        }
    }
    Ok(())
}

/// The actions as the daemon runs them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionsState {
    pub bindings: Vec<ActionBinding>,
    /// The desktop's shortcut session: `None` while not set up, else what it said.
    pub shortcuts: Option<String>,
    /// Whether text can be typed on this desktop (`Type` actions).
    pub typing: bool,
    /// Why the last action or change didn't go through.
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(key: u8) -> ActionBinding {
        ActionBinding {
            key,
            label: "Terminal".into(),
            action: Action::Run {
                argv: vec!["kitty".into()],
            },
        }
    }

    #[test]
    fn spare_keys_are_f13_to_f24() {
        assert_eq!(ActionBinding::key_name(0x68).as_deref(), Some("F13"));
        assert_eq!(ActionBinding::key_name(0x73).as_deref(), Some("F24"));
        assert_eq!(ActionBinding::key_name(0x67), None);
        assert_eq!(ActionBinding::key_name(0x74), None);
    }

    #[test]
    fn bad_lists_are_refused() {
        validate(&[run(0x68), run(0x69)]).unwrap();
        assert!(validate(&[run(0x68), run(0x68)]).is_err());
        assert!(validate(&[run(0x45)]).is_err());
        let mut empty = run(0x68);
        empty.action = Action::Run { argv: vec![] };
        assert!(validate(&[empty]).is_err());
        let mut text = run(0x68);
        text.action = Action::Type {
            text: String::new(),
        };
        assert!(validate(&[text]).is_err());
        let mut unnamed = run(0x68);
        unnamed.label = " ".into();
        assert!(validate(&[unnamed]).is_err());
    }

    #[test]
    fn actions_have_stable_json() {
        let b = run(0x68);
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains(r#""kind":"run""#), "{json}");
        assert_eq!(serde_json::from_str::<ActionBinding>(&json).unwrap(), b);
    }
}
