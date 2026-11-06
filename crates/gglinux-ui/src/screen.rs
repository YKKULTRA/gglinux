// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apex Pro TKL Gen 3's screen, in the keyboard card: the keyboard's own,
//! text, a clock, a picture, an animation or the System Monitor, and burn-in care.

use std::io::Cursor;
use std::rc::Rc;

use gg_ipc::{KeyboardState, MonitorItem, Request, Screen, ScreenCare};
use image::{AnimationDecoder, ImageFormat};
use slint::{ComponentHandle, Rgb8Pixel, SharedPixelBuffer, SharedString};

use crate::client::{Client, State, TIMEOUT};
use crate::keyboard::{keyboard, marker};
use crate::{act, act_then, AppWindow, Forms, ScreenUi};

/// The screen: 128 × 40, one bit a pixel, 16 bytes a row.
pub const W: usize = 128;
pub const H: usize = 40;

/// The panel's tabs, in the order `screen.slint` lists them.
const OWN: i32 = 0;
const TEXT: i32 = 1;
const CLOCK: i32 = 2;
const PICTURE: i32 = 3;
const ANIMATION: i32 = 4;
const MONITOR: i32 = 5;

/// The System Monitor's readings, in the order they're offered and drawn.
pub const READINGS: [MonitorItem; 6] = [
    MonitorItem::Cpu,
    MonitorItem::CpuTemp,
    MonitorItem::Ram,
    MonitorItem::Gpu,
    MonitorItem::GpuTemp,
    MonitorItem::Clock,
];
/// What the System Monitor shows when it's first picked: room left for one more.
pub const FIRST_READINGS: [MonitorItem; 3] =
    [MonitorItem::Cpu, MonitorItem::CpuTemp, MonitorItem::Ram];

const NOT_A_PICTURE: &str = "That file isn't a picture this app can read.";

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
    pub animation: Option<Animation>,
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

/// An animation chosen for the screen: each of its pictures is turned into 1-bit pixels
/// like a still one, with the same threshold, dithering and inverting for all.
pub struct Animation {
    pub name: String,
    /// Each picture already fitted into the screen, in colour: the threshold can change
    /// without decoding the file again.
    pub fitted: Vec<image::RgbaImage>,
    /// How long each picture shows, pulled into what the screen allows.
    pub delays_ms: Vec<u16>,
    /// The file had more pictures than an animation holds; the rest were left out.
    pub cut: bool,
    /// Some pictures were meant to show for less than the shortest time allowed.
    pub slowed: bool,
    pub threshold: u8,
    pub dither: bool,
    pub invert: bool,
    pub frames: Vec<Vec<u8>>,
}

impl Animation {
    pub fn convert(&mut self) {
        self.frames = self
            .fitted
            .iter()
            .map(|f| bits(f, self.threshold, self.invert, self.dither))
            .collect();
    }

    pub fn screen(&self) -> Screen {
        Screen::Animation {
            frames: self.frames.clone(),
            delays_ms: self.delays_ms.clone(),
        }
    }
}

/// A file read for the screen: one picture, or several that move.
pub enum Loaded {
    Still(Picture),
    Moving(Animation),
}

/// Read a picture file. A GIF, APNG or WebP with more than one picture becomes an
/// animation (of its first [`Screen::MAX_FRAMES`]); anything else, or one with a single
/// picture, a still.
pub fn load(name: String, bytes: &[u8]) -> Result<Loaded, String> {
    let format = image::guess_format(bytes).map_err(|_| NOT_A_PICTURE.to_string())?;
    if let Some(frames) = frames_of(format, bytes) {
        let animation = animation(name.clone(), frames)?;
        if animation.fitted.len() > 1 {
            return Ok(Loaded::Moving(animation));
        }
    }
    let rgba = image::load_from_memory_with_format(bytes, format)
        .map_err(|_| NOT_A_PICTURE.to_string())?
        .to_rgba8();
    Ok(Loaded::Still(Picture::new(name, rgba)))
}

/// The pictures of a file that can hold several, `None` for one that holds a single one.
fn frames_of(format: ImageFormat, bytes: &[u8]) -> Option<image::Frames<'_>> {
    match format {
        ImageFormat::Gif => {
            let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).ok()?;
            Some(decoder.into_frames())
        }
        ImageFormat::Png => {
            let decoder = image::codecs::png::PngDecoder::new(Cursor::new(bytes)).ok()?;
            if !decoder.is_apng().ok()? {
                return None;
            }
            Some(decoder.apng().ok()?.into_frames())
        }
        ImageFormat::WebP => {
            let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).ok()?;
            decoder.has_animation().then(|| decoder.into_frames())
        }
        _ => None,
    }
}

/// Each picture fitted into the screen as it's decoded (so a big file never sits in
/// memory whole), up to [`Screen::MAX_FRAMES`]. A picture that can't be decoded ends the
/// animation there, unless it's the first.
fn animation(name: String, frames: image::Frames<'_>) -> Result<Animation, String> {
    let mut a = Animation {
        name,
        fitted: Vec::new(),
        delays_ms: Vec::new(),
        cut: false,
        slowed: false,
        threshold: 128,
        dither: true,
        invert: false,
        frames: Vec::new(),
    };
    for frame in frames {
        let Ok(frame) = frame else {
            if a.fitted.is_empty() {
                return Err(NOT_A_PICTURE.to_string());
            }
            break;
        };
        if a.fitted.len() == Screen::MAX_FRAMES {
            a.cut = true;
            break;
        }
        let (numer, denom) = frame.delay().numer_denom_ms();
        let wanted = if denom == 0 {
            0.0
        } else {
            f64::from(numer) / f64::from(denom)
        };
        a.slowed |= wanted < f64::from(*Screen::FRAME_DELAY_MS.start());
        a.delays_ms.push(delay_ms(wanted));
        a.fitted.push(fit(frame.buffer()));
    }
    if a.fitted.is_empty() {
        return Err(NOT_A_PICTURE.to_string());
    }
    a.convert();
    Ok(a)
}

/// A picture's delay in whole milliseconds, pulled into what the screen allows
/// (0.1 to 10 s).
pub fn delay_ms(wanted: f64) -> u16 {
    let range = Screen::FRAME_DELAY_MS;
    let (least, most) = (f64::from(*range.start()), f64::from(*range.end()));
    if wanted.is_nan() {
        return *range.start();
    }
    wanted.round().clamp(least, most) as u16
}

/// "12 pictures, 1.2 s".
pub fn summary(frames: usize, delays_ms: &[u16]) -> String {
    let ms: u64 = delays_ms.iter().map(|&d| u64::from(d)).sum();
    format!("{frames} pictures, {:.1} s", ms as f64 / 1000.0)
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
        Screen::Keyboard => OWN,
        Screen::Text { .. } => TEXT,
        Screen::Clock { .. } => CLOCK,
        Screen::Image { .. } => PICTURE,
        Screen::Animation { .. } => ANIMATION,
        Screen::Monitor { .. } => MONITOR,
    }
}

fn clock_of(s: &Screen) -> (bool, bool, bool) {
    match s {
        Screen::Clock { seconds, h24, date } => (*h24, *seconds, *date),
        _ => (true, false, true),
    }
}

/// The System Monitor's readings: the daemon's while it shows them, else the first ones.
pub fn readings_of(s: &Screen) -> Vec<MonitorItem> {
    match s {
        Screen::Monitor { items } => items.clone(),
        _ => FIRST_READINGS.to_vec(),
    }
}

/// `items` with `item` turned on or off, in [`READINGS`] order; `None` if that leaves
/// none, or more than the screen holds.
pub fn with_reading(
    items: &[MonitorItem],
    item: MonitorItem,
    on: bool,
) -> Option<Vec<MonitorItem>> {
    let new: Vec<MonitorItem> = READINGS
        .into_iter()
        .filter(|&i| if i == item { on } else { items.contains(&i) })
        .collect();
    (!new.is_empty() && new.len() <= Screen::MAX_MONITOR_ITEMS).then_some(new)
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
    show_animation(ui, kb, f);
    show_readings(ui, kb);
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

/// The animation chosen here, or else the one the keyboard shows: its first picture and
/// how long a round lasts.
fn show_animation(ui: &AppWindow, kb: &KeyboardState, f: &ScreenForm) {
    let g = ui.global::<ScreenUi>();
    let (first, about) = match (&f.animation, &kb.screen) {
        (Some(a), _) => (a.frames.first(), summary(a.frames.len(), &a.delays_ms)),
        (None, Screen::Animation { frames, delays_ms }) => {
            (frames.first(), summary(frames.len(), delays_ms))
        }
        _ => (None, String::new()),
    };
    g.set_has_anim_preview(first.is_some());
    if let Some(bits) = first {
        g.set_anim_preview(preview(bits));
    }
    g.set_anim_summary(about.into());
    let a = f.animation.as_ref();
    g.set_has_anim_source(a.is_some());
    g.set_anim_cut(a.is_some_and(|a| a.cut));
    g.set_anim_slowed(a.is_some_and(|a| a.slowed));
    if let Some(a) = a {
        g.set_anim_name(a.name.clone().into());
        g.set_anim_threshold(a.threshold as i32);
        g.set_anim_dither(a.dither);
        g.set_anim_invert(a.invert);
    }
}

fn show_readings(ui: &AppWindow, kb: &KeyboardState) {
    let g = ui.global::<ScreenUi>();
    let items = readings_of(&kb.screen);
    let on = |i: MonitorItem| items.contains(&i);
    g.set_mon_cpu(on(MonitorItem::Cpu));
    g.set_mon_cpu_temp(on(MonitorItem::CpuTemp));
    g.set_mon_ram(on(MonitorItem::Ram));
    g.set_mon_gpu(on(MonitorItem::Gpu));
    g.set_mon_gpu_temp(on(MonitorItem::GpuTemp));
    g.set_mon_clock(on(MonitorItem::Clock));
    g.set_mon_count(items.len() as i32);
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
    bits(&fit(img), threshold, invert, dither)
}

/// The picture scaled to fit 128 × 40, keeping its shape (not yet centred).
pub fn fit(img: &image::RgbaImage) -> image::RgbaImage {
    let (iw, ih) = img.dimensions();
    if iw == 0 || ih == 0 {
        return image::RgbaImage::new(0, 0);
    }
    let scale = (W as f32 / iw as f32).min(H as f32 / ih as f32);
    let (w, h) = (
        ((iw as f32 * scale).round() as u32).clamp(1, W as u32),
        ((ih as f32 * scale).round() as u32).clamp(1, H as u32),
    );
    image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle)
}

/// A fitted picture centred on black and turned into 1-bit pixels, as [`convert`] says.
pub fn bits(small: &image::RgbaImage, threshold: u8, invert: bool, dither: bool) -> Vec<u8> {
    let mut lum = vec![0f32; W * H];
    let (w, h) = small.dimensions();
    let (ox, oy) = (
        (W as u32).saturating_sub(w) / 2,
        (H as u32).saturating_sub(h) / 2,
    );
    for (x, y, p) in small.enumerate_pixels() {
        if x + ox >= W as u32 || y + oy >= H as u32 {
            continue;
        }
        // Transparent parts are on black, as the canvas drew them.
        let a = p[3] as f32 / 255.0;
        let l = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) * a;
        lum[(y + oy) as usize * W + (x + ox) as usize] = l;
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

/// A GIF in memory, from pictures and their delays in ms.
fn gif(frames: Vec<image::RgbaImage>, delays_ms: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        let frames = frames.into_iter().zip(delays_ms).map(|(f, &ms)| {
            image::Frame::from_parts(f, 0, 0, image::Delay::from_numer_denom_ms(ms, 1))
        });
        encoder.encode_frames(frames).expect("a GIF in memory");
    }
    bytes
}

/// An animation as if it was chosen, read through the same path as a file: a ball
/// bouncing across a floor, twelve pictures a tenth of a second each.
fn test_animation() -> Animation {
    let frames = (0..12)
        .map(|i| {
            let cx = 20.0 + i as f32 * 20.0;
            let cy = 52.0 - 36.0 * (1.0 - ((i as f32 - 5.5) / 6.0).powi(2));
            image::RgbaImage::from_fn(256, 80, |x, y| {
                let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                let ball = dx * dx + dy * dy < 14.0 * 14.0;
                let floor = y >= 72;
                let v = if ball || floor { 255 } else { 0 };
                image::Rgba([v, v, v, 255])
            })
        })
        .collect();
    match load("bounce.gif".into(), &gif(frames, &[100; 12])) {
        Ok(Loaded::Moving(a)) => a,
        _ => unreachable!("twelve pictures make an animation"),
    }
}

/// Put what was read into the panel's working copy, on its own tab.
fn chosen(forms: &Forms, loaded: Loaded) {
    let mut f = forms.screen.borrow_mut();
    match loaded {
        Loaded::Still(p) => {
            f.picture = Some(p);
            f.picked = Some(PICTURE);
        }
        Loaded::Moving(a) => {
            f.animation = Some(a);
            f.picked = Some(ANIMATION);
        }
    }
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
        // The keyboard's own screen, the clock and the monitor are shown at once;
        // text and pictures wait for "Show".
        let current = keyboard(&client2).map(|kb| kb.screen).unwrap_or_default();
        match k {
            OWN => send2(Screen::Keyboard),
            CLOCK => {
                let (h24, seconds, date) = clock_of(&current);
                send2(Screen::Clock { seconds, h24, date });
            }
            MONITOR => send2(Screen::Monitor {
                items: readings_of(&current),
            }),
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
    let forms2 = forms.clone();
    let send2 = send.clone();
    g.on_show_animation(move || {
        let screen = forms2
            .screen
            .borrow()
            .animation
            .as_ref()
            .map(Animation::screen);
        if let Some(screen) = screen {
            send2(screen);
        }
    });
    let client2 = client.clone();
    let send2 = send.clone();
    g.on_set_reading(move |i, on| {
        let current = keyboard(&client2).map(|kb| kb.screen).unwrap_or_default();
        let Some(&item) = READINGS.get(i as usize) else {
            return;
        };
        if let Some(items) = with_reading(&readings_of(&current), item, on) {
            send2(Screen::Monitor { items });
        }
    });
    bind_conversion(ui, client, forms);
    bind_chooser(ui, client);

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

/// The threshold, dithering and inverting of a still picture and of an animation. The
/// threshold follows the slider as it's dragged, so only the preview is drawn again, and
/// only when something changed.
fn bind_conversion(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    let g = ui.global::<ScreenUi>();
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

    let animation = |change: fn(&mut Animation, i32)| {
        let weak = ui.as_weak();
        let client = client.clone();
        let forms = forms.clone();
        move |v: i32| {
            let Some(ui) = weak.upgrade() else { return };
            let mut f = forms.screen.borrow_mut();
            let Some(a) = f.animation.as_mut() else {
                return;
            };
            let before = (a.threshold, a.dither, a.invert);
            change(a, v);
            if before == (a.threshold, a.dither, a.invert) {
                return;
            }
            a.convert();
            if let Some(kb) = keyboard(&client) {
                show_animation(&ui, &kb, &f);
            }
        }
    };
    g.on_set_anim_threshold(animation(|a, v| a.threshold = v.clamp(1, 254) as u8));
    let f = animation(|a, v| a.dither = v != 0);
    g.on_set_anim_dither(move |on| f(on as i32));
    let f = animation(|a, v| a.invert = v != 0);
    g.on_set_anim_invert(move |on| f(on as i32));
}

/// The file chooser, for a picture or an animation alike: what the file holds decides
/// which tab it lands on.
fn bind_chooser(ui: &AppWindow, client: &Client) {
    let weak = ui.as_weak();
    let client2 = client.clone();
    ui.global::<ScreenUi>().on_choose_picture(move || {
        let weak = weak.clone();
        client2.spawn(async move {
            let loaded = match crate::system::pick_picture().await {
                Ok(Some(path)) => tokio::task::spawn_blocking(move || {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                    load(name, &bytes)
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string())),
                Ok(None) => return,
                Err(e) => Err(e),
            };
            let _ = weak.upgrade_in_event_loop(move |ui| match loaded {
                Ok(loaded) => {
                    crate::with_forms(|forms| chosen(forms, loaded));
                    ui.global::<ScreenUi>().set_error(SharedString::new());
                    crate::refresh(&ui);
                }
                Err(e) => ui.global::<ScreenUi>().set_error(e.into()),
            });
        });
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
        // As if a GIF of twelve pictures was chosen: it lands on the Animation tab.
        "screen.test-animation" => {
            crate::with_forms(|forms| chosen(forms, Loaded::Moving(test_animation())));
            crate::refresh(ui);
        }
        "screen.show-animation" => screen.invoke_show_animation(),
        // A System Monitor reading by its place in READINGS, on or off.
        "screen.reading-on" => screen.invoke_set_reading(n, true),
        "screen.reading-off" => screen.invoke_set_reading(n, false),
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

    /// A small picture all one grey, the screen's shape so it fills it.
    fn grey(v: u8) -> image::RgbaImage {
        image::RgbaImage::from_pixel(32, 10, image::Rgba([v, v, v, 255]))
    }

    fn moving(loaded: Result<Loaded, String>) -> Animation {
        match loaded {
            Ok(Loaded::Moving(a)) => a,
            Ok(Loaded::Still(_)) => panic!("read as a still"),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn a_gif_keeps_every_picture_each_converted_like_a_still() {
        let file = gif(vec![grey(255), grey(0), grey(128)], &[150, 200, 300]);
        let a = moving(load("three.gif".into(), &file));
        assert_eq!(a.frames.len(), 3);
        assert_eq!(a.delays_ms, [150, 200, 300]);
        assert!(!a.cut && !a.slowed);
        assert!(a.frames[0].iter().all(|&b| b == 0xff), "white: all lit");
        assert!(a.frames[1].iter().all(|&b| b == 0), "black: all dark");
        assert_eq!(
            a.frames[2],
            convert(&grey(128), 128, false, true),
            "as a still"
        );
        assert_eq!(summary(a.frames.len(), &a.delays_ms), "3 pictures, 0.7 s");
        assert_eq!(
            a.screen().validate(),
            Ok(()),
            "what's sent is what the daemon takes"
        );
    }

    #[test]
    fn delays_are_pulled_into_what_the_screen_allows() {
        // GIF counts in hundredths: 20 ms, then 0 ("as fast as you can"), then 20 s.
        let file = gif(vec![grey(0), grey(255), grey(0)], &[20, 0, 20_000]);
        let a = moving(load("fast.gif".into(), &file));
        assert_eq!(a.delays_ms, [100, 100, 10_000]);
        assert!(a.slowed, "the panel says so");
        assert_eq!(delay_ms(99.4), 100);
        assert_eq!(delay_ms(100.6), 101);
        assert_eq!(delay_ms(1e9), 10_000);
        assert_eq!(delay_ms(f64::NAN), 100, "nothing sensible: the shortest");
    }

    #[test]
    fn an_animation_is_cut_at_256_pictures() {
        let frames: Vec<_> = (0..300)
            .map(|i| grey(if i % 2 == 0 { 0 } else { 255 }))
            .collect();
        let a = moving(load("long.gif".into(), &gif(frames, &[100; 300])));
        assert_eq!(a.frames.len(), Screen::MAX_FRAMES);
        assert_eq!(a.delays_ms.len(), Screen::MAX_FRAMES);
        assert!(a.cut, "the panel says so");
        let exactly = (0..256)
            .map(|i| grey(if i % 2 == 0 { 0 } else { 255 }))
            .collect();
        let full = moving(load("full.gif".into(), &gif(exactly, &[100; 256])));
        assert_eq!(full.frames.len(), 256);
        assert!(!full.cut, "256 is all of it");
    }

    #[test]
    fn a_single_picture_stays_a_still_and_anything_else_is_refused() {
        let one = gif(vec![grey(255)], &[100]);
        assert!(matches!(load("one.gif".into(), &one), Ok(Loaded::Still(_))));
        let mut png = Vec::new();
        grey(200)
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        match load("still.png".into(), &png) {
            Ok(Loaded::Still(p)) => assert_eq!(p.name, "still.png"),
            _ => panic!("a PNG is a still"),
        }
        assert_eq!(
            load("notes.txt".into(), b"hello").err().as_deref(),
            Some(NOT_A_PICTURE)
        );
    }

    #[test]
    fn fitting_then_turning_into_bits_is_the_same_as_converting() {
        let ramp = test_picture();
        for (t, invert, dither) in [(128, false, true), (40, true, false)] {
            assert_eq!(
                bits(&fit(&ramp.rgba), t, invert, dither),
                convert(&ramp.rgba, t, invert, dither)
            );
        }
        let fitted = fit(&ramp.rgba);
        assert!(fitted.width() <= W as u32 && fitted.height() <= H as u32);
        assert_eq!(
            bits(&fit(&image::RgbaImage::new(0, 0)), 128, false, true),
            vec![0; Screen::IMAGE_BYTES]
        );
    }

    #[test]
    fn the_shot_s_animation_is_twelve_pictures_of_a_tenth_of_a_second() {
        let a = test_animation();
        assert_eq!(summary(a.frames.len(), &a.delays_ms), "12 pictures, 1.2 s");
        assert_ne!(a.frames[0], a.frames[6], "the ball moves");
    }

    #[test]
    fn each_kind_of_screen_has_its_own_tab() {
        let kinds = [
            Screen::Keyboard,
            Screen::Text { text: "a".into() },
            Screen::Clock {
                seconds: false,
                h24: true,
                date: false,
            },
            Screen::Image { pixels: vec![] },
            Screen::Animation {
                frames: vec![],
                delays_ms: vec![],
            },
            Screen::Monitor { items: vec![] },
        ];
        let tabs: Vec<i32> = kinds.iter().map(screen_kind).collect();
        assert_eq!(tabs, [OWN, TEXT, CLOCK, PICTURE, ANIMATION, MONITOR]);
        assert_eq!(
            tabs,
            [0, 1, 2, 3, 4, 5],
            "the order screen.slint lists them in"
        );
    }

    #[test]
    fn readings_go_on_and_off_in_their_own_order_one_to_four_of_them() {
        use MonitorItem::*;
        assert_eq!(readings_of(&Screen::Keyboard), FIRST_READINGS);
        let three = readings_of(&Screen::Monitor {
            items: vec![Ram, Cpu, Clock],
        });
        assert_eq!(
            with_reading(&three, Gpu, true),
            Some(vec![Cpu, Ram, Gpu, Clock]),
            "in the order they're offered"
        );
        let four = [Cpu, CpuTemp, Ram, Clock];
        assert_eq!(with_reading(&four, Gpu, true), None, "five don't fit");
        assert_eq!(
            with_reading(&four, Ram, false),
            Some(vec![Cpu, CpuTemp, Clock])
        );
        assert_eq!(with_reading(&[Cpu], Cpu, false), None, "at least one");
        assert_eq!(with_reading(&[Cpu], Cpu, true), Some(vec![Cpu]));
        for items in [FIRST_READINGS.to_vec(), four.to_vec()] {
            assert_eq!(Screen::Monitor { items }.validate(), Ok(()));
        }
    }
}
