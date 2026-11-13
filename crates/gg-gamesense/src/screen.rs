// SPDX-License-Identifier: GPL-3.0-or-later
//! GameSense screen handlers (SDK `json-handlers-screen.md`) on the Apex Pro TKL Gen 3's
//! 128 × 40 screen: parsed from a handler, played frame by frame from the moment an event
//! starts them ([`Show`]), and drawn ([`draw`]).
//!
//! A handler matches this screen with device-type `screened` (any screen) or
//! `screened-128x40`; other `screened-WIDTHxHEIGHT` handlers are for other screens. Its
//! `datas` are frames, or ranges of event values each with their own frames. A frame is
//! one line of text or a progress bar (the single-line form), several of them (`lines`), or
//! a raw 1-bit picture (`image-data`, or the event's own `image-data-128x40`). Each frame
//! shows for its `length-millis` (0: until another event writes the screen), and the last
//! frame's `repeats` says how often the list plays. After the last frame the screen goes
//! back to what it showed before (the user's own screen).
//!
//! Left out, and listed in the handler's notes for the user: `icon-id` pictures (the SDK's
//! icon list isn't part of the pinned docs; the 32 columns an icon takes stay blank), `arg`
//! expressions other than the doc's own forms ([`arg`]), and pictures for another screen size.

pub mod arg;
mod draw;

use serde_json::{Map, Value};

pub use draw::{draw, BAR_HEIGHT, ICON_WIDTH, MAX_ROWS};
use gg_device_keyboard::Bitmap;

/// This screen's device-type, for handlers that must match its size.
pub const EXACT_DEVICE: &str = "screened-128x40";
/// Bytes in a 128 × 40 picture, one bit a pixel.
pub const IMAGE_BYTES: usize = 640;
/// The event frame key that carries a picture for this screen.
pub const IMAGE_KEY: &str = "image-data-128x40";
/// The longest text kept from an event's frame for a screen line.
pub const MAX_TEXT: usize = 256;

/// What a screen handler shows.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenSpec {
    pub datas: Datas,
    /// Matched as `screened-128x40`: preferred over a `screened` handler of the same event.
    pub exact: bool,
    /// What it asks for that this server leaves out, in words for the user.
    pub notes: Vec<String>,
}

/// The frames, for every value or by range of values.
#[derive(Debug, Clone, PartialEq)]
pub enum Datas {
    Frames(Vec<Frame>),
    /// `(low, high, frames)`, both ends included; the first range holding the value is used.
    Ranges(Vec<(i64, i64, Vec<Frame>)>),
}

/// One picture on the screen, and how long it stays.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub content: Content,
    /// 0: it stays until another event writes the screen.
    pub length_ms: u64,
    /// An SDK event icon, drawn left of the text: here a blank 32-pixel column.
    pub icon: u32,
    /// How often the frames play; read from the last frame only.
    pub repeats: Repeats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeats {
    Once,
    Forever,
    /// The frames play this many times in all.
    Times(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// The single-line form: one line, drawn as big as it fits.
    Single(Line),
    /// The multi-line form, one line under the other.
    Lines(Vec<Line>),
    /// A raw picture, 640 bytes; `None` draws dark unless the event sends its own.
    Image(Option<Vec<u8>>),
}

/// A line of a frame: what it shows, and where its value comes from.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub kind: LineKind,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LineKind {
    /// `prefix`, the value, `suffix`; `wrap` more rows if it's too long for one.
    Text {
        prefix: String,
        suffix: String,
        bold: bool,
        wrap: u32,
    },
    /// The value, 0 to 100, as a bar.
    Bar,
    /// Neither text nor a bar: it takes no room.
    Nothing,
}

/// Where a line's value comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// The event's `value`.
    Value,
    /// An entry of the event's `frame`.
    FrameKey(String),
    /// An `arg` expression.
    Arg(arg::Expr),
    /// An `arg` this server can't work out: no value.
    Unknown,
}

/// The event data a show draws from, as it was when the show started.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Data {
    pub value: Option<i64>,
    /// The frame entries its lines read.
    pub frame: Map<String, Value>,
    /// The event's own picture (`image-data-128x40`).
    pub image: Option<Vec<u8>>,
}

impl ScreenSpec {
    /// The handler as this screen shows it: `Ok(None)` for another screen (another size or
    /// zone), an error for one this screen should show but can't read.
    pub fn parse(device: &str, o: &Map<String, Value>) -> Result<Option<ScreenSpec>, String> {
        let exact = device == EXACT_DEVICE;
        if !(exact || device == "screened") {
            return Ok(None);
        }
        if o.get("zone").is_some_and(|z| z.as_str() != Some("one")) {
            return Ok(None);
        }
        if o.get("mode").and_then(Value::as_str) != Some("screen") {
            return Err("a screen handler's mode must be screen".into());
        }
        let list = o
            .get("datas")
            .and_then(Value::as_array)
            .filter(|l| !l.is_empty())
            .ok_or("a screen handler needs a list of frames in datas")?;
        let mut notes = Vec::new();
        let ranged = list
            .first()
            .and_then(Value::as_object)
            .is_some_and(|f| f.contains_key("low") || f.contains_key("high"));
        let datas = if ranged {
            Datas::Ranges(
                list.iter()
                    .map(|r| {
                        let low = r.get("low").and_then(Value::as_i64);
                        let high = r.get("high").and_then(Value::as_i64);
                        let (Some(low), Some(high)) = (low, high) else {
                            return Err("a screen range needs whole-number low and high".into());
                        };
                        let frames = r
                            .get("datas")
                            .and_then(Value::as_array)
                            .ok_or("a screen range needs a list of frames in datas")?;
                        Ok((low, high, frames_of(frames, exact, &mut notes)?))
                    })
                    .collect::<Result<_, String>>()?,
            )
        } else {
            Datas::Frames(frames_of(list, exact, &mut notes)?)
        };
        Ok(Some(ScreenSpec {
            datas,
            exact,
            notes,
        }))
    }

    /// The frames for `value`: all of them, or those of the first range that holds it.
    pub fn frames(&self, value: i64) -> Option<&[Frame]> {
        match &self.datas {
            Datas::Frames(f) => Some(f.as_slice()),
            Datas::Ranges(r) => r
                .iter()
                .find(|(low, high, _)| (*low..=*high).contains(&value))
                .map(|(_, _, f)| f.as_slice()),
        }
        .filter(|f| !f.is_empty())
    }

    fn all_frames(&self) -> impl Iterator<Item = &Frame> {
        let lists: Vec<&Vec<Frame>> = match &self.datas {
            Datas::Frames(f) => vec![f],
            Datas::Ranges(r) => r.iter().map(|(_, _, f)| f).collect(),
        };
        lists.into_iter().flatten()
    }

    /// The frame entries its lines read, so an event keeps only those.
    pub fn frame_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        for frame in self.all_frames() {
            let lines: &[Line] = match &frame.content {
                Content::Single(l) => std::slice::from_ref(l),
                Content::Lines(l) => l,
                Content::Image(_) => &[],
            };
            for line in lines {
                match &line.source {
                    Source::FrameKey(k) => keys.push(k.clone()),
                    Source::Arg(e) => keys.extend(arg::frame_keys(e)),
                    Source::Value | Source::Unknown => {}
                }
            }
        }
        keys.sort();
        keys.dedup();
        keys
    }

    /// Whether a frame is a picture, so an event's own picture is kept.
    pub fn has_image(&self) -> bool {
        self.all_frames()
            .any(|f| matches!(f.content, Content::Image(_)))
    }
}

fn frames_of(list: &[Value], exact: bool, notes: &mut Vec<String>) -> Result<Vec<Frame>, String> {
    list.iter()
        .map(|f| {
            let f = f.as_object().ok_or("a screen frame must be an object")?;
            frame(f, exact, notes)
        })
        .collect()
}

fn note(notes: &mut Vec<String>, text: String) {
    if !notes.contains(&text) {
        notes.push(text);
    }
}

fn frame(f: &Map<String, Value>, exact: bool, notes: &mut Vec<String>) -> Result<Frame, String> {
    // Some games send whole numbers as floats (250.0).
    let length_ms = match f.get("length-millis") {
        None => 0,
        Some(v) => v
            .as_f64()
            .filter(|ms| ms.is_finite() && *ms >= 0.0)
            .ok_or("length-millis must be a number of milliseconds, 0 or more")?
            .round() as u64,
    };
    let repeats = match f.get("repeats") {
        None | Some(Value::Bool(false)) => Repeats::Once,
        Some(Value::Bool(true)) => Repeats::Forever,
        Some(v) => match v.as_u64() {
            Some(0) => Repeats::Forever,
            Some(n) => Repeats::Times(n.min(u64::from(u32::MAX)) as u32),
            None => return Err("repeats must be true, false or a whole number".into()),
        },
    };
    let icon = f
        .get("icon-id")
        .and_then(Value::as_u64)
        .map_or(0, |n| n.min(u64::from(u32::MAX)) as u32);
    let content = if let Some(lines) = f.get("lines") {
        let lines = lines
            .as_array()
            .filter(|l| !l.is_empty())
            .ok_or("lines must be a list of one or more lines")?;
        Content::Lines(
            lines
                .iter()
                .map(|l| line(l.as_object().ok_or("a line must be an object")?, notes))
                .collect::<Result<_, String>>()?,
        )
    } else if let Some(image) = f.get("image-data") {
        Content::Image(image_data(image, exact, notes)?)
    } else {
        let single = line(f, notes)?;
        if single.kind == LineKind::Nothing {
            Content::Image(None) // `has-text: false` alone: the event's own picture
        } else {
            Content::Single(single)
        }
    };
    if icon != 0 && !matches!(content, Content::Image(_)) {
        note(
            notes,
            format!("icon {icon} is left out: gglinux has no pictures of the GameSense icons"),
        );
    }
    Ok(Frame {
        content,
        length_ms,
        icon,
        repeats,
    })
}

fn image_data(v: &Value, exact: bool, notes: &mut Vec<String>) -> Result<Option<Vec<u8>>, String> {
    let bytes = picture(v);
    match bytes {
        Some(b) if b.len() == IMAGE_BYTES => Ok(Some(b)),
        Some(b) if b.is_empty() => Ok(None), // the SDK's default: the event sends the picture
        _ if exact => Err(format!(
            "image-data for a 128x40 screen must be {IMAGE_BYTES} numbers from 0 to 255"
        )),
        _ => {
            note(
                notes,
                "a picture made for another screen size is shown dark".into(),
            );
            Ok(None)
        }
    }
}

/// A list of bytes, each 0 to 255; `None` if it's anything else.
pub fn picture(v: &Value) -> Option<Vec<u8>> {
    v.as_array()?
        .iter()
        .map(|b| b.as_u64().filter(|&b| b <= 255).map(|b| b as u8))
        .collect()
}

fn line(o: &Map<String, Value>, notes: &mut Vec<String>) -> Result<Line, String> {
    let flag = |k: &str| match o.get(k) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{k} must be true or false")),
    };
    let text = |k: &str| match o.get(k) {
        None => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(format!("{k} must be text")),
    };
    let has_text = flag("has-text")?;
    let kind = if flag("has-progress-bar")? == Some(true) && has_text != Some(true) {
        LineKind::Bar
    } else if has_text.unwrap_or(true) {
        LineKind::Text {
            prefix: text("prefix")?,
            suffix: text("suffix")?,
            bold: flag("bold")?.unwrap_or(false),
            wrap: o
                .get("wrap")
                .and_then(Value::as_u64)
                .map_or(0, |n| n.min(MAX_ROWS as u64) as u32),
        }
    } else {
        LineKind::Nothing
    };
    let source = match (o.get("arg"), o.get("context-frame-key")) {
        (Some(a), _) => match a.as_str().and_then(arg::parse) {
            Some(e) => Source::Arg(e),
            None => {
                note(
                    notes,
                    format!("arg {a} is beyond what gglinux works out, so its line shows no value"),
                );
                Source::Unknown
            }
        },
        (None, Some(Value::String(k))) => Source::FrameKey(k.clone()),
        (None, Some(_)) => return Err("context-frame-key must be text".into()),
        (None, None) => Source::Value,
    };
    Ok(Line { kind, source })
}

/// A screen handler's frames playing from `started` (seconds on the registry's clock).
#[derive(Debug, Clone, PartialEq)]
pub struct Show {
    pub frames: Vec<Frame>,
    pub data: Data,
    pub started: f64,
}

impl Show {
    /// The frame shown at `now`, and the seconds until that changes by itself (`None`: it
    /// stays until another event); `None` once the frames have played out.
    pub fn at(&self, now: f64) -> Option<(usize, Option<f64>)> {
        // Whole milliseconds: a frame ends exactly when its time is up, never a hair after.
        let ms = ((now - self.started).max(0.0) * 1000.0) as u64;
        let left = |end: u64, at: u64| Some((end - at) as f64 / 1000.0);
        // The first time through. A frame with no length holds: those after it never show.
        let mut start = 0;
        for (i, f) in self.frames.iter().enumerate() {
            if f.length_ms == 0 {
                return Some((i, None));
            }
            if ms < start + f.length_ms {
                return Some((i, left(start + f.length_ms, ms)));
            }
            start += f.length_ms;
        }
        // Every frame has a length, and the first play is over: `repeats` says if it goes on.
        let pass = start;
        let plays = match self.frames.last()?.repeats {
            Repeats::Once => 1,
            Repeats::Times(n) => u64::from(n.max(1)),
            Repeats::Forever => u64::MAX,
        };
        if ms / pass >= plays {
            return None;
        }
        let within = ms % pass;
        let mut start = 0;
        for (i, f) in self.frames.iter().enumerate() {
            if within < start + f.length_ms {
                return Some((i, left(start + f.length_ms, within)));
            }
            start += f.length_ms;
        }
        None
    }

    /// The picture at `now`, and the seconds until it changes by itself.
    pub fn picture(&self, now: f64) -> Option<(Bitmap, Option<f64>)> {
        let (i, next) = self.at(now)?;
        Some((draw(&self.frames[i], &self.data), next))
    }
}

#[cfg(test)]
mod tests;
