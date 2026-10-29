// SPDX-License-Identifier: GPL-3.0-or-later
//! Draw the app to a PNG with Slint's software renderer, without a display:
//!
//! ```text
//! shot <page> <out.png> [width height]
//! ```
//!
//! It connects to whatever daemon `$XDG_RUNTIME_DIR` points at, so with the fake
//! one (`XDG_RUNTIME_DIR=/tmp/gg-fake cargo run -p gglinuxd --example fakemouse`)
//! it draws every page with made-up devices, from a machine nobody is looking at.
//! `SHOT_DO=global.callback[:arg],...` runs callbacks first (a click, in effect),
//! and `SHOT_WAIT_MS` is how long to wait for the daemon's answers.
//!
//! `SHOT_SCROLL` scrolls the page first, as its scroll bar would:
//! - a number of pixels;
//! - `card:N`, the page's Nth card (counted from 1, top to bottom) at the top;
//! - `end:N`, the bottom of the Nth card at the bottom;
//! - `line:N`, the Nth divider line inside the cards at the top;
//!
//! each optionally followed by `+px` or `-px`. `SHOT_FIT=1` makes the picture no
//! taller than the page. Either one prints the cards and lines it found, so a
//! picture can be aimed without guessing.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, Rgb8Pixel};

type Job = Box<dyn FnOnce() + Send>;

struct Shot(Rc<MinimalSoftwareWindow>, Arc<Mutex<Vec<Job>>>);

/// Hands work from the daemon's thread to this one, as the real event loop does.
struct Proxy(Arc<Mutex<Vec<Job>>>);

impl slint::platform::EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        Ok(())
    }
    fn invoke_from_event_loop(&self, job: Job) -> Result<(), slint::EventLoopError> {
        self.0.lock().unwrap().push(job);
        Ok(())
    }
}

impl Platform for Shot {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
    fn new_event_loop_proxy(&self) -> Option<Box<dyn slint::platform::EventLoopProxy>> {
        Some(Box::new(Proxy(self.1.clone())))
    }
}

/// Run what the daemon's answers queued for `ms`.
fn settle(jobs: &Mutex<Vec<Job>>, ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < until {
        let now: Vec<Job> = std::mem::take(&mut *jobs.lock().unwrap());
        for job in now {
            job();
        }
        slint::platform::update_timers_and_animations();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One frame of the window at `width` × `height`.
struct Frame {
    width: usize,
    height: usize,
    pixels: Vec<Rgb8Pixel>,
}

impl Frame {
    fn at(&self, x: usize, y: usize) -> Rgb8Pixel {
        self.pixels[y * self.width + x]
    }
}

fn draw(window: &MinimalSoftwareWindow, width: usize, height: usize) -> Frame {
    window.set_size(slint::PhysicalSize::new(width as u32, height as u32));
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    let mut pixels = vec![Rgb8Pixel::default(); width * height];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width);
    });
    Frame {
        width,
        height,
        pixels,
    }
}

/// Where things are on a page drawn taller than itself, read off the pixels: the
/// page background is the colour of the bottom right corner, the sidebar ends
/// where the bottom row turns to it, and a card is a tall run of anything else
/// just inside the cards' left edge.
#[derive(Debug, Default)]
struct PageMap {
    /// The first column right of the sidebar.
    page_x: usize,
    /// Each card's first and past-the-last row.
    cards: Vec<(usize, usize)>,
    /// The rows of the 1 px divider lines inside the cards.
    lines: Vec<usize>,
    /// Past the page's last drawn row.
    bottom: usize,
}

fn map(f: &Frame) -> PageMap {
    let bg = f.at(f.width - 1, f.height - 1);
    let last = f.height - 1;
    let page_x = (0..f.width).find(|&x| f.at(x, last) == bg).unwrap_or(0);
    let first_drawn = |y: usize| (page_x..f.width).find(|&x| f.at(x, y) != bg);
    let left = (0..f.height).filter_map(&first_drawn).min();
    let bottom = (0..f.height)
        .rev()
        .find(|&y| first_drawn(y).is_some())
        .map_or(0, |y| y + 1);
    let mut m = PageMap {
        page_x,
        bottom,
        ..PageMap::default()
    };
    let Some(left) = left else { return m };

    // Text in the page heading also crosses this column, but never for 40 rows.
    let column = left + 7;
    let mut start = None;
    for y in 0..=f.height {
        let inside = y < f.height && f.at(column, y) != bg;
        match (inside, start) {
            (true, None) => start = Some(y),
            (false, Some(s)) => {
                if y - s >= 40 {
                    m.cards.push((s, y));
                }
                start = None;
            }
            _ => {}
        }
    }

    // A divider spans the card's content from side to side in one colour; one or
    // two rows of it (a line between two pixel rows is drawn on both).
    for &(top, end) in &m.cards {
        let mid = (top + end) / 2;
        let Some(right) = (left..f.width).rev().find(|&x| f.at(x, mid) != bg) else {
            continue;
        };
        let (from, to) = (left + 17, right.saturating_sub(17));
        if to <= from {
            continue;
        }
        let ruled = |y: usize| {
            let c = f.at(from, y);
            c != f.at(column, y) && (from..=to).all(|x| f.at(x, y) == c)
        };
        let mut y = top + 12;
        while y + 12 < end {
            if ruled(y) {
                let run = (y..end).take_while(|&r| ruled(r)).count();
                if run <= 2 {
                    m.lines.push(y);
                }
                y += run;
            } else {
                y += 1;
            }
        }
    }
    m
}

/// The scroll offset `spec` asks for (see the module docs), not yet clamped.
fn scroll_for(spec: &str, m: &PageMap, height: usize) -> Result<i64, String> {
    let cut = spec
        .rfind(['+', '-'])
        .filter(|&i| i > 0)
        .unwrap_or(spec.len());
    let (anchor, nudge) = spec.split_at(cut);
    let nudge: i64 = match nudge.strip_prefix('+') {
        Some(n) => n.parse(),
        None if nudge.is_empty() => Ok(0),
        None => nudge.parse(),
    }
    .map_err(|_| format!("SHOT_SCROLL={spec}: {nudge:?} isn't a number of pixels"))?;
    let nth = |list_len: usize, what: &str| -> Result<usize, String> {
        let n: usize = anchor
            .split_once(':')
            .and_then(|(_, n)| n.parse().ok())
            .filter(|&n| n >= 1)
            .ok_or_else(|| format!("SHOT_SCROLL={spec}: no number after {what}:"))?;
        if n > list_len {
            return Err(format!(
                "SHOT_SCROLL={spec}: the page has {list_len} {what}s"
            ));
        }
        Ok(n - 1)
    };
    // Less than the gap between two cards, so none of the one before shows.
    let gap = 14;
    let at = if anchor.starts_with("card:") {
        m.cards[nth(m.cards.len(), "card")?].0 as i64 - gap
    } else if anchor.starts_with("end:") {
        m.cards[nth(m.cards.len(), "card")?].1 as i64 + gap - height as i64
    } else if anchor.starts_with("line:") {
        m.lines[nth(m.lines.len(), "line")?] as i64 - gap
    } else {
        anchor
            .parse()
            .map_err(|_| format!("SHOT_SCROLL={spec}: not card:N, end:N, line:N or pixels"))?
    };
    Ok(at + nudge)
}

fn save(out: &str, width: usize, height: usize, pixels: &[Rgb8Pixel]) {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    let file = std::fs::File::create(out).expect("the PNG file");
    // The README's pictures live in the repository, so the smaller the better.
    PngEncoder::new_with_quality(
        std::io::BufWriter::new(file),
        CompressionType::Best,
        FilterType::Adaptive,
    )
    .write_image(
        &bytes,
        width as u32,
        height as u32,
        image::ExtendedColorType::Rgb8,
    )
    .expect("the PNG");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let page = args.first().map(String::as_str).unwrap_or("engine");
    let out = args.get(1).map(String::as_str).unwrap_or("shot.png");
    let width: usize = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(1180);
    let height: usize = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(820);
    let wait: u64 = std::env::var("SHOT_WAIT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500);
    let scroll = std::env::var("SHOT_SCROLL").ok().filter(|s| !s.is_empty());
    let fit = std::env::var("SHOT_FIT").is_ok_and(|v| v == "1");

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let jobs: Arc<Mutex<Vec<Job>>> = Arc::default();
    slint::platform::set_platform(Box::new(Shot(window.clone(), jobs.clone())))
        .expect("the platform");
    let ui = gglinux::AppWindow::new().expect("the window");
    let _client = gglinux::start(&ui);
    settle(&jobs, wait);
    let gg = ui.global::<gglinux::Gg>();
    gg.set_page(match page {
        "sonar" => gglinux::Page::Sonar,
        "games" => gglinux::Page::Games,
        "moments" => gglinux::Page::Moments,
        "settings" => gglinux::Page::Settings,
        _ => gglinux::Page::Engine,
    });
    if let Ok(steps) = std::env::var("SHOT_DO") {
        for step in steps.split(',').filter(|s| !s.is_empty()) {
            gglinux::poke(&ui, step);
            settle(&jobs, 400);
        }
    }
    window.set_size(slint::PhysicalSize::new(width as u32, height as u32));
    ui.show().expect("shown");
    slint::platform::update_timers_and_animations();

    if scroll.is_none() && !fit {
        let frame = draw(&window, width, height);
        save(out, width, height, &frame.pixels);
        println!("{out}");
        return;
    }

    // Draw the whole page at once in a window taller than it, then take the
    // sidebar from a window of the asked size and the page from the tall one,
    // moved up by the scroll: what the window shows scrolled that far.
    let tall = draw(&window, width, height.max(8000));
    let m = map(&tall);
    eprintln!("page from x {}, drawn down to y {}", m.page_x, m.bottom);
    for (i, (top, end)) in m.cards.iter().enumerate() {
        let lines: Vec<String> = m
            .lines
            .iter()
            .enumerate()
            .filter(|(_, y)| (*top..*end).contains(*y))
            .map(|(j, y)| format!("line:{} at {y}", j + 1))
            .collect();
        eprintln!(
            "card:{} at {top}..{end} ({} px){}{}",
            i + 1,
            end - top,
            if lines.is_empty() { "" } else { ": " },
            lines.join(", ")
        );
    }
    if m.bottom + 40 >= tall.height {
        eprintln!("the page may go on below {} px", tall.height);
    }
    // As far as a scroll bar goes: the page's end (and its bottom padding) at the bottom.
    let page_end = m.bottom + 36;
    let height = if fit { height.min(page_end) } else { height };
    let most = page_end.min(tall.height).saturating_sub(height) as i64;
    let top = match scroll.as_deref().map(|s| scroll_for(s, &m, height)) {
        None => 0,
        Some(Ok(at)) => at.clamp(0, most) as usize,
        Some(Err(e)) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let side = draw(&window, width, height);
    let mut pixels = side.pixels;
    for y in 0..height {
        let row = (top + y) * width;
        pixels[y * width + m.page_x..(y + 1) * width]
            .copy_from_slice(&tall.pixels[row + m.page_x..row + width]);
    }
    save(out, width, height, &pixels);
    println!("{out} (scrolled {top} px)");
}
