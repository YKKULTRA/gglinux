// SPDX-License-Identifier: GPL-3.0-or-later
//! Reflect: each key of a Reflect zone takes the colour of the part of the screen above
//! it, as GG's Prism Reflect does on keyboards (SteelSeries support article 15898869633165:
//! colours "extracted from designated sampling areas on your monitor" go to the keys).
//!
//! Frames come from a [`ScreenSampler`]. The real one is to be the ScreenCast portal with
//! a PipeWire video stream and a restore token, so the screen is picked once and never
//! asked again. It isn't built yet: reading a PipeWire video stream needs a crate the
//! project doesn't have, so [`start_portal`] says so and nothing is opened. Tests use a fake
//! sampler that serves synthetic frames.
//!
//! Each frame is shrunk to the key grid: the board (the keyboard's own, from
//! [`gg_device_keyboard::layout::for_region`]) is stretched over the whole frame, and a key
//! takes the mean of [`GRID`]² points in the part of the frame where it lies on the board.
//! At most [`FPS`] frames a second are taken, and each key moves part of the way toward
//! its new colour ([`SMOOTHING`]), so a cut in a film doesn't flash the keyboard.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gg_device_keyboard::layout::{Key, WIDTH};
use gg_device_keyboard::Rgb;
use gg_ipc::ReflectStatus;
use tracing::{debug, info};

/// The most frames taken a second.
pub const FPS: u32 = 15;
const SAMPLE_EVERY: Duration = Duration::from_nanos(1_000_000_000 / FPS as u64);
/// How far a key moves toward its new colour with each frame, 0 to 1.
pub const SMOOTHING: f32 = 0.5;
/// Points sampled per key, across and down.
pub const GRID: u32 = 4;
/// How long to wait before trying a sampler that wouldn't start again.
const RETRY: Duration = Duration::from_secs(5);

/// One picture of the screen.
// Only tests build one until the real sampler exists.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Bytes from the start of one row to the next.
    pub stride: usize,
    /// Four bytes a pixel: blue, green, red and one unused (PipeWire's `BGRx`, what the
    /// ScreenCast portal's streams commonly carry). Shared, so a frame is never copied.
    pub data: Arc<[u8]>,
}

impl Frame {
    /// The colour at `(x, y)`, which must be inside the frame.
    fn pixel(&self, x: u32, y: u32) -> [u32; 3] {
        let at = y as usize * self.stride + x as usize * 4;
        match self.data.get(at..at + 3) {
            Some(&[b, g, r]) => [u32::from(r), u32::from(g), u32::from(b)],
            _ => [0; 3], // a short buffer; never index past it
        }
    }

    /// A frame drawn by `f`, for tests and fakes.
    #[cfg(test)]
    pub fn from_fn(width: u32, height: u32, f: impl Fn(u32, u32) -> Rgb) -> Frame {
        let stride = width as usize * 4;
        let mut data = vec![0u8; stride * height as usize];
        for y in 0..height {
            for x in 0..width {
                let c = f(x, y);
                let at = y as usize * stride + x as usize * 4;
                data[at..at + 4].copy_from_slice(&[c.b, c.g, c.r, 0]);
            }
        }
        Frame {
            width,
            height,
            stride,
            data: data.into(),
        }
    }
}

/// Where frames of the screen come from.
pub trait ScreenSampler: Send {
    /// The newest frame since the last call, if one has come. An error ends the capture;
    /// a new sampler is started later.
    fn latest(&mut self) -> Result<Option<Frame>, String>;
}

/// Starts a sampler. The real one is [`start_portal`].
pub type Start = Box<dyn FnMut() -> Result<Box<dyn ScreenSampler>, String> + Send>;

/// The ScreenCast portal's sampler: not built yet, so it says so and opens nothing.
pub fn start_portal() -> Result<Box<dyn ScreenSampler>, String> {
    Err(ReflectStatus::NOT_BUILT.into())
}

/// Each key's mean colour in the part of `frame` under it, with `board` stretched over
/// the whole frame.
pub fn key_colors(frame: &Frame, board: &[Key]) -> HashMap<u8, Rgb> {
    if frame.width == 0 || frame.height == 0 {
        return HashMap::new();
    }
    let rows = board.iter().map(|k| k.3 + 1.0).fold(1.0, f32::max);
    let sx = frame.width as f32 / WIDTH;
    let sy = frame.height as f32 / rows;
    let n = GRID * GRID;
    board
        .iter()
        .map(|&(code, _, x, y, w)| {
            let mut sum = [0u32; 3];
            for i in 0..GRID {
                let px = (x + w * (i as f32 + 0.5) / GRID as f32) * sx;
                let px = (px as u32).min(frame.width - 1);
                for j in 0..GRID {
                    let py = (y + (j as f32 + 0.5) / GRID as f32) * sy;
                    let py = (py as u32).min(frame.height - 1);
                    for (s, c) in sum.iter_mut().zip(frame.pixel(px, py)) {
                        *s += c;
                    }
                }
            }
            let mean = |v: u32| ((v + n / 2) / n) as u8;
            let c = Rgb {
                r: mean(sum[0]),
                g: mean(sum[1]),
                b: mean(sum[2]),
            };
            (code, c)
        })
        .collect()
}

/// The screen's colours under the keys, while a Reflect zone is drawn.
pub struct Reflect {
    start: Start,
    sampler: Option<Box<dyn ScreenSampler>>,
    last_try: Option<Instant>,
    last_sample: Option<Instant>,
    smooth: HashMap<u8, [f32; 3]>,
    colors: HashMap<u8, Rgb>,
    status: ReflectStatus,
}

impl Reflect {
    pub fn new(start: Start) -> Reflect {
        Reflect {
            start,
            sampler: None,
            last_try: None,
            last_sample: None,
            smooth: HashMap::new(),
            colors: HashMap::new(),
            status: ReflectStatus::Off,
        }
    }

    /// With the real sampler.
    pub fn portal() -> Reflect {
        Reflect::new(Box::new(start_portal))
    }

    /// Run the sampler exactly while it's wanted: `Ok(true)` while a Reflect zone is drawn
    /// and its gate is on, `Ok(false)` while none is drawn, `Err(why)` while one is drawn
    /// but mayn't look at the screen.
    pub fn sync(&mut self, wanted: Result<bool, String>, now: Instant) {
        match wanted {
            Ok(true) => {
                let due = self
                    .last_try
                    .is_none_or(|t| now.saturating_duration_since(t) >= RETRY);
                if self.sampler.is_some() || !due {
                    return;
                }
                self.last_try = Some(now);
                match (self.start)() {
                    Ok(sampler) => {
                        info!("Reflect: looking at the screen");
                        self.sampler = Some(sampler);
                        self.last_sample = None;
                        self.status = ReflectStatus::Running;
                    }
                    Err(why) => {
                        if self.status != (ReflectStatus::Stopped { why: why.clone() }) {
                            info!(%why, "Reflect can't look at the screen");
                        }
                        self.status = ReflectStatus::Stopped { why };
                    }
                }
            }
            Ok(false) => self.stop(ReflectStatus::Off),
            Err(why) => self.stop(ReflectStatus::Stopped { why }),
        }
    }

    fn stop(&mut self, status: ReflectStatus) {
        if self.sampler.take().is_some() {
            debug!("Reflect stopped looking at the screen");
        }
        self.last_try = None;
        self.smooth.clear();
        self.colors.clear();
        self.status = status;
    }

    /// Take a frame if one is due, at most [`FPS`] a second, and move the keys toward its
    /// colours.
    pub fn sample(&mut self, board: &[Key], now: Instant) {
        let Some(sampler) = self.sampler.as_mut() else {
            return;
        };
        let soon = self
            .last_sample
            .is_some_and(|t| now.saturating_duration_since(t) < SAMPLE_EVERY);
        if soon {
            return;
        }
        self.last_sample = Some(now);
        match sampler.latest() {
            Ok(Some(frame)) => self.fold(key_colors(&frame, board)),
            Ok(None) => {} // no new frame since the last one
            Err(why) => {
                info!(%why, "Reflect's screen capture ended");
                self.sampler = None;
                self.status = ReflectStatus::Stopped { why };
            }
        }
    }

    fn fold(&mut self, fresh: HashMap<u8, Rgb>) {
        for (code, c) in fresh {
            let new = [f32::from(c.r), f32::from(c.g), f32::from(c.b)];
            let v = self.smooth.entry(code).or_insert(new);
            for (v, new) in v.iter_mut().zip(new) {
                *v += (new - *v) * SMOOTHING;
            }
            let byte = |x: f32| x.round().clamp(0.0, 255.0) as u8;
            self.colors.insert(
                code,
                Rgb {
                    r: byte(v[0]),
                    g: byte(v[1]),
                    b: byte(v[2]),
                },
            );
        }
    }

    /// The keys' colours now; empty until a frame has come.
    pub fn colors(&self) -> &HashMap<u8, Rgb> {
        &self.colors
    }

    pub fn status(&self) -> ReflectStatus {
        self.status.clone()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use gg_device_keyboard::layout::US;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    const RED: Rgb = Rgb { r: 255, g: 0, b: 0 };
    const BLUE: Rgb = Rgb { r: 0, g: 0, b: 255 };
    const GREEN: Rgb = Rgb { r: 0, g: 255, b: 0 };

    /// A fake screen: serves the frames it's given, one per ask, the last one again once
    /// they run out. Like a real stream it lags: the first ask after it starts has no
    /// frame yet.
    pub(crate) struct FakeScreen {
        pub frames: Arc<Mutex<Vec<Frame>>>,
        pub asked: Arc<AtomicUsize>,
        started: bool,
    }

    impl ScreenSampler for FakeScreen {
        fn latest(&mut self) -> Result<Option<Frame>, String> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            if !std::mem::replace(&mut self.started, true) {
                return Ok(None);
            }
            let mut frames = self.frames.lock().unwrap();
            Ok(match frames.len() {
                0 => None,
                1 => frames.first().cloned(),
                _ => Some(frames.remove(0)),
            })
        }
    }

    /// A Reflect on a fake screen serving `frames`, and how often it's been asked for one.
    pub(crate) fn on_fake(frames: Vec<Frame>) -> (Reflect, Arc<AtomicUsize>) {
        let asked = Arc::new(AtomicUsize::new(0));
        let shared = Arc::new(Mutex::new(frames));
        let counter = asked.clone();
        let start: Start = Box::new(move || {
            Ok(Box::new(FakeScreen {
                frames: shared.clone(),
                asked: counter.clone(),
                started: false,
            }) as Box<dyn ScreenSampler>)
        });
        (Reflect::new(start), asked)
    }

    pub(crate) fn solid(c: Rgb) -> Frame {
        Frame::from_fn(64, 24, move |_, _| c)
    }

    /// Left half red, right half blue, with a green band along the top sixth.
    fn halves(width: u32, height: u32) -> Frame {
        Frame::from_fn(width, height, move |x, y| {
            if y < height / 7 {
                GREEN
            } else if x < width / 2 {
                RED
            } else {
                BLUE
            }
        })
    }

    #[test]
    fn each_key_takes_the_colour_of_the_screen_where_it_sits_on_the_board() {
        let colors = key_colors(&halves(640, 360), &US);
        assert_eq!(colors.len(), US.len(), "every key");
        assert_eq!(colors[&0x04], RED, "A: left, below the band");
        assert_eq!(colors[&0x4F], BLUE, "Right arrow: right");
        assert_eq!(colors[&0x3A], GREEN, "F1: the top row");
        assert_eq!(colors[&0xFB], GREEN, "the screen button: top right");
        assert!(key_colors(&Frame::from_fn(0, 0, |_, _| RED), &US).is_empty());
    }

    #[test]
    fn a_short_buffer_reads_as_black_rather_than_past_its_end() {
        let mut f = solid(RED);
        f.data = f.data[..f.data.len() / 2].into();
        let colors = key_colors(&f, &US);
        assert_eq!(colors[&0x3A], RED, "the top half is there");
        assert_eq!(colors[&0x2C], Rgb::default(), "Space: past the end");
    }

    #[test]
    fn the_real_sampler_is_not_built_and_opens_nothing() {
        let err = start_portal().err().unwrap();
        assert_eq!(err, ReflectStatus::NOT_BUILT);
        let mut r = Reflect::portal();
        r.sync(Ok(true), Instant::now());
        assert_eq!(r.status(), ReflectStatus::Stopped { why: err });
        assert!(r.colors().is_empty());
    }

    #[test]
    fn a_sampler_that_wont_start_is_tried_again_after_a_while() {
        let tries = Arc::new(AtomicUsize::new(0));
        let counted = tries.clone();
        let mut r = Reflect::new(Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Err("no screen".into())
        }));
        let t0 = Instant::now();
        r.sync(Ok(true), t0);
        r.sync(Ok(true), t0 + Duration::from_secs(1));
        assert_eq!(tries.load(Ordering::SeqCst), 1);
        r.sync(Ok(true), t0 + RETRY);
        assert_eq!(tries.load(Ordering::SeqCst), 2);
        r.sync(Ok(false), t0 + RETRY);
        assert_eq!(r.status(), ReflectStatus::Off);
        r.sync(Ok(true), t0 + RETRY);
        assert_eq!(tries.load(Ordering::SeqCst), 3, "turned on again: at once");
    }

    #[test]
    fn the_first_frame_comes_late_and_the_keys_wait_for_it() {
        let (mut r, asked) = on_fake(vec![solid(RED)]);
        let t0 = Instant::now();
        r.sync(Ok(true), t0);
        assert_eq!(r.status(), ReflectStatus::Running);
        r.sample(&US, t0);
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert!(r.colors().is_empty(), "no frame on the first ask");
        r.sample(&US, t0 + SAMPLE_EVERY);
        assert_eq!(r.colors()[&0x04], RED, "the first frame: as it is");
    }

    #[test]
    fn keys_move_halfway_to_a_new_colour_with_each_frame() {
        let (mut r, _) = on_fake(vec![solid(RED), solid(BLUE)]);
        let t0 = Instant::now();
        r.sync(Ok(true), t0);
        for i in 0..3 {
            r.sample(&US, t0 + SAMPLE_EVERY * i);
        }
        assert_eq!(
            r.colors()[&0x04],
            Rgb {
                r: 128,
                g: 0,
                b: 128
            },
            "a cut, smoothed"
        );
        for i in 3..20 {
            r.sample(&US, t0 + SAMPLE_EVERY * i);
        }
        assert_eq!(r.colors()[&0x04], BLUE, "and there in a moment");
    }

    #[test]
    fn at_most_fifteen_frames_are_taken_a_second_however_often_it_is_asked() {
        let (mut r, asked) = on_fake(vec![solid(RED)]);
        let t0 = Instant::now();
        r.sync(Ok(true), t0);
        for ms in (0..1000).step_by(5) {
            r.sample(&US, t0 + Duration::from_millis(ms));
        }
        assert_eq!(asked.load(Ordering::SeqCst), FPS as usize);
    }

    #[test]
    fn a_gate_turned_off_or_no_reflect_zone_stops_the_capture_and_forgets_the_colours() {
        let (mut r, asked) = on_fake(vec![solid(RED)]);
        let t0 = Instant::now();
        r.sync(Ok(true), t0);
        r.sample(&US, t0);
        r.sample(&US, t0 + SAMPLE_EVERY);
        assert!(!r.colors().is_empty());
        r.sync(Err("gate off".into()), t0);
        assert_eq!(
            r.status(),
            ReflectStatus::Stopped {
                why: "gate off".into()
            }
        );
        assert!(r.colors().is_empty());
        r.sample(&US, t0 + SAMPLE_EVERY * 2);
        assert_eq!(asked.load(Ordering::SeqCst), 2, "not asked any more");
        r.sync(Ok(false), t0);
        assert_eq!(r.status(), ReflectStatus::Off);
    }

    #[test]
    fn a_capture_that_ends_is_started_again_later() {
        struct Ends;
        impl ScreenSampler for Ends {
            fn latest(&mut self) -> Result<Option<Frame>, String> {
                Err("the stream ended".into())
            }
        }
        let starts = Arc::new(AtomicUsize::new(0));
        let counted = starts.clone();
        let mut r = Reflect::new(Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Ends) as Box<dyn ScreenSampler>)
        }));
        let t0 = Instant::now();
        r.sync(Ok(true), t0);
        r.sample(&US, t0);
        let ended = ReflectStatus::Stopped {
            why: "the stream ended".into(),
        };
        assert_eq!(r.status(), ended);
        r.sync(Ok(true), t0 + Duration::from_secs(1));
        assert_eq!(starts.load(Ordering::SeqCst), 1, "not at once");
        r.sync(Ok(true), t0 + RETRY);
        assert_eq!(starts.load(Ordering::SeqCst), 2);
    }

    /// The CPU it costs: 10 s of full-HD frames at the frame cap (150 frames) take well
    /// under the 66 ms one frame may take, even in a debug build on a busy machine.
    #[test]
    fn ten_seconds_of_full_hd_frames_cost_little_cpu() {
        let frame = halves(1920, 1080);
        let frames = vec![frame; (FPS * 10) as usize];
        let (mut r, asked) = on_fake(frames);
        let t0 = Instant::now();
        r.sync(Ok(true), t0);
        let began = Instant::now();
        for i in 0..=(FPS * 10) {
            r.sample(&US, t0 + SAMPLE_EVERY * i);
        }
        let took = began.elapsed();
        assert_eq!(asked.load(Ordering::SeqCst), (FPS * 10 + 1) as usize);
        assert_eq!(r.colors()[&0x04], RED);
        let per_frame = took / (FPS * 10);
        eprintln!("Reflect: {per_frame:?} a frame, {took:?} for 150 full-HD frames");
        assert!(
            per_frame < Duration::from_millis(5),
            "{per_frame:?} a frame is too much"
        );
    }
}
