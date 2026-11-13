// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl profile`: saved profiles, gglinux's own presets, sharing them as files, and the
//! rules that switch to them.

use anyhow::{bail, Context, Result};
use gg_ipc::{
    GameRule, ProfileFile, ProfilesState, Request, ResponseData, RuleMatch, RuleWhen, Section,
};

use crate::Action;

const USAGE: &str = "usage: ggctl profile list | save all|<parts> <name> | apply <name> | \
                     preset <name> | copy <name> | copy-preset <name> | delete <name> | \
                     export <name> | export-preset <name> | import <file> [replace] | \
                     rule add focus|running program|class <match> <profile> | rule rm <n>";

/// `all`, or parts by name with commas: `lighting,switches`.
fn sections(word: &str) -> Result<Vec<Section>> {
    if word == "all" {
        return Ok(Section::ALL.to_vec());
    }
    word.split(',')
        .map(|w| {
            Section::from_key(w.trim()).with_context(|| {
                let names: Vec<&str> = Section::ALL.iter().map(|s| s.key()).collect();
                format!("'{w}' is not a part of a profile (all, or some of: {})", names.join(", "))
            })
        })
        .collect()
}

/// Names may have spaces, so `profile apply Late Night` needs no quotes.
fn name(words: &[&str]) -> Result<String> {
    if words.is_empty() {
        bail!("{USAGE}");
    }
    Ok(words.join(" "))
}

pub fn action(rest: &[&str]) -> Result<Action> {
    Ok(Action::Send(match rest {
        ["list"] => Request::GetProfiles,
        ["save", parts, n @ ..] => Request::SaveProfile {
            sections: Some(sections(parts)?),
            name: name(n)?,
        },
        ["apply", n @ ..] => Request::ApplyProfile {
            name: name(n)?,
            preset: false,
        },
        ["preset", n @ ..] => Request::ApplyProfile {
            name: name(n)?,
            preset: true,
        },
        [copy @ ("copy" | "copy-preset"), n @ ..] => Request::CopyProfile {
            from: name(n)?,
            preset: *copy == "copy-preset",
            to: None,
        },
        ["delete", n @ ..] => Request::DeleteProfile { name: name(n)? },
        [export @ ("export" | "export-preset"), n @ ..] => Request::GetProfile {
            name: name(n)?,
            preset: *export == "export-preset",
        },
        ["import", file, more @ ..] if more.is_empty() || more == ["replace"] => {
            let text = std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?;
            let shared = ProfileFile::parse(&text).map_err(anyhow::Error::msg)?;
            Request::ImportProfile {
                profile: Box::new(shared.profile),
                replace: !more.is_empty(),
            }
        }
        ["rule", "add", when, by, matched, profile @ ..] => {
            let rule = GameRule {
                process: matched.to_string(),
                profile: name(profile)?,
                by: match *by {
                    "program" => RuleMatch::Program,
                    "class" => RuleMatch::Class,
                    other => bail!("'{other}' is not program or class"),
                },
                when: match *when {
                    "focus" => RuleWhen::Focused,
                    "running" => RuleWhen::Running,
                    other => bail!("'{other}' is not focus or running"),
                },
            };
            rule.validate().map_err(anyhow::Error::msg)?;
            let mut rules = read()?.rules;
            rules.push(rule);
            Request::SetGameRules { rules }
        }
        ["rule", "rm", n] => {
            let n: usize = n.parse().with_context(|| format!("'{n}' is not a rule's number"))?;
            let mut rules = read()?.rules;
            if n == 0 || n > rules.len() {
                bail!("there's no rule {n}: `ggctl profile list` numbers them");
            }
            rules.remove(n - 1);
            Request::SetGameRules { rules }
        }
        _ => bail!("{USAGE}"),
    }))
}

fn read() -> Result<ProfilesState> {
    match crate::send(crate::connect()?, Request::GetProfiles)? {
        ResponseData::Profiles { state } => Ok(state),
        _ => bail!("the daemon did not send its profiles"),
    }
}

/// After `apply` or `preset` went through: what was skipped, if anything. The first read
/// after a change may still show the last one, so a second is made if it does.
pub fn after_success(words: &[&str]) -> Result<()> {
    let ["profile", "apply" | "preset", n @ ..] = words else {
        return Ok(());
    };
    let wanted = n.join(" ");
    for _ in 0..2 {
        if let Some(applied) = read()?.applied.filter(|a| a.profile == wanted) {
            for note in &applied.notes {
                println!("note: {note}");
            }
            return Ok(());
        }
    }
    Ok(())
}

fn joined(sections: &[Section]) -> String {
    let keys: Vec<&str> = sections.iter().map(|s| s.key()).collect();
    keys.join(", ")
}

/// What `ggctl profile list` prints.
pub fn print(state: &ProfilesState) {
    for line in lines(state) {
        println!("{line}");
    }
}

fn lines(state: &ProfilesState) -> Vec<String> {
    let mut out = vec!["Profiles:".to_string()];
    if state.profiles.is_empty() {
        out.push("  none yet".into());
    }
    for (i, name) in state.profiles.iter().enumerate() {
        let parts = state.sections.get(i).map(|s| joined(s)).unwrap_or_default();
        out.push(format!("  {name:<24} {parts}"));
    }
    out.push("Presets (gglinux's own):".into());
    for p in &state.presets {
        out.push(format!("  {:<24} {}", p.name, p.about));
    }
    out.push("Rules:".into());
    if state.rules.is_empty() {
        out.push("  none yet".into());
    }
    for (i, r) in state.rules.iter().enumerate() {
        let what = match r.by {
            RuleMatch::Program => r.process.clone(),
            RuleMatch::Class => format!("window class {}", r.process),
        };
        let when = match r.when {
            RuleWhen::Running => "while it runs",
            RuleWhen::Focused => "on focus",
        };
        out.push(format!("  {}. {what} → {} ({when})", i + 1, r.profile));
    }
    if let Some(a) = &state.active {
        out.push(format!("Now: {}, because {}", a.profile, a.reason()));
    }
    let f = &state.focus;
    if let Some(source) = &f.source {
        let window = f
            .window
            .as_ref()
            .map(|w| match &w.program {
                Some(p) => format!(" (focused: {}, {p})", w.class),
                None => format!(" (focused: {})", w.class),
            })
            .unwrap_or_default();
        out.push(format!("Focus: followed through {source}{window}"));
    }
    if let Some(note) = &f.note {
        out.push(format!("Focus: {note}"));
    }
    if !state.default_sections.is_empty() {
        out.push(format!(
            "Your own settings kept to put back: {}",
            joined(&state.default_sections)
        ));
    }
    if let Some(a) = &state.applied {
        out.push(format!("Last applied: {}", a.profile));
        for note in &a.notes {
            out.push(format!("  note: {note}"));
        }
    }
    out
}

/// What `ggctl profile export` prints: the file to share (`> name.json`).
pub fn print_file(file: &ProfileFile) {
    println!("{}", file.to_json());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sends;

    #[test]
    fn profile_names_may_contain_spaces() {
        match sends("profile apply Late Night") {
            Request::ApplyProfile { name, preset } => {
                assert_eq!((name.as_str(), preset), ("Late Night", false))
            }
            other => panic!("{other:?}"),
        }
        match sends("profile preset Competitive FPS") {
            Request::ApplyProfile { name, preset } => {
                assert_eq!((name.as_str(), preset), ("Competitive FPS", true))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn save_takes_the_parts_first() {
        match sends("profile save lighting,switches Late Night") {
            Request::SaveProfile { name, sections } => {
                assert_eq!(name, "Late Night");
                assert_eq!(sections, Some(vec![Section::Lighting, Section::Switches]));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            sends("profile save all FPS"),
            Request::SaveProfile { sections: Some(s), .. } if s.len() == 6
        ));
        let e = action(&["save", "lights", "x"]).err().unwrap().to_string();
        assert!(e.contains("headset, mouse, ") && e.contains("'lights'"), "{e}");
        assert!(action(&["save", "all"]).is_err(), "no name");
    }

    #[test]
    fn copy_export_and_delete_name_a_profile_or_a_preset() {
        assert!(matches!(
            sends("profile copy-preset Typing"),
            Request::CopyProfile { preset: true, to: None, from } if from == "Typing"
        ));
        assert!(matches!(
            sends("profile copy My FPS"),
            Request::CopyProfile { preset: false, from, .. } if from == "My FPS"
        ));
        assert!(matches!(
            sends("profile export-preset MOBA"),
            Request::GetProfile { preset: true, name } if name == "MOBA"
        ));
        assert!(matches!(
            sends("profile delete Old"),
            Request::DeleteProfile { name } if name == "Old"
        ));
    }

    #[test]
    fn import_reads_and_checks_the_file_first() {
        let dir = std::env::temp_dir().join(format!("ggctl-profile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut p = gg_ipc::Profile::named("Shared");
        p.lighting = Some(gg_ipc::Lighting::default());
        let good = dir.join("good.json");
        std::fs::write(&good, ProfileFile::new(p.clone(), "").to_json()).unwrap();
        let bad = dir.join("bad.json");
        std::fs::write(&bad, r#"{"hello":1}"#).unwrap();
        let line = format!("profile import {} replace", good.display());
        assert!(matches!(
            sends(&line),
            Request::ImportProfile { profile, replace: true } if *profile == p
        ));
        let e = action(&["import", bad.to_str().unwrap()]).err().unwrap().to_string();
        assert!(e.contains("isn't a gglinux profile"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_rule_is_checked_before_the_daemon_is_asked() {
        let e = action(&["rule", "add", "running", "class", "steam", "X"])
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("on focus"), "{e}");
        assert!(action(&["rule", "add", "sometimes", "program", "a", "X"]).is_err());
        assert!(action(&["rule", "rm", "first"]).is_err());
    }

    #[test]
    fn the_list_says_what_each_profile_holds_and_why_one_is_on() {
        let state = ProfilesState {
            profiles: vec!["FPS".into()],
            sections: vec![vec![Section::Mouse, Section::Switches]],
            rules: vec![GameRule {
                when: RuleWhen::Focused,
                ..GameRule::running("RDR2.exe", "FPS")
            }],
            active: Some(GameRule {
                when: RuleWhen::Focused,
                ..GameRule::running("RDR2.exe", "FPS")
            }),
            applied: Some(gg_ipc::Applied {
                profile: "FPS".into(),
                notes: vec!["Mouse: skipped. why".into()],
            }),
            ..ProfilesState::default()
        };
        let text = lines(&state).join("\n");
        for part in [
            "FPS                      mouse, switches",
            "1. RDR2.exe → FPS (on focus)",
            "Now: FPS, because RDR2.exe has the focus",
            "  note: Mouse: skipped. why",
        ] {
            assert!(text.contains(part), "{part}\n{text}");
        }
    }
}
