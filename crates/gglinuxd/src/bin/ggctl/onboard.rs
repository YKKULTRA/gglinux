// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl onboard`: the keyboard's onboard profile slots, numbered 1 to 5 as the keyboard
//! counts them. Reading backs each slot up; a write backs the slot up first and puts it
//! back if anything goes wrong. `restore N <file> --damaged` puts a backup into a slot that
//! doesn't check out any more, after its own question. All of it needs `keyboard_onboard`
//! on.

use std::io::{BufRead, Write};

use anyhow::{bail, ensure, Context, Result};
use gg_ipc::{OnboardSlot, OnboardSource, OnboardState, Request, ONBOARD_SLOTS};

use crate::Action;

const USAGE: &str = "usage: ggctl onboard [read [slot] | load <slot> | write <slot> current|<backup file> | restore <slot> <backup file> [--damaged]] (slots 1 to 5)";

pub fn action(rest: &[&str]) -> Result<Action> {
    let request = request(rest)?;
    if let Request::WriteOnboard {
        slot,
        from: OnboardSource::Recover { path },
    } = &request
    {
        confirm(
            *slot,
            path,
            &mut std::io::stdin().lock(),
            &mut std::io::stdout(),
        )?;
    }
    Ok(Action::Send(request))
}

fn request(rest: &[&str]) -> Result<Request> {
    let damaged = rest.contains(&"--damaged");
    let words: Vec<&str> = rest.iter().copied().filter(|&w| w != "--damaged").collect();
    if damaged {
        let ["restore", n, file] = words.as_slice() else {
            bail!("--damaged goes only with restore: ggctl onboard restore <slot> <backup file> --damaged");
        };
        return Ok(Request::WriteOnboard {
            slot: slot(n)?,
            from: OnboardSource::Recover {
                path: absolute(file)?,
            },
        });
    }
    Ok(match words.as_slice() {
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

/// Ask before a backup goes into a damaged slot: it's sent only after a yes.
fn confirm(slot: u8, path: &str, input: &mut impl BufRead, out: &mut impl Write) -> Result<()> {
    let n = u16::from(slot) + 1;
    writeln!(
        out,
        "This is for slot {n} only if it doesn't check out any more (`ggctl onboard` shows NO),"
    )?;
    writeln!(
        out,
        "for example after a write to it was cut short. The daemon saves what the slot holds"
    )?;
    writeln!(
        out,
        "as it is (a -damaged.bin file next to the backups), then erases slot {n} and writes"
    )?;
    writeln!(out, "  {path}")?;
    writeln!(
        out,
        "into it. Leave the keyboard plugged in and the daemon running until ggctl answers"
    )?;
    write!(out, "(up to a minute). Type yes to go on: ")?;
    out.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    ensure!(
        answer.trim().eq_ignore_ascii_case("yes"),
        "nothing was sent: slot {n} is as it was"
    );
    Ok(())
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
    for s in state.slots.iter().filter(|s| s.valid == Some(false)) {
        let n = u16::from(s.index) + 1;
        let file = s.backup.as_deref().unwrap_or("<backup file>");
        out.push(format!(
            "slot {n} doesn't check out. If a write to it was cut short, put its backup back with: ggctl onboard restore {n} {file} --damaged"
        ));
    }
    if state.busy {
        out.push("busy: a read or write is under way".into());
    }
    if let Some(e) = &state.error {
        out.push(format!("last error: {e}"));
    }
    if let Some(note) = &state.note {
        out.push(format!("note: {note}"));
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
            note: None,
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

    #[test]
    fn a_damaged_slot_takes_its_backup_only_with_damaged_and_a_yes() {
        match parse("restore 5 /tmp/slot5.bin --damaged").unwrap() {
            Request::WriteOnboard {
                slot: 4,
                from: OnboardSource::Recover { path },
            } => assert_eq!(path, "/tmp/slot5.bin"),
            other => panic!("{other:?}"),
        }
        // Anywhere on the line, as ggctl's flags go.
        assert!(matches!(
            parse("--damaged restore 5 /tmp/slot5.bin").unwrap(),
            Request::WriteOnboard {
                from: OnboardSource::Recover { .. },
                ..
            }
        ));
        for line in [
            "write 5 /tmp/x.bin --damaged",
            "write 5 current --damaged",
            "read --damaged",
            "restore 5 --damaged",
            "load 5 --damaged",
        ] {
            assert!(parse(line).is_err(), "{line}");
        }
        // Its own question first: only a yes sends it.
        let mut out = Vec::new();
        confirm(4, "/tmp/slot5.bin", &mut "yes\n".as_bytes(), &mut out).unwrap();
        let asked = String::from_utf8(out).unwrap();
        assert!(
            asked.contains("slot 5") && asked.contains("/tmp/slot5.bin") && asked.contains("yes"),
            "{asked}"
        );
        for answer in ["", "\n", "y\n", "no\n", "yes please\n"] {
            let e =
                confirm(4, "/tmp/slot5.bin", &mut answer.as_bytes(), &mut Vec::new()).unwrap_err();
            assert!(
                e.to_string().contains("nothing was sent"),
                "{answer:?}: {e}"
            );
        }
    }

    #[test]
    fn a_slot_that_doesnt_check_out_prints_the_command_that_puts_its_backup_back() {
        let state = OnboardState {
            slots: vec![OnboardSlot {
                index: 2,
                valid: Some(false),
                backup: Some("/b/slot3-x.bin".into()),
                ..OnboardSlot::default()
            }],
            note: Some("the keyboard's own check of the slot answered 0x01".into()),
            ..OnboardState::default()
        };
        let out = lines(&state);
        assert!(
            out.iter()
                .any(|l| l.contains("ggctl onboard restore 3 /b/slot3-x.bin --damaged")),
            "{out:?}"
        );
        assert!(
            out.iter()
                .any(|l| l == "note: the keyboard's own check of the slot answered 0x01"),
            "{out:?}"
        );
    }
}
