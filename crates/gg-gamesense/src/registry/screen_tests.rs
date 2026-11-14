// SPDX-License-Identifier: GPL-3.0-or-later
//! The registry's screen frames and bitmaps: which game and event write the screen, for
//! how long, and how a bitmap composes with the other handlers on the keys. Time is the
//! registry's own (seconds passed in), so these run on a made-up clock.

use super::*;
use gg_device_keyboard::layout::US;
use serde_json::json;

const RED: Rgb = Rgb { r: 255, g: 0, b: 0 };
const GREEN: Rgb = Rgb { r: 0, g: 255, b: 0 };
const BLUE: Rgb = Rgb { r: 0, g: 0, b: 255 };
const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

fn bind(r: &mut Registry, game: &str, event: &str, handlers: Value) {
    bind_with(
        r,
        json!({ "game": game, "event": event, "handlers": handlers }),
    );
}

fn bind_with(r: &mut Registry, v: Value) {
    r.bind_event(serde_json::from_value(v).unwrap()).unwrap();
}

fn send(r: &mut Registry, game: &str, event: &str, data: Value, now: f64) {
    r.game_event(
        serde_json::from_value(json!({ "game": game, "event": event, "data": data })).unwrap(),
        now,
    )
    .unwrap();
}

fn screened(datas: Value) -> Value {
    json!([{ "device-type": "screened", "zone": "one", "mode": "screen", "datas": datas }])
}

fn text_at(x: usize, y: usize, text: &str, scale: usize) -> Bitmap {
    let mut b = Bitmap::default();
    b.draw_text(x, y, text, scale);
    b
}

fn shown(r: &Registry, now: f64) -> Option<Bitmap> {
    r.screen(now).map(|s| s.picture)
}

#[test]
fn a_timed_frame_shows_for_its_length_then_the_screen_goes_back() {
    let mut r = Registry::default();
    bind(
        &mut r,
        "G",
        "KILLS",
        screened(json!([{ "has-text": true, "suffix": " kills", "length-millis": 250 }])),
    );
    assert_eq!(r.screen(0.0), None, "nothing before an event");
    send(&mut r, "G", "KILLS", json!({ "value": 15 }), 1.0);
    let now = r.screen(1.1).unwrap();
    // Eight characters fill the width at twice the font's size.
    assert_eq!(now.picture, text_at(0, 12, "15 kills", 2));
    assert!((now.next_change.unwrap() - 0.15).abs() < 0.002, "{now:?}");
    assert_eq!(r.screen(1.25), None, "back to the user's own screen");
    assert_eq!(r.paint(1.1), None, "and no keys taken for it");
}

#[test]
fn a_held_frame_stays_while_the_game_is_up_and_goes_with_it() {
    // The doc's "Headshot!" example: a quarter second, then "15 kills" until a new event.
    let mut r = Registry::default();
    bind(
        &mut r,
        "G",
        "KILLS",
        screened(json!([
            { "has-text": true, "suffix": "Headshot!", "length-millis": 250, "arg": "" },
            { "has-text": true, "suffix": " kills" }
        ])),
    );
    send(&mut r, "G", "KILLS", json!({ "value": 15 }), 1.0);
    assert_eq!(shown(&r, 1.1), Some(text_at(28, 16, "Headshot!", 1)));
    let held = r.screen(1.3).unwrap();
    assert_eq!(held.picture, text_at(0, 12, "15 kills", 2));
    let quiet = held.next_change.unwrap();
    assert!(
        (quiet - (16.0 - 1.3)).abs() < 0.01,
        "it next changes when the game goes quiet, 15 s after its event: {quiet}"
    );
    assert!(r.screen(15.9).is_some());
    assert_eq!(r.screen(16.1), None, "the game went quiet");
    r.heartbeat("G", 16.2).unwrap();
    assert!(r.screen(20.0).is_some(), "a heartbeat brings it back up");
    r.stop_game("G").unwrap();
    assert_eq!(r.screen(20.1), None, "stopped");
    send(&mut r, "G", "KILLS", json!({ "value": 16 }), 21.0);
    assert_eq!(shown(&r, 21.1), Some(text_at(28, 16, "Headshot!", 1)));
}

#[test]
fn a_new_event_writes_over_the_frames_playing_and_the_same_value_does_not() {
    let mut r = Registry::default();
    bind(
        &mut r,
        "G",
        "A",
        screened(json!([{ "has-text": true, "prefix": "A" }])),
    );
    bind(
        &mut r,
        "G",
        "B",
        screened(json!([{ "has-text": true, "prefix": "B", "length-millis": 500 }])),
    );
    send(&mut r, "G", "A", json!({ "value": 1 }), 0.0);
    assert_eq!(shown(&r, 0.5), Some(text_at(40, 8, "A1", 3)));
    send(&mut r, "G", "B", json!({ "value": 1 }), 1.0);
    assert_eq!(shown(&r, 1.2), Some(text_at(40, 8, "B1", 3)));
    assert_eq!(
        r.screen(1.6),
        None,
        "after B's frames the screen goes back, not to A's"
    );
    send(&mut r, "G", "A", json!({ "value": 1 }), 2.0);
    assert_eq!(r.screen(2.1), None, "the same value isn't a change");
    send(&mut r, "G", "A", json!({ "value": 2 }), 3.0);
    assert_eq!(shown(&r, 3.1), Some(text_at(40, 8, "A2", 3)));
}

#[test]
fn custom_text_with_value_optional_shows_each_time_and_keeps_only_what_it_reads() {
    // The doc's "handler to display custom text", bound with value_optional.
    let mut r = Registry::default();
    bind_with(
        &mut r,
        json!({ "game": "G", "event": "TEXT", "value_optional": true,
            "handlers": screened(json!([{ "has-text": true, "context-frame-key": "custom-text" }])) }),
    );
    let frame = json!({ "custom-text": "hello", "unread": vec![0; 64] });
    send(&mut r, "G", "TEXT", json!({ "frame": frame }), 0.0);
    assert_eq!(shown(&r, 0.1), Some(text_at(4, 8, "hello", 3)));
    let kept = r.games["G"].events["TEXT"].frame.clone().map(Value::Object);
    assert_eq!(kept, Some(json!({ "custom-text": "hello" })));
    send(
        &mut r,
        "G",
        "TEXT",
        json!({ "frame": { "custom-text": "hello" } }),
        1.0,
    );
    send(
        &mut r,
        "G",
        "TEXT",
        json!({ "frame": { "custom-text": "again" } }),
        2.0,
    );
    assert_eq!(shown(&r, 2.1), Some(text_at(4, 8, "again", 3)));
    let long = "x".repeat(1000);
    send(
        &mut r,
        "G",
        "TEXT",
        json!({ "frame": { "custom-text": long } }),
        3.0,
    );
    let kept = &r.games["G"].events["TEXT"].frame.as_ref().unwrap()["custom-text"];
    assert_eq!(kept.as_str().unwrap().len(), screen::MAX_TEXT);
}

#[test]
fn an_events_own_picture_shows_in_a_picture_frame() {
    let mut r = Registry::default();
    bind_with(
        &mut r,
        json!({ "game": "G", "event": "OLED", "value_optional": true, "handlers": [{
            "device-type": "screened-128x40", "zone": "one", "mode": "screen",
            "datas": [{ "has-text": false, "image-data": [] }] }] }),
    );
    let mut rows = vec![0u8; 640];
    rows[0] = 0x80;
    send(
        &mut r,
        "G",
        "OLED",
        json!({ "frame": { "image-data-128x40": rows } }),
        0.0,
    );
    let mut want = Bitmap::default();
    want.set(0, 0, true);
    assert_eq!(shown(&r, 0.1), Some(want));
    send(
        &mut r,
        "G",
        "OLED",
        json!({ "frame": { "image-data-128x40": [1, 2, 3] } }),
        1.0,
    );
    assert!(
        shown(&r, 1.1).unwrap().is_dark(),
        "a picture of the wrong size isn't drawn"
    );
}

#[test]
fn a_handler_made_for_this_screen_size_wins_over_one_for_any_screen() {
    let mut r = Registry::default();
    let mut rows = vec![0u8; 640];
    rows[639] = 1;
    bind(
        &mut r,
        "G",
        "E",
        json!([
            { "device-type": "screened", "zone": "one", "mode": "screen", "datas": [{ "has-text": true }] },
            { "device-type": "screened-128x40", "zone": "one", "mode": "screen",
              "datas": [{ "has-text": false, "image-data": rows }] },
            { "device-type": "screened-128x36", "zone": "one", "mode": "screen",
              "datas": [{ "has-text": false, "image-data": vec![255; 576] }] }
        ]),
    );
    send(&mut r, "G", "E", json!({ "value": 5 }), 0.0);
    let mut want = Bitmap::default();
    want.set(127, 39, true);
    assert_eq!(shown(&r, 0.1), Some(want));
}

#[test]
fn only_the_active_game_shows_and_a_game_turned_off_shows_nothing() {
    let mut r = Registry::default();
    let held = screened(json!([{ "has-text": true }]));
    bind(&mut r, "G", "E", held.clone());
    bind(
        &mut r,
        "H",
        "E",
        json!([{ "device-type": "keyboard", "zone": "w",
        "color": { "red": 0, "green": 0, "blue": 255 } }]),
    );
    send(&mut r, "G", "E", json!({ "value": 7 }), 0.0);
    assert!(r.screen(0.5).is_some());
    send(&mut r, "H", "E", json!({ "value": 1 }), 1.0);
    assert_eq!(
        r.screen(1.5),
        None,
        "H is the game in front now, and shows nothing there"
    );
    r.remove_game("H").unwrap();
    assert!(r.screen(1.5).is_some(), "G again");
    r.set_enabled("G", false).unwrap();
    assert_eq!(r.screen(1.5), None);
    r.set_enabled("G", true).unwrap();
    r.remove_event("G", "E").unwrap();
    assert_eq!(r.screen(1.5), None, "its event is gone");
}

/// The grid with every cell black but these.
fn grid(lit: &[(usize, [u8; 3])]) -> Value {
    let mut cells = vec![json!([0, 0, 0]); bitmap::CELLS];
    for (i, c) in lit {
        cells[*i] = json!(c);
    }
    json!(cells)
}

fn bitmap_event(r: &mut Registry, mode: &str, excluded: Option<Value>) {
    let mut h = json!({ "device-type": "rgb-per-key-zones", "mode": mode });
    if let Some(e) = excluded {
        h["excluded-events"] = e;
    }
    bind_with(
        r,
        json!({ "game": "G", "event": "BITMAP_EVENT", "value_optional": true, "handlers": [h] }),
    );
}

#[test]
fn a_bitmap_paints_every_key_from_its_grid() {
    let mut r = Registry::default();
    bitmap_event(&mut r, "bitmap", None);
    // "[255,0,0], // Color for top left"
    let frame = json!({ "bitmap": grid(&[(0, [255, 0, 0]), (46, [0, 255, 0])]) });
    send(&mut r, "G", "BITMAP_EVENT", json!({ "frame": frame }), 0.0);
    let layer = r.paint(0.1).unwrap();
    assert_eq!(layer.len(), US.len(), "every key of the TKL");
    assert_eq!(layer[&0x29], RED, "Esc, the top left");
    assert_eq!(layer[&0x14], GREEN, "Q, cell 46");
    assert_eq!(layer[&0x04], BLACK, "black is a colour too");
    send(&mut r, "G", "BITMAP_EVENT", json!({ "frame": {} }), 1.0);
    assert_eq!(r.paint(1.1), None, "no grid in the frame: nothing painted");
}

/// The doc's example: a background over every key but AMMO's and HEALTH's.
fn background_with_ammo_and_health() -> Registry {
    let mut r = Registry::default();
    bind(
        &mut r,
        "G",
        "AMMO",
        json!([{ "device-type": "keyboard", "zone": "q-row",
        "color": { "red": 0, "green": 0, "blue": 255 } }]),
    );
    bind(
        &mut r,
        "G",
        "HEALTH",
        json!([{ "device-type": "keyboard", "zone": "function-keys",
        "color": { "red": 0, "green": 255, "blue": 0 } }]),
    );
    bitmap_event(&mut r, "partial-bitmap", Some(json!(["AMMO", "HEALTH"])));
    send(&mut r, "G", "AMMO", json!({ "value": 30 }), 0.0);
    send(&mut r, "G", "HEALTH", json!({ "value": 90 }), 0.0);
    r
}

#[test]
fn a_partial_bitmap_leaves_the_excluded_events_keys_alone() {
    let mut r = background_with_ammo_and_health();
    let all_red = json!({ "bitmap": vec![json!([255, 0, 0]); bitmap::CELLS] });
    send(
        &mut r,
        "G",
        "BITMAP_EVENT",
        json!({ "frame": all_red }),
        1.0,
    );
    let layer = r.paint(1.1).unwrap();
    assert_eq!(
        layer[&0x3A], GREEN,
        "F1 stays HEALTH's though the background came later"
    );
    assert_eq!(layer[&0x14], BLUE, "Q stays AMMO's");
    assert_eq!(layer[&0x04], RED, "A is the background's");
    assert_eq!(layer[&0x29], RED, "so is Esc");
    // "To send an update to the event above that overwrites AMMO and only leaves HEALTH
    // visible": the frame's excluded-events overrides the handler's.
    let health_only = json!({ "bitmap": vec![json!([255, 0, 0]); bitmap::CELLS],
        "excluded-events": ["HEALTH"] });
    send(
        &mut r,
        "G",
        "BITMAP_EVENT",
        json!({ "frame": health_only }),
        2.0,
    );
    let layer = r.paint(2.1).unwrap();
    assert_eq!((layer[&0x3A], layer[&0x14]), (GREEN, RED));
}

#[test]
fn a_bitmap_composes_with_a_colour_handler_on_another_zone() {
    let mut r = Registry::default();
    bitmap_event(&mut r, "bitmap", None);
    bind(
        &mut r,
        "G",
        "W",
        json!([{ "device-type": "keyboard", "zone": "w",
        "color": { "red": 0, "green": 0, "blue": 255 } }]),
    );
    let all_red = json!({ "bitmap": vec![json!([255, 0, 0]); bitmap::CELLS] });
    send(
        &mut r,
        "G",
        "BITMAP_EVENT",
        json!({ "frame": all_red.clone() }),
        0.0,
    );
    send(&mut r, "G", "W", json!({ "value": 1 }), 1.0);
    let layer = r.paint(1.1).unwrap();
    assert_eq!(
        layer[&0x1A], BLUE,
        "W changed after the bitmap: it paints over it"
    );
    assert_eq!(layer[&0x16], RED, "S is the bitmap's");
    send(
        &mut r,
        "G",
        "BITMAP_EVENT",
        json!({ "frame": all_red }),
        2.0,
    );
    assert_eq!(
        r.paint(2.1).unwrap()[&0x1A],
        RED,
        "a full bitmap that changes later covers every key, as the SDK says"
    );
}

#[test]
fn summaries_say_what_each_handler_draws_and_whether_the_game_is_on_the_screen() {
    let mut r = background_with_ammo_and_health();
    bind(
        &mut r,
        "G",
        "KILLS",
        screened(json!([{ "has-text": true, "icon-id": 7, "length-millis": 500 }])),
    );
    bind(
        &mut r,
        "G",
        "RUMBLE",
        json!([{ "device-type": "tactile", "zone": "one", "mode": "vibrate" }]),
    );
    let s = &r.summaries(0.5)[0];
    assert!(!s.on_screen);
    let draws: Vec<(&str, &Draws)> = s.handlers.iter().map(|(e, d)| (e.as_str(), d)).collect();
    assert_eq!(draws[0], ("AMMO", &Draws::Keys));
    assert_eq!(
        draws[1],
        (
            "BITMAP_EVENT",
            &Draws::PartialBitmap {
                excluded: vec!["AMMO".into(), "HEALTH".into()]
            }
        )
    );
    assert_eq!(draws[2], ("HEALTH", &Draws::Keys));
    let Draws::Screen { notes } = draws[3].1 else {
        panic!("{draws:?}")
    };
    assert_eq!(draws[3].0, "KILLS");
    assert!(notes[0].contains("icon 7"));
    assert_eq!(
        draws[4],
        (
            "RUMBLE",
            &Draws::Elsewhere {
                device: "tactile".into()
            }
        )
    );
    send(&mut r, "G", "KILLS", json!({ "value": 3 }), 1.0);
    assert!(r.summaries(1.2)[0].on_screen);
    assert!(!r.summaries(1.6)[0].on_screen, "its frame is over");
    let back = Registry::restore(r.saved());
    assert_eq!(
        back.summaries(0.0)[0].handlers,
        r.summaries(0.0)[0].handlers
    );
}
