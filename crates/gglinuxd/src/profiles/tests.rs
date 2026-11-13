// SPDX-License-Identifier: GPL-3.0-or-later
//! The controller against a daemon in memory that lags like a real one.

use std::sync::Arc;

use gg_ipc::{Effect, Lighting, RuleMatch, RuleWhen, Switches};

use super::focus::testing::fake_hyprland;
use super::sections::testing::{Fake, Shared};
use super::*;

fn look(p: f32) -> Lighting {
    Lighting {
        enabled: true,
        effect: Effect::Wave { period_s: p },
        ..Lighting::default()
    }
}

fn lit(name: &str, p: f32) -> Profile {
    Profile {
        lighting: Some(look(p)),
        ..Profile::named(name)
    }
}

fn switches(actuation: u8) -> Switches {
    Switches {
        enabled: true,
        actuation,
        ..Switches::default()
    }
}

/// The scratch folder of one test, removed when it ends.
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        let d = std::env::temp_dir().join(format!(
            "gg-prof-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
    fn file(&self) -> PathBuf {
        self.0.join("profiles.json")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rules_store(rules: Vec<GameRule>) -> Store {
    let mut store = Store::default();
    store.put(lit("A", 2.0)).unwrap();
    store.put(lit("B", 3.0)).unwrap();
    store.set_rules(rules).unwrap();
    store
}

/// A controller with profiles A and B and a rule for each, wired to a fake daemon whose
/// keyboard has the owner's own lighting.
fn controller(d: &Dir) -> (Controller, Arc<Fake>) {
    let store = rules_store(vec![
        GameRule::running("game-a", "A"),
        GameRule::running("game-b", "B"),
    ]);
    wired(d, store)
}

fn wired(d: &Dir, store: Store) -> (Controller, Arc<Fake>) {
    let fake = Fake::new();
    fake.now.lock().unwrap().keyboard.lighting = look(1.0);
    let mut c = Controller::at(&d.file(), store);
    c.connect(Arc::new(Shared(Arc::clone(&fake))));
    // No programs running but the ones a test names.
    c.proc_root = d.0.join("proc");
    (c, fake)
}

fn lighting(fake: &Fake) -> Lighting {
    fake.now.lock().unwrap().keyboard.lighting.clone()
}

async fn running(c: &mut Controller, names: &[&str]) {
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    c.set_running(&names);
    c.follow().await;
}

#[tokio::test]
async fn a_game_starting_applies_its_profile_and_ending_restores_your_settings() {
    let d = Dir::new("start");
    let (mut c, fake) = controller(&d);
    running(&mut c, &["game-a"]).await;
    assert_eq!(lighting(&fake), look(2.0));
    assert_eq!(c.active.as_ref().map(|r| r.profile.as_str()), Some("A"));
    assert_eq!(c.state().default_sections, [Section::Lighting]);
    running(&mut c, &[]).await;
    assert_eq!(lighting(&fake), look(1.0), "back to the owner's lighting");
    assert!(c.active.is_none());
    assert_eq!(c.state().applied.unwrap().profile, DEFAULT);
}

#[tokio::test]
async fn switching_games_keeps_the_original_to_restore() {
    let d = Dir::new("switch");
    let (mut c, fake) = controller(&d);
    running(&mut c, &["game-a"]).await;
    running(&mut c, &["game-b"]).await;
    assert_eq!(lighting(&fake), look(3.0));
    running(&mut c, &[]).await;
    assert_eq!(lighting(&fake), look(1.0), "not A's lighting");
}

#[tokio::test]
async fn restart_gaps_never_lose_the_original() {
    let d = Dir::new("gaps");
    let (mut c, fake) = controller(&d);
    running(&mut c, &["game-a"]).await;
    running(&mut c, &[]).await; // the launcher exits…
    running(&mut c, &["game-a"]).await; // …and the game starts
    running(&mut c, &[]).await;
    assert_eq!(lighting(&fake), look(1.0));
}

#[tokio::test]
async fn a_restart_during_a_game_never_loses_the_original() {
    let d = Dir::new("restart");
    let (mut c, fake) = controller(&d);
    store::save(&c.path, &c.store); // the profiles and rules, as saved
    running(&mut c, &["game-a"]).await;
    // The daemon stops mid-game; the keyboard keeps A's lighting.
    let restart = |fake: &Arc<Fake>| {
        let mut c = Controller::at(&d.file(), store::load(&d.file()));
        c.connect(Arc::new(Shared(Arc::clone(fake))));
        c
    };
    let mut c = restart(&fake);
    running(&mut c, &["game-a"]).await;
    assert_eq!(lighting(&fake), look(2.0));
    running(&mut c, &[]).await;
    assert_eq!(lighting(&fake), look(1.0), "not A's lighting");

    running(&mut c, &["game-a"]).await;
    // And when the game ends while the daemon is stopped.
    let mut c = restart(&fake);
    running(&mut c, &[]).await;
    assert_eq!(lighting(&fake), look(1.0));
}

#[tokio::test]
async fn a_manual_apply_during_a_game_is_undone_when_the_game_ends() {
    let d = Dir::new("manual");
    let (mut c, fake) = controller(&d);
    running(&mut c, &["game-a"]).await;
    c.apply("B", false).await.unwrap();
    assert_eq!(lighting(&fake), look(3.0));
    running(&mut c, &[]).await;
    assert_eq!(lighting(&fake), look(1.0));
    assert!(c.apply("Nope", false).await.unwrap_err().contains("no profile"));
}

#[tokio::test]
async fn the_running_game_is_shared_even_before_the_daemon_is_wired_in() {
    let d = Dir::new("unwired");
    let (game_tx, game) = watch::channel(None);
    let mut c = Controller::new(
        rules_store(vec![GameRule::running("game-b", "B")]),
        d.file(),
        broadcast::channel(8).0,
        game_tx,
    );
    running(&mut c, &["game-b"]).await;
    assert_eq!(game.borrow().as_deref(), Some("game-b"), "clips hear of the game");
    assert!(c.active.is_none(), "though nothing was applied");
    assert!(c.apply("B", false).await.unwrap_err().contains("can't reach"));
    running(&mut c, &[]).await;
    assert_eq!(*game.borrow(), None);
}

#[tokio::test]
async fn a_rule_profile_takes_your_own_settings_back_for_what_the_next_one_lacks() {
    let d = Dir::new("lacks");
    let mut store = rules_store(vec![
        GameRule::running("game-a", "A"),
        GameRule::running("game-b", "B"),
    ]);
    // A also sets the switches; B only the lighting.
    let mut a = lit("A", 2.0);
    a.switches = Some(switches(10));
    store.put(a).unwrap();
    let (mut c, fake) = wired(&d, store);
    fake.now.lock().unwrap().keyboard.switches = switches(20);
    running(&mut c, &["game-a"]).await;
    assert_eq!(fake.now.lock().unwrap().keyboard.switches, switches(10));
    running(&mut c, &["game-b"]).await;
    assert_eq!(
        fake.now.lock().unwrap().keyboard.switches,
        switches(20),
        "B has no switches, so yours are back"
    );
    assert_eq!(lighting(&fake), look(3.0));
    running(&mut c, &[]).await;
    assert_eq!(lighting(&fake), look(1.0));
    assert_eq!(fake.now.lock().unwrap().keyboard.switches, switches(20));
    assert!(c.store.before.is_none());
}

#[tokio::test]
async fn saving_takes_the_sections_asked_for_and_says_which_cant_be_read() {
    let d = Dir::new("save");
    let (mut c, fake) = controller(&d);
    fake.now.lock().unwrap().keyboard.lighting = look(9.0);
    fake.now.lock().unwrap().keyboard.switches = switches(12);
    c.save_current("Nine", Some(vec![Section::Lighting, Section::Switches]))
        .await
        .unwrap();
    let nine = c.store.profile("Nine").unwrap();
    assert_eq!(nine.sections(), [Section::Switches, Section::Lighting]);
    assert_eq!(nine.lighting, Some(look(9.0)));
    let e = c
        .save_current("Mouse", Some(vec![Section::Mouse]))
        .await
        .unwrap_err();
    assert!(e.starts_with("Mouse can't be saved: "), "{e}");
    // Without sections: every one that can be read (the mouse can't, the headset says nothing).
    c.save_current("All", None).await.unwrap();
    assert_eq!(
        c.store.profile("All").unwrap().sections(),
        [Section::Switches, Section::Bindings, Section::Screen, Section::Lighting]
    );
    // Saved again without sections: the ones it holds.
    fake.now.lock().unwrap().keyboard.lighting = look(5.0);
    c.save_current(" Nine ", None).await.unwrap();
    let nine = c.store.profile("Nine").unwrap();
    assert_eq!(nine.sections(), [Section::Switches, Section::Lighting]);
    assert_eq!(nine.lighting, Some(look(5.0)));
    assert_eq!(store::load(&d.file()).profiles.len(), c.store.profiles.len(), "saved");
}

#[tokio::test]
async fn saving_right_after_applying_saves_what_was_applied_though_the_daemon_lags() {
    let d = Dir::new("lag");
    let (mut c, _fake) = controller(&d);
    c.apply("B", false).await.unwrap();
    // The fake still answers the lighting from before B on this read.
    c.save_current("Copy of B", Some(vec![Section::Lighting]))
        .await
        .unwrap();
    assert_eq!(c.store.profile("Copy of B").unwrap().lighting, Some(look(3.0)));
}

#[tokio::test]
async fn an_apply_says_what_was_skipped_in_the_state() {
    let d = Dir::new("notes");
    let (mut c, fake) = controller(&d);
    let mut p = lit("Keys", 4.0);
    p.bindings = Some(Default::default());
    c.store.put(p).unwrap();
    c.apply("Keys", false).await.unwrap();
    let applied = c.state().applied.unwrap();
    assert_eq!(applied.profile, "Keys");
    assert_eq!(applied.notes.len(), 1);
    assert!(applied.notes[0].starts_with("Key bindings: skipped."), "{applied:?}");
    assert_eq!(fake.sent(), ["set_lighting"], "the bindings were never sent");
}

fn focus_rules() -> Store {
    rules_store(vec![
        GameRule {
            by: RuleMatch::Class,
            when: RuleWhen::Focused,
            ..GameRule::running("steam_app_1174180", "A")
        },
        GameRule {
            when: RuleWhen::Focused,
            ..GameRule::running("game-b", "B")
        },
    ])
}

fn window(class: &str, pid: Option<u32>) -> FocusEvent {
    FocusEvent::Window(Some(focus::Window {
        class: class.into(),
        pid,
    }))
}

#[tokio::test]
async fn a_focus_rule_holds_while_its_window_has_the_focus_and_switching_away_restores() {
    let d = Dir::new("focus");
    let (mut c, fake) = wired(&d, focus_rules());
    // game-b's process, for the program rule.
    let proc_dir = d.0.join("proc/77");
    std::fs::create_dir_all(&proc_dir).unwrap();
    std::fs::write(proc_dir.join("comm"), "game-b\n").unwrap();
    let names: Vec<String> = vec!["game-b".into(), "steam".into()];
    c.set_running(&names);
    let t = Instant::now();
    assert!(!c.on_focus(FocusEvent::Up("Hyprland"), t));
    c.on_focus(window("steam_app_1174180", Some(4242)), t);
    assert!(!c.focus_due(t + focus::DEBOUNCE / 2), "not yet");
    assert!(c.focus_due(t + focus::DEBOUNCE));
    c.follow().await;
    assert_eq!(lighting(&fake), look(2.0));
    assert_eq!(c.state().focus.source.as_deref(), Some("Hyprland"));
    assert_eq!(c.state().focus.window.unwrap().class, "steam_app_1174180");
    // Alt-Tab past a window to game-b's: only game-b's counts.
    let t = t + focus::DEBOUNCE * 2;
    c.on_focus(window("kitty", None), t);
    c.on_focus(window("game-b-window", Some(77)), t + focus::DEBOUNCE / 4);
    assert!(!c.focus_due(t + focus::DEBOUNCE));
    assert!(c.focus_due(t + focus::DEBOUNCE * 2));
    c.follow().await;
    assert_eq!(lighting(&fake), look(3.0));
    assert_eq!(c.state().focus.window.unwrap().program.as_deref(), Some("game-b"));
    // Away to the desktop: your own lighting, though game-b still runs.
    let t = t + focus::DEBOUNCE * 4;
    c.on_focus(FocusEvent::Window(None), t);
    assert!(c.focus_due(t + focus::DEBOUNCE));
    c.follow().await;
    assert_eq!(lighting(&fake), look(1.0));
    assert!(c.active.is_none());
    // The desktop stops reporting: rules on focus act while their program runs.
    assert!(c.on_focus(FocusEvent::Down("Hyprland closed its event socket".into()), t));
    c.follow().await;
    assert_eq!(lighting(&fake), look(3.0), "game-b runs");
    let status = c.state().focus;
    assert_eq!(status.source, None);
    assert!(status.note.unwrap().contains("act while their program runs"));
}

#[tokio::test]
async fn presets_apply_copy_and_share_like_profiles() {
    let d = Dir::new("presets");
    let (mut c, fake) = controller(&d);
    assert_eq!(c.state().presets.len(), 4);
    c.apply("Competitive FPS", true).await.unwrap();
    assert_eq!(fake.now.lock().unwrap().keyboard.switches.actuation, 10);
    assert!(c.apply("Competitive FPS", false).await.is_err(), "not a saved profile");
    let copy = |c: &mut Controller, to: Option<&str>| {
        c.copy("Typing", true, to.map(String::from))
    };
    copy(&mut c, None).unwrap();
    copy(&mut c, None).unwrap();
    assert!(c.store.profile("Typing").is_some() && c.store.profile("Typing 2").is_some());
    assert!(copy(&mut c, Some("Typing")).unwrap_err().contains("saved already"));
    let file = c.file("Typing", false).unwrap();
    assert_eq!(file.profile.switches.as_ref().unwrap().actuation, 20);
    // Shared and read back in.
    let shared = ProfileFile::parse(&file.to_json()).unwrap().profile;
    assert!(c.import(shared.clone(), false).unwrap_err().contains("saved already"));
    c.import(Profile { name: "Friend's".into(), ..shared.clone() }, false).unwrap();
    c.import(shared, true).unwrap();
    assert!(c.import(Profile::named("Empty"), false).is_err());
    assert!(store::load(&d.file()).profile("Friend's").is_some(), "saved");
}

#[tokio::test]
async fn requests_are_answered_through_the_handle() {
    let d = Dir::new("handle");
    let (c, _fake) = controller(&d);
    let h = spawn_with(c, FocusEnv { runtime: None, signature: None, bus: false }, Duration::from_secs(60));
    let ResponseData::Profiles { state } = h.ask(Request::GetProfiles).await else {
        panic!("no state");
    };
    assert_eq!(state.profiles, ["A", "B"]);
    assert_eq!(state.sections, [vec![Section::Lighting], vec![Section::Lighting]]);
    let answer = h
        .ask(Request::GetProfile {
            name: "MOBA".into(),
            preset: true,
        })
        .await;
    assert!(matches!(answer, ResponseData::Profile { file } if file.about.starts_with("gglinux's own")));
    assert!(matches!(
        h.ask(Request::DeleteProfile { name: "A".into() }).await,
        ResponseData::Error { message } if message.contains("game-a")
    ));
    assert!(matches!(
        h.ask(Request::CopyProfile { from: "A".into(), preset: false, to: None }).await,
        ResponseData::Success
    ));
    // The actions on the spare keys apply through their own command.
    h.send::<()>(|reply| Command::Apply { name: "A 2".into(), reply })
        .await
        .unwrap();
}

/// End to end: a fake Hyprland's event stream, through the task, to the fake daemon.
#[tokio::test]
async fn the_task_follows_hyprlands_focus_and_alt_tab_out_restores() {
    let d = Dir::new("e2e");
    let hypr = d.0.join("hypr/sig");
    std::fs::create_dir_all(&hypr).unwrap();
    fake_hyprland(
        &hypr,
        vec![
            (300, "activewindow>>steam_app_1174180,Red Dead Redemption 2"),
            (0, "activewindowv2>>55d0c3a5e8b0"),
            (1200, "activewindow>>kitty,fish"),
            (0, "activewindowv2>>55d0c3a60000"),
        ],
        true,
    )
    .await;
    let (c, fake) = wired(&d, focus_rules());
    let env = FocusEnv {
        runtime: Some(d.0.clone()),
        signature: None,
        bus: false,
    };
    let h = spawn_with(c, env, Duration::from_millis(50));
    let state = |h: Handle| async move {
        match h.ask(Request::GetProfiles).await {
            ResponseData::Profiles { state } => state,
            other => panic!("{other:?}"),
        }
    };
    // Started on the terminal (j/activewindow), then the game's window for 1.2 s.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(lighting(&fake), look(2.0), "the game has had the focus for 0.8 s");
    let s = state(h.clone()).await;
    assert_eq!(s.focus.source.as_deref(), Some("Hyprland"));
    assert_eq!(s.active.unwrap().profile, "A");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(lighting(&fake), look(1.0), "Alt-Tabbed out to the terminal");
    assert!(state(h).await.active.is_none());
}
