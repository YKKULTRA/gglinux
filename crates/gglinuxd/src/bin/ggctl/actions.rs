// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl actions`: actions on the spare keys F13–F24. A mouse button bound to one of
//! those keys (in the mouse's settings) then runs the action.
//!
//! ```text
//! ggctl actions                         # the list, and what the desktop bound
//! ggctl actions set f13 run kitty       # start a program (no shell; "kitty --single-instance" in quotes)
//! ggctl actions set f14 type "hello"    # type text (Hyprland and other wlroots desktops, with wtype)
//! ggctl actions set f15 profile Night   # switch to a saved profile
//! ggctl actions set f16 clip            # save the instant replay
//! ggctl actions set f17 media playpause # playpause, next, previous, stop, mute, volup, voldown
//! ggctl actions rm f13
//! ```
//!
//! A change is read, edited and written back whole ([`Edit::apply`]), as `parts` and
//! `experimental` are.

use anyhow::{bail, Context, Result};
use gg_ipc::{Action as Do, ActionBinding, ActionsState};

/// What `ggctl actions …` asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Print the actions.
    List,
    /// Read the actions, change one, and send them back.
    Edit(Edit),
}

/// One change to the list.
#[derive(Debug, PartialEq, Eq)]
pub enum Edit {
    /// Give a key this action, in place of the one it has.
    Set(ActionBinding),
    /// Take a key's action away.
    Remove(u8),
}

/// The media keys by the names `ggctl` takes, with their consumer-page usage.
const MEDIA: [(&str, u16, &str); 7] = [
    ("playpause", 0xCD, "Play/pause"),
    ("next", 0xB5, "Next track"),
    ("previous", 0xB6, "Previous track"),
    ("stop", 0xB7, "Stop"),
    ("mute", 0xE2, "Mute"),
    ("volup", 0xE9, "Volume up"),
    ("voldown", 0xEA, "Volume down"),
];

const USAGE: &str =
    "usage: ggctl actions [set f13..f24 run|type|profile|clip|media … | rm f13..f24]";

pub fn command(rest: &[&str]) -> Result<Command> {
    Ok(match rest {
        [] => Command::List,
        ["rm", key] => Command::Edit(Edit::Remove(spare_key(key)?)),
        ["set", key, kind, what @ ..] => {
            Command::Edit(Edit::Set(binding(spare_key(key)?, kind, what)?))
        }
        _ => bail!("{USAGE}"),
    })
}

/// `f13` … `f24`, in any case.
fn spare_key(word: &str) -> Result<u8> {
    (0x68..=0x73)
        .find(|&k| ActionBinding::key_name(k).is_some_and(|n| n.eq_ignore_ascii_case(word)))
        .with_context(|| format!("'{word}' is not one of the spare keys f13 to f24"))
}

fn binding(key: u8, kind: &str, what: &[&str]) -> Result<ActionBinding> {
    let joined = what.join(" ");
    let (label, action) = match (kind, what) {
        ("run", [_, ..]) => {
            // Each word is an argument as it is; a quoted word with spaces is split on them,
            // so a program's own --options can be given in quotes. Nothing goes to a shell.
            let argv: Vec<String> = what
                .iter()
                .flat_map(|w| w.split_whitespace())
                .map(String::from)
                .collect();
            let program = argv.first().context("run what?")?;
            let label = program.rsplit('/').next().unwrap_or(program).to_string();
            (label, Do::Run { argv })
        }
        ("type", [_, ..]) => ("Type text".to_string(), Do::Type { text: joined }),
        ("profile", [_, ..]) => (joined.clone(), Do::ProfileApply { name: joined }),
        ("clip", []) => ("Save clip".to_string(), Do::ClipSave),
        ("media", [name]) => {
            let (_, usage, label) =
                MEDIA.iter().find(|(n, _, _)| n == name).with_context(|| {
                    let names: Vec<&str> = MEDIA.iter().map(|(n, _, _)| *n).collect();
                    format!("no media key '{name}' (try: {})", names.join(", "))
                })?;
            (label.to_string(), Do::Media { usage: *usage })
        }
        _ => bail!("{USAGE}"),
    };
    let label: String = label.chars().take(64).collect();
    Ok(ActionBinding { key, label, action })
}

impl Edit {
    /// Change `list` as asked, checked as the daemon checks it.
    pub fn apply(self, list: &mut Vec<ActionBinding>) -> Result<()> {
        match self {
            Edit::Set(b) => match list.iter_mut().find(|old| old.key == b.key) {
                Some(old) => *old = b,
                None => list.push(b),
            },
            Edit::Remove(key) => {
                let before = list.len();
                list.retain(|b| b.key != key);
                if list.len() == before {
                    bail!(
                        "{} has no action",
                        ActionBinding::key_name(key).unwrap_or_default()
                    );
                }
            }
        }
        gg_ipc::validate_actions(list).map_err(anyhow::Error::msg)
    }
}

/// After a change: read the actions back until the daemon shows `sent`, three reads at
/// most, 200 ms apart. A daemon may answer the first read after a change with the list
/// from before it, and the next `ggctl actions set` must not start from that list.
pub fn read_back(
    sent: &[ActionBinding],
    mut read: impl FnMut() -> Result<ActionsState>,
) -> Result<bool> {
    for n in 0..3 {
        if n > 0 {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if read()?.bindings == sent {
            return Ok(true);
        }
    }
    Ok(false)
}

/// What `ggctl actions` prints.
pub fn print(state: &ActionsState) {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let hyprland = desktop.to_ascii_lowercase().contains("hyprland")
        || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some();
    for line in lines(state, hyprland) {
        println!("{line}");
    }
}

fn lines(state: &ActionsState, hyprland: bool) -> Vec<String> {
    let mut out: Vec<String> = state
        .bindings
        .iter()
        .map(|b| {
            let name = ActionBinding::key_name(b.key).unwrap_or_default();
            format!("{name:<4} {:<20} {}", b.label, what(&b.action))
        })
        .collect();
    if out.is_empty() {
        out.push("no actions yet (ggctl actions set f13 run kitty)".into());
    }
    out.push(format!(
        "shortcuts: {}",
        state.shortcuts.as_deref().unwrap_or("not set up")
    ));
    out.push(format!(
        "typing: {}",
        if state.typing {
            "works here"
        } else {
            "not on this desktop (Hyprland and other wlroots desktops, with wtype)"
        }
    ));
    if let Some(e) = &state.error {
        out.push(format!("error: {e}"));
    }
    if hyprland && !state.bindings.is_empty() {
        out.push("Hyprland binds them only from its config. In hyprland.lua:".into());
        out.extend(
            state
                .bindings
                .iter()
                .map(|b| format!("  {}", lua_line(b.key))),
        );
        out.push("or in hyprland.conf:".into());
        out.extend(
            state
                .bindings
                .iter()
                .map(|b| format!("  {}", conf_line(b.key))),
        );
    }
    out
}

/// An action in a few words.
fn what(action: &Do) -> String {
    match action {
        Do::Run { argv } => format!("run {}", argv.join(" ")),
        Do::Type { text } => format!("type {text:?}"),
        Do::ProfileApply { name } => format!("profile {name}"),
        Do::ClipSave => "clip".into(),
        Do::Media { usage } => match MEDIA.iter().find(|(_, u, _)| u == usage) {
            Some((name, _, _)) => format!("media {name}"),
            None => format!("media {usage:#06x}"),
        },
    }
}

/// The global shortcut's name in Hyprland's `global` dispatcher.
fn global(key: u8) -> String {
    let name = ActionBinding::key_name(key).unwrap_or_default();
    format!("io.github.ykk.gglinux:action-{}", name.to_ascii_lowercase())
}

fn lua_line(key: u8) -> String {
    let name = ActionBinding::key_name(key).unwrap_or_default();
    format!("hl.bind(\"{name}\", hl.dsp.global(\"{}\"))", global(key))
}

fn conf_line(key: u8) -> String {
    let name = ActionBinding::key_name(key).unwrap_or_default();
    format!("bind = , {name}, global, {}", global(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(line: &str) -> Edit {
        let words: Vec<&str> = line.split_whitespace().collect();
        match command(&words).unwrap() {
            Command::Edit(e) => e,
            Command::List => panic!("{line} is a list"),
        }
    }

    fn set(line: &str) -> ActionBinding {
        match edit(line) {
            Edit::Set(b) => b,
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn each_kind_of_action_is_set_on_its_key() {
        assert_eq!(command(&[]).unwrap(), Command::List);
        let b = set("set f13 run kitty");
        assert_eq!((b.key, b.label.as_str()), (0x68, "kitty"));
        assert_eq!(
            b.action,
            Do::Run {
                argv: vec!["kitty".into()]
            }
        );
        assert_eq!(
            set("set F14 type hello there").action,
            Do::Type {
                text: "hello there".into()
            }
        );
        assert_eq!(
            set("set f15 profile Night").action,
            Do::ProfileApply {
                name: "Night".into()
            }
        );
        assert_eq!(set("set f16 clip").action, Do::ClipSave);
        assert_eq!(
            set("set f17 media playpause").action,
            Do::Media { usage: 0xCD }
        );
        assert_eq!(
            set("set f24 media voldown").action,
            Do::Media { usage: 0xEA }
        );
        assert_eq!(edit("rm f13"), Edit::Remove(0x68));
    }

    #[test]
    fn a_program_and_its_arguments_reach_no_shell() {
        // As the shell hands them over: a quoted word stays one word unless it has spaces.
        let words = [
            "set",
            "f13",
            "run",
            "/usr/bin/kitty --single-instance",
            ";",
            "rm",
        ];
        let Command::Edit(Edit::Set(b)) = command(&words).unwrap() else {
            panic!("not a set");
        };
        assert_eq!(
            b.action,
            Do::Run {
                argv: ["/usr/bin/kitty", "--single-instance", ";", "rm"]
                    .map(String::from)
                    .to_vec()
            }
        );
        assert_eq!(b.label, "kitty");
    }

    #[test]
    fn mistakes_are_errors_before_anything_is_sent() {
        for bad in [
            "set f12 run kitty",
            "set f25 clip",
            "set f13",
            "set f13 run",
            "set f13 type",
            "set f13 clip now",
            "set f13 media louder",
            "set f13 dance",
            "rm",
            "rm f99",
            "list",
        ] {
            let words: Vec<&str> = bad.split_whitespace().collect();
            assert!(command(&words).is_err(), "{bad}");
        }
        let e = command(&["set", "f13", "media", "louder"]).unwrap_err();
        assert!(e.to_string().contains("playpause, next"), "{e}");
    }

    #[test]
    fn an_edit_replaces_adds_or_removes_one_key() {
        let mut list = Vec::new();
        set_on(&mut list, "set f13 run kitty");
        set_on(&mut list, "set f14 clip");
        set_on(&mut list, "set f13 run foot");
        assert_eq!(list.len(), 2);
        assert_eq!(
            list[0].action,
            Do::Run {
                argv: vec!["foot".into()]
            }
        );
        edit("rm f14").apply(&mut list).unwrap();
        assert_eq!(list.len(), 1);
        let e = edit("rm f14").apply(&mut list).unwrap_err();
        assert_eq!(e.to_string(), "F14 has no action");
    }

    #[test]
    fn a_change_is_read_back_past_a_stale_answer() {
        let mut list = Vec::new();
        set_on(&mut list, "set f13 run kitty");
        let stale = ActionsState::default();
        let fresh = ActionsState {
            bindings: list.clone(),
            ..ActionsState::default()
        };
        let mut answers = vec![fresh, stale.clone()];
        let mut reads = 0;
        let seen = read_back(&list, || {
            reads += 1;
            Ok(answers.pop().unwrap())
        });
        assert!(seen.unwrap());
        assert_eq!(reads, 2, "the stale one, then the change");
        let mut reads = 0;
        let never = read_back(&list, || {
            reads += 1;
            Ok(stale.clone())
        });
        assert!(!never.unwrap());
        assert_eq!(reads, 3);
    }

    fn set_on(list: &mut Vec<ActionBinding>, line: &str) {
        edit(line).apply(list).unwrap();
    }

    #[test]
    fn the_list_shows_each_key_what_the_desktop_said_and_the_hyprland_lines() {
        let mut list = Vec::new();
        set_on(&mut list, "set f13 run kitty");
        set_on(&mut list, "set f17 media playpause");
        let state = ActionsState {
            bindings: list,
            shortcuts: Some("bound (fake)".into()),
            typing: false,
            error: None,
        };
        let lines = lines(&state, true);
        assert_eq!(lines[0], "F13  kitty                run kitty");
        assert_eq!(lines[1], "F17  Play/pause           media playpause");
        assert_eq!(lines[2], "shortcuts: bound (fake)");
        assert!(lines[3].starts_with("typing: not on this desktop"));
        assert!(lines.contains(
            &r#"  hl.bind("F13", hl.dsp.global("io.github.ykk.gglinux:action-f13"))"#.to_string()
        ));
        assert!(
            lines.contains(&"  bind = , F17, global, io.github.ykk.gglinux:action-f17".to_string())
        );
        let elsewhere = super::lines(&state, false);
        assert!(!elsewhere.iter().any(|l| l.contains("hl.bind")));
        let empty = super::lines(&ActionsState::default(), true);
        assert_eq!(empty[0], "no actions yet (ggctl actions set f13 run kitty)");
        assert_eq!(empty[1], "shortcuts: not set up");
    }
}
