// SPDX-License-Identifier: GPL-3.0-or-later
//! gglinux's own presets: profile files shipped with gglinux (`presets/*.json`), each saying in
//! its `about` that it's gglinux's own, not SteelSeries'. They set only the keyboard switches,
//! so applying one leaves everything else as it is. They're applied as they are, or copied
//! into a saved profile to change.

use gg_ipc::{PresetInfo, Profile, ProfileFile};

/// The preset files, in the order they're listed.
const FILES: [&str; 4] = [
    include_str!("presets/competitive-fps.json"),
    include_str!("presets/moba.json"),
    include_str!("presets/typing.json"),
    include_str!("presets/default-switches.json"),
];

pub fn all() -> Vec<ProfileFile> {
    FILES
        .iter()
        .map(|f| ProfileFile::parse(f).expect("a preset file reads (tested)"))
        .collect()
}

pub fn find(name: &str) -> Option<ProfileFile> {
    all().into_iter().find(|f| f.profile.name == name)
}

pub fn infos() -> Vec<PresetInfo> {
    all()
        .into_iter()
        .map(|f| PresetInfo {
            sections: f.profile.sections(),
            name: f.profile.name,
            about: f.about,
        })
        .collect()
}

/// A preset as a saved profile to change: its own name, unless that's taken.
pub fn as_profile(file: &ProfileFile, name: String) -> Profile {
    Profile {
        name,
        ..file.profile.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::{Section, TapMode};

    #[test]
    fn every_preset_reads_sets_only_the_switches_and_says_whose_it_is() {
        let all = all();
        assert_eq!(all.len(), FILES.len());
        for f in &all {
            assert_eq!(
                f.profile.sections(),
                [Section::Switches],
                "{}",
                f.profile.name
            );
            assert!(f.about.starts_with("gglinux's own preset"), "{}", f.about);
            assert!(
                !f.about.contains("GG's"),
                "never labelled as GG's: {}",
                f.about
            );
            let s = f.profile.switches.as_ref().unwrap();
            assert!(s.enabled, "a preset turns gglinux's switch settings on");
            assert!(!s.protection.enabled, "Protection Mode needs its gate");
            assert!(s.second.is_empty(), "deep presses need their gate");
        }
        let names: Vec<&str> = all.iter().map(|f| f.profile.name.as_str()).collect();
        assert_eq!(
            names,
            ["Competitive FPS", "MOBA", "Typing", "Default switches"]
        );
    }

    #[test]
    fn competitive_fps_is_what_its_about_says() {
        let fps = find("Competitive FPS").unwrap();
        let s = fps.profile.switches.unwrap();
        assert_eq!(s.actuation, 10, "1.0 mm");
        assert!(s.rapid_trigger.enabled);
        assert_eq!(s.rapid_trigger.keys, [0x1A, 0x04, 0x16, 0x07], "W A S D");
        assert_eq!(s.rapid_trigger.sensitivity, 2, "0.2 mm");
        assert!(s.rapid_tap.enabled);
        assert_eq!(s.rapid_tap.pairs.len(), 1);
        let pair = s.rapid_tap.pairs[0];
        assert_eq!(
            (pair.a, pair.b, pair.mode),
            (0x04, 0x07, TapMode::LastWins),
            "A and D"
        );
        let typing = find("Typing").unwrap().profile.switches.unwrap();
        assert_eq!(
            (typing.actuation, typing.rapid_trigger.enabled),
            (20, false)
        );
        assert!(find("Nope").is_none());
        assert_eq!(infos()[0].sections, [Section::Switches]);
    }
}
