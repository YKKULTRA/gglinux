// SPDX-License-Identifier: GPL-3.0-or-later
//! Games (GameSense), Moments (clips), and Settings: parts, profiles, start at
//! login and backups.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gg_ipc::{ClipQuality, ClipsSettings, GameRule, Part, Parts, Request};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::client::{Client, State, SLOW, TIMEOUT};
use crate::{
    act, act_then, refresh, sync_rows, system, AppWindow, Clip, Game, GamesUi, MomentsUi, PartRow,
    PartsUi, ProfilesUi, RuleRow, SettingsUi,
};

fn strings(v: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        v.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    ))
}

// --- Games ---------------------------------------------------------------------

pub fn show_games(ui: &AppWindow, s: &State) {
    let g = ui.global::<GamesUi>();
    let Some(gs) = s.gamesense.as_ref() else {
        g.set_known(false);
        return;
    };
    g.set_known(true);
    g.set_enabled(gs.enabled);
    g.set_listening(
        format!(
            "Listening on {} · in {} Proton prefix{}",
            gs.address.as_deref().unwrap_or("–"),
            gs.prefixes,
            if gs.prefixes == 1 { "" } else { "es" }
        )
        .into(),
    );
    let games: Vec<Game> = gs
        .games
        .iter()
        .map(|x| Game {
            name: x
                .display_name
                .clone()
                .unwrap_or_else(|| x.name.clone())
                .into(),
            who: x.developer.clone().unwrap_or_default().into(),
            events: draws_line(x).into(),
            notes: x.screen_notes().join(" · ").into(),
            enabled: x.enabled,
            active: x.active,
            screen: x.on_screen,
        })
        .collect();
    sync_rows(g.get_games(), games, |m| g.set_games(m));
}

/// A game's events, each with what it draws on: "HEALTH: keys and screen · AMMO: keys".
fn draws_line(game: &gg_ipc::GameInfo) -> String {
    game.draws_by_event()
        .into_iter()
        .map(|(event, draws)| {
            let labels: Vec<String> = draws.iter().map(|d| d.label()).collect();
            if labels.is_empty() {
                event.to_owned()
            } else {
                format!("{event}: {}", labels.join(" and "))
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn mark_games(ui: &AppWindow, busy: bool, error: SharedString) {
    let g = ui.global::<GamesUi>();
    g.set_busy(busy);
    g.set_error(error);
}

pub fn bind_games(ui: &AppWindow, client: &Client) {
    let g = ui.global::<GamesUi>();
    let send = |make: fn(&gg_ipc::GameSenseState, i32, bool) -> Option<Request>| {
        let weak = ui.as_weak();
        let client = client.clone();
        move |i: i32, on: bool| {
            let (Some(ui), Some(gs)) = (weak.upgrade(), client.state().gamesense) else {
                return;
            };
            if let Some(req) = make(&gs, i, on) {
                act(&ui, &client, req, TIMEOUT, mark_games);
            }
        }
    };
    let f = send(|_, _, on| Some(Request::SetGamesenseEnabled { enabled: on }));
    g.on_set_enabled(move |on| f(0, on));
    let f = send(|_, _, _| Some(Request::GamesenseTest));
    g.on_test(move || f(0, false));
    let f = send(|_, _, _| Some(Request::GamesenseTestScreen));
    g.on_test_screen(move || f(0, false));
    g.on_set_game(send(|gs, i, on| {
        gs.games.get(i as usize).map(|x| Request::SetGameEnabled {
            game: x.name.clone(),
            enabled: on,
        })
    }));
    let f = send(|gs, i, _| {
        gs.games.get(i as usize).map(|x| Request::RemoveGame {
            game: x.name.clone(),
        })
    });
    g.on_remove(move |i| f(i, false));
}

// --- Moments -------------------------------------------------------------------

const QUALITIES: [ClipQuality; 4] = [
    ClipQuality::Medium,
    ClipQuality::High,
    ClipQuality::VeryHigh,
    ClipQuality::Ultra,
];

/// The usual rates, plus whatever clips.json holds (15 to 240 are allowed).
pub fn rates(fps: u32) -> Vec<u32> {
    let mut r = vec![30, 60, 120, 144];
    if !r.contains(&fps) {
        r.push(fps);
    }
    r.sort_unstable();
    r
}

pub fn size(bytes: u64) -> String {
    if bytes as f64 > 1e9 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.1} MB", bytes as f64 / 1e6)
    }
}

/// A file time in local time, `YYYY-MM-DD HH:MM`.
pub fn when(unix: u64) -> String {
    let t = unix as libc::time_t;
    // SAFETY: localtime_r writes only into `tm`, which outlives the call.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return system::stamp(unix);
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

pub fn show_moments(ui: &AppWindow, s: &State) {
    let g = ui.global::<MomentsUi>();
    let Some(st) = s.clips.as_ref() else {
        g.set_known(false);
        g.set_confirm(SharedString::new());
        return;
    };
    let set = &st.settings;
    g.set_known(true);
    g.set_profiles_off(!s.running().profiles);
    g.set_available(st.available);
    g.set_enabled(st.enabled);
    g.set_recording(st.recording);
    g.set_games_only(set.games_only);
    let now = if st.recording {
        match (&st.game, set.games_only) {
            (Some(game), true) => format!(" Recording while {game} runs."),
            _ => " Recording now.".into(),
        }
    } else {
        String::new()
    };
    g.set_keeping(
        format!(
            "While on, the last {} seconds of the screen are kept in memory and nothing is written until you save.{now}",
            set.seconds
        )
        .into(),
    );
    g.set_waiting(st.waiting);
    g.set_seconds(set.seconds as i32);
    let r = rates(set.fps);
    g.set_rates(strings(r.iter().map(|f| format!("{f} fps"))));
    g.set_rate(r.iter().position(|f| *f == set.fps).unwrap_or(0) as i32);
    g.set_quality(
        QUALITIES
            .iter()
            .position(|q| *q == set.quality)
            .unwrap_or(1) as i32,
    );
    g.set_desktop_audio(set.desktop_audio);
    g.set_mic(set.mic);
    let (hotkey, bad) = match (&st.hotkey, &st.hotkey_error) {
        (Some(k), _) => (format!("Hotkey: {k}"), false),
        (None, Some(e)) => (e.clone(), true),
        (None, None) => (
            "No hotkey yet: when clips start, KDE asks to confirm Alt+S (Hyprland takes it from its config).".into(),
            false,
        ),
    };
    g.set_hotkey(hotkey.into());
    g.set_hotkey_bad(bad);
    g.set_recorder_error(st.error.clone().unwrap_or_default().into());
    g.set_dir(st.dir.clone().into());
    let clips: Vec<Clip> = st
        .clips
        .iter()
        .map(|c| Clip {
            name: c.name.clone().into(),
            meta: format!("{} · {}", size(c.bytes), when(c.modified)).into(),
        })
        .collect();
    sync_rows(g.get_clips(), clips, |m| g.set_clips(m));
    // A clip that went while its Delete waited for a yes takes the question with it.
    if !st.clips.iter().any(|c| c.name == g.get_confirm().as_str()) {
        g.set_confirm(SharedString::new());
    }
}

fn mark_moments(ui: &AppWindow, busy: bool, error: SharedString) {
    let g = ui.global::<MomentsUi>();
    g.set_busy(busy);
    // A change that didn't take: the controls go back to the settings in force
    // (the frame-rate list holds on to the pick it was given until told).
    if !error.is_empty() {
        refresh(ui);
    }
    g.set_error(error);
}

pub fn bind_moments(ui: &AppWindow, client: &Client) {
    let g = ui.global::<MomentsUi>();
    // Each change sends the whole settings, so the controls wait while one is in
    // flight: a second change built from the old settings would undo the first.
    let set = |patch: fn(&mut ClipsSettings, i32)| {
        let weak = ui.as_weak();
        let client = client.clone();
        move |v: i32| {
            let (Some(ui), Some(st)) = (weak.upgrade(), client.state().clips) else {
                return;
            };
            let mut settings = st.settings.clone();
            patch(&mut settings, v);
            // Restarts the recorder, which can take it 4 s to stop.
            act(
                &ui,
                &client,
                Request::SetClipsSettings { settings },
                SLOW,
                mark_moments,
            );
        }
    };
    let f = set(|s, v| s.enabled = v != 0);
    g.on_set_enabled(move |on| f(on as i32));
    let f = set(|s, v| s.games_only = v != 0);
    g.on_set_games_only(move |on| f(on as i32));
    g.on_set_seconds(set(|s, v| s.seconds = (v.clamp(5, 300) / 5 * 5) as u32));
    g.on_set_rate(set(|s, i| {
        if let Some(f) = rates(s.fps).get(i as usize) {
            s.fps = *f;
        }
    }));
    g.on_set_quality(set(|s, i| s.quality = QUALITIES[(i.clamp(0, 3)) as usize]));
    let f = set(|s, v| s.desktop_audio = v != 0);
    g.on_set_desktop_audio(move |on| f(on as i32));
    let f = set(|s, v| s.mic = v != 0);
    g.on_set_mic(move |on| f(on as i32));
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_save(move || {
        // The new file shows up a moment later, in a notification.
        if let Some(ui) = weak.upgrade() {
            act(&ui, &c, Request::SaveClip, TIMEOUT, mark_moments);
        }
    });
    let weak = ui.as_weak();
    let c = client.clone();
    // By name: the clip asked about, whatever moved in the list meanwhile.
    g.on_delete(move |name| {
        let Some(ui) = weak.upgrade() else { return };
        ui.global::<MomentsUi>().set_confirm(SharedString::new());
        let name = name.to_string();
        act(&ui, &c, Request::DeleteClip { name }, TIMEOUT, mark_moments);
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_open_folder(move || {
        let (Some(ui), Some(st)) = (weak.upgrade(), c.state().clips) else {
            return;
        };
        if let Err(e) = system::open_folder(Path::new(&st.dir)) {
            ui.global::<MomentsUi>().set_error(e.into());
        }
    });
}

// --- Settings: parts -----------------------------------------------------------

struct PartText {
    what: &'static str,
    /// Said when a part that runs is about to be turned off.
    off: Option<&'static str>,
}

fn text(p: Part) -> PartText {
    match p {
        Part::Keyboard => PartText {
            what: "Apex Pro: lighting, screen and switches.",
            off: None,
        },
        Part::Headset => PartText {
            what: "Arctis Nova Pro: battery, controls, and keeping the base station's screen dark.",
            off: Some("The base station's screen won't be kept dark any more, so it can burn in."),
        },
        Part::Mouse => PartText {
            what: "Rival 3: DPI, polling rate, buttons and battery.",
            off: None,
        },
        Part::Sonar => PartText {
            what: "Audio channels, EQ, the mic, Smart Volume and streamer mode.",
            off: Some(
                "If Sonar's channels are on, they're taken out of PipeWire (it restarts once).",
            ),
        },
        Part::Gamesense => PartText {
            what: "Games drive the keyboard lighting.",
            off: None,
        },
        Part::Profiles => PartText {
            what: "Saved lighting, switched by the game you're playing.",
            off: Some(
                "Games-only clips find the game through Profiles' rules, so they won't start.",
            ),
        },
        Part::Clips => PartText {
            what: "Instant replay and the Moments page.",
            off: None,
        },
    }
}

/// The note under a part, for the draft against what runs.
pub fn note(part: Part, draft: &Parts, saved: &Parts) -> String {
    if part == Part::Gamesense && draft.gamesense && !draft.keyboard {
        return "GameSense draws on the keyboard, so it waits until Keyboard is on.".into();
    }
    match text(part).off {
        Some(off) if !draft.on(part) && saved.on(part) => off.into(),
        _ => String::new(),
    }
}

/// The parts being chosen: they start from what the daemon runs, and again
/// after each restart.
#[derive(Default)]
pub struct PartsForm {
    pub draft: Option<Parts>,
    seen: Option<Parts>,
}

pub fn show_parts(ui: &AppWindow, s: &State, form: &RefCell<PartsForm>) {
    let saved = s.parts.unwrap_or_default();
    let mut f = form.borrow_mut();
    if f.seen != s.parts || f.draft.is_none() {
        f.seen = s.parts;
        f.draft = Some(saved);
    }
    let draft = f.draft.unwrap_or_default();
    let rows: Vec<PartRow> = Part::ALL
        .iter()
        .map(|p| PartRow {
            label: p.label().into(),
            what: text(*p).what.into(),
            on: draft.on(*p),
            note: note(*p, &draft, &saved).into(),
        })
        .collect();
    let g = ui.global::<PartsUi>();
    sync_rows(g.get_parts(), rows, |m| g.set_parts(m));
    g.set_changed(draft != saved);
    g.set_busy(s.restarting);
}

pub fn bind_parts(ui: &AppWindow, client: &Client, form: Rc<RefCell<PartsForm>>) {
    let g = ui.global::<PartsUi>();
    let weak = ui.as_weak();
    let c = client.clone();
    let f = form.clone();
    g.on_set(move |i, on| {
        if let Some(part) = Part::ALL.get(i as usize) {
            let mut form = f.borrow_mut();
            let mut draft = form.draft.unwrap_or_default();
            draft.set(*part, on);
            form.draft = Some(draft);
        }
        if let Some(ui) = weak.upgrade() {
            show_parts(&ui, &c.state(), &f);
        }
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_apply(move || {
        let Some(ui) = weak.upgrade() else { return };
        let Some(parts) = form.borrow().draft else {
            return;
        };
        ui.global::<PartsUi>().set_busy(true);
        ui.global::<PartsUi>().set_error(SharedString::new());
        let weak = ui.as_weak();
        // A change makes the daemon start again with the new parts; the reconnect
        // reads them, and the page follows.
        c.set_parts(parts, move |result| {
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let g = ui.global::<PartsUi>();
                if let Err(e) = result {
                    g.set_busy(false);
                    g.set_error(e.into());
                }
            });
        });
    });
}

// --- Settings: profiles --------------------------------------------------------

/// How many running programs the picker lists at most: typing narrows it.
const MAX_PICKS: usize = 40;

/// The picker beside the program box: a line saying what it holds, then the
/// running programs that fit what's typed (all of them while nothing is),
/// sorted. The flag says whether there's anything to pick.
pub fn picks(running: &[String], typed: &str) -> (Vec<String>, bool) {
    let typed = typed.trim().to_lowercase();
    let mut fit: Vec<&String> = running
        .iter()
        .filter(|p| p.to_lowercase().contains(&typed))
        .collect();
    fit.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b)));
    fit.dedup();
    let head = if running.is_empty() {
        "Nothing seen running"
    } else if fit.is_empty() {
        "No running program fits"
    } else {
        "Pick a running program"
    };
    let any = !fit.is_empty();
    let list = std::iter::once(head.to_string())
        .chain(fit.into_iter().take(MAX_PICKS).cloned())
        .collect();
    (list, any)
}

/// Whether saving under `name` replaces a saved profile (the daemon trims it).
pub fn taken(profiles: &[String], name: &str) -> bool {
    let name = name.trim();
    !name.is_empty() && profiles.iter().any(|p| p == name)
}

fn show_picks(g: &ProfilesUi<'_>, running: &[String], typed: &str) {
    let (list, any) = picks(running, typed);
    g.set_running(strings(list));
    g.set_has_running(any);
    g.set_running_pick(0);
}

pub fn show_profiles(ui: &AppWindow, s: &State) {
    let g = ui.global::<ProfilesUi>();
    let Some(st) = s.profiles.as_ref() else {
        g.set_known(false);
        g.set_confirm(SharedString::new());
        return;
    };
    g.set_known(true);
    g.set_now(
        st.active
            .as_ref()
            .map(|a| format!("Now: {}, because {} is running", a.profile, a.process))
            .unwrap_or_default()
            .into(),
    );
    g.set_profiles(strings(st.profiles.iter().cloned()));
    let rules: Vec<RuleRow> = st
        .rules
        .iter()
        .map(|r| RuleRow {
            process: r.process.clone().into(),
            profile: r.profile.clone().into(),
        })
        .collect();
    sync_rows(g.get_rules(), rules, |m| g.set_rules(m));
    show_picks(&g, &st.running, &g.get_new_process());
    g.set_name_taken(taken(&st.profiles, &g.get_new_name()));
    // A rule needs a profile: the first is picked until another is.
    let count = st.profiles.len() as i32;
    let pick = g.get_new_profile();
    if pick >= count || (pick < 0 && count > 0) {
        g.set_new_profile(if count > 0 { 0 } else { -1 });
    }
    if !st.profiles.iter().any(|p| p == g.get_confirm().as_str()) {
        g.set_confirm(SharedString::new());
    }
}

fn mark_profiles(ui: &AppWindow, busy: bool, error: SharedString) {
    let g = ui.global::<ProfilesUi>();
    g.set_busy(busy);
    g.set_error(error);
}

pub fn bind_profiles(ui: &AppWindow, client: &Client) {
    let g = ui.global::<ProfilesUi>();
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_apply(move |i| {
        let (Some(ui), Some(st)) = (weak.upgrade(), c.state().profiles) else {
            return;
        };
        if let Some(name) = st.profiles.get(i as usize).cloned() {
            act(
                &ui,
                &c,
                Request::ApplyProfile { name },
                TIMEOUT,
                mark_profiles,
            );
        }
    });
    let weak = ui.as_weak();
    let c = client.clone();
    // By name: the profile asked about, whatever moved in the list meanwhile.
    g.on_delete(move |name| {
        let Some(ui) = weak.upgrade() else { return };
        ui.global::<ProfilesUi>().set_confirm(SharedString::new());
        let name = name.to_string();
        act(
            &ui,
            &c,
            Request::DeleteProfile { name },
            TIMEOUT,
            mark_profiles,
        );
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_name_edited(move |name| {
        let (Some(ui), Some(st)) = (weak.upgrade(), c.state().profiles) else {
            return;
        };
        ui.global::<ProfilesUi>()
            .set_name_taken(taken(&st.profiles, &name));
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_process_edited(move |typed| {
        let (Some(ui), Some(st)) = (weak.upgrade(), c.state().profiles) else {
            return;
        };
        show_picks(&ui.global::<ProfilesUi>(), &st.running, &typed);
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_save(move || {
        let Some(ui) = weak.upgrade() else { return };
        // A blank or too long name is the daemon's to refuse, in its words.
        let name = ui.global::<ProfilesUi>().get_new_name().trim().to_string();
        act_then(
            &ui,
            &c,
            Request::SaveProfile { name },
            TIMEOUT,
            mark_profiles,
            |ui, _| {
                let g = ui.global::<ProfilesUi>();
                g.set_new_name(SharedString::new());
                g.set_name_taken(false);
            },
        );
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_remove_rule(move |i| {
        let (Some(ui), Some(st)) = (weak.upgrade(), c.state().profiles) else {
            return;
        };
        let mut rules = st.rules.clone();
        if (i as usize) < rules.len() {
            rules.remove(i as usize);
            act(
                &ui,
                &c,
                Request::SetGameRules { rules },
                TIMEOUT,
                mark_profiles,
            );
        }
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_add_rule(move || {
        let (Some(ui), Some(st)) = (weak.upgrade(), c.state().profiles) else {
            return;
        };
        let g = ui.global::<ProfilesUi>();
        let process = g.get_new_process().trim().to_string();
        let Some(profile) = usize::try_from(g.get_new_profile())
            .ok()
            .and_then(|i| st.profiles.get(i).cloned())
        else {
            return;
        };
        if process.is_empty() {
            return;
        }
        let mut rules = st.rules.clone();
        rules.push(GameRule { process, profile });
        act_then(
            &ui,
            &c,
            Request::SetGameRules { rules },
            TIMEOUT,
            mark_profiles,
            |ui, _| {
                ui.global::<ProfilesUi>()
                    .set_new_process(SharedString::new());
            },
        );
    });
    let c = client.clone();
    // Notifications carry no list of running programs; a read does.
    g.on_refresh(move || c.refresh_part(Part::Profiles));
}

// --- Settings: start at login and backups -------------------------------------

pub fn show_settings(ui: &AppWindow) {
    let g = ui.global::<SettingsUi>();
    g.set_autostart_known(true);
    g.set_autostart(system::autostart_on(&system::autostart_dir()));
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let tray = system::tray_command(&system::this_exe());
    g.set_reads_autostart(system::reads_autostart(&desktop));
    g.set_desktop(desktop.into());
    g.set_lua_line(format!("hl.exec_cmd(\"{}\")", tray.replace('"', "\\\"")).into());
    g.set_conf_line(format!("exec-once = {tray}").into());
    g.set_version(env!("CARGO_PKG_VERSION").into());
}

fn mark_settings(ui: &AppWindow, busy: bool, error: SharedString) {
    let g = ui.global::<SettingsUi>();
    g.set_busy(busy);
    g.set_error(error);
}

/// An action starts: busy, and the last one's note and error go.
fn begin(g: &SettingsUi<'_>) {
    g.set_busy(true);
    g.set_note(SharedString::new());
    g.set_error(SharedString::new());
}

fn forget_restore(g: &SettingsUi<'_>) {
    g.set_restore_path(SharedString::new());
    g.set_restore_what(SharedString::new());
}

/// What restoring a backup does, said before it's done.
pub fn restore_what(info: &system::BackupInfo) -> String {
    let made = info
        .created
        .map(|t| format!(", made {}", when(t)))
        .unwrap_or_default();
    format!(
        "It holds {}{made}. They replace the settings you have now, and gglinuxd restarts to load them.",
        info.files.join(", ")
    )
}

/// A backup was picked or typed and read: ask before restoring it.
fn offer_restore(ui: &AppWindow, path: &Path, info: Result<system::BackupInfo, String>) {
    let g = ui.global::<SettingsUi>();
    g.set_busy(false);
    match info {
        Err(e) => g.set_error(e.into()),
        Ok(info) if info.files.is_empty() => {
            g.set_note("The backup holds no settings, so there's nothing to restore.".into())
        }
        Ok(info) => {
            g.set_typing(false);
            g.set_restore_path(path.display().to_string().into());
            g.set_restore_what(restore_what(&info).into());
        }
    }
}

/// Run blocking work off the window's thread, then `done` on it.
fn off_thread<T: Send + 'static>(
    ui: &AppWindow,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&AppWindow, T) + Send + 'static,
) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let out = work();
        let _ = weak.upgrade_in_event_loop(move |ui| done(&ui, out));
    });
}

/// What the desktop's file chooser came back with.
enum Picked {
    File(PathBuf, Result<system::BackupInfo, String>),
    Cancelled,
    /// It didn't open or failed: the path can be typed instead.
    NoChooser(String),
}

pub fn bind_settings(ui: &AppWindow, client: &Client) {
    let g = ui.global::<SettingsUi>();
    let weak = ui.as_weak();
    g.on_opened(move || {
        if let Some(ui) = weak.upgrade() {
            show_settings(&ui);
        }
    });
    let weak = ui.as_weak();
    g.on_set_autostart(move |on| {
        let Some(ui) = weak.upgrade() else { return };
        begin(&ui.global::<SettingsUi>());
        // systemctl can take a moment. Read it back whatever happened: the entry
        // is written even when the service step fails, and a refused change has
        // to untick the box.
        off_thread(
            &ui,
            move || {
                system::set_autostart(
                    &system::autostart_dir(),
                    &system::this_exe(),
                    on,
                    &system::run_quietly,
                )
            },
            |ui, result| {
                show_settings(ui);
                mark_settings(ui, false, result.err().unwrap_or_default().into());
            },
        );
    });
    let weak = ui.as_weak();
    g.on_export(move || {
        let Some(ui) = weak.upgrade() else { return };
        let g = ui.global::<SettingsUi>();
        begin(&g);
        match system::export_settings() {
            Ok(path) => g.set_exported(path.display().to_string().into()),
            Err(e) => g.set_error(e.into()),
        }
        g.set_busy(false);
    });
    let weak = ui.as_weak();
    g.on_open_export_folder(move || {
        let Some(ui) = weak.upgrade() else { return };
        let g = ui.global::<SettingsUi>();
        let exported = PathBuf::from(g.get_exported().as_str());
        let folder = exported
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| "/".into());
        if let Err(e) = system::open_folder(&folder) {
            g.set_error(e.into());
        }
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_restore(move || {
        let Some(ui) = weak.upgrade() else { return };
        let g = ui.global::<SettingsUi>();
        begin(&g);
        forget_restore(&g);
        let weak = ui.as_weak();
        c.spawn(async move {
            let filter: (&str, &[&str]) = ("Backups", &["application/json", "*.json"]);
            let picked = match system::pick_file("A gglinux backup", filter).await {
                Ok(Some(path)) => {
                    let p = path.clone();
                    let info = tokio::task::spawn_blocking(move || system::read_backup(&p))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()));
                    Picked::File(path, info)
                }
                Ok(None) => Picked::Cancelled,
                Err(e) => Picked::NoChooser(e),
            };
            let _ = weak.upgrade_in_event_loop(move |ui| match picked {
                Picked::File(path, info) => offer_restore(&ui, &path, info),
                Picked::Cancelled => ui.global::<SettingsUi>().set_busy(false),
                Picked::NoChooser(e) => {
                    let g = ui.global::<SettingsUi>();
                    mark_settings(&ui, false, e.into());
                    g.set_typing(true);
                    if g.get_typed_path().is_empty() {
                        let docs = system::export_dir(&system::home());
                        g.set_typed_path(format!("{}/", docs.display()).into());
                    }
                }
            });
        });
    });
    let weak = ui.as_weak();
    g.on_restore_typed(move || {
        let Some(ui) = weak.upgrade() else { return };
        let g = ui.global::<SettingsUi>();
        begin(&g);
        match system::typed_path(&g.get_typed_path(), &system::home()) {
            Ok(path) => offer_restore(&ui, &path, system::read_backup(&path)),
            Err(e) => mark_settings(&ui, false, e.into()),
        }
    });
    let weak = ui.as_weak();
    g.on_confirm_restore(move || {
        let Some(ui) = weak.upgrade() else { return };
        let g = ui.global::<SettingsUi>();
        let path = PathBuf::from(g.get_restore_path().as_str());
        begin(&g);
        // The restart waits for the daemon to stop and start again.
        off_thread(
            &ui,
            move || system::import_settings(&path),
            |ui, result| {
                let g = ui.global::<SettingsUi>();
                forget_restore(&g);
                match result {
                    Ok(note) => {
                        g.set_note(note.into());
                        mark_settings(ui, false, SharedString::new());
                    }
                    Err(e) => mark_settings(ui, false, e.into()),
                }
            },
        );
    });
    let weak = ui.as_weak();
    g.on_cancel_restore(move || {
        if let Some(ui) = weak.upgrade() {
            forget_restore(&ui.global::<SettingsUi>());
        }
    });
}

/// The `shot` example's scripted steps for these pages (see [`crate::poke`]).
/// Steps that only draw a state set it on the page and ask nothing. None of
/// them restores a backup, writes one, or touches start at login: those act
/// on the real settings of whoever runs the shot.
pub(crate) fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let i = n.max(0) as usize;
    let games = ui.global::<GamesUi>();
    let moments = ui.global::<MomentsUi>();
    let parts = ui.global::<PartsUi>();
    let profiles = ui.global::<ProfilesUi>();
    let settings = ui.global::<SettingsUi>();
    match name {
        "games.enable" => games.invoke_set_enabled(true),
        "games.test" => games.invoke_test(),
        "games.off" => games.invoke_set_game(n, false),
        "moments.enable" => moments.invoke_set_enabled(true),
        "moments.games-only" => moments.invoke_set_games_only(true),
        "moments.save" => moments.invoke_save(),
        "moments.ask-delete" => {
            if let Some(c) = moments.get_clips().row_data(i) {
                moments.set_confirm(c.name);
            }
        }
        // As on a machine without gpu-screen-recorder.
        "moments.no-recorder" => moments.set_available(false),
        "parts.toggle" => {
            if let Some(p) = parts.get_parts().row_data(i) {
                parts.invoke_set(n, !p.on);
            }
        }
        "parts.apply" => parts.invoke_apply(),
        "profiles.name" => {
            profiles.set_new_name("Evening".into());
            profiles.invoke_name_edited("Evening".into());
        }
        "profiles.save" => {
            profiles.set_new_name("Evening".into());
            profiles.invoke_save();
        }
        "profiles.rule" => {
            profiles.set_new_process("RDR2.exe".into());
            profiles.set_new_profile(n);
            profiles.invoke_add_rule();
        }
        "profiles.ask-delete" => {
            if let Some(p) = profiles.get_profiles().row_data(i) {
                profiles.set_confirm(p);
            }
        }
        "settings.hyprland" => {
            settings.set_desktop("Hyprland".into());
            settings.set_reads_autostart(false);
        }
        "settings.exported" => settings
            .set_exported("/home/you/Documents/gglinux-backup-2026-10-29_12-00-00.json".into()),
        // Headless there's no session bus, so the file chooser fails and the
        // path box shows.
        "settings.restore" => settings.invoke_restore(),
        // Reads `$XDG_RUNTIME_DIR/backup.json` (the fake daemon's folder) and
        // asks; it stops at the question.
        "settings.typed" => {
            let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
            settings.set_typed_path(format!("{dir}/backup.json").into());
            settings.invoke_restore_typed();
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_games_row_says_what_each_event_draws() {
        use gg_ipc::{Draws, GameInfo, HandlerInfo};
        let h = |event: &str, draws: Draws| HandlerInfo {
            event: event.into(),
            draws,
        };
        let game = GameInfo {
            name: "G".into(),
            display_name: None,
            developer: None,
            enabled: true,
            active: false,
            events: vec!["AMMO".into(), "HEALTH".into(), "OLD".into()],
            handlers: vec![
                h("AMMO", Draws::Keys),
                h("HEALTH", Draws::Keys),
                h("HEALTH", Draws::Screen { notes: Vec::new() }),
            ],
            on_screen: false,
        };
        assert_eq!(
            draws_line(&game),
            "AMMO: keys · HEALTH: keys and screen · OLD",
            "an event with nothing known about it is just its name"
        );
    }

    #[test]
    fn the_rate_list_keeps_an_unusual_saved_rate() {
        assert_eq!(rates(60), vec![30, 60, 120, 144]);
        assert_eq!(rates(90), vec![30, 60, 90, 120, 144]);
    }

    #[test]
    fn sizes_read_in_megabytes_until_a_gigabyte() {
        assert_eq!(size(12_300_000), "12.3 MB");
        assert_eq!(size(2_500_000_000), "2.5 GB");
    }

    #[test]
    fn the_program_picker_narrows_to_what_is_typed() {
        let running: Vec<String> = ["steam", "RDR2.exe", "rdr2-launcher", "bash", "steam"]
            .map(String::from)
            .to_vec();
        let (all, any) = picks(&running, "");
        assert!(any);
        assert_eq!(
            all,
            [
                "Pick a running program",
                "bash",
                "rdr2-launcher",
                "RDR2.exe",
                "steam"
            ],
            "sorted without case, once each"
        );
        let (fit, _) = picks(&running, " rdr ");
        assert_eq!(fit, ["Pick a running program", "rdr2-launcher", "RDR2.exe"]);
        assert_eq!(
            picks(&running, "nope"),
            (vec!["No running program fits".into()], false)
        );
        assert_eq!(picks(&[], ""), (vec!["Nothing seen running".into()], false));
        let many: Vec<String> = (0..100).map(|i| format!("p{i:03}")).collect();
        assert_eq!(picks(&many, "").0.len(), MAX_PICKS + 1);
    }

    #[test]
    fn saving_over_a_profile_is_said_before_it_happens() {
        let saved = vec!["Evening".to_string(), "Work".to_string()];
        assert!(taken(&saved, "Evening"));
        assert!(taken(&saved, "  Work "), "the daemon trims the name");
        assert!(!taken(&saved, "evening"), "names are exact");
        assert!(!taken(&saved, "   "));
    }

    #[test]
    fn a_restore_says_what_it_replaces() {
        let info = system::BackupInfo {
            files: vec!["lighting.json".into(), "parts.json".into()],
            created: None,
        };
        assert_eq!(
            restore_what(&info),
            "It holds lighting.json, parts.json. They replace the settings you have now, and gglinuxd restarts to load them."
        );
        let dated = system::BackupInfo {
            created: Some(1_790_606_404),
            ..info
        };
        assert!(restore_what(&dated).contains(&format!(", made {}.", when(1_790_606_404))));
    }

    #[test]
    fn a_note_warns_only_about_turning_off_what_runs() {
        let saved = Parts::default();
        let mut draft = saved;
        assert_eq!(note(Part::Headset, &draft, &saved), "");
        draft.headset = false;
        assert!(note(Part::Headset, &draft, &saved).contains("burn in"));
        let off = Parts {
            headset: false,
            ..saved
        };
        assert_eq!(
            note(Part::Headset, &draft, &off),
            "",
            "already off: nothing new to say"
        );
        draft.keyboard = false;
        assert!(note(Part::Gamesense, &draft, &saved).contains("waits until Keyboard"));
    }
}
