// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake profiles: saved profiles with their sections, rules, gglinux's own presets (the
//! daemon's own files), import, export and copy. Saving takes the fake keyboard's lighting,
//! switches and screen, the mouse as last set and the headset as it reports; applying sets the
//! fake keyboard's and says what a real daemon would have skipped. While a rule acts on focus
//! it pretends Hyprland reports a game's window (`steam_app_1174180`, `RDR2.exe`) as focused.
//!
//! `--profiles-demo` starts it with two profiles, two rules and an apply's notes.
//!
//! Like a real daemon it lags: the first `get_profiles` after a change still answers the
//! state from before it. The saved profiles are kept here rather than in [`Fake`], so the
//! fake's profiles stay in this file.

use std::sync::Mutex;

use gg_ipc::{
    Applied, Feature, FocusStatus, FocusedWindow, GameRule, HeadsetChange, Lighting, Profile,
    ProfileBindings, ProfileFile, ProfilesState, Request, ResponseData, RuleMatch, RuleWhen,
    Section, Switches,
};

use crate::log::method_name;
use crate::{error, Fake};

struct Saved {
    profiles: Vec<Profile>,
    /// What the next read answers while the last change hasn't shown yet.
    stale: Option<ProfilesState>,
    started: bool,
}

static SAVED: Mutex<Saved> = Mutex::new(Saved {
    profiles: Vec::new(),
    stale: None,
    started: false,
});

/// The daemon's preset files.
const PRESETS: [&str; 4] = [
    include_str!("../../src/profiles/presets/competitive-fps.json"),
    include_str!("../../src/profiles/presets/moba.json"),
    include_str!("../../src/profiles/presets/typing.json"),
    include_str!("../../src/profiles/presets/default-switches.json"),
];

/// What the fake says runs.
const RUNNING: [&str; 5] = ["firefox", "kitty", "RDR2.exe", "steam", "wine64-preload"];

/// The window the fake pretends has the focus.
const FOCUSED: (&str, &str) = ("steam_app_1174180", "RDR2.exe");

fn presets() -> Vec<ProfileFile> {
    PRESETS
        .iter()
        .map(|t| ProfileFile::parse(t).expect("a preset file reads"))
        .collect()
}

fn demo(saved: &mut Saved, s: &mut Fake) {
    let evening = Profile {
        lighting: Some(Lighting {
            enabled: true,
            brightness: 40,
            ..Lighting::default()
        }),
        ..Profile::named("Evening")
    };
    let fps = Profile {
        switches: Some(Switches {
            enabled: true,
            actuation: 10,
            ..Switches::default()
        }),
        mouse: Some(gg_ipc::MouseSettings {
            polling_hz: Some(1000),
            ..Default::default()
        }),
        bindings: Some(ProfileBindings::default()),
        ..Profile::named("Red Dead")
    };
    saved.profiles = vec![evening, fps];
    s.profiles.rules = vec![
        GameRule {
            when: RuleWhen::Focused,
            ..GameRule::running("RDR2.exe", "Red Dead")
        },
        GameRule::running("obs", "Evening"),
    ];
    s.profiles.applied = Some(Applied {
        profile: "Red Dead".into(),
        notes: vec![format!(
            "Key bindings: skipped. {}",
            gg_ipc::Experimental::default()
                .refusal(Feature::KeyboardBindings)
                .unwrap_or_default()
        )],
    });
}

/// The state as a read answers it.
fn state(s: &Fake, saved: &Saved) -> ProfilesState {
    let wants_focus = s.profiles.rules.iter().any(|r| r.when == RuleWhen::Focused);
    let holds = |r: &GameRule| {
        let wanted = r.process.to_lowercase();
        match (r.when, r.by) {
            (RuleWhen::Focused, RuleMatch::Class) => wanted == FOCUSED.0,
            (RuleWhen::Focused, RuleMatch::Program) => wanted == FOCUSED.1.to_lowercase(),
            (RuleWhen::Running, _) => RUNNING.iter().any(|n| n.to_lowercase() == wanted),
        }
    };
    let active = s.profiles.rules.iter().find(|r| holds(r)).cloned();
    let default_sections = active
        .as_ref()
        .and_then(|r| saved.profiles.iter().find(|p| p.name == r.profile))
        .map(Profile::sections)
        .unwrap_or_default();
    ProfilesState {
        profiles: saved.profiles.iter().map(|p| p.name.clone()).collect(),
        rules: s.profiles.rules.clone(),
        active,
        running: RUNNING.iter().map(|n| n.to_string()).collect(),
        sections: saved.profiles.iter().map(Profile::sections).collect(),
        presets: presets()
            .into_iter()
            .map(|f| gg_ipc::PresetInfo {
                sections: f.profile.sections(),
                name: f.profile.name,
                about: f.about,
            })
            .collect(),
        focus: if wants_focus {
            FocusStatus {
                source: Some("Hyprland".into()),
                window: Some(FocusedWindow {
                    class: FOCUSED.0.into(),
                    program: Some(FOCUSED.1.into()),
                }),
                note: None,
            }
        } else {
            FocusStatus::default()
        },
        applied: s.profiles.applied.clone(),
        default_sections,
    }
}

/// The fake's current settings of `sections`.
fn current(s: &Fake, name: String, sections: &[Section]) -> Profile {
    let mut p = Profile::named(&name);
    for section in sections {
        match section {
            Section::Lighting => p.lighting = Some(s.keyboard.lighting.clone()),
            Section::Switches => p.switches = Some(s.keyboard.switches.clone()),
            Section::Screen => p.screen = Some(s.keyboard.screen.clone()),
            Section::Bindings => p.bindings = Some(ProfileBindings::default()),
            Section::Mouse => p.mouse = s.applied.clone(),
            Section::Headset => {
                let changes: Vec<HeadsetChange> = [
                    s.controls.noise_mode.map(HeadsetChange::NoiseMode),
                    s.controls.sidetone.map(HeadsetChange::Sidetone),
                    s.controls.mic_volume.map(HeadsetChange::MicVolume),
                ]
                .into_iter()
                .flatten()
                .collect();
                p.headset = (!changes.is_empty()).then_some(changes);
            }
        }
    }
    p
}

/// What applying `p` would skip on a real daemon with the fake's gates.
fn skipped(p: &Profile, s: &Fake) -> Vec<String> {
    let e = &s.experimental;
    let mut notes = Vec::new();
    let mut gate = |section: Section, on: bool, feature: Feature| {
        if on {
            if let Some(why) = e.refusal(feature) {
                notes.push(format!("{}: skipped. {why}", section.label()));
            }
        }
    };
    gate(Section::Bindings, p.bindings.is_some(), Feature::KeyboardBindings);
    let protection = p.switches.as_ref().is_some_and(|s| s.protection.enabled);
    gate(Section::Switches, protection, Feature::KeyboardProtection);
    let power = p
        .mouse
        .as_ref()
        .is_some_and(|m| m.sleep_minutes.is_some() || m.radio.is_some());
    gate(Section::Mouse, power, Feature::MousePower);
    notes
}

fn find(saved: &Saved, name: &str, preset: bool) -> Result<ProfileFile, String> {
    if preset {
        presets()
            .into_iter()
            .find(|f| f.profile.name == name)
            .ok_or_else(|| format!("no preset named \"{name}\""))
    } else {
        saved
            .profiles
            .iter()
            .find(|p| p.name == name)
            .map(|p| ProfileFile::new(p.clone(), ""))
            .ok_or_else(|| format!("no profile named \"{name}\""))
    }
}

fn put(saved: &mut Saved, p: Profile) {
    match saved.profiles.iter_mut().find(|x| x.name == p.name) {
        Some(x) => *x = p,
        None => saved.profiles.push(p),
    }
}

fn change(req: Request, s: &mut Fake, saved: &mut Saved) -> Result<ResponseData, String> {
    match req {
        Request::SaveProfile { name, sections } => {
            let name = Profile::clean_name(&name)?;
            let sections = sections.unwrap_or_else(|| Section::ALL.to_vec());
            let p = current(s, name, &sections);
            if p.sections().is_empty() {
                return Err("nothing could be read to save".into());
            }
            put(saved, p);
        }
        Request::ApplyProfile { name, preset } => {
            let p = find(saved, &name, preset)?.profile;
            if let Some(l) = &p.lighting {
                s.keyboard.lighting = l.clone();
            }
            if let Some(sw) = &p.switches {
                s.keyboard.switches = sw.clone();
            }
            s.profiles.applied = Some(Applied {
                notes: skipped(&p, s),
                profile: p.name,
            });
        }
        Request::DeleteProfile { name } => {
            if let Some(r) = s.profiles.rules.iter().find(|r| r.profile == name) {
                return Err(format!(
                    "\"{name}\" is used by the rule for {}; remove that rule first",
                    r.process
                ));
            }
            if !saved.profiles.iter().any(|p| p.name == name) {
                return Err(format!("no profile named \"{name}\""));
            }
            saved.profiles.retain(|p| p.name != name);
        }
        Request::SetGameRules { rules } => {
            for r in &rules {
                r.validate()?;
                if !saved.profiles.iter().any(|p| p.name == r.profile) {
                    return Err(format!(
                        "the rule for {} names a missing profile \"{}\"",
                        r.process, r.profile
                    ));
                }
            }
            s.profiles.rules = rules;
        }
        Request::ImportProfile { profile, replace } => {
            profile.validate()?;
            let name = Profile::clean_name(&profile.name)?;
            if !replace && saved.profiles.iter().any(|p| p.name == name) {
                return Err(format!(
                    "a profile named \"{name}\" is saved already: replace it, or delete it first"
                ));
            }
            put(saved, Profile { name, ..*profile });
        }
        Request::CopyProfile { from, preset, to } => {
            let source = find(saved, &from, preset)?.profile;
            let base = to.unwrap_or(source.name.clone());
            let name = std::iter::once(base.clone())
                .chain((2..).map(|n| format!("{base} {n}")))
                .find(|n| !saved.profiles.iter().any(|p| &p.name == n))
                .expect("a free name");
            put(saved, Profile { name, ..source });
        }
        other => {
            return Err(format!(
                "{} is not a profiles request",
                method_name(&other)
            ))
        }
    }
    Ok(ResponseData::Success)
}

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    let mut saved = SAVED.lock().unwrap_or_else(|e| e.into_inner());
    if !saved.started {
        saved.started = true;
        if std::env::args().any(|a| a == "--profiles-demo") {
            demo(&mut saved, s);
        }
    }
    match req {
        Request::GetProfiles => {
            let now = state(s, &saved);
            ResponseData::Profiles {
                state: saved.stale.take().unwrap_or(now),
            }
        }
        Request::GetProfile { name, preset } => match find(&saved, &name, preset) {
            Ok(file) => ResponseData::Profile { file },
            Err(message) => error(message),
        },
        req => {
            let before = state(s, &saved);
            match change(req, s, &mut saved) {
                Ok(answer) => {
                    saved.stale.get_or_insert(before);
                    answer
                }
                Err(message) => error(message),
            }
        }
    }
}
