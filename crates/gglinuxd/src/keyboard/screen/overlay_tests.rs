// SPDX-License-Identifier: GPL-3.0-or-later
//! A game's screen frame over the screen setting: it goes over any setting while it's
//! there, the setting (or the keyboard's own screen) comes back after, and screen care
//! treats it as a picture of gglinux's.

use super::*;
use gg_device_keyboard::testing::{FakeTransport, Kind};

/// A screen state for `setting` that never touches a settings file.
fn bare(setting: Screen) -> ScreenState {
    ScreenState {
        source: source_for(&setting),
        overlay: None,
        setting,
        path: PathBuf::new(),
        taken: false,
        shown: None,
        last: None,
        error: None,
        care: ScreenCare::default(),
        care_path: PathBuf::new(),
        away: false,
        resting: false,
        held: false,
    }
}

/// A picture with only pixel (`x`, `y`) lit, well inside the screen.
fn lit(x: usize, y: usize) -> Bitmap {
    let mut b = Bitmap::default();
    b.set(x, y, true);
    b
}

fn game(picture: &Bitmap, until: Option<Instant>) -> Option<Overlaid> {
    Some(Overlaid {
        picture: picture.clone(),
        until,
    })
}

#[test]
fn a_games_picture_goes_over_the_setting_and_the_setting_comes_back_after() {
    let mut s = bare(Screen::Text { text: "GG".into() });
    s.care.shift = false;
    let now = Now::current();
    assert_eq!(s.frame(&now), Some(oled::text("GG")));
    let picture = lit(10, 10);
    s.set_overlay(game(&picture, None));
    assert!(s.draws());
    assert_eq!(s.frame(&now), Some(picture));
    s.set_overlay(None);
    assert_eq!(
        s.frame(&now),
        Some(oled::text("GG")),
        "the setting comes back"
    );
}

#[test]
fn over_the_keyboards_own_screen_a_games_picture_takes_it_and_hands_it_back() {
    let fake = FakeTransport::new();
    let kb = fake.keyboard();
    let mut s = bare(Screen::Keyboard);
    s.care.shift = false;
    s.render(Some(&kb));
    assert!(
        fake.sent().is_empty() && !s.draws(),
        "the keyboard's own: nothing sent"
    );
    let picture = lit(10, 10);
    s.set_overlay(game(&picture, None));
    assert!(s.draws(), "a game's picture takes the screen");
    s.render(Some(&kb));
    s.render(Some(&kb));
    let sent = fake.sent();
    assert_eq!(sent.len(), 1, "unchanged: sent once");
    assert_eq!(sent[0].report[..3], [0x00, 0x1F, 0x81]);
    assert_eq!(sent[0].report[3..643], picture.packed()[..]);
    s.set_overlay(None);
    s.render(Some(&kb));
    s.render(Some(&kb));
    let sent = fake.sent();
    assert_eq!(sent.len(), 2, "handed back once");
    assert_eq!(
        (sent[1].kind, &sent[1].report[..3]),
        (Kind::Output, &[0x00, 0x1F, 0x82][..])
    );
    assert!(!s.draws());
}

#[test]
fn the_loop_wakes_when_the_games_picture_changes_and_not_for_a_clock_under_it() {
    let mut s = bare(Screen::Clock {
        seconds: true,
        h24: true,
        date: false,
    });
    let soon = Instant::now() + Duration::from_millis(300);
    s.set_overlay(game(&lit(10, 10), Some(soon)));
    let wait = s.wait(true).unwrap();
    assert!(
        wait > Duration::from_millis(250) && wait <= Duration::from_millis(300),
        "{wait:?}"
    );
    s.set_overlay(game(&lit(10, 10), None));
    assert_eq!(
        s.wait(true),
        None,
        "a held frame: only a new event changes it"
    );
    assert_eq!(s.wait(false), None, "no keyboard, no wake");
    s.set_overlay(None);
    assert!(s.wait(true).is_some(), "the clock wakes it again");
}

#[test]
fn screen_care_moves_a_games_picture_and_rests_it_even_over_the_keyboards_own_screen() {
    let mut s = bare(Screen::Keyboard);
    assert!(
        !s.care.own_screen_too,
        "the default: the keyboard's own screen is left alone"
    );
    let picture = lit(20, 20);
    s.set_overlay(game(&picture, None));
    let step = 7 * SHIFT_EVERY;
    let now = Now {
        since_epoch: Duration::from_secs(step),
        ..Now::current()
    };
    let (dx, dy) = shift_at(step);
    assert_ne!((dx, dy), (0, 0));
    assert_eq!(s.frame(&now), Some(picture.shifted(dx, dy)), "it moves");
    s.set_away(true);
    assert!(
        s.frame(&now).unwrap().is_dark(),
        "a game's picture is gglinux's: dark while nobody is there"
    );
    assert_eq!(s.wait(true), None, "and the loop isn't woken for it");
    s.set_overlay(None);
    assert_eq!(
        s.frame(&now),
        None,
        "the keyboard's own screen, left alone as before"
    );
}

#[test]
fn a_game_sending_fast_gets_ten_pictures_a_second_and_the_last_one_is_never_lost() {
    let fake = FakeTransport::new();
    let kb = fake.keyboard();
    let mut s = bare(Screen::Keyboard);
    s.care.shift = false;
    s.set_overlay(game(&lit(10, 10), None));
    s.render(Some(&kb));
    assert_eq!(fake.sent().len(), 1);
    // A new picture straight after: it waits out the gap, and the loop wakes for it.
    s.set_overlay(game(&lit(11, 10), None));
    s.render(Some(&kb));
    assert_eq!(fake.sent().len(), 1, "not yet");
    let wait = s.wait(true).unwrap();
    assert!(wait <= OVERLAY_GAP, "{wait:?}");
    s.set_overlay(game(&lit(12, 10), None));
    s.render(Some(&kb));
    assert_eq!(fake.sent().len(), 1, "still within the gap");
    std::thread::sleep(OVERLAY_GAP);
    s.render(Some(&kb));
    let sent = fake.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[1].report[3..643],
        lit(12, 10).packed()[..],
        "the newest one"
    );
    assert_eq!(s.wait(true), None, "nothing waits any more");
    s.set_overlay(game(&lit(13, 10), None));
    s.set_away(true);
    s.render(Some(&kb));
    let sent = fake.sent();
    assert_eq!(
        sent.len(),
        3,
        "going dark for nobody being there doesn't wait"
    );
    assert!(sent[2].report[3..643].iter().all(|&b| b == 0));
}
