// SPDX-License-Identifier: GPL-3.0-or-later
//! The actions service against its fakes: no program starts, nothing is typed, no
//! portal session opens.

use std::time::Duration;

use gg_ipc::{Action, ActionBinding, Event, Request, ResponseData};
use tokio::sync::broadcast;

use super::run::{PLAY_PAUSE, VOLUME_UP};
use super::store;
use super::testing::Fakes;
use super::{serve, Service};
use crate::scratch::Scratch;

const F13: u8 = 0x68;
const F14: u8 = 0x69;
const F15: u8 = 0x6A;

fn bind(key: u8, label: &str, action: Action) -> ActionBinding {
    ActionBinding {
        key,
        label: label.into(),
        action,
    }
}

fn run(key: u8, argv: &[&str]) -> ActionBinding {
    bind(
        key,
        "Program",
        Action::Run {
            argv: argv.iter().map(|a| a.to_string()).collect(),
        },
    )
}

/// A service over fresh fakes in its own folder, typing allowed.
fn service(d: &Scratch) -> (Service, Fakes, broadcast::Receiver<Event>) {
    let (event_tx, events) = broadcast::channel(32);
    let fakes = Fakes {
        spawner: super::testing::FakeSpawner::with(&["playerctl", "wpctl", "wtype"]),
        ..Fakes::default()
    };
    let s = fakes.service(d.0.join("actions.json"), Ok(()), event_tx);
    (s, fakes, events)
}

/// The last actions state announced, if any.
fn last_announced(events: &mut broadcast::Receiver<Event>) -> Option<gg_ipc::ActionsState> {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|e| match e {
            Event::ActionsChanged { state } => Some(state),
            _ => None,
        })
        .last()
}

#[tokio::test]
async fn the_program_gets_exactly_its_argv_with_no_shell() {
    let d = Scratch::new("actions-argv");
    let (s, fakes, _) = service(&d);
    s.set(vec![run(F13, &["echo", "; rm -rf ~", "$HOME"])])
        .await
        .unwrap();
    s.activated("action-f13").await;
    assert_eq!(
        fakes.spawner.spawned(),
        [["echo", "; rm -rf ~", "$HOME"]],
        "one literal argument each: nothing split, nothing expanded"
    );
    assert_eq!(s.state().error, None);
}

#[tokio::test]
async fn typed_text_goes_to_wtype_as_one_argument_after_the_dashes() {
    let d = Scratch::new("actions-type");
    let (s, fakes, _) = service(&d);
    let text = "-n hello; world";
    s.set(vec![bind(
        F14,
        "Greeting",
        Action::Type { text: text.into() },
    )])
    .await
    .unwrap();
    s.activated("action-f14").await;
    assert_eq!(fakes.spawner.spawned(), [["wtype", "--", text]]);
}

#[tokio::test]
async fn where_typing_doesnt_work_nothing_is_typed_and_the_error_says_why() {
    let d = Scratch::new("actions-no-type");
    let (event_tx, mut events) = broadcast::channel(32);
    let fakes = Fakes::default();
    let why = "Plasma needs its remote-desktop permission for typing; not built yet";
    let s = fakes.service(d.0.join("actions.json"), Err(why.into()), event_tx);
    assert!(!s.state().typing);
    s.set(vec![bind(
        F14,
        "Greeting",
        Action::Type { text: "hi".into() },
    )])
    .await
    .unwrap();
    s.activated("action-f14").await;
    assert!(fakes.spawner.spawned().is_empty());
    let error = last_announced(&mut events).and_then(|st| st.error).unwrap();
    assert_eq!(error, format!("F14 (Greeting): {why}"));
}

#[tokio::test]
async fn keys_outside_f13_to_f24_and_bad_lists_are_refused_and_change_nothing() {
    let d = Scratch::new("actions-refused");
    let (s, fakes, mut events) = service(&d);
    for bad in [
        vec![run(0x45, &["kitty"])],
        vec![run(0x74, &["kitty"])],
        vec![run(F13, &["kitty"]), run(F13, &["foot"])],
        vec![run(F13, &[" "])],
    ] {
        assert!(s.set(bad.clone()).await.is_err(), "{bad:?}");
    }
    let e = s.set(vec![run(0x45, &["kitty"])]).await.unwrap_err();
    assert!(e.contains("spare keys F13–F24"), "{e}");
    assert!(fakes.portal.binds().is_empty(), "nothing was bound");
    assert!(!d.0.join("actions.json").exists(), "nothing was saved");
    assert!(last_announced(&mut events).is_none());
    // A key that isn't a spare key's, or a spare key with no action, runs nothing.
    s.set(vec![run(F13, &["kitty"])]).await.unwrap();
    for id in ["save-clip", "action-f12", "action-f25", "action-f14"] {
        s.activated(id).await;
    }
    assert!(fakes.spawner.spawned().is_empty());
}

#[tokio::test]
async fn an_unchanged_list_doesnt_rebind_and_new_actions_on_the_same_keys_dont_either() {
    let d = Scratch::new("actions-unchanged");
    let (s, fakes, mut events) = service(&d);
    let list = vec![run(F13, &["kitty"]), run(F14, &["foot"])];
    s.set(list.clone()).await.unwrap();
    assert_eq!(fakes.portal.binds(), [["action-f13", "action-f14"]]);
    assert_eq!(s.state().shortcuts.as_deref(), Some("bound (fake)"));
    last_announced(&mut events).unwrap();

    s.set(list.clone()).await.unwrap();
    assert_eq!(fakes.portal.binds().len(), 1, "same list: no new bind");
    assert!(
        last_announced(&mut events).is_none(),
        "and nothing to announce"
    );

    // Other programs and names, the same keys in another order: saved, not rebound.
    let changed = vec![
        run(F14, &["alacritty"]),
        run(F13, &["kitty", "--single-instance"]),
    ];
    s.set(changed.clone()).await.unwrap();
    assert_eq!(fakes.portal.binds().len(), 1);
    assert_eq!(last_announced(&mut events).unwrap().bindings, changed);
    assert_eq!(store::load(&d.0.join("actions.json")).bindings, changed);

    // A key more is a new set of keys.
    let mut more = changed;
    more.push(bind(F15, "Clip", Action::ClipSave));
    s.set(more).await.unwrap();
    assert_eq!(
        fakes.portal.binds()[1],
        ["action-f14", "action-f13", "action-f15"]
    );
}

#[tokio::test]
async fn a_rebind_failure_leaves_the_old_keys_working_and_says_why() {
    let d = Scratch::new("actions-rebind-fails");
    let (s, fakes, mut events) = service(&d);
    let old = vec![run(F13, &["kitty"])];
    s.set(old.clone()).await.unwrap();

    fakes.portal.fail_with(Some("the owner closed the dialog"));
    let new = vec![run(F13, &["foot"]), run(F14, &["firefox"])];
    let e = s.set(new).await.unwrap_err();
    assert_eq!(e, "the owner closed the dialog");
    assert_eq!(fakes.portal.binds().len(), 2, "it did ask");
    let announced = last_announced(&mut events).unwrap();
    assert_eq!(announced.bindings, old, "the old list stands");
    assert_eq!(
        announced.error.as_deref(),
        Some("the owner closed the dialog")
    );
    assert_eq!(store::load(&d.0.join("actions.json")).bindings, old);

    // The old key still runs the old action.
    s.activated("action-f13").await;
    assert_eq!(fakes.spawner.spawned(), [["kitty"]]);
    assert_eq!(s.state().error, None, "an action that ran clears the error");
}

#[tokio::test]
async fn a_restart_binds_the_keys_of_last_time_again_and_asks_for_no_new_ones() {
    let d = Scratch::new("actions-restart");
    let path = d.0.join("actions.json");
    // F13 was bound last time; F14 is in the list but the desktop never bound it.
    store::save(
        &path,
        &store::Saved {
            bindings: vec![run(F13, &["kitty"]), run(F14, &["foot"])],
            bound: vec!["action-f13".into()],
        },
    )
    .unwrap();
    let (event_tx, mut events) = broadcast::channel(32);
    let fakes = Fakes::default();
    let s = fakes.service(path.clone(), Ok(()), event_tx);
    assert!(fakes.portal.binds().is_empty(), "nothing before restore");
    assert_eq!(s.state().bindings.len(), 2, "the list is there at once");

    s.restore().await.unwrap();
    assert_eq!(
        fakes.portal.binds(),
        [["action-f13"]],
        "only what was bound"
    );
    assert_eq!(
        last_announced(&mut events).unwrap().shortcuts.as_deref(),
        Some("bound (fake)")
    );
    s.restore().await.unwrap();
    assert_eq!(fakes.portal.binds().len(), 1, "once");
    s.activated("action-f13").await;
    assert_eq!(fakes.spawner.spawned(), [["kitty"]]);

    // Nothing bound last time: a start binds nothing.
    let fresh = Scratch::new("actions-restart-none");
    let (event_tx, _) = broadcast::channel(4);
    let none = Fakes::default();
    let s = none.service(fresh.0.join("actions.json"), Ok(()), event_tx);
    s.restore().await.unwrap();
    assert!(none.portal.binds().is_empty());
    assert_eq!(s.state().shortcuts, None);
}

#[tokio::test]
async fn restoring_tries_again_until_the_desktop_is_there() {
    let d = Scratch::new("actions-restore-retry");
    let path = d.0.join("actions.json");
    store::save(
        &path,
        &store::Saved {
            bindings: vec![run(F13, &["kitty"])],
            bound: vec!["action-f13".into()],
        },
    )
    .unwrap();
    let (event_tx, _) = broadcast::channel(32);
    let fakes = Fakes::default();
    fakes.portal.fail_with(Some("no portal yet"));
    let s = std::sync::Arc::new(fakes.service(path, Ok(()), event_tx));
    let restoring = std::sync::Arc::clone(&s);
    let task =
        tokio::spawn(async move { restoring.keep_restoring(Duration::from_millis(20)).await });
    tokio::time::sleep(Duration::from_millis(70)).await;
    assert!(fakes.portal.binds().len() >= 2, "it tried again");
    assert_eq!(s.state().error.as_deref(), Some("no portal yet"));
    fakes.portal.fail_with(None);
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("it stops once bound")
        .unwrap();
    assert_eq!(s.state().shortcuts.as_deref(), Some("bound (fake)"));
    assert_eq!(s.state().error, None);
}

#[tokio::test]
async fn profiles_clips_and_media_keys_go_where_they_belong() {
    let d = Scratch::new("actions-kinds");
    let (s, fakes, _) = service(&d);
    s.set(vec![
        bind(
            F13,
            "Night",
            Action::ProfileApply {
                name: "Night".into(),
            },
        ),
        bind(F14, "Clip", Action::ClipSave),
        bind(F15, "Play", Action::Media { usage: PLAY_PAUSE }),
        bind(0x6B, "Louder", Action::Media { usage: VOLUME_UP }),
    ])
    .await
    .unwrap();
    for id in ["action-f13", "action-f14", "action-f15", "action-f16"] {
        s.activated(id).await;
    }
    assert_eq!(fakes.daemon.asked(), ["profile Night", "clip"]);
    assert_eq!(
        fakes.spawner.spawned(),
        [
            vec!["playerctl", "play-pause"],
            vec![
                "wpctl",
                "set-volume",
                "-l",
                "1.0",
                "@DEFAULT_AUDIO_SINK@",
                "5%+"
            ]
        ]
    );
}

#[tokio::test]
async fn removing_every_action_closes_the_session() {
    let d = Scratch::new("actions-none");
    let (s, fakes, _) = service(&d);
    s.set(vec![run(F13, &["kitty"])]).await.unwrap();
    s.set(Vec::new()).await.unwrap();
    assert_eq!(fakes.portal.binds()[1], Vec::<String>::new());
    assert_eq!(s.state().shortcuts, None);
    assert_eq!(
        store::load(&d.0.join("actions.json")),
        store::Saved::default()
    );
}

#[tokio::test]
async fn get_and_set_are_answered_and_a_change_is_announced() {
    let d = Scratch::new("actions-serve");
    let (s, _fakes, mut events) = service(&d);
    let list = vec![run(F13, &["kitty"])];
    assert!(matches!(
        serve(
            Request::SetActions {
                bindings: list.clone()
            },
            &s
        )
        .await,
        ResponseData::Success
    ));
    assert_eq!(last_announced(&mut events).unwrap().bindings, list);
    match serve(Request::GetActions, &s).await {
        ResponseData::Actions { state } => {
            assert_eq!(state.bindings, list);
            assert!(state.typing);
            assert_eq!(state.shortcuts.as_deref(), Some("bound (fake)"));
        }
        other => panic!("{other:?}"),
    }
    match serve(
        Request::SetActions {
            bindings: vec![run(0x45, &["kitty"])],
        },
        &s,
    )
    .await
    {
        ResponseData::Error { message } => assert!(message.contains("F13–F24"), "{message}"),
        other => panic!("{other:?}"),
    }
}
