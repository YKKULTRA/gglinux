//! Profiles and game rules, as saved, and which rule applies when.

use std::collections::HashSet;
use std::path::Path;

use gg_ipc::{GameRule, Lighting, Profile};
use serde::{Deserialize, Serialize};
use tracing::warn;

const MAX_NAME: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub rules: Vec<GameRule>,
    /// The owner's own lighting while a game rule's profile is on, put back when
    /// the game ends. Saved, since the profile is saved as the lighting.
    #[serde(default)]
    pub before: Option<Lighting>,
}

fn clean(name: &str, what: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(format!("a {what} needs 1–{MAX_NAME} characters"));
    }
    Ok(name.to_string())
}

impl Store {
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    pub fn save_profile(&mut self, name: &str, lighting: Lighting) -> Result<(), String> {
        let name = clean(name, "profile name")?;
        match self.profiles.iter_mut().find(|p| p.name == name) {
            Some(p) => p.lighting = lighting,
            None => self.profiles.push(Profile { name, lighting }),
        }
        Ok(())
    }

    pub fn delete_profile(&mut self, name: &str) -> Result<(), String> {
        if let Some(r) = self.rules.iter().find(|r| r.profile == name) {
            return Err(format!("\"{name}\" is used by the rule for {}; remove that rule first", r.process));
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
                let process = clean(&r.process, "process name")?;
                if self.profile(&r.profile).is_none() {
                    return Err(format!("the rule for {process} names a missing profile \"{}\"", r.profile));
                }
                Ok(GameRule { process, profile: r.profile })
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.rules = rules;
        Ok(())
    }
}

pub fn load(path: &Path) -> Store {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            warn!(path = %path.display(), error = %e, "ignoring unreadable profiles");
            Store::default()
        }),
        Err(_) => Store::default(),
    }
}

/// Save atomically; a failure is logged, never fatal.
pub fn save(path: &Path, store: &Store) {
    let write = || -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(store)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    if let Err(e) = write() {
        warn!(path = %path.display(), error = %format!("{e:#}"), "could not save profiles");
    }
}

/// The first rule whose program is running and whose profile exists.
pub fn pick<'a>(store: &'a Store, running: &HashSet<String>) -> Option<&'a GameRule> {
    store
        .rules
        .iter()
        .find(|r| running.contains(&r.process.to_lowercase()) && store.profile(&r.profile).is_some())
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
    use gg_ipc::Effect;

    fn store() -> Store {
        let mut s = Store::default();
        s.save_profile("Night", Lighting { effect: Effect::Static, ..Lighting::default() }).unwrap();
        s.save_profile("Rainbow", Lighting::default()).unwrap();
        s
    }

    fn rule(process: &str, profile: &str) -> GameRule {
        GameRule { process: process.into(), profile: profile.into() }
    }

    fn running(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_lowercase()).collect()
    }

    #[test]
    fn saving_a_profile_replaces_one_of_the_same_name_and_names_are_checked() {
        let mut s = store();
        s.save_profile("  Night ", Lighting::default()).unwrap();
        assert_eq!(s.profiles.len(), 2, "trimmed, replaced");
        assert_eq!(s.profile("Night").unwrap().lighting, Lighting::default());
        assert!(s.save_profile("   ", Lighting::default()).is_err());
        assert!(s.save_profile(&"x".repeat(65), Lighting::default()).is_err());
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
        s.set_rules(vec![rule(" kate ", "Night")]).unwrap();
        assert_eq!(s.rules, vec![rule("kate", "Night")], "trimmed");
    }

    #[test]
    fn the_first_matching_rule_wins_ignoring_case() {
        let mut s = store();
        s.set_rules(vec![rule("RDR2.exe", "Night"), rule("kate", "Rainbow")]).unwrap();
        assert_eq!(pick(&s, &running(&["KATE", "rdr2.EXE"])), Some(&rule("RDR2.exe", "Night")));
        assert_eq!(pick(&s, &running(&["kate"])), Some(&rule("kate", "Rainbow")));
        assert_eq!(pick(&s, &running(&["firefox"])), None);
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

    #[test]
    fn the_store_survives_a_round_trip_and_garbage_means_empty() {
        let dir = std::env::temp_dir().join(format!("gglinux-profiles-{}", std::process::id()));
        let path = dir.join("profiles.json");
        let mut s = store();
        s.set_rules(vec![rule("kate", "Night")]).unwrap();
        save(&path, &s);
        assert_eq!(load(&path), s);
        std::fs::write(&path, "{ nope").unwrap();
        assert_eq!(load(&path), Store::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
