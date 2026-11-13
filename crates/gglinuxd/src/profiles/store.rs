// SPDX-License-Identifier: GPL-3.0-or-later
//! Profiles and game rules, as saved, and which rule holds when.
//!
//! `profiles.json` from before sections (version 1) holds lighting-only profiles, rules
//! without `by`/`when`, and `before` as a bare lighting. All of it reads as it was meant:
//! lighting sections, program rules that act while the program runs, and a Default that holds
//! lighting. The first time such a file is read, a copy is kept beside it as
//! `profiles.json.v1`, since a version-1 gglinux can't read what this one saves.

use std::collections::HashSet;
use std::path::Path;

use gg_ipc::{GameRule, Lighting, Profile, RuleMatch, RuleWhen};
use serde::{Deserialize, Deserializer, Serialize};
use tracing::{info, warn};

/// What this gglinux saves.
const VERSION: u32 = 2;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    /// 0 in files from before sections, which had no version.
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub rules: Vec<GameRule>,
    /// Your own settings, for the sections a rule's profile replaced, put back when no rule
    /// holds any more (the Default). Saved, since what a profile applies is saved as the
    /// current settings too.
    #[serde(default, deserialize_with = "before_v1_or_v2")]
    pub before: Option<Profile>,
}

/// Version 1 saved `before` as a bare lighting.
fn before_v1_or_v2<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Profile>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Before {
        V2(Box<Profile>),
        V1(Lighting),
    }
    Ok(match Option::<Before>::deserialize(d)? {
        None => None,
        Some(Before::V2(p)) => Some(*p),
        Some(Before::V1(lighting)) => Some(Profile {
            lighting: Some(lighting),
            ..Profile::named(DEFAULT)
        }),
    })
}

/// The name of your own settings as they come back.
pub const DEFAULT: &str = "Default";

impl Store {
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Save `profile` under its (cleaned) name, replacing one of the same name.
    pub fn put(&mut self, mut profile: Profile) -> Result<(), String> {
        profile.name = Profile::clean_name(&profile.name)?;
        match self.profiles.iter_mut().find(|p| p.name == profile.name) {
            Some(p) => *p = profile,
            None => self.profiles.push(profile),
        }
        Ok(())
    }

    /// A name no saved profile has: `name`, else `name 2`, `name 3`, ...
    pub fn free_name(&self, name: &str) -> String {
        let base: String = name.trim().chars().take(Profile::MAX_NAME - 4).collect();
        if self.profile(&base).is_none() {
            return base;
        }
        (2..)
            .map(|n| format!("{base} {n}"))
            .find(|n| self.profile(n).is_none())
            .expect("some number is free")
    }

    pub fn delete_profile(&mut self, name: &str) -> Result<(), String> {
        if let Some(r) = self.rules.iter().find(|r| r.profile == name) {
            return Err(format!(
                "\"{name}\" is used by the rule for {}; remove that rule first",
                r.process
            ));
        }
        let before = self.profiles.len();
        self.profiles.retain(|p| p.name != name);
        if self.profiles.len() == before {
            return Err(format!("no profile named \"{name}\""));
        }
        Ok(())
    }

    pub fn set_rules(&mut self, rules: Vec<GameRule>) -> Result<(), String> {
        let rules = rules
            .into_iter()
            .map(|r| {
                r.validate()?;
                let process = r.process.trim().to_string();
                if self.profile(&r.profile).is_none() {
                    return Err(format!(
                        "the rule for {process} names a missing profile \"{}\"",
                        r.profile
                    ));
                }
                Ok(GameRule { process, ..r })
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.rules = rules;
        Ok(())
    }

    /// Whether any rule acts on the focused window.
    pub fn wants_focus(&self) -> bool {
        self.rules.iter().any(|r| r.when == RuleWhen::Focused)
    }
}

/// Saved profiles; a missing or unreadable file means none. Read as bytes, so a file that
/// isn't UTF-8 is unreadable too and moved aside, not saved over.
pub fn load(path: &Path) -> Store {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Store>(&bytes) {
            Ok(store) => {
                if store.version < VERSION {
                    keep_v1(path, &bytes);
                }
                store
            }
            Err(e) => {
                let moved_to = gg_ipc::set_aside(path);
                warn!(path = %path.display(), error = %e, %moved_to, "ignoring unreadable profiles");
                Store::default()
            }
        },
        Err(_) => Store::default(),
    }
}

/// Keep a version-1 file as it was, once, beside the file.
fn keep_v1(path: &Path, bytes: &[u8]) {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".v1");
    let copy = path.with_file_name(name);
    if copy.exists() {
        return;
    }
    match std::fs::write(&copy, bytes) {
        Ok(()) => info!(copy = %copy.display(), "profiles from an older gglinux: kept a copy"),
        Err(e) => {
            warn!(copy = %copy.display(), error = %e, "could not keep a copy of the older profiles")
        }
    }
}

/// Save atomically, as the current version; a failure is logged, never fatal.
pub fn save(path: &Path, store: &Store) {
    let write = || -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let store = Store {
            version: VERSION,
            ..store.clone()
        };
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&store)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    if let Err(e) = write() {
        warn!(path = %path.display(), error = %format!("{e:#}"), "could not save profiles");
    }
}

/// The focused window as rules see it: its class and its program's names, lower-cased.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    pub class: String,
    pub names: HashSet<String>,
}

/// What rules are checked against.
pub struct Seen<'a> {
    /// Lower-cased names of the programs running.
    pub running: &'a HashSet<String>,
    /// `None` while the focused window isn't followed: focus rules then act while their
    /// program runs. `Some(None)` while it's followed and no window has the focus.
    pub focus: Option<Option<&'a Window>>,
}

fn holds(rule: &GameRule, seen: &Seen) -> bool {
    let wanted = rule.process.to_lowercase();
    match (rule.when, seen.focus) {
        (RuleWhen::Focused, Some(window)) => window.is_some_and(|w| match rule.by {
            RuleMatch::Program => w.names.contains(&wanted),
            RuleMatch::Class => w.class == wanted,
        }),
        _ => rule.by == RuleMatch::Program && seen.running.contains(&wanted),
    }
}

/// The first rule that holds and whose profile exists.
pub fn pick<'a>(store: &'a Store, seen: &Seen) -> Option<&'a GameRule> {
    store
        .rules
        .iter()
        .find(|r| holds(r, seen) && store.profile(&r.profile).is_some())
}

/// The first program rule whose program runs, whatever has the focus: games-only clips
/// record while a game runs, not only while it has the focus.
pub fn running_game<'a>(store: &'a Store, running: &HashSet<String>) -> Option<&'a GameRule> {
    store
        .rules
        .iter()
        .find(|r| r.by == RuleMatch::Program && running.contains(&r.process.to_lowercase()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Stay,
    Apply(GameRule),
    Restore,
}

pub fn decide(active: Option<&GameRule>, picked: Option<&GameRule>) -> Action {
    match (active, picked) {
        (a, Some(p)) if a != Some(p) => Action::Apply(p.clone()),
        (Some(_), None) => Action::Restore,
        _ => Action::Stay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::{Effect, Section};

    fn lit(effect: Effect) -> Profile {
        Profile {
            lighting: Some(Lighting {
                effect,
                ..Lighting::default()
            }),
            ..Profile::named("")
        }
    }

    fn store() -> Store {
        let mut s = Store::default();
        s.put(Profile {
            name: "Night".into(),
            ..lit(Effect::Static)
        })
        .unwrap();
        s.put(Profile {
            name: "Rainbow".into(),
            ..lit(Effect::Wave { period_s: 4.0 })
        })
        .unwrap();
        s
    }

    fn rule(process: &str, profile: &str) -> GameRule {
        GameRule::running(process, profile)
    }

    fn focused(by: RuleMatch, process: &str, profile: &str) -> GameRule {
        GameRule {
            by,
            when: RuleWhen::Focused,
            ..rule(process, profile)
        }
    }

    fn names(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_lowercase()).collect()
    }

    fn window(class: &str, programs: &[&str]) -> Window {
        Window {
            class: class.to_lowercase(),
            names: names(programs),
        }
    }

    fn running<'a>(r: &'a HashSet<String>) -> Seen<'a> {
        Seen {
            running: r,
            focus: None,
        }
    }

    /// The scratch folder of one test, removed when it ends.
    struct Dir(std::path::PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let d =
                std::env::temp_dir().join(format!("gglinux-profiles-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Dir(d)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saving_a_profile_replaces_one_of_the_same_name_and_names_are_checked() {
        let mut s = store();
        s.put(Profile {
            name: "  Night ".into(),
            ..lit(Effect::Spectrum { period_s: 2.0 })
        })
        .unwrap();
        assert_eq!(s.profiles.len(), 2, "trimmed, replaced");
        assert_eq!(
            s.profile("Night")
                .unwrap()
                .lighting
                .as_ref()
                .unwrap()
                .effect,
            Effect::Spectrum { period_s: 2.0 }
        );
        assert!(s.put(Profile::named("   ")).is_err());
        assert!(s.put(Profile::named(&"x".repeat(65))).is_err());
        assert_eq!(s.free_name("Typing"), "Typing");
        assert_eq!(s.free_name("Night"), "Night 2");
    }

    #[test]
    fn profiles_in_use_cannot_be_deleted() {
        let mut s = store();
        s.set_rules(vec![rule("RDR2.exe", "Night")]).unwrap();
        let err = s.delete_profile("Night").unwrap_err();
        assert!(err.contains("RDR2.exe"), "{err}");
        s.delete_profile("Rainbow").unwrap();
        assert!(s.delete_profile("Rainbow").is_err(), "already gone");
    }

    #[test]
    fn rules_must_name_real_profiles_and_processes() {
        let mut s = store();
        assert!(s.set_rules(vec![rule("x", "Nope")]).is_err());
        assert!(s.set_rules(vec![rule("  ", "Night")]).is_err());
        let class_while_running = GameRule {
            by: RuleMatch::Class,
            ..rule("steam", "Night")
        };
        assert!(s.set_rules(vec![class_while_running]).is_err());
        s.set_rules(vec![rule(" kate ", "Night")]).unwrap();
        assert_eq!(s.rules, vec![rule("kate", "Night")], "trimmed");
        assert!(!s.wants_focus());
        s.set_rules(vec![focused(RuleMatch::Program, "kate", "Night")])
            .unwrap();
        assert!(s.wants_focus());
    }

    #[test]
    fn the_first_matching_rule_wins_ignoring_case() {
        let mut s = store();
        s.set_rules(vec![rule("RDR2.exe", "Night"), rule("kate", "Rainbow")])
            .unwrap();
        let both = names(&["KATE", "rdr2.EXE"]);
        assert_eq!(pick(&s, &running(&both)), Some(&rule("RDR2.exe", "Night")));
        let kate = names(&["kate"]);
        assert_eq!(pick(&s, &running(&kate)), Some(&rule("kate", "Rainbow")));
        assert_eq!(pick(&s, &running(&names(&["firefox"]))), None);
    }

    #[test]
    fn focus_rules_hold_while_their_window_has_the_focus() {
        let mut s = store();
        let by_program = focused(RuleMatch::Program, "RDR2.exe", "Night");
        let by_class = focused(RuleMatch::Class, "steam_app_1174180", "Rainbow");
        s.set_rules(vec![by_program.clone(), by_class.clone()])
            .unwrap();
        let both = names(&["rdr2.exe", "steam"]);
        let game = window("RDR2", &["rdr2.exe", "wine64-preload"]);
        let seen = |w: Option<&Window>| {
            pick(
                &s,
                &Seen {
                    running: &both,
                    focus: Some(w),
                },
            )
            .cloned()
        };
        assert_eq!(seen(Some(&game)), Some(by_program.clone()));
        assert_eq!(
            seen(Some(&window("Steam_App_1174180", &["x"]))),
            Some(by_class),
            "classes ignore case"
        );
        assert_eq!(
            seen(Some(&window("kitty", &["kitty"]))),
            None,
            "Alt-Tabbed out"
        );
        assert_eq!(seen(None), None, "no window has the focus");
        // Not followed: a program rule acts while it runs, a class rule can't.
        assert_eq!(pick(&s, &running(&both)), Some(&by_program));
        assert_eq!(pick(&s, &running(&names(&["steam"]))), None);
        // Games-only clips go by what runs.
        assert_eq!(running_game(&s, &both), Some(&by_program));
    }

    #[test]
    fn transitions() {
        let (a, b) = (rule("a", "Night"), rule("b", "Rainbow"));
        assert_eq!(decide(None, None), Action::Stay);
        assert_eq!(decide(None, Some(&a)), Action::Apply(a.clone()));
        assert_eq!(decide(Some(&a), Some(&a)), Action::Stay);
        assert_eq!(decide(Some(&a), Some(&b)), Action::Apply(b.clone()));
        assert_eq!(decide(Some(&a), None), Action::Restore);
    }

    /// A `profiles.json` exactly as version 1 wrote it (`Store { profiles, rules, before }`,
    /// with `Profile { name, lighting }`, `GameRule { process, profile }`, `before: Lighting`).
    const V1: &str = r#"{
  "profiles": [
    {
      "name": "Western",
      "lighting": {
        "enabled": true,
        "effect": { "kind": "breathing", "color": { "r": 200, "g": 80, "b": 0 }, "period_s": 3.0 },
        "keys": [ { "code": 4, "color": { "r": 1, "g": 2, "b": 3 } } ],
        "brightness": 80,
        "reactive": { "enabled": true, "mode": "ripple", "color": { "r": 255, "g": 255, "b": 255 }, "duration_s": 0.6 },
        "mic_mute": { "enabled": false, "key": 251, "color": { "r": 255, "g": 0, "b": 0 } }
      }
    }
  ],
  "rules": [ { "process": "RDR2.exe", "profile": "Western" } ],
  "before": { "enabled": true, "effect": { "kind": "static" }, "keys": [], "brightness": 100 }
}"#;

    #[test]
    fn a_version_1_file_reads_losslessly_and_is_kept_once() {
        let d = Dir::new("v1");
        let path = d.0.join("profiles.json");
        std::fs::write(&path, V1).unwrap();
        let s = load(&path);
        assert_eq!(s.version, 0);
        let western = s.profile("Western").unwrap();
        assert_eq!(western.sections(), [Section::Lighting]);
        let l = western.lighting.as_ref().unwrap();
        assert_eq!(
            l.effect,
            Effect::Breathing {
                color: gg_ipc::Rgb {
                    r: 200,
                    g: 80,
                    b: 0
                },
                period_s: 3.0
            }
        );
        assert_eq!(
            (l.brightness, l.keys.len(), l.reactive.enabled),
            (80, 1, true)
        );
        assert_eq!(s.rules, vec![rule("RDR2.exe", "Western")], "while it runs");
        let before = s.before.as_ref().unwrap();
        assert_eq!(before.name, DEFAULT);
        assert_eq!(before.lighting.as_ref().unwrap().effect, Effect::Static);
        // Kept as it was, beside it.
        let copy = d.0.join("profiles.json.v1");
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), V1);
        // Saved as version 2, it reads back the same, and the copy isn't written again.
        save(&path, &s);
        let again = load(&path);
        assert_eq!(again.version, VERSION);
        assert_eq!(
            Store {
                version: 0,
                ..again.clone()
            },
            s
        );
        std::fs::write(&copy, "older copy").unwrap();
        std::fs::write(&path, V1).unwrap();
        load(&path);
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "older copy");
    }

    #[test]
    fn the_store_survives_a_round_trip_and_garbage_means_empty() {
        let d = Dir::new("trip");
        let path = d.0.join("profiles.json");
        let mut s = store();
        s.set_rules(vec![focused(RuleMatch::Class, "steam", "Night")])
            .unwrap();
        s.before = Some(Profile {
            name: DEFAULT.into(),
            ..lit(Effect::Static)
        });
        save(&path, &s);
        assert_eq!(
            load(&path),
            Store {
                version: VERSION,
                ..s
            }
        );
        std::fs::write(&path, "{ nope").unwrap();
        assert_eq!(load(&path), Store::default());
        save(&path, &Store::default()); // the next change
        let aside: Vec<String> = std::fs::read_dir(&d.0)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name() != "profiles.json")
            .map(|e| std::fs::read_to_string(e.path()).unwrap())
            .collect();
        assert_eq!(aside, ["{ nope"], "kept, not saved over");
    }

    #[test]
    fn a_file_that_is_not_utf8_is_garbage_too() {
        let d = Dir::new("latin1");
        let path = d.0.join("profiles.json");
        let mut s = Store::default();
        s.put(Profile {
            name: "Café".into(),
            ..lit(Effect::Static)
        })
        .unwrap();
        save(&path, &s);
        // The same file hand-edited and saved as Latin-1, where é is one byte that isn't UTF-8.
        let latin1: Vec<u8> = std::fs::read_to_string(&path)
            .unwrap()
            .chars()
            .map(|c| c as u8)
            .collect();
        std::fs::write(&path, &latin1).unwrap();
        assert_eq!(load(&path), Store::default());
        save(&path, &Store::default()); // the next change
        let aside: Vec<Vec<u8>> = std::fs::read_dir(&d.0)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name() != "profiles.json")
            .map(|e| std::fs::read(e.path()).unwrap())
            .collect();
        assert_eq!(aside, [latin1], "kept, not saved over");
    }
}
