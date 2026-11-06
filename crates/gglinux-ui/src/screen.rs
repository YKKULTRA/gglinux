// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apex Pro TKL Gen 3's screen, in the keyboard card: the keyboard's own,
//! text, a clock or a picture, and burn-in care.

use std::rc::Rc;

use gg_ipc::{KeyboardState, Request, Screen, ScreenCare};
use slint::{ComponentHandle, Rgb8Pixel, SharedPixelBuffer, SharedString};

use crate::client::{Client, State, TIMEOUT};
use crate::keyboard::{keyboard, marker};
use crate::{act, act_then, AppWindow, Forms, ScreenUi};

/// The screen: 128 × 40, one bit a pixel, 16 bytes a row.
pub const W: usize = 128;
pub const H: usize = 40;

/// What the screen panel keeps between the daemon's answers.
#[derive(Default)]
pub struct ScreenForm {
    /// The screen tab picked but not sent yet (text and pictures wait for "Show").
    pub picked: Option<i32>,
    pub text: String,
    /// The daemon's text last seen, so its own change shows and a refresh doesn't
    /// undo typing.
    seen_text: Option<String>,
    pub picture: Option<Picture>,
}

impl ScreenForm {
    /// Take in what the daemon has: the text follows only when the daemon's
    /// own changes, so a refresh doesn't undo typing.
    pub fn follow(&mut self, kb: &KeyboardState) {
        if let Screen::Text { text } = &kb.screen {
            if self.seen_text.as_ref() != Some(text) {
                self.seen_text = Some(text.clone());
                self.text = text.clone();
            }
        }
    }
}

/// A picture chosen for the screen, and how it's turned into 1-bit pixels.
pub struct Picture {
    pub name: String,
    pub rgba: image::RgbaImage,
    pub threshold: u8,
    pub dither: bool,
    pub invert: bool,
    pub pixels: Vec<u8>,
}

impl Picture {
    pub fn new(name: String, rgba: image::RgbaImage) -> Picture {
        let mut p = Picture {
            name,
            rgba,
            threshold: 128,
            dither: true,
            invert: false,
            pixels: Vec::new(),
        };
        p.convert();
        p
    }

    pub fn convert(&mut self) {
        self.pixels = convert(&self.rgba, self.threshold, self.invert, self.dither);
    }
}

pub fn show(ui: &AppWindow, s: &State, forms: &Forms) {
    let Some(kb) = s.keyboard.as_ref() else {
        // Gone with the daemon, like the lighting's paint, and errors from before too.
        reset(forms);
        mark(ui, false, SharedString::new());
        return;
    };
    let mut f = forms.screen.borrow_mut();
    f.follow(kb);
    show_screen(ui, kb, &f);
}

fn screen_kind(s: &Screen) -> i32 {
    match s {
        Screen::Keyboard => 0,
        Screen::Text { .. } => 1,
        Screen::Clock { .. } => 2,
        Screen::Image { .. } => 3,
        // Shown as a picture until the screen unit (M11 A4) gives them their own modes.
        Screen::Animation { .. } => 3,
        Screen::Monitor { .. } => 0,
    }
}

fn clock_of(s: &Screen) -> (bool, bool, bool) {
    match s {
        Screen::Clock { seconds, h24, date } => (*h24, *seconds, *date),
        _ => (true, false, true),
    }
}

/// Nothing to show but spaces.
pub fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// More than the screen's 5 lines of 16 characters.
pub fn too_long(text: &str) -> bool {
    let lines: Vec<&str> = text.split('\n').collect();
    lines.len() > Screen::MAX_LINES
        || lines
            .iter()
            .any(|l| l.chars().count() > Screen::MAX_COLUMNS)
}

fn show_screen(ui: &AppWindow, kb: &KeyboardState, f: &ScreenForm) {
    let g = ui.global::<ScreenUi>();
    g.set_kind(f.picked.unwrap_or(screen_kind(&kb.screen)));
    if g.get_text().as_str() != f.text {
        g.set_text(f.text.clone().into());
    }
    g.set_too_long(too_long(&f.text));
    g.set_blank(blank(&f.text));
    let (h24, seconds, date) = clock_of(&kb.screen);
    g.set_h24(h24);
    g.set_seconds(seconds);
    g.set_date(date);
    show_picture(ui, kb, f);
    let c = kb.screen_care;
    g.set_care(true);
    g.set_shift(c.shift);
    g.set_away(c.dark_when_away);
    g.set_own_too(c.own_screen_too);
    g.set_resting(kb.screen_resting);
    g.set_refused(kb.screen_error.clone().unwrap_or_default().into());
}

fn show_picture(ui: &AppWindow, kb: &KeyboardState, f: &ScreenForm) {
    let g = ui.global::<ScreenUi>();
    let bits = match (&f.picture, &kb.screen) {
        (Some(p), _) => Some(p.pixels.as_slice()),
        (None, Screen::Image { pixels }) => Some(pixels.as_slice()),
        _ => None,
    };
    g.set_has_preview(bits.is_some());
    if let Some(bits) = bits {
        g.set_preview(preview(bits));
    }
    g.set_has_source(f.picture.is_some());
    if let Some(p) = &f.picture {
        g.set_file_name(p.name.clone().into());
        g.set_threshold(p.threshold as i32);
        g.set_dither(p.dither);
        g.set_invert(p.invert);
    }
}

/// The 1-bit picture as the screen would show it.
pub fn preview(bits: &[u8]) -> slint::Image {
    let mut buf = SharedPixelBuffer::<Rgb8Pixel>::new(W as u32, H as u32);
    for (i, px) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i % W, i / W);
        let on = bits
            .get(y * 16 + x / 8)
            .is_some_and(|b| b & (0x80 >> (x % 8)) != 0);
        let v = if on { 235 } else { 10 };
        *px = Rgb8Pixel { r: v, g: v, b: v };
    }
    slint::Image::from_rgb8(buf)
}

/// A picture fitted into 128 × 40 on black, one bit a pixel, 16 bytes a row,
/// the leftmost pixel in each byte's high bit. Floyd–Steinberg dithering keeps
/// the greys.
pub fn convert(img: &image::RgbaImage, threshold: u8, invert: bool, dither: bool) -> Vec<u8> {
    let (iw, ih) = img.dimensions();
    let mut lum = vec![0f32; W * H];
    if iw > 0 && ih > 0 {
        let scale = (W as f32 / iw as f32).min(H as f32 / ih as f32);
        let (w, h) = (
            ((iw as f32 * scale).round() as u32).clamp(1, W as u32),
            ((ih as f32 * scale).round() as u32).clamp(1, H as u32),
        );
        let small = image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle);
        let (ox, oy) = ((W as u32 - w) / 2, (H as u32 - h) / 2);
        for (x, y, p) in small.enumerate_pixels() {
            // Transparent parts are on black, as the canvas drew them.
            let a = p[3] as f32 / 255.0;
            let l = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) * a;
            lum[(y + oy) as usize * W + (x + ox) as usize] = l;
        }
    }
    let mut bits = vec![0u8; Screen::IMAGE_BYTES];
    for y in 0..H {
        for x in 0..W {
            let i = y * W + x;
            let on = lum[i] >= threshold as f32;
            if dither {
                let err = lum[i] - if on { 255.0 } else { 0.0 };
                if x + 1 < W {
                    lum[i + 1] += err * 7.0 / 16.0;
                }
                if y + 1 < H {
                    if x > 0 {
                        lum[i + W - 1] += err * 3.0 / 16.0;
                    }
                    lum[i + W] += err * 5.0 / 16.0;
                    if x + 1 < W {
                        lum[i + W + 1] += err / 16.0;
                    }
                }
            }
            if on != invert {
                bits[y * 16 + (x >> 3)] |= 0x80 >> (x & 7);
            }
        }
    }
    bits
}

fn mark(ui: &AppWindow, busy: bool, error: SharedString) {
    let g = ui.global::<ScreenUi>();
    g.set_busy(busy);
    g.set_error(error);
}

fn redraw(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    show(ui, &client.state(), forms);
}

/// A picture as if it was chosen: a grey ramp behind a dark ring, so the
/// dithering shows (the shot example's step; the portal needs a desktop).
fn test_picture() -> Picture {
    let img = image::RgbaImage::from_fn(256, 80, |x, y| {
        let (dx, dy) = (x as f32 - 128.0, (y as f32 - 40.0) * 2.0);
        let r = (dx * dx + dy * dy).sqrt();
        let v = if (48.0..64.0).contains(&r) {
            0
        } else {
            x as u8
        };
        image::Rgba([v, v, v, 255])
    });
    Picture::new("ramp.png".into(), img)
}

pub fn bind(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    let g = ui.global::<ScreenUi>();
    let send = {
        let weak = ui.as_weak();
        let client = client.clone();
        move |screen: Screen| {
            let Some(ui) = weak.upgrade() else { return };
            act_then(
                &ui,
                &client,
                Request::SetScreen { screen },
                TIMEOUT,
                marker(&client, mark, show),
                |_, forms| {
                    forms.screen.borrow_mut().picked = None;
                },
            );
        }
    };
    let weak = ui.as_weak();
    let client2 = client.clone();
    let forms2 = forms.clone();
    let send2 = send.clone();
    g.on_pick(move |k| {
        let Some(ui) = weak.upgrade() else { return };
        forms2.screen.borrow_mut().picked = Some(k);
        redraw(&ui, &client2, &forms2);
        // The keyboard's own screen and the clock are shown at once; text and
        // pictures wait for "Show".
        let current = keyboard(&client2).map(|kb| kb.screen).unwrap_or_default();
        match k {
            0 => send2(Screen::Keyboard),
            2 => {
                let (h24, seconds, date) = clock_of(&current);
                send2(Screen::Clock { seconds, h24, date });
            }
            _ => {}
        }
    });
    let weak = ui.as_weak();
    let forms2 = forms.clone();
    g.on_text_edited(move |t| {
        forms2.screen.borrow_mut().text = t.to_string();
        if let Some(ui) = weak.upgrade() {
            let g = ui.global::<ScreenUi>();
            g.set_too_long(too_long(t.as_str()));
            g.set_blank(blank(t.as_str()));
        }
    });
    let forms2 = forms.clone();
    let send2 = send.clone();
    g.on_show_text(move || {
        let text = forms2.screen.borrow().text.clone();
        send2(Screen::Text { text });
    });
    let send2 = send.clone();
    g.on_set_clock(move |h24, seconds, date| send2(Screen::Clock { seconds, h24, date }));
    let forms2 = forms.clone();
    let send2 = send.clone();
    g.on_show_picture(move || {
        let pixels = forms2
            .screen
            .borrow()
            .picture
            .as_ref()
            .map(|p| p.pixels.clone());
        if let Some(pixels) = pixels {
            send2(Screen::Image { pixels });
        }
    });
    // The threshold follows the slider as it's dragged, so only the preview is
    // drawn again, and only when something changed.
    let picture = |change: fn(&mut Picture, i32)| {
        let weak = ui.as_weak();
        let client = client.clone();
        let forms = forms.clone();
        move |v: i32| {
            let Some(ui) = weak.upgrade() else { return };
            let mut f = forms.screen.borrow_mut();
            let Some(p) = f.picture.as_mut() else { return };
            let before = (p.threshold, p.dither, p.invert);
            change(p, v);
            if before == (p.threshold, p.dither, p.invert) {
                return;
            }
            p.convert();
            if let Some(kb) = keyboard(&client) {
                show_picture(&ui, &kb, &f);
            }
        }
    };
    g.on_set_threshold(picture(|p, v| p.threshold = v.clamp(1, 254) as u8));
    let f = picture(|p, v| p.dither = v != 0);
    g.on_set_dither(move |on| f(on as i32));
    let f = picture(|p, v| p.invert = v != 0);
    g.on_set_invert(move |on| f(on as i32));

    let weak = ui.as_weak();
    let client2 = client.clone();
    g.on_choose_picture(move || {
        let weak = weak.clone();
        client2.spawn(async move {
            let loaded = match crate::system::pick_picture().await {
                Ok(Some(path)) => tokio::task::spawn_blocking(move || {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    image::open(&path)
                        .map(|img| (name, img.to_rgba8()))
                        .map_err(|_| "That file isn't a picture this app can read.".to_string())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string())),
                Ok(None) => return,
                Err(e) => Err(e),
            };
            let _ = weak.upgrade_in_event_loop(move |ui| match loaded {
                Ok((name, rgba)) => {
                    crate::with_forms(|forms| {
                        forms.screen.borrow_mut().picture = Some(Picture::new(name, rgba));
                    });
                    ui.global::<ScreenUi>().set_error(SharedString::new());
                    crate::refresh(&ui);
                }
                Err(e) => ui.global::<ScreenUi>().set_error(e.into()),
            });
        });
    });

    let weak = ui.as_weak();
    let client2 = client.clone();
    g.on_set_care(move |shift, away, own| {
        let Some(ui) = weak.upgrade() else { return };
        let care = ScreenCare {
            shift,
            dark_when_away: away,
            own_screen_too: own,
        };
        act(
            &ui,
            &client2,
            Request::SetScreenCare { care },
            TIMEOUT,
            marker(&client2, mark, show),
        );
    });
}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.screen.borrow_mut() = ScreenForm::default();
}

/// Sample text for the shot example: 0 fits, 1 is a line too long.
const SAMPLE_TEXT: [&str; 2] = ["gglinux\nhello", "gglinux\nthis line is too long"];

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]).
pub(crate) fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let screen = ui.global::<ScreenUi>();
    match name {
        "screen.pick" => screen.invoke_pick(n),
        "screen.text" => {
            let t = SAMPLE_TEXT[(n as usize).min(1)];
            screen.set_text(t.into());
            screen.invoke_text_edited(t.into());
        }
        "screen.show-text" => screen.invoke_show_text(),
        "screen.set-clock" => screen.invoke_set_clock(true, n != 0, true),
        "screen.test-picture" => {
            crate::with_forms(|forms| forms.screen.borrow_mut().picture = Some(test_picture()));
            crate::refresh(ui);
        }
        "screen.set-threshold" => screen.invoke_set_threshold(n),
        "screen.set-dither" => screen.invoke_set_dither(n != 0),
        "screen.show-picture" => screen.invoke_show_picture(),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::tests::board;

    #[test]
    fn text_is_too_long_past_five_lines_or_sixteen_columns() {
        assert!(!too_long("hello\nworld"));
        assert!(!too_long("0123456789abcdef"));
        assert!(too_long("0123456789abcdefg"));
        assert!(too_long("1\n2\n3\n4\n5\n6"));
    }

    #[test]
    fn a_white_picture_lights_every_pixel_and_inverting_clears_them() {
        let white = image::RgbaImage::from_pixel(256, 80, image::Rgba([255, 255, 255, 255]));
        assert!(convert(&white, 128, false, true).iter().all(|b| *b == 0xff));
        assert!(convert(&white, 128, true, true).iter().all(|b| *b == 0));
    }

    #[test]
    fn a_narrow_picture_is_centred_on_black() {
        // 40 × 40 white in a 128 × 40 screen: columns 44..84 lit, the rest dark.
        let white = image::RgbaImage::from_pixel(40, 40, image::Rgba([255, 255, 255, 255]));
        let bits = convert(&white, 128, false, false);
        let lit = |x: usize, y: usize| bits[y * 16 + x / 8] & (0x80 >> (x % 8)) != 0;
        assert!(!lit(0, 20) && !lit(43, 20) && lit(44, 20) && lit(83, 20) && !lit(84, 20));
        assert_eq!(bits.len(), Screen::IMAGE_BYTES);
    }

    #[test]
    fn mid_grey_dithers_to_about_half_lit() {
        let grey = image::RgbaImage::from_pixel(128, 40, image::Rgba([128, 128, 128, 255]));
        let lit: u32 = convert(&grey, 128, false, true)
            .iter()
            .map(|b| b.count_ones())
            .sum();
        let half = (W * H / 2) as u32;
        assert!(lit.abs_diff(half) < half / 10, "{lit} of {}", W * H);
    }

    #[test]
    fn the_text_follows_the_daemon_only_when_its_own_changes() {
        let mut f = ScreenForm::default();
        let hello = || {
            board(
                &[],
                Screen::Text {
                    text: "hello".into(),
                },
            )
        };
        f.follow(&hello());
        assert_eq!(f.text, "hello");
        f.text = "hello there".into();
        // The same text again (any refresh) leaves the typing alone.
        f.follow(&hello());
        assert_eq!(f.text, "hello there");
        // Another screen leaves it too.
        f.follow(&board(&[], Screen::Keyboard));
        assert_eq!(f.text, "hello there");
        // New text from elsewhere (ggctl, a profile) is shown.
        f.follow(&board(&[], Screen::Text { text: "hi".into() }));
        assert_eq!(f.text, "hi");
    }

    #[test]
    fn text_of_only_spaces_is_blank() {
        assert!(blank(""));
        assert!(blank("  \n \n"));
        assert!(!blank(" a "));
        // Five full lines fit; a sixth, or a seventeenth character, doesn't.
        let full = ["0123456789abcdef"; 5].join("\n");
        assert!(!too_long(&full));
        assert!(too_long(&(full.clone() + "\nx")));
        assert!(too_long(&full.replacen('0', "00", 1)));
    }

    #[test]
    fn without_dithering_the_threshold_alone_decides() {
        let grey = image::RgbaImage::from_pixel(128, 40, image::Rgba([100, 100, 100, 255]));
        let lit = |t: u8, invert: bool| -> u32 {
            convert(&grey, t, invert, false)
                .iter()
                .map(|b| b.count_ones())
                .sum()
        };
        let all = (W * H) as u32;
        assert_eq!(lit(99, false), all);
        assert_eq!(lit(101, false), 0);
        assert_eq!(lit(101, true), all);
    }

    #[test]
    fn transparent_parts_are_black() {
        let clear = image::RgbaImage::from_pixel(128, 40, image::Rgba([255, 255, 255, 0]));
        assert!(convert(&clear, 1, false, false).iter().all(|b| *b == 0));
    }

    #[test]
    fn the_preview_lights_the_pixels_the_bits_say() {
        let mut bits = vec![0u8; Screen::IMAGE_BYTES];
        // The top-left pixel, and the last one of the second row.
        bits[0] = 0x80;
        bits[16 + 15] = 0x01;
        let img = preview(&bits).to_rgb8().unwrap();
        let px = |x: usize, y: usize| img.as_slice()[y * W + x].r;
        assert_eq!((px(0, 0), px(1, 0)), (235, 10));
        assert_eq!((px(127, 1), px(126, 1)), (235, 10));
    }
}
