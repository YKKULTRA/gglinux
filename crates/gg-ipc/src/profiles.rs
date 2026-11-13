// SPDX-License-Identifier: GPL-3.0-or-later
//! Profiles: saved device settings, the rules that switch to them, and the file a profile is
//! shared in.
//!
//! A profile holds any of six sections (headset, mouse, keyboard switches, key bindings,
//! keyboard screen, lighting); a section it doesn't hold is left as it is when the profile is
//! applied. Profiles saved before sections existed hold lighting only, and still read: their
//! `lighting` is the same field.

use serde::{Deserialize, Serialize};

use crate::{Bindings, HeadsetChange, Lighting, MouseSettings, Screen, Switches};

/// One part of a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    Headset,
    Mouse,
    Switches,
    Bindings,
    Screen,
    Lighting,
}

impl Section {
    /// Every section, in the order a profile's are applied.
    pub const ALL: [Section; 6] = [
        Section::Headset,
        Section::Mouse,
        Section::Switches,
        Section::Bindings,
        Section::Screen,
        Section::Lighting,
    ];

    /// The name `ggctl profile save` takes.
    pub fn key(self) -> &'static str {
        match self {
            Section::Headset => "headset",
            Section::Mouse => "mouse",
            Section::Switches => "switches",
            Section::Bindings => "bindings",
            Section::Screen => "screen",
            Section::Lighting => "lighting",
        }
    }

    /// The name the app shows.
    pub fn label(self) -> &'static str {
        match self {
            Section::Headset => "Headset",
            Section::Mouse => "Mouse",
            Section::Switches => "Keyboard switches",
            Section::Bindings => "Key bindings",
            Section::Screen => "Keyboard screen",
            Section::Lighting => "Lighting",
        }
    }

    pub fn from_key(key: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// The key bindings as a profile keeps them: what `set_bindings` takes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProfileBindings {
    pub enabled: bool,
    pub bindings: Bindings,
}

/// Saved device settings. Each section is optional: one that's `None` is left as it is when
/// the profile is applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// The headset settings to send, one change each, as `set_headset` takes them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headset: Option<Vec<HeadsetChange>>,
    /// The parts that are `Some` are sent; the sleep timer and the radio need `mouse_power`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse: Option<MouseSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switches: Option<Switches>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bindings: Option<ProfileBindings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<Screen>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lighting: Option<Lighting>,
}

impl Profile {
    pub const MAX_NAME: usize = 64;

    /// A profile with no sections yet.
    pub fn named(name: &str) -> Profile {
        Profile {
            name: name.to_string(),
            headset: None,
            mouse: None,
            switches: None,
            bindings: None,
            screen: None,
            lighting: None,
        }
    }

    /// A name as it's saved: trimmed, 1 to [`Profile::MAX_NAME`] characters.
    pub fn clean_name(name: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > Self::MAX_NAME {
            return Err(format!(
                "a profile name needs 1–{} characters",
                Self::MAX_NAME
            ));
        }
        Ok(name.to_string())
    }

    /// The sections it holds, in the order they're applied.
    pub fn sections(&self) -> Vec<Section> {
        Section::ALL.into_iter().filter(|&s| self.has(s)).collect()
    }

    pub fn has(&self, section: Section) -> bool {
        match section {
            Section::Headset => self.headset.is_some(),
            Section::Mouse => self.mouse.is_some(),
            Section::Switches => self.switches.is_some(),
            Section::Bindings => self.bindings.is_some(),
            Section::Screen => self.screen.is_some(),
            Section::Lighting => self.lighting.is_some(),
        }
    }

    /// Take `section` from `other`, or drop it here if `other` hasn't it.
    pub fn take_section(&mut self, other: &Profile, section: Section) {
        match section {
            Section::Headset => self.headset = other.headset.clone(),
            Section::Mouse => self.mouse = other.mouse.clone(),
            Section::Switches => self.switches = other.switches.clone(),
            Section::Bindings => self.bindings = other.bindings.clone(),
            Section::Screen => self.screen = other.screen.clone(),
            Section::Lighting => self.lighting = other.lighting.clone(),
        }
    }

    /// Keep only these sections.
    pub fn keep_only(&mut self, sections: &[Section]) {
        let empty = Profile::named("");
        for s in Section::ALL {
            if !sections.contains(&s) {
                self.take_section(&empty, s);
            }
        }
    }

    /// Check the name and every section, with a message meant for the user. Whether the
    /// devices take each value is the daemon's to check when it's applied.
    pub fn validate(&self) -> Result<(), String> {
        Self::clean_name(&self.name)?;
        if self.sections().is_empty() {
            return Err(format!("the profile \"{}\" holds no settings", self.name));
        }
        let within = |s: Section, r: Result<(), String>| r.map_err(|e| format!("{}: {e}", s.label()));
        if let Some(l) = &self.lighting {
            within(Section::Lighting, l.validate())?;
        }
        if let Some(s) = &self.switches {
            within(Section::Switches, s.validate())?;
        }
        if let Some(s) = &self.screen {
            within(Section::Screen, s.validate())?;
        }
        if let Some(b) = &self.bindings {
            within(Section::Bindings, b.bindings.validate())?;
        }
        if let Some(changes) = &self.headset {
            let mut seen = Vec::new();
            for c in changes {
                let kind = std::mem::discriminant(c);
                if seen.contains(&kind) {
                    return Err(format!("Headset: {c:?} is in the profile twice"));
                }
                seen.push(kind);
            }
        }
        Ok(())
    }
}

/// What a rule looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleMatch {
    /// A program, by any of its names (as `running` lists them).
    #[default]
    Program,
    /// A window, by its class (Hyprland's `class`, KWin's `resourceClass`).
    Class,
}

/// When a rule's profile is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleWhen {
    /// While the program runs.
    #[default]
    Running,
    /// While its window has the focus; switching away puts your own settings back, as GG
    /// does. Where no desktop reports the focused window, it acts as `running`.
    Focused,
}

/// Use this profile while this program runs, or while its window has the focus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameRule {
    /// The program's name, or with [`RuleMatch::Class`] the window's class.
    pub process: String,
    pub profile: String,
    /// Absent in rules saved before window classes: a program.
    #[serde(default)]
    pub by: RuleMatch,
    /// Absent in rules saved before focus switching: while it runs.
    #[serde(default)]
    pub when: RuleWhen,
}

impl GameRule {
    /// A rule as rules were before focus switching: a program, while it runs.
    pub fn running(process: &str, profile: &str) -> GameRule {
        GameRule {
            process: process.into(),
            profile: profile.into(),
            by: RuleMatch::Program,
            when: RuleWhen::Running,
        }
    }

    /// Check what can be checked without the profiles, with a message meant for the user.
    pub fn validate(&self) -> Result<(), String> {
        if self.process.trim().is_empty() || self.process.trim().chars().count() > Profile::MAX_NAME
        {
            return Err(format!(
                "a rule's program or window class needs 1–{} characters",
                Profile::MAX_NAME
            ));
        }
        if self.by == RuleMatch::Class && self.when == RuleWhen::Running {
            return Err(format!(
                "the rule for the window class {} needs \"on focus\": a class belongs to a window, not to a running program",
                self.process.trim()
            ));
        }
        Ok(())
    }

    /// The rule's reason, for messages: "RDR2.exe has the focus".
    pub fn reason(&self) -> String {
        let what = match self.by {
            RuleMatch::Program => self.process.clone(),
            RuleMatch::Class => format!("a {} window", self.process),
        };
        match self.when {
            RuleWhen::Running => format!("{what} is running"),
            RuleWhen::Focused => format!("{what} has the focus"),
        }
    }
}

/// One of gglinux's own presets, as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetInfo {
    pub name: String,
    /// What it sets, in a sentence that says it's gglinux's own.
    pub about: String,
    pub sections: Vec<Section>,
}

/// The window that has the focus, as a desktop reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusedWindow {
    pub class: String,
    /// The program the window belongs to, when its process could be found.
    #[serde(default)]
    pub program: Option<String>,
}

/// How rules that act on focus follow the focused window.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FocusStatus {
    /// The desktop that reports the focused window now: "Hyprland" or "KDE Plasma".
    pub source: Option<String>,
    pub window: Option<FocusedWindow>,
    /// Why focus rules act while their program runs instead, when they do.
    pub note: Option<String>,
}

/// What the last apply did: the profile ("Default" for your own settings coming back), and a
/// line for each section that was skipped or didn't go through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub profile: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProfilesState {
    pub profiles: Vec<String>,
    pub rules: Vec<GameRule>,
    /// The rule applied right now, if one holds.
    pub active: Option<GameRule>,
    /// Process names running now, for picking rules. Only in answers, not in events.
    pub running: Vec<String>,
    /// The sections each profile holds, in `profiles` order.
    #[serde(default)]
    pub sections: Vec<Vec<Section>>,
    /// gglinux's own presets.
    #[serde(default)]
    pub presets: Vec<PresetInfo>,
    #[serde(default)]
    pub focus: FocusStatus,
    #[serde(default)]
    pub applied: Option<Applied>,
    /// The sections of your own settings kept to put back when no rule holds (the Default).
    #[serde(default)]
    pub default_sections: Vec<Section>,
}

/// A profile as a file, for sharing, and for gglinux's own presets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileFile {
    /// [`ProfileFile::FORMAT`], so another JSON file isn't taken for a profile.
    pub format: String,
    pub version: u32,
    /// A sentence about what it sets; presets say they're gglinux's own.
    #[serde(default)]
    pub about: String,
    pub profile: Profile,
}

impl ProfileFile {
    pub const FORMAT: &'static str = "gglinux-profile";
    pub const VERSION: u32 = 2;

    pub fn new(profile: Profile, about: &str) -> ProfileFile {
        ProfileFile {
            format: Self::FORMAT.into(),
            version: Self::VERSION,
            about: about.into(),
            profile,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a profile file serializes")
    }

    /// Read a shared profile: a profile file, or a bare profile as `profiles.json` holds them
    /// (lighting-only ones from before sections, too). Checked with [`Profile::validate`].
    pub fn parse(text: &str) -> Result<ProfileFile, String> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| format!("this isn't a gglinux profile file (not JSON: {e})"))?;
        let file = if value.get("format").is_some() {
            if value["format"] != Self::FORMAT {
                return Err("this isn't a gglinux profile file".into());
            }
            let file: ProfileFile = serde_json::from_value(value)
                .map_err(|e| format!("this profile file doesn't read: {e}"))?;
            if file.version > Self::VERSION {
                return Err(format!(
                    "this profile file was made by a newer gglinux (version {}); update gglinux to read it",
                    file.version
                ));
            }
            file
        } else {
            let profile: Profile = serde_json::from_value(value)
                .map_err(|e| format!("this isn't a gglinux profile file: {e}"))?;
            ProfileFile::new(profile, "")
        };
        file.profile.validate()?;
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Effect, KeyActuation, NoiseMode};

    #[test]
    fn a_profile_saved_before_sections_reads_as_lighting_only() {
        // The shape gglinux saved profiles in before sections (`Profile { name, lighting }`).
        let v1 = r#"{"name":"Night","lighting":{"enabled":true,"effect":{"kind":"static"},"keys":[],"brightness":70}}"#;
        let p: Profile = serde_json::from_str(v1).unwrap();
        assert_eq!(p.sections(), [Section::Lighting]);
        assert_eq!(p.lighting.as_ref().unwrap().effect, Effect::Static);
        assert_eq!(p.lighting.as_ref().unwrap().brightness, 70);
        // Saved again, it has just the two fields.
        let back = serde_json::to_value(&p).unwrap();
        assert_eq!(back.as_object().unwrap().len(), 2, "{back}");
    }

    #[test]
    fn sections_come_in_the_order_they_are_applied_and_can_be_dropped() {
        let mut p = Profile::named("All");
        p.lighting = Some(Lighting::default());
        p.headset = Some(vec![HeadsetChange::Sidetone(1)]);
        p.switches = Some(Switches::default());
        assert_eq!(
            p.sections(),
            [Section::Headset, Section::Switches, Section::Lighting]
        );
        p.keep_only(&[Section::Lighting, Section::Mouse]);
        assert_eq!(p.sections(), [Section::Lighting]);
        for s in Section::ALL {
            assert_eq!(Section::from_key(s.key()), Some(s));
        }
    }

    #[test]
    fn a_profile_is_checked_section_by_section() {
        let mut p = Profile::named("  ");
        assert!(p.validate().unwrap_err().contains("1–64"));
        p.name = "Empty".into();
        assert!(p.validate().unwrap_err().contains("holds no settings"));
        p.switches = Some(Switches {
            actuation: 50,
            ..Switches::default()
        });
        let e = p.validate().unwrap_err();
        assert!(e.starts_with("Keyboard switches: "), "{e}");
        p.switches = Some(Switches {
            keys: vec![KeyActuation {
                code: 0x1A,
                actuation: 10,
            }],
            ..Switches::default()
        });
        p.validate().unwrap();
        p.headset = Some(vec![
            HeadsetChange::NoiseMode(NoiseMode::Anc),
            HeadsetChange::NoiseMode(NoiseMode::Off),
        ]);
        assert!(p.validate().unwrap_err().contains("twice"));
    }

    #[test]
    fn rules_from_before_focus_switching_run_by_program_and_a_class_needs_focus() {
        let old: GameRule =
            serde_json::from_str(r#"{"process":"RDR2.exe","profile":"Night"}"#).unwrap();
        assert_eq!(old, GameRule::running("RDR2.exe", "Night"));
        old.validate().unwrap();
        let class = GameRule {
            by: RuleMatch::Class,
            ..GameRule::running("steam_app_1174180", "Night")
        };
        assert!(class.validate().unwrap_err().contains("on focus"));
        let focused = GameRule {
            when: RuleWhen::Focused,
            ..class
        };
        focused.validate().unwrap();
        assert_eq!(focused.reason(), "a steam_app_1174180 window has the focus");
        assert_eq!(old.reason(), "RDR2.exe is running");
        let json = serde_json::to_value(&focused).unwrap();
        assert_eq!((json["by"].as_str(), json["when"].as_str()), (Some("class"), Some("focused")));
    }

    #[test]
    fn a_shared_file_round_trips_and_other_json_is_refused() {
        let mut p = Profile::named("Night");
        p.lighting = Some(Lighting::default());
        let file = ProfileFile::new(p.clone(), "");
        let text = file.to_json();
        assert!(text.contains(r#""format": "gglinux-profile""#), "{text}");
        assert_eq!(ProfileFile::parse(&text).unwrap().profile, p);
        // A bare profile, as profiles.json holds them, reads too.
        let bare = serde_json::to_string(&p).unwrap();
        assert_eq!(ProfileFile::parse(&bare).unwrap().profile, p);
        for (bad, why) in [
            ("{ nope", "not JSON"),
            (r#"{"format":"something-else","version":1}"#, "isn't a gglinux"),
            (r#"{"hello":1}"#, "isn't a gglinux"),
        ] {
            let e = ProfileFile::parse(bad).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
        }
        let newer = text.replace(r#""version": 2"#, r#""version": 3"#);
        assert!(ProfileFile::parse(&newer).unwrap_err().contains("newer gglinux"));
        let empty = ProfileFile::new(Profile::named("Empty"), "").to_json();
        assert!(ProfileFile::parse(&empty).unwrap_err().contains("no settings"));
    }
}
