// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl onboard`: the keyboard's onboard profile slots, numbered 1 to 5 as the keyboard
//! counts them. Reading backs each slot up; a write backs the slot up first and puts it
//! back if anything goes wrong. All of it needs `keyboard_onboard` on.

use anyhow::{bail, Context, Result};
use gg_ipc::{OnboardSlot, OnboardSource, OnboardState, Request, ONBOARD_SLOTS};

use crate::Action;

const USAGE: &str = "usage: ggctl onboard [read [slot] | load <slot> | write <slot> current|<backup file> | restore <slot> <backup file>] (slots 1 to 5)";

pub fn action(rest: &[&str]) -> Result<Action> {
    Ok(Action::Send(request(rest)?))
}

fn request(rest: &[&str]) -> Result<Request> {
    Ok(match rest {
        [] => Request::GetOnboard,
        ["read"] => Request::ReadOnboard { slot: None },
        ["read", n] => Request::ReadOnboard {
            slot: Some(slot(n)?),
        },
        ["load", n] => Request::LoadOnboard { slot: slot(n)? },
        ["write", n, "current"] => Request::WriteOnboard {
            slot: slot(n)?,
            from: OnboardSource::Current,
        },
        ["write" | "restore", n, file] => Request::WriteOnboard {
            slot: slot(n)?,
            from: OnboardSource::Backup {
                path: absolute(file)?,
            },
        },
        _ => bail!("{USAGE}"),
    })
}

/// A slot as the user counts it, 1 to 5, as the keyboard's 0 to 4.
fn slot(word: &str) -> Result<u8> {
    let n: u8 = word
        .parse()
        .with_context(|| format!("'{word}' is not a slot number (1 to 5)"))?;
    match n {
        1..=5 => Ok(n - 1),
        _ => bail!("the keyboard has slots 1 to 5, not {n}"),
    }
}

/// The daemon runs elsewhere: it gets the file's whole path.
fn absolute(file: &str) -> Result<String> {
    let path = std::path::absolute(file).with_context(|| format!("finding {file}"))?;
    Ok(path.display().to_string())
}

/// What `ggctl onboard` prints for the slots.
pub fn print(state: &OnboardState) {
    for line in lines(state) {
        println!("{line}");
    }
}

fn lines(state: &OnboardState) -> Vec<String> {
    let mut out = vec![format!(
        "{:<5} {:<24} {:<9} backup",
        "slot", "name", "checks"
    )];
    // The daemon lists none before it knows anything about them: all five, still.
    for index in 0..ONBOARD_SLOTS {
        let unknown = OnboardSlot {
            index,
            ..OnboardSlot::default()
        };
        let s = state
            .slots
            .iter()
            .find(|s| s.index == index)
            .unwrap_or(&unknown);
        let active = if state.active == Some(s.index) {
            " (running)"
        } else {
            ""
        };
        let checks = match s.valid {
            Some(true) => "yes",
            Some(false) => "NO",
            None => "not read",
        };
        out.push(format!(
            "{:<5} {:<24} {checks:<9} {}",
            format!("{}{active}", s.index + 1),
            s.name.as_deref().unwrap_or("-"),
            s.backup.as_deref().unwrap_or("none yet")
        ));
    }
    if state.busy {
        out.push("busy: a read or write is under way".into());
    }
    if let Some(e) = &state.error {
        out.push(format!("last error: {e}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<Request> {
        let words: Vec<&str> = line.split_whitespace().collect();
        request(&words)
    }

    #[test]
    fn slots_are_1_to_5_on_the_command_line_and_0_to_4_on_the_wire() {
        assert!(matches!(parse("").unwrap(), Request::GetOnboard));
        assert!(matches!(
            parse("read").unwrap(),
            Request::ReadOnboard { slot: None }
        ));
        assert!(matches!(
            parse("read 1").unwrap(),
            Request::ReadOnboard { slot: Some(0) }
        ));
        assert!(matches!(
            parse("load 5").unwrap(),
            Request::LoadOnboard { slot: 4 }
        ));
        assert!(matches!(
            parse("write 3 current").unwrap(),
            Request::WriteOnboard {
                slot: 2,
                from: OnboardSource::Current
            }
        ));
        for line in ["write 2 /tmp/slot2.bin", "restore 2 /tmp/slot2.bin"] {
            match parse(line).unwrap() {
                Request::WriteOnboard {
                    slot: 1,
                    from: OnboardSource::Backup { path },
                } => assert_eq!(path, "/tmp/slot2.bin"),
                other => panic!("{line}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_backup_file_goes_with_its_whole_path() {
        let Request::WriteOnboard {
            from: OnboardSource::Backup { path },
            ..
        } = parse("restore 1 slot1.bin").unwrap()
        else {
            panic!("not a write")
        };
        assert!(
            path.starts_with('/') && path.ends_with("/slot1.bin"),
            "{path}"
        );
    }

    #[test]
    fn mistakes_are_errors_before_connecting() {
        for line in [
            "read 0",
            "read 6",
            "load",
            "load x",
            "write 1",
            "write 9 current",
            "restore 1",
            "erase 1",
            "read 1 2",
        ] {
            assert!(parse(line).is_err(), "{line}");
        }
        assert!(parse("load 6")
            .unwrap_err()
            .to_string()
            .contains("slots 1 to 5"));
    }

    #[test]
    fn the_slots_print_as_the_user_counts_them() {
        let state = OnboardState {
            slots: vec![
                OnboardSlot {
                    index: 0,
                    name: Some("Config 1".into()),
                    valid: Some(true),
                    backup: Some("/b/slot1-x.bin".into()),
                },
                OnboardSlot {
                    index: 1,
                    ..OnboardSlot::default()
                },
            ],
            active: Some(0),
            busy: false,
            error: Some("slot 2 couldn't be read".into()),
        };
        let out = lines(&state);
        assert!(
            out[1].starts_with("1 (running)") && out[1].contains("Config 1"),
            "{out:?}"
        );
        assert!(out[1].contains("yes") && out[1].ends_with("/b/slot1-x.bin"));
        assert!(
            out[2].starts_with("2 ") && out[2].contains("not read") && out[2].ends_with("none yet")
        );
        assert!(
            out[5].starts_with("5 ") && out[5].contains("not read"),
            "{out:?}"
        );
        assert_eq!(out[6], "last error: slot 2 couldn't be read");
        assert_eq!(
            lines(&OnboardState::default()).len(),
            6,
            "a heading and five slots"
        );
        print(&state);
    }
}
