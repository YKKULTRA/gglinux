// SPDX-License-Identifier: GPL-3.0-or-later
//! Drawing a screen frame on the 128 × 40 screen, with the keyboard's 8 × 8 font.
//!
//! The SDK says what a frame holds, not where each pixel goes: the layout is gglinux's own.
//! - A picture is drawn as sent: rows from the top, the leftmost pixel in each byte's high
//!   bit (the SDK's "most significant bit first" packing).
//! - Text and bars are rows. A text line takes 1 row, or up to `1 + wrap` rows when it's
//!   longer than a row holds (it breaks between words where it can); what still doesn't
//!   fit is cut. A bar takes 1 row. At most [`MAX_ROWS`] rows show.
//! - A single-line frame whose text fits one row is drawn as big as it fits (up to three
//!   times the font's size); everything else at the font's size.
//! - The rows are stacked with 2 pixels between them (none when they wouldn't fit
//!   otherwise), and the stack is centred top to bottom. Each text row is centred left to
//!   right; `bold` draws it twice, one pixel apart.
//! - With an `icon-id`, the leftmost [`ICON_WIDTH`] columns are the icon's (left blank:
//!   gglinux has no icon pictures) and the rows use the rest.
//! - A bar is an outline [`BAR_HEIGHT`] pixels high across the row, a pixel in from each
//!   side, filled from the left with a pixel's gap inside the outline.

use gg_device_keyboard::oled::{HEIGHT, WIDTH};
use gg_device_keyboard::Bitmap;
use serde_json::Value;

use super::{arg, Content, Data, Frame, Line, LineKind, Source};

/// Columns an icon takes on the left, as the SDK says.
pub const ICON_WIDTH: usize = 32;
/// The most rows of text and bars a frame shows: the screen's height in font rows.
pub const MAX_ROWS: usize = HEIGHT / 8;
/// A progress bar's height.
pub const BAR_HEIGHT: usize = 8;
const GAP: usize = 2;
const GLYPH: usize = 8;

/// One row of a frame as drawn.
#[derive(Debug, Clone, PartialEq)]
enum Row {
    Text { text: String, bold: bool },
    Bar(u8),
}

/// The frame's picture for this event data.
pub fn draw(frame: &Frame, data: &Data) -> Bitmap {
    match &frame.content {
        // The event's own picture, sent for "any image binding", replaces the handler's.
        Content::Image(own) => data
            .image
            .as_deref()
            .or(own.as_deref())
            .and_then(|rows| Bitmap::from_rows(rows).ok())
            .unwrap_or_default(),
        Content::Single(line) => layout(std::slice::from_ref(line), frame.icon, true, data),
        Content::Lines(lines) => layout(lines, frame.icon, false, data),
    }
}

/// The rows of `lines`, at most [`MAX_ROWS`].
fn rows(lines: &[Line], columns: usize, data: &Data) -> Vec<Row> {
    let mut rows = Vec::new();
    for line in lines {
        match &line.kind {
            LineKind::Text {
                prefix,
                suffix,
                bold,
                wrap,
            } => {
                let text = format!("{prefix}{}{suffix}", value_text(&line.source, data));
                for text in wrapped(&text, columns, 1 + *wrap as usize) {
                    rows.push(Row::Text { text, bold: *bold });
                }
            }
            LineKind::Bar => rows.push(Row::Bar(percent(&line.source, data))),
            LineKind::Nothing => {}
        }
    }
    rows.truncate(MAX_ROWS);
    rows
}

/// Lay `lines` out and draw them.
fn layout(lines: &[Line], icon: u32, single: bool, data: &Data) -> Bitmap {
    let left = if icon != 0 { ICON_WIDTH } else { 0 };
    let width = WIDTH - left;
    let rows = rows(lines, width / GLYPH, data);
    let scale = match rows.as_slice() {
        [Row::Text { text, .. }] if single => (1..=3)
            .rev()
            .find(|s| text.chars().count() * GLYPH * s <= width && GLYPH * s <= HEIGHT)
            .unwrap_or(1),
        _ => 1,
    };
    let height = |r: &Row| match r {
        Row::Text { .. } => GLYPH * scale,
        Row::Bar(_) => BAR_HEIGHT,
    };
    let total: usize = rows.iter().map(height).sum();
    let gap = if total + GAP * rows.len().saturating_sub(1) <= HEIGHT {
        GAP
    } else {
        0
    };
    let mut y = HEIGHT.saturating_sub(total + gap * rows.len().saturating_sub(1)) / 2;
    let mut b = Bitmap::default();
    for row in &rows {
        match row {
            Row::Text { text, bold } => {
                let w = text.chars().count() * GLYPH * scale;
                let x = left + width.saturating_sub(w) / 2;
                b.draw_text(x, y, text, scale);
                if *bold {
                    b.draw_text(x + 1, y, text, scale);
                }
            }
            Row::Bar(p) => bar(&mut b, left, width, y, *p),
        }
        y += height(row) + gap;
    }
    b
}

/// A bar across `width` columns from `left`, with its top at `y`, filled `percent` %.
fn bar(b: &mut Bitmap, left: usize, width: usize, y: usize, percent: u8) {
    let (x0, x1) = (left + 1, left + width - 2);
    let (y0, y1) = (y, y + BAR_HEIGHT - 1);
    for x in x0..=x1 {
        b.set(x, y0, true);
        b.set(x, y1, true);
    }
    for yy in y0..=y1 {
        b.set(x0, yy, true);
        b.set(x1, yy, true);
    }
    let inner = x1 - x0 - 3;
    let filled = (inner * usize::from(percent) + 50) / 100;
    for x in x0 + 2..x0 + 2 + filled {
        for yy in y0 + 2..=y1 - 2 {
            b.set(x, yy, true);
        }
    }
}

/// `text` in rows of at most `columns` characters, at most `max` rows, broken between words
/// where it can be; what doesn't fit is cut.
fn wrapped(text: &str, columns: usize, max: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= columns || max <= 1 {
        return vec![chars.iter().take(columns).collect()];
    }
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in text.split(' ') {
        let mut word: Vec<char> = word.chars().collect();
        let fits = row.chars().count() + usize::from(!row.is_empty()) + word.len() <= columns;
        if fits {
            if !row.is_empty() {
                row.push(' ');
            }
            row.extend(word);
            continue;
        }
        if !row.is_empty() {
            rows.push(std::mem::take(&mut row));
        }
        while word.len() > columns {
            rows.push(word.drain(..columns).collect());
        }
        row = word.into_iter().collect();
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows.truncate(max);
    rows
}

/// A line's value, as it reads on the screen.
fn value_text(source: &Source, data: &Data) -> String {
    match value(source, data) {
        Some(Value::String(s)) => s,
        Some(Value::Number(n)) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(f)) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
            _ => n.to_string(),
        },
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// A line's value as a bar's fill, 0 to 100.
fn percent(source: &Source, data: &Data) -> u8 {
    let n = match value(source, data) {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    n.filter(|f| f.is_finite())
        .map_or(0, |f| f.round().clamp(0.0, 100.0) as u8)
}

fn value(source: &Source, data: &Data) -> Option<Value> {
    match source {
        Source::Value => data.value.map(Value::from),
        Source::FrameKey(k) => data.frame.get(k).cloned(),
        Source::Arg(e) => arg::eval(e, &arg::this(data.value, &data.frame)),
        Source::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_breaks_between_words_and_long_words_anywhere() {
        assert_eq!(wrapped("Got 15 kills", 16, 1), vec!["Got 15 kills"]);
        assert_eq!(
            wrapped("this is some text that wraps", 16, 2),
            vec!["this is some", "text that wraps"]
        );
        assert_eq!(
            wrapped("this is some text that wraps around", 16, 2),
            vec!["this is some", "text that wraps"],
            "the third row is cut"
        );
        assert_eq!(
            wrapped("abcdefghijklmnopqrstuvwxyz", 16, 1),
            vec!["abcdefghijklmnop"],
            "no wrap: cut at the edge"
        );
        assert_eq!(
            wrapped("abcdefghijklmnopqrstuvwxyz", 16, 3),
            vec!["abcdefghijklmnop", "qrstuvwxyz"]
        );
    }

    #[test]
    fn values_read_as_text_and_as_a_fill() {
        let frame = serde_json::json!({ "t": "hi", "n": 88, "f": 2.0, "half": 49.5, "s": "70" });
        let data = Data {
            value: Some(15),
            frame: frame.as_object().unwrap().clone(),
            image: None,
        };
        let key = |k: &str| Source::FrameKey(k.into());
        assert_eq!(value_text(&Source::Value, &data), "15");
        assert_eq!(value_text(&key("t"), &data), "hi");
        assert_eq!(value_text(&key("f"), &data), "2");
        assert_eq!(value_text(&key("missing"), &data), "");
        assert_eq!(value_text(&Source::Unknown, &data), "");
        assert_eq!(percent(&key("n"), &data), 88);
        assert_eq!(percent(&key("half"), &data), 50);
        assert_eq!(percent(&key("s"), &data), 70);
        assert_eq!(percent(&key("t"), &data), 0);
        let over = Data {
            value: Some(250),
            ..Data::default()
        };
        assert_eq!(percent(&Source::Value, &over), 100);
    }
}
