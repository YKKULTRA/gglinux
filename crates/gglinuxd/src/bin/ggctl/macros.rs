// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl macro`: keyboard macros, played by the keyboard itself from an onboard slot.
//!
//! ```text
//! ggctl macro                        # the macros, the keys that play them, the slot last written
//! ggctl macro set NAME <steps>       # make or change one: a ctrl+c down:shift up:shift wait:100
//!                                    #   click:1 'text:"hello"'
//! ggctl macro rm NAME                # remove one, and its keys
//! ggctl macro bind KEY NAME [once|hold|times N|toggle] [delay MS]
//!                                    # KEY plays it once per press (the default), again and
//!                                    #   again while held, N times per press, or again and
//!                                    #   again until pressed again; MS between plays (times
//!                                    #   and toggle; GG's 15 if left out)
//! ggctl macro unbind KEY
//! ggctl macro write N                # write them into slot N (1 to 5), a backup of it first
//! ```
//!
//! A change reads the macros from the daemon, edits them and sends them back, after the
//! words have been checked (so a typo fails the same either way). It's saved; nothing
//! reaches the keyboard until `write`. Key names are the ones `ggctl bind` takes.

use anyhow::{anyhow, bail, Context, Result};
use gg_ipc::{Macro, MacroBinding, MacroLibrary, MacroMode, MacrosState, Request, ResponseData};

const USAGE: &str = "usage: ggctl macro [set NAME <steps> | rm NAME | bind KEY NAME [once|hold|times N|toggle] [delay MS] | unbind KEY | write SLOT] (steps: a ctrl+c down:shift up:shift wait:100 click:1 'text:\"hi\"'; slots 1 to 5)";

/// What `ggctl macro ...` asks for, before anything is read from the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    List,
    /// Write the macros into this slot (0 to 4).
    Write(u8),
    Edit(Edit),
}

/// A change to the macros.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Make a macro, or change the steps of the one with its name.
    Set(Macro),
    Remove(String),
    /// A key plays a macro, in a play mode, with its numbers.
    Bind(MacroBinding),
    Unbind(u8),
}

/// `ggctl macro`'s words, read as a command.
pub fn command(words: &[&str]) -> Result<Command> {
    Ok(match words {
        [] => Command::List,
        ["write", n] => Command::Write(slot(n)?),
        ["set", name, steps @ ..] if !steps.is_empty() => {
            let events = Macro::parse_steps(&steps.join(" ")).map_err(|e| anyhow!(e))?;
            Command::Edit(Edit::Set(Macro {
                name: (*name).to_owned(),
                events,
            }))
        }
        ["rm" | "remove", name] => Command::Edit(Edit::Remove((*name).to_owned())),
        ["bind", key, name, play @ ..] => {
            Command::Edit(Edit::Bind(binding(key_of(key)?, name, play)?))
        }
        ["unbind", key] => Command::Edit(Edit::Unbind(key_of(key)?)),
        _ => bail!("{USAGE}"),
    })
}

/// A key by name, on the normal layer only.
fn key_of(word: &str) -> Result<u8> {
    if word.contains('+') {
        bail!(
            "a macro goes on a key of its own: one on the Fn or deep-press layer isn't tried yet"
        );
    }
    MacroBinding::key_by_name(word)
        .with_context(|| format!("'{word}' is not a key name (try: p, f1, capslock, 0x13)"))
}

/// How key `code` plays macro `name`, from the words after the name:
/// `[once|hold|times N|toggle] [delay MS]`. Once per press with GG's numbers if none.
fn binding(code: u8, name: &str, words: &[&str]) -> Result<MacroBinding> {
    let mut b = MacroBinding::new(code, name, MacroMode::Once);
    let mut rest = words;
    if let Some((&first, after)) = words.split_first() {
        let mode = match first {
            "once" => Some(MacroMode::Once),
            "hold" => Some(MacroMode::WhileHeld),
            "times" => Some(MacroMode::Times),
            "toggle" => Some(MacroMode::Toggle),
            "delay" => None,
            other => bail!(
                "'{other}' is not a way to play a macro: once (the default), hold, times N or toggle"
            ),
        };
        if let Some(mode) = mode {
            b.set_mode(mode);
            rest = after;
        }
        if mode == Some(MacroMode::Times) {
            let (n, after) = rest
                .split_first()
                .context("times needs how many: times 3 (1 to 255)")?;
            b.times = n
                .parse()
                .ok()
                .filter(|&n| n >= 1)
                .with_context(|| format!("'{n}' is not a number of times (1 to 255)"))?;
            rest = after;
        }
    }
    match rest {
        [] => {}
        ["delay", ms] if b.mode.waits() => {
            b.repeat_delay_ms = ms
                .trim_end_matches("ms")
                .parse()
                .ok()
                .filter(|&ms| ms >= 1)
                .with_context(|| format!("'{ms}' is not a wait (1 to 65535 ms)"))?;
        }
        ["delay", ..] if !b.mode.waits() => bail!(
            "a wait between plays goes with times and toggle only: GG's saves show {} ms for the others",
            MacroBinding::DEFAULT_DELAY_MS
        ),
        _ => bail!("{USAGE}"),
    }
    Ok(b)
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

impl Edit {
    /// Make the change in `library`; the daemon checks the rest.
    pub fn apply(&self, library: &mut MacroLibrary) -> Result<()> {
        match self {
            Edit::Set(new) => match library.macros.iter_mut().find(|m| m.name == new.name) {
                Some(m) => m.events.clone_from(&new.events),
                None => library.macros.push(new.clone()),
            },
            Edit::Remove(name) => {
                let before = library.macros.len();
                library.macros.retain(|m| &m.name != name);
                if library.macros.len() == before {
                    bail!("there's no macro called '{name}'");
                }
                library.bindings.retain(|b| &b.name != name);
            }
            Edit::Bind(binding) => {
                let name = &binding.name;
                if library.find(name).is_none() {
                    bail!("there's no macro called '{name}': make it first with `ggctl macro set {name} <steps>`");
                }
                library.bindings.retain(|b| b.code != binding.code);
                library.bindings.push(binding.clone());
            }
            Edit::Unbind(code) => {
                let before = library.bindings.len();
                library.bindings.retain(|b| b.code != *code);
                if library.bindings.len() == before {
                    bail!("key {} plays no macro", MacroBinding::key_name(*code));
                }
            }
        }
        Ok(())
    }
}

/// The request `ggctl macro ...` sends. A change reads the macros first.
pub fn request(words: &[&str]) -> Result<Request> {
    Ok(match command(words)? {
        Command::List => Request::GetMacros,
        Command::Write(slot) => Request::WriteMacros { slot },
        Command::Edit(edit) => {
            // A daemon may answer the first read after a change with the state from
            // before it, as a device does (the fake daemon does): the second read is the
            // one edited, so a change made just before isn't lost.
            let read = || -> Result<MacrosState> {
                match crate::send(crate::connect()?, Request::GetMacros)? {
                    ResponseData::Macros { state } => Ok(state),
                    _ => bail!("the daemon did not send its macros (is it older than ggctl?)"),
                }
            };
            read()?;
            let mut library = read()?.library;
            edit.apply(&mut library)?;
            Request::SetMacros { library }
        }
    })
}

/// What `ggctl macro` prints.
pub fn print(state: &MacrosState) {
    for line in lines(state) {
        println!("{line}");
    }
}

fn lines(state: &MacrosState) -> Vec<String> {
    let lib = &state.library;
    let mut out = Vec::new();
    if lib.macros.is_empty() {
        out.push("macros: none yet (ggctl macro set NAME <steps>)".to_owned());
    } else {
        out.push("macros:".to_owned());
        for m in &lib.macros {
            out.push(format!("  {}: {}", m.name, Macro::steps_text(&m.events)));
        }
    }
    if !lib.bindings.is_empty() {
        out.push("keys:".to_owned());
        for b in &lib.bindings {
            out.push(format!(
                "  {} → {} ({})",
                MacroBinding::key_name(b.code),
                b.name,
                b.play_text()
            ));
        }
    }
    out.push(match state.written_slot {
        Some(s) => format!("last written into slot {}", u16::from(s) + 1),
        None => "not written into a slot yet (ggctl macro write N)".to_owned(),
    });
    if state.busy {
        out.push("writing into the slot now…".to_owned());
    }
    if let Some(path) = &state.backup {
        out.push(format!("backup of the slot from before: {path}"));
    }
    if let Some(note) = &state.note {
        out.push(format!("note: {note}"));
    }
    if let Some(e) = &state.error {
        out.push(format!("the last write didn't go through: {e}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bind::{self, Bind, BindEdit};
    use gg_ipc::{KeyAction, MacroEvent};

    fn edit(line: &str) -> Edit {
        let words: Vec<&str> = line.split_whitespace().collect();
        match command(&words).unwrap() {
            Command::Edit(e) => e,
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn the_words_make_their_commands() {
        assert_eq!(command(&[]).unwrap(), Command::List);
        assert_eq!(command(&["write", "5"]).unwrap(), Command::Write(4));
        assert_eq!(
            edit("bind p copy hold"),
            Edit::Bind(MacroBinding::new(0x13, "copy", MacroMode::WhileHeld))
        );
        assert_eq!(
            edit("bind capslock copy"),
            Edit::Bind(MacroBinding::new(0x39, "copy", MacroMode::Once))
        );
        assert_eq!(edit("unbind menu"), Edit::Unbind(0xE7));
        assert_eq!(edit("rm copy"), Edit::Remove("copy".into()));
        match edit("set copy ctrl+c wait:20 text:hi") {
            Edit::Set(m) => {
                assert_eq!(m.name, "copy");
                assert_eq!(Macro::steps_text(&m.events), "ctrl+c wait:20 h i");
            }
            other => panic!("{other:?}"),
        }
        // As the shell hands over a quoted text: one word with its spaces.
        let quoted = command(&["set", "hi", "text:\"a b\"", "enter"]).unwrap();
        let Command::Edit(Edit::Set(m)) = quoted else {
            panic!("{quoted:?}")
        };
        assert_eq!(Macro::steps_text(&m.events), "a space b enter");
    }

    #[test]
    fn mistakes_are_errors_that_say_why() {
        for (line, why) in [
            ("write 6", "slots 1 to 5"),
            ("write x", "not a slot number"),
            ("set copy", "usage"),
            ("set copy click:2", "left mouse button"),
            ("bind fn+p copy", "Fn or deep-press"),
            ("bind nokey copy", "not a key name"),
            ("bind p copy twice", "hold"),
            ("bind p copy times", "how many"),
            ("bind p copy times 0", "1 to 255"),
            ("bind p copy times 256", "1 to 255"),
            ("bind p copy toggle delay 0", "1 to 65535"),
            ("bind p copy toggle delay soon", "1 to 65535"),
            ("bind p copy delay 100", "times and toggle only"),
            ("bind p copy hold delay 100", "times and toggle only"),
            ("bind p copy times 3 delay", "usage"),
            ("bind p copy toggle 3", "usage"),
            ("frobnicate", "usage"),
        ] {
            let words: Vec<&str> = line.split_whitespace().collect();
            let e = format!("{:#}", command(&words).unwrap_err());
            assert!(e.contains(why), "{line}: {e}");
        }
    }

    /// GG's apex-30: P three times, 100 ms apart; O toggled, 200 ms apart.
    #[test]
    fn the_play_modes_take_their_numbers() {
        let Edit::Bind(p) = edit("bind p copy times 3 delay 100") else {
            panic!()
        };
        assert_eq!(
            (p.mode, p.times, p.repeat_delay_ms),
            (MacroMode::Times, 3, 100)
        );
        let Edit::Bind(o) = edit("bind o copy toggle delay 200ms") else {
            panic!()
        };
        assert_eq!(
            (o.mode, o.times, o.repeat_delay_ms),
            (MacroMode::Toggle, 1, 200)
        );
        // Without a delay, GG's 15 ms; with once and hold, GG's numbers.
        let Edit::Bind(t) = edit("bind p copy times 255") else {
            panic!()
        };
        assert_eq!((t.times, t.repeat_delay_ms), (255, 15));
        assert_eq!(
            edit("bind p copy once"),
            Edit::Bind(MacroBinding::new(0x13, "copy", MacroMode::Once))
        );
        let mut lib = MacroLibrary::default();
        edit("set copy ctrl+c").apply(&mut lib).unwrap();
        edit("bind p copy times 3 delay 100")
            .apply(&mut lib)
            .unwrap();
        edit("bind o copy toggle delay 200")
            .apply(&mut lib)
            .unwrap();
        lib.validate().unwrap();
        let state = MacrosState {
            library: lib,
            ..MacrosState::default()
        };
        assert_eq!(
            lines(&state)[3..5],
            [
                "  p → copy (3 times per press, 100 ms apart)",
                "  o → copy (again and again until pressed again, 200 ms apart)"
            ]
        );
    }

    #[test]
    fn edits_change_the_library_as_asked() {
        let mut lib = MacroLibrary::default();
        edit("set copy ctrl+c").apply(&mut lib).unwrap();
        edit("bind p copy").apply(&mut lib).unwrap();
        edit("bind p copy hold").apply(&mut lib).unwrap();
        assert_eq!(lib.bindings.len(), 1, "a key plays one macro");
        assert_eq!(lib.bindings[0].mode, MacroMode::WhileHeld);
        edit("set copy ctrl+insert").apply(&mut lib).unwrap();
        assert_eq!(lib.macros.len(), 1, "changed, not added");
        assert!(edit("bind o paste").apply(&mut lib).is_err());
        assert!(edit("unbind o").apply(&mut lib).is_err());
        assert!(edit("rm paste").apply(&mut lib).is_err());
        edit("rm copy").apply(&mut lib).unwrap();
        assert_eq!(lib, MacroLibrary::default(), "its key goes with it");
    }

    #[test]
    fn the_list_says_what_each_key_plays_and_where_it_was_written() {
        let state = MacrosState {
            library: MacroLibrary {
                macros: vec![Macro {
                    name: "copy".into(),
                    events: Macro::parse_steps("ctrl+c").unwrap(),
                }],
                bindings: vec![MacroBinding::new(0x13, "copy", MacroMode::Once)],
            },
            written_slot: Some(4),
            note: Some("load it".into()),
            ..MacrosState::default()
        };
        assert_eq!(
            lines(&state),
            [
                "macros:",
                "  copy: ctrl+c",
                "keys:",
                "  p → copy (once per press)",
                "last written into slot 5",
                "note: load it",
            ]
        );
        assert_eq!(
            lines(&MacrosState::default())[0],
            "macros: none yet (ggctl macro set NAME <steps>)"
        );
    }

    /// The names macros take are `ggctl bind`'s, for every usage either knows.
    #[test]
    fn key_names_are_the_same_as_ggctl_binds() {
        for code in 0u8..=0xFF {
            let name = MacroEvent::usage_name(code);
            assert_eq!(
                bind::describe(&KeyAction::Keys { codes: vec![code] }),
                name,
                "{code:#04x}"
            );
            let bound = bind::parse(&["a", &name]).unwrap();
            assert_eq!(
                bound,
                Bind::Edit(BindEdit::Key {
                    layer: gg_ipc::Layer::Normal,
                    code: 0x04,
                    action: KeyAction::Keys { codes: vec![code] },
                }),
                "{name}"
            );
        }
        for alias in [
            "caps", "escape", "return", "del", "ins", "pgup", "pgdn", "prtsc", "control", "lctrl",
            "lshift", "lalt", "win", "lsuper", "altgr", "backtick", "menu", "fn",
        ] {
            let Bind::Edit(BindEdit::Key { code, .. }) = bind::parse(&[alias, "default"]).unwrap()
            else {
                panic!("{alias}")
            };
            assert_eq!(MacroBinding::key_by_name(alias), Some(code), "{alias}");
        }
    }
}
