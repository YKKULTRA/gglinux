// SPDX-License-Identifier: GPL-3.0-or-later
//! What the actions start: the program itself for `Run`, `wtype` for `Type`,
//! `playerctl`/`wpctl` for `Media`. Nothing goes through a shell. The programs are
//! started by a [`Spawner`], so tests can see exactly what would run without
//! running it.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use tracing::{debug, warn};

/// Consumer-page usages (HID Usage Tables, page 0x0C) the media actions know.
pub const PLAY_PAUSE: u16 = 0xCD;
pub const NEXT: u16 = 0xB5;
pub const PREVIOUS: u16 = 0xB6;
pub const STOP: u16 = 0xB7;
pub const MUTE: u16 = 0xE2;
pub const VOLUME_UP: u16 = 0xE9;
pub const VOLUME_DOWN: u16 = 0xEA;

/// Starts programs for the actions.
pub trait Spawner: Send + Sync {
    /// Start `argv[0]` with the rest as its arguments, as they are: no shell. It runs
    /// on its own, in the home folder, with its output thrown away; nobody waits for it.
    fn spawn(&self, argv: &[String]) -> Result<(), String>;
    /// Whether `program` can be found in `PATH`.
    fn has(&self, program: &str) -> bool;
}

/// What a media key runs, or why it can't.
pub fn media(usage: u16, spawner: &dyn Spawner) -> Result<Vec<String>, String> {
    let (program, args): (&str, &[&str]) = match usage {
        PLAY_PAUSE => ("playerctl", &["play-pause"]),
        NEXT => ("playerctl", &["next"]),
        PREVIOUS => ("playerctl", &["previous"]),
        STOP => ("playerctl", &["stop"]),
        // At most 100 %, as the desktop's own volume keys.
        VOLUME_UP => ("wpctl", &["set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "5%+"]),
        VOLUME_DOWN => ("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%-"]),
        MUTE => ("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]),
        other => return Err(format!("gglinux doesn't know the media key {other:#06x}")),
    };
    if !spawner.has(program) {
        return Err(match program {
            "playerctl" => "media keys need playerctl: install it (sudo pacman -S playerctl)",
            _ => "volume keys need wpctl, which comes with WirePlumber",
        }
        .into());
    }
    Ok(std::iter::once(program)
        .chain(args.iter().copied())
        .map(String::from)
        .collect())
}

/// What a `Type` action runs: the text as one argument, after `--` so text that
/// starts with a dash is typed too.
pub fn typing(text: &str) -> Vec<String> {
    vec!["wtype".into(), "--".into(), text.into()]
}

/// Whether text can be typed here, or why not. `desktop` is `XDG_CURRENT_DESKTOP`,
/// `hyprland` whether `HYPRLAND_INSTANCE_SIGNATURE` is set.
pub fn can_type(desktop: &str, hyprland: bool, has_wtype: bool) -> Result<(), String> {
    let desktop = desktop.to_ascii_lowercase();
    if desktop.contains("kde") {
        return Err("Plasma needs its remote-desktop permission for typing; not built yet".into());
    }
    // wtype types through the virtual-keyboard protocol of wlroots and Hyprland.
    let wlroots = ["hyprland", "sway", "river", "wayfire", "labwc"];
    if !hyprland && !wlroots.iter().any(|w| desktop.contains(w)) {
        return Err(
            "typing works on Hyprland and other wlroots desktops (with wtype); not on this one yet"
                .into(),
        );
    }
    if !has_wtype {
        return Err("typing needs wtype: install it (sudo pacman -S wtype)".into());
    }
    Ok(())
}

/// The real [`Spawner`].
pub struct Processes;

impl Spawner for Processes {
    fn spawn(&self, argv: &[String]) -> Result<(), String> {
        use std::os::unix::process::CommandExt;
        let (program, args) = argv.split_first().ok_or("nothing to run")?;
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let mut child = Command::new(program)
            .args(args)
            .current_dir(home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Its own process group: a Ctrl+C meant for the daemon isn't its.
            .process_group(0)
            .spawn()
            .map_err(|e| format!("could not start {program}: {e}"))?;
        debug!(%program, pid = child.id(), "action started");
        // Reaped when it ends, so it doesn't linger as a zombie; nobody waits on this.
        let program = program.clone();
        std::thread::spawn(move || match child.wait() {
            Ok(status) if !status.success() => debug!(%program, %status, "action ended"),
            Ok(_) => {}
            Err(e) => warn!(%program, error = %e, "lost track of an action"),
        });
        Ok(())
    }

    fn has(&self, program: &str) -> bool {
        use std::os::unix::fs::PermissionsExt;
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&path).any(|dir| {
            std::fs::metadata(dir.join(program))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Has only the programs it's given.
    struct Only(&'static [&'static str]);

    impl Spawner for Only {
        fn spawn(&self, _argv: &[String]) -> Result<(), String> {
            unreachable!("these tests only ask what would run")
        }
        fn has(&self, program: &str) -> bool {
            self.0.contains(&program)
        }
    }

    #[test]
    fn media_keys_run_playerctl_and_wpctl() {
        let both = Only(&["playerctl", "wpctl"]);
        assert_eq!(media(PLAY_PAUSE, &both).unwrap(), ["playerctl", "play-pause"]);
        assert_eq!(media(NEXT, &both).unwrap(), ["playerctl", "next"]);
        assert_eq!(media(PREVIOUS, &both).unwrap(), ["playerctl", "previous"]);
        assert_eq!(
            media(VOLUME_UP, &both).unwrap(),
            ["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "5%+"]
        );
        assert_eq!(
            media(MUTE, &both).unwrap(),
            ["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]
        );
        assert!(media(0x30, &both).unwrap_err().contains("0x0030"));
        let none = Only(&[]);
        assert!(media(PLAY_PAUSE, &none)
            .unwrap_err()
            .contains("install it (sudo pacman -S playerctl)"));
        assert!(media(VOLUME_DOWN, &none).unwrap_err().contains("wpctl"));
    }

    #[test]
    fn text_goes_to_wtype_as_one_argument_after_the_dashes() {
        assert_eq!(typing("-v; rm -rf ~"), ["wtype", "--", "-v; rm -rf ~"]);
    }

    #[test]
    fn typing_works_on_hyprland_and_wlroots_with_wtype_and_says_why_not_elsewhere() {
        assert_eq!(can_type("Hyprland", true, true), Ok(()));
        assert_eq!(can_type("", true, true), Ok(()), "Hyprland's own variable");
        assert_eq!(can_type("sway", false, true), Ok(()));
        assert!(can_type("Hyprland", true, false)
            .unwrap_err()
            .contains("needs wtype"));
        assert_eq!(
            can_type("KDE", false, true),
            Err("Plasma needs its remote-desktop permission for typing; not built yet".into())
        );
        assert!(can_type("GNOME", false, true).is_err());
        assert!(can_type("", false, true).is_err());
    }
}
