//! The replay recorder's command line, restart pacing, and clip files. Pure,
//! apart from reading and writing its own files.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use gg_ipc::{ClipInfo, ClipQuality, ClipsSettings};
use tracing::warn;

pub const PROGRAM: &str = "gpu-screen-recorder";
const MIN_DELAY: Duration = Duration::from_secs(5);
const MAX_DELAY: Duration = Duration::from_secs(60);
const HEALTHY_RUN: Duration = Duration::from_secs(60);
const VIDEO: [&str; 3] = ["mp4", "mkv", "webm"];
const MAX_CLIPS: usize = 200;

pub fn argv(s: &ClipsSettings, dir: &Path) -> Vec<String> {
    let quality = match s.quality {
        ClipQuality::Medium => "medium",
        ClipQuality::High => "high",
        ClipQuality::VeryHigh => "very_high",
        ClipQuality::Ultra => "ultra",
    };
    let mut v: Vec<String> = [
        PROGRAM, "-w", "portal", "-restore-portal-session", "yes", "-c", "mp4",
        "-f", &s.fps.to_string(), "-r", &s.seconds.to_string(), "-q", quality, "-o", &dir.to_string_lossy(),
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    if s.desktop_audio {
        v.extend(["-a".to_string(), "default_output".to_string()]);
    }
    if s.mic {
        v.extend(["-a".to_string(), "default_input".to_string()]);
    }
    v
}

pub fn resolve_dir(s: &ClipsSettings, home: &Path) -> PathBuf {
    s.dir.as_deref().map(PathBuf::from).unwrap_or_else(|| home.join("Videos/gglinux"))
}

/// How long to wait before restarting a recorder that ran for `ran_for` and died.
pub fn next_delay(ran_for: Duration, previous: Duration) -> Duration {
    if ran_for >= HEALTHY_RUN {
        MIN_DELAY
    } else {
        (previous * 2).clamp(MIN_DELAY, MAX_DELAY)
    }
}

fn is_video(name: &str) -> bool {
    Path::new(name).extension().and_then(OsStr::to_str).is_some_and(|e| VIDEO.iter().any(|v| v.eq_ignore_ascii_case(e)))
}

pub fn list(dir: &Path) -> Vec<ClipInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut clips: Vec<ClipInfo> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let meta = e.metadata().ok()?;
            (meta.is_file() && is_video(&name)).then(|| ClipInfo {
                name,
                bytes: meta.len(),
                modified: meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs()),
            })
        })
        .collect();
    clips.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.name.cmp(&b.name)));
    clips.truncate(MAX_CLIPS);
    clips
}

/// The file a delete request may touch: a plain video name that exists in `dir`.
pub fn clip_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || !is_video(name) {
        return Err(format!("{name:?} is not a clip name"));
    }
    let p = dir.join(name);
    if !p.is_file() {
        return Err(format!("no clip named {name}"));
    }
    Ok(p)
}

pub fn find_program(path_var: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path_var?).map(|d| d.join(PROGRAM)).find(|p| p.is_file())
}

pub fn load(path: &Path) -> ClipsSettings {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            let moved_to = gg_ipc::set_aside(path);
            warn!(path = %path.display(), error = %e, %moved_to, "ignoring unreadable clip settings");
            ClipsSettings::default()
        }),
        Err(_) => ClipsSettings::default(),
    }
}

pub fn save(path: &Path, s: &ClipsSettings) {
    let write = || -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(s)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    if let Err(e) = write() {
        warn!(path = %path.display(), error = %format!("{e:#}"), "could not save clip settings");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::ClipQuality;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gglinux-clips-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_command_line_follows_the_settings() {
        let s = ClipsSettings::default();
        assert_eq!(argv(&s, Path::new("/v")), [
            "gpu-screen-recorder", "-w", "portal", "-restore-portal-session", "yes", "-c", "mp4",
            "-f", "60", "-r", "30", "-q", "high", "-o", "/v", "-a", "default_output",
        ]);
        let s = ClipsSettings { desktop_audio: false, mic: true, quality: ClipQuality::VeryHigh, fps: 30, seconds: 90, ..s };
        let a = argv(&s, Path::new("/v"));
        assert!(a.windows(2).any(|w| w == ["-a", "default_input"]));
        assert!(!a.iter().any(|x| x == "default_output"));
        assert!(a.windows(2).any(|w| w == ["-q", "very_high"]) && a.windows(2).any(|w| w == ["-r", "90"]));
    }

    #[test]
    fn the_folder_defaults_under_videos() {
        assert_eq!(resolve_dir(&ClipsSettings::default(), Path::new("/home/u")), PathBuf::from("/home/u/Videos/gglinux"));
        let s = ClipsSettings { dir: Some("/data/clips".into()), ..ClipsSettings::default() };
        assert_eq!(resolve_dir(&s, Path::new("/home/u")), PathBuf::from("/data/clips"));
    }

    #[test]
    fn restarts_back_off_and_a_long_run_resets_them() {
        let s = Duration::from_secs;
        assert_eq!(next_delay(s(1), s(0)), s(5));
        assert_eq!(next_delay(s(1), s(5)), s(10));
        assert_eq!(next_delay(s(1), s(40)), s(60), "capped");
        assert_eq!(next_delay(s(61), s(60)), s(5), "it ran a while: start over");
    }

    #[test]
    fn clips_are_video_files_newest_first() {
        let d = scratch("list");
        for (name, age) in [("old.mp4", 30), ("new.MKV", 10), ("notes.txt", 0), ("mid.webm", 20)] {
            let p = d.join(name);
            std::fs::write(&p, b"x").unwrap();
            let t = std::time::SystemTime::now() - Duration::from_secs(age);
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
        }
        std::fs::create_dir(d.join("folder.mp4")).unwrap();
        let names: Vec<String> = list(&d).into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["new.MKV", "mid.webm", "old.mp4"]);
        assert!(list(&d.join("missing")).is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn only_plain_video_names_in_the_folder_can_be_deleted() {
        let d = scratch("delete");
        std::fs::write(d.join("a.mp4"), b"x").unwrap();
        std::fs::write(d.join("notes.txt"), b"x").unwrap();
        assert_eq!(clip_path(&d, "a.mp4").unwrap(), d.join("a.mp4"));
        for bad in ["../a.mp4", "sub/a.mp4", "", ".", "..", "notes.txt", "missing.mp4"] {
            assert!(clip_path(&d, bad).is_err(), "{bad:?}");
        }
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn the_program_is_found_on_path_or_not_at_all() {
        let d = scratch("path");
        assert_eq!(find_program(Some(d.as_os_str())), None);
        std::fs::write(d.join(PROGRAM), b"#!/bin/sh\n").unwrap();
        assert_eq!(find_program(Some(d.as_os_str())), Some(d.join(PROGRAM)));
        assert_eq!(find_program(None), None);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn settings_round_trip_and_garbage_means_defaults() {
        let d = scratch("settings");
        let p = d.join("clips.json");
        let s = ClipsSettings { enabled: true, seconds: 45, ..ClipsSettings::default() };
        save(&p, &s);
        assert_eq!(load(&p), s);
        std::fs::write(&p, "{ nope").unwrap();
        assert_eq!(load(&p), ClipsSettings::default());
        assert!(!p.exists(), "moved aside, so a save can't destroy it");
        std::fs::remove_dir_all(d).unwrap();
    }
}
