// SPDX-License-Identifier: GPL-3.0-or-later
//! The screen handler examples of the SDK's `json-handlers-screen.md` (pinned in the audit's
//! evidence), parsed, timed and drawn. Expected pictures follow the layout in `draw.rs`: each
//! test works out the positions by hand.

use super::*;
use gg_device_keyboard::oled::{HEIGHT, WIDTH};
use serde_json::json;

fn spec(v: Value) -> ScreenSpec {
    let device = v["device-type"].as_str().unwrap().to_owned();
    ScreenSpec::parse(&device, v.as_object().unwrap())
        .unwrap()
        .expect("a handler for this screen")
}

/// A `screened` handler with these `datas`.
fn screened(datas: Value) -> ScreenSpec {
    spec(json!({ "device-type": "screened", "zone": "one", "mode": "screen", "datas": datas }))
}

fn valued(value: i64) -> Data {
    Data {
        value: Some(value),
        ..Data::default()
    }
}

/// The first frame for the data's value, drawn.
fn first_picture(s: &ScreenSpec, data: &Data) -> Bitmap {
    draw(&s.frames(data.value.unwrap_or(0)).unwrap()[0], data)
}

/// `text` drawn at (`x`, `y`), each font pixel `scale` × `scale`.
fn text_at(x: usize, y: usize, text: &str, scale: usize) -> Bitmap {
    let mut b = Bitmap::default();
    b.draw_text(x, y, text, scale);
    b
}

/// Everything lit in either picture.
fn both(a: &Bitmap, b: &Bitmap) -> Bitmap {
    let mut out = a.clone();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if b.get(x, y) {
                out.set(x, y, true);
            }
        }
    }
    out
}

fn show(s: &ScreenSpec, value: i64) -> Show {
    Show {
        frames: s.frames(value).unwrap().to_vec(),
        data: valued(value),
        started: 10.0,
    }
}

#[test]
fn the_value_alone_is_drawn_as_big_as_it_fits() {
    // "With an event data value of 15, the text "15" will be displayed on the screen."
    let s = screened(json!([{ "has-text": true }]));
    // Two characters at three times the font: 48 × 24, centred.
    assert_eq!(first_picture(&s, &valued(15)), text_at(40, 8, "15", 3));
}

#[test]
fn prefix_and_suffix_go_round_the_value() {
    // "With an event data value of 15, the text "Got 15 kills" will be displayed"
    let s = screened(json!([{ "has-text": true, "prefix": "Got ", "suffix": " kills" }]));
    // Twelve characters: too wide at twice the size (192 px), so the font's own size.
    assert_eq!(
        first_picture(&s, &valued(15)),
        text_at(16, 16, "Got 15 kills", 1)
    );
}

#[test]
fn bold_draws_the_text_twice_a_pixel_apart() {
    let s = screened(json!([{ "has-text": true, "bold": true }]));
    let want = both(&text_at(40, 8, "15", 3), &text_at(41, 8, "15", 3));
    assert_eq!(first_picture(&s, &valued(15)), want);
}

#[test]
fn the_progress_bar_example_draws_a_label_over_a_bar() {
    let s = screened(json!([{ "lines": [
        { "arg": "", "prefix": "Progress" },
        { "has-progress-bar": true }
    ] }]));
    // Two rows of 8 with a 2-pixel gap: 18 high, from y 11. The label is centred; the bar's
    // outline runs from x 1 to 126 and y 21 to 28; inside it 122 columns, 60 % = 73 filled.
    let mut want = text_at(32, 11, "Progress", 1);
    for x in 1..=126 {
        want.set(x, 21, true);
        want.set(x, 28, true);
    }
    for y in 21..=28 {
        want.set(1, y, true);
        want.set(126, y, true);
    }
    for x in 3..76 {
        for y in 23..=26 {
            want.set(x, y, true);
        }
    }
    assert_eq!(first_picture(&s, &valued(60)), want);
    let empty = first_picture(&s, &valued(0));
    assert!(
        !empty.get(3, 24) && empty.get(1, 24),
        "0 %: only the outline"
    );
    let full = first_picture(&s, &valued(100));
    assert!(
        full.get(3, 24) && full.get(124, 24) && !full.get(125, 24),
        "100 %: all 122"
    );
}

#[test]
fn wrap_gives_a_line_more_rows_and_context_frame_keys_pick_the_text() {
    let s = screened(json!([{ "lines": [
        { "has-text": true, "context-frame-key": "first-line", "wrap": 1 },
        { "has-text": true, "context-frame-key": "second-line" }
    ] }]));
    let frame = json!({ "first-line": "this is some text that wraps", "second-line": "hi" });
    let data = Data {
        value: Some(1),
        frame: frame.as_object().unwrap().clone(),
        image: None,
    };
    // Three rows, 8 + 2 + 8 + 2 + 8 = 28 high, from y 6; each centred.
    let want = both(
        &both(
            &text_at(16, 6, "this is some", 1),
            &text_at(4, 16, "text that wraps", 1),
        ),
        &text_at(56, 26, "hi", 1),
    );
    assert_eq!(first_picture(&s, &data), want);
}

#[test]
fn the_docs_accessors_fill_lines_from_the_frame() {
    let s = screened(json!([{ "lines": [
        { "has-text": true, "arg": "(string-upcase (textvalue: (context-frame: self)))", "wrap": 1 },
        { "has-text": true, "arg": "(/ (numericalvalue: (context-frame: self)) 44)" }
    ] }]));
    let frame = json!({ "textvalue": "this is some text", "numericalvalue": 88 });
    let data = Data {
        value: Some(56),
        frame: frame.as_object().unwrap().clone(),
        image: None,
    };
    let want = both(
        &both(
            &text_at(16, 6, "THIS IS SOME", 1),
            &text_at(48, 16, "TEXT", 1),
        ),
        &text_at(60, 26, "2", 1),
    );
    assert_eq!(first_picture(&s, &data), want);
    assert_eq!(s.frame_keys(), vec!["numericalvalue", "textvalue"]);
}

#[test]
fn an_icon_takes_the_left_32_columns_and_is_noted() {
    let s = screened(json!([{ "has-text": true, "suffix": "stuff", "icon-id": 16 }]));
    // "15stuff": 7 characters in the 96 columns right of the icon; twice the size is 112, too
    // wide, so the font's size, centred in those columns.
    assert_eq!(
        first_picture(&s, &valued(15)),
        text_at(52, 16, "15stuff", 1)
    );
    assert_eq!(s.notes.len(), 1);
    assert!(s.notes[0].contains("icon 16"), "{:?}", s.notes);
}

#[test]
fn a_raw_picture_is_drawn_most_significant_bit_first() {
    let mut rows = vec![0u8; IMAGE_BYTES];
    rows[0] = 240; // "0b11110000, or 240, is four white pixels followed by four black pixels"
    rows[16] = 0x01; // row 1, column 7
    rows[IMAGE_BYTES - 1] = 0x01; // the bottom-right pixel
    let s = spec(
        json!({ "device-type": "screened-128x40", "zone": "one", "mode": "screen",
        "datas": [{ "has-text": false, "image-data": rows }] }),
    );
    assert!(s.exact && s.has_image());
    let mut want = Bitmap::default();
    for x in 0..4 {
        want.set(x, 0, true);
    }
    want.set(7, 1, true);
    want.set(127, 39, true);
    assert_eq!(first_picture(&s, &valued(1)), want);
}

#[test]
fn an_events_own_picture_replaces_the_handlers() {
    let s = spec(
        json!({ "device-type": "screened-128x40", "zone": "one", "mode": "screen",
        "datas": [{ "has-text": false, "image-data": vec![0xFFu8; IMAGE_BYTES] }] }),
    );
    let mut own = vec![0u8; IMAGE_BYTES];
    own[0] = 0x80;
    let data = Data {
        image: Some(own),
        ..Data::default()
    };
    let mut want = Bitmap::default();
    want.set(0, 0, true);
    assert_eq!(first_picture(&s, &data), want);
    // `has-text: false` with no picture of its own waits for the event's.
    let bare = screened(json!([{ "has-text": false }]));
    assert!(bare.has_image());
    assert_eq!(first_picture(&bare, &data), want);
    assert!(first_picture(&bare, &valued(1)).is_dark());
}

#[test]
fn the_docs_128x36_picture_is_for_another_screen() {
    // The doc's own example targets `screened-128x36`, 576 bytes.
    let other = json!({ "device-type": "screened-128x36", "mode": "screen",
        "datas": [{ "has-text": false, "image-data": vec![0u8; 576] }] });
    assert_eq!(
        ScreenSpec::parse("screened-128x36", other.as_object().unwrap()),
        Ok(None)
    );
    // Sent to any screen, it can't be drawn here: dark, with a note.
    let any = screened(json!([{ "has-text": false, "image-data": vec![0u8; 576] }]));
    assert!(first_picture(&any, &valued(1)).is_dark());
    assert!(any.notes[0].contains("another screen size"));
    // For this screen's size it's a mistake.
    let wrong = json!({ "device-type": "screened-128x40", "mode": "screen",
        "datas": [{ "has-text": false, "image-data": vec![0u8; 576] }] });
    assert!(ScreenSpec::parse("screened-128x40", wrong.as_object().unwrap()).is_err());
}

#[test]
fn ranges_pick_frames_by_value() {
    let s = screened(json!([
        { "low": 0, "high": 15, "datas": [{ "has-text": true, "prefix": "low " }] },
        { "low": 16, "high": 100, "datas": [{ "has-text": true, "prefix": "high " }] }
    ]));
    // Six and seven characters fit at twice the font's size.
    assert_eq!(first_picture(&s, &valued(15)), text_at(16, 12, "low 15", 2));
    assert_eq!(first_picture(&s, &valued(16)), text_at(8, 12, "high 16", 2));
    assert!(s.frames(101).is_none(), "no range: nothing to show");
}

#[test]
fn a_timed_frame_shows_for_its_length_then_the_screen_goes_back() {
    // "the text "15 kills" will be displayed for a quarter second before the screen returns
    // to the background image."
    let s = screened(json!([{ "has-text": true, "suffix": " kills", "length-millis": 250 }]));
    let sh = show(&s, 15);
    assert_eq!(sh.at(10.0), Some((0, Some(0.25))));
    let (frame, left) = sh.at(10.2).unwrap();
    assert_eq!(frame, 0);
    assert!((left.unwrap() - 0.05).abs() < 0.002, "{left:?}");
    assert_eq!(sh.at(10.25), None);
    assert_eq!(
        sh.at(9.0),
        Some((0, Some(0.25))),
        "a clock read just before the start"
    );
}

#[test]
fn a_frame_without_a_length_stays_until_another_event() {
    // "Headshot!" for a quarter second, then "15 kills" until a new event value.
    let s = screened(json!([
        { "has-text": true, "suffix": "Headshot!", "length-millis": 250, "arg": "", "icon-id": 7 },
        { "has-text": true, "suffix": " kills", "icon-id": 6 }
    ]));
    let sh = show(&s, 15);
    assert_eq!(sh.at(10.1).map(|a| a.0), Some(0));
    assert_eq!(sh.at(10.3), Some((1, None)));
    assert_eq!(sh.at(1000.0), Some((1, None)));
    let (picture, _) = sh.picture(10.3).unwrap();
    assert_eq!(picture, text_at(32 + 16, 16, "15 kills", 1));
    assert_eq!(s.notes.len(), 2, "icons 7 and 6: {:?}", s.notes);
}

#[test]
fn repeats_on_the_last_frame_loops_the_frames() {
    // "15 frame 1" and "15 frame 2" alternate every 200 ms until a new event.
    let s = screened(json!([
        { "has-text": true, "suffix": " frame 1", "length-millis": 200 },
        { "has-text": true, "suffix": " frame 2", "length-millis": 200, "repeats": true }
    ]));
    let sh = show(&s, 15);
    let frame = |t: f64| sh.at(10.0 + t).map(|a| a.0);
    assert_eq!(
        [frame(0.1), frame(0.3), frame(0.5), frame(0.7)],
        [Some(0), Some(1), Some(0), Some(1)]
    );
    assert_eq!(frame(1000.05), Some(0));
    assert_eq!(
        sh.at(10.5).unwrap().1.map(|s| (s * 1000.0).round()),
        Some(100.0)
    );
}

#[test]
fn repeats_counts_plays_ignores_other_frames_and_0_means_forever() {
    let frames = |repeats: Value| {
        screened(json!([
            { "has-text": true, "length-millis": 200, "repeats": true },
            { "has-text": true, "length-millis": 200, "repeats": repeats }
        ]))
    };
    let twice = show(&frames(json!(2)), 1);
    assert_eq!(twice.at(10.7).map(|a| a.0), Some(1), "second play");
    assert_eq!(twice.at(10.8), None, "played twice");
    let once = show(&frames(json!(false)), 1);
    assert_eq!(
        once.at(10.45),
        None,
        "the first frame's repeats doesn't count"
    );
    let forever = show(&frames(json!(0)), 1);
    assert_eq!(forever.at(99.9).map(|a| a.0), Some(1));
}

#[test]
fn handlers_for_other_screens_are_not_ours_and_mistakes_are_errors() {
    let parse = |v: Value| {
        let device = v["device-type"].as_str().unwrap_or_default().to_owned();
        ScreenSpec::parse(&device, v.as_object().unwrap())
    };
    let other_zone = json!({ "device-type": "screened", "zone": "two", "mode": "screen",
        "datas": [{ "has-text": true }] });
    assert_eq!(parse(other_zone), Ok(None));
    for bad in [
        json!({ "device-type": "screened", "zone": "one", "datas": [{ "has-text": true }] }),
        json!({ "device-type": "screened", "mode": "screen" }),
        json!({ "device-type": "screened", "mode": "screen", "datas": [] }),
        json!({ "device-type": "screened", "mode": "screen", "datas": ["text"] }),
        json!({ "device-type": "screened", "mode": "screen", "datas": [{ "lines": [] }] }),
        json!({ "device-type": "screened", "mode": "screen", "datas": [{ "repeats": "often" }] }),
        json!({ "device-type": "screened", "mode": "screen", "datas": [{ "length-millis": -5 }] }),
        json!({ "device-type": "screened", "mode": "screen", "datas": [{ "prefix": 3 }] }),
        json!({ "device-type": "screened", "mode": "screen", "datas": [{ "low": 0, "datas": [] }] }),
    ] {
        assert!(parse(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn an_arg_beyond_the_docs_forms_shows_no_value_and_is_noted() {
    let s = screened(json!([{ "has-text": true, "prefix": "x", "arg": "(+ (value: self) 1)" }]));
    assert_eq!(first_picture(&s, &valued(5)), text_at(52, 8, "x", 3));
    assert!(s.notes[0].contains("(+ (value: self) 1)"), "{:?}", s.notes);
}

#[test]
fn five_rows_fill_the_screen_with_no_gaps_and_more_are_cut() {
    let line = |t: &str| json!({ "has-text": true, "arg": "", "prefix": t });
    let s = screened(
        json!([{ "lines": [line("1"), line("2"), line("3"), line("4"), line("5"), line("6")] }]),
    );
    let mut want = Bitmap::default();
    for (i, t) in ["1", "2", "3", "4", "5"].iter().enumerate() {
        want = both(&want, &text_at(60, i * 8, t, 1));
    }
    assert_eq!(first_picture(&s, &valued(0)), want);
}
